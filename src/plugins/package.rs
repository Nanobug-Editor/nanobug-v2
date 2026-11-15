//! Validated package staging, compatibility, and atomic local install records.

use super::pipe::{Worker, check_scripts};
mod rule_paths;
use anyhow::{Context, Result, bail, ensure};
use nanobug_plugin_sdk::{API, Permission, Reply, Request};
pub use rule_paths::ApprovedRulePaths;
#[cfg(windows)]
pub(crate) use rule_paths::windows_component as windows_rule_component;
pub(crate) use rule_paths::{ApprovedRuleFile, DirectoryId};
use semver::{Op, Version, VersionReq};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{self, BufReader, BufWriter, Cursor, Read, Seek, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tempfile::{NamedTempFile, TempDir};
use zip::ZipArchive;

const MANIFEST: &str = "nanobug-plugin.toml";
const MAX_ARCHIVE: u64 = 128 * 1024 * 1024;
const MAX_UNPACKED: u64 = 256 * 1024 * 1024;
pub(super) const MAX_FILE: u64 = 64 * 1024 * 1024;
const MAX_MANIFEST: u64 = 256 * 1024;
const MAX_INDEX: u64 = 16 * 1024 * 1024;
/// Registry presence also marks that this installation has initialized its package store.
const INSTALLED_INDEX: &str = "installed.json";
const STAGING_DIRECTORY: &str = "package-staging";
const STAGING_PREFIX: &str = "stage-";
const STAGING_ID_LEN: usize = 10;
/// Current contract identifier for the UI SDK's opt-in experimental interface.
pub const UI_CONTRACT: &str = "ui-2026-11-12-1";

/// Match the canonical name and ID exactly. Display translations are independent.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Identity {
    pub name: String,
    pub id: String,
}

impl Identity {
    /// Check a complete ASCII ID without trimming or restricting interior hyphens.
    pub fn new(name: String, id: String) -> Result<Self> {
        ensure!(
            !name.trim().is_empty() && !name.chars().any(char::is_control),
            "Invalid plugin name"
        );
        ensure!(
            valid_id(&id),
            "Plugin ID must use lowercase letters, digits, or interior hyphens"
        );
        Ok(Self { name, id })
    }

    /// Stable, filesystem-safe namespace based on both values, even when names contain separators.
    pub fn namespace(&self) -> String {
        let mut hash = Sha256::new();
        hash.update((self.name.len() as u64).to_le_bytes());
        hash.update(self.name.as_bytes());
        hash.update(self.id.as_bytes());
        format!("{:x}", hash.finalize())
    }
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('-')
        && !value.ends_with('-')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// Publicly declared editor compatibility.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Engines {
    pub nanobug: VersionReq,
}

/// One independently instantiated module from a package.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Module {
    pub id: String,
    pub wasm: Option<String>,
    pub rule: Option<String>,
    #[serde(default)]
    pub interactive: bool,
    /// The module may propose ordinary settings drafts from its SDK surface.
    #[serde(default)]
    pub settings_ui: bool,
    /// Notify this module when a saved secret changes in its own installation.
    #[serde(default)]
    pub credential_events: bool,
    /// Use the host's persistent rust-analyzer for Rust language queries and live diagnostics.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub rust_analyzer: bool,
    /// Use a host-supported language server for this module's document extensions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language_server: Option<crate::plugins::tools::rust_analyzer::Server>,
    #[serde(default)]
    pub provides: BTreeSet<Provider>,
    /// Document features apply to these extensions. An empty set matches any document.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub document_extensions: BTreeSet<String>,
}

impl Module {
    /// Check the active path without consulting the filesystem.
    pub fn accepts_document(&self, path: Option<&Path>) -> bool {
        self.document_extensions.is_empty()
            || path
                .and_then(Path::extension)
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| {
                    self.document_extensions
                        .contains(&extension.to_ascii_lowercase())
                })
    }
}

/// A document feature contributed by an enabled module.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Format,
    Complete,
    Diagnose,
    Definition,
    /// Provide symbol details from a native language server.
    Hover,
    /// Receive document changes and supply highlighting, folds and outline entries.
    Document,
}

/// Static command metadata can be displayed without starting a plugin.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommandDecl {
    pub id: String,
    pub title: String,
    pub module: String,
    pub key: Option<String>,
    /// Empty means all non-text workspace contexts.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub contexts: BTreeSet<CommandContext>,
}

/// Workspace input contexts where a declared command may run.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandContext {
    EditorNormal,
    EditorVisual,
    Explorer,
}

impl CommandDecl {
    pub fn applies(&self, context: CommandContext) -> bool {
        self.contexts.is_empty() || self.contexts.contains(&context)
    }

    pub fn shares_context(&self, other: &Self) -> bool {
        self.contexts.is_empty()
            || other.contexts.is_empty()
            || !self.contexts.is_disjoint(&other.contexts)
    }
}

/// Static setting with an explicit scope and JSON-compatible default.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SettingDecl {
    pub title: String,
    /// A secret has no package-supplied value and leaves this field out of TOML.
    #[serde(default)]
    pub default: Value,
    #[serde(default)]
    pub description: String,
    /// Old keys with the same meaning and type. Existing values stay under their original key until Apply.
    #[serde(default)]
    pub renamed_from: Vec<String>,
    #[serde(default)]
    pub project: bool,
    #[serde(default)]
    pub secret: bool,
}

impl SettingDecl {
    /// Ordinary values use the declared JSON type. A null default permits any JSON value.
    pub fn accepts_value(&self, value: &Value) -> bool {
        self.default.is_null()
            || std::mem::discriminant(value) == std::mem::discriminant(&self.default)
    }
}

/// A native executable contributed by a plugin, optionally replaced by an existing user tool.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolDecl {
    pub executable: Option<String>,
    pub path_setting: Option<String>,
    /// HTTPS archives keyed by the host platform, for example windows-x86_64.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub downloads: BTreeMap<String, ToolDownload>,
    /// An opt-in rebuildable service speaking the SDK snapshot protocol.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<ToolBackground>,
}

/// An immutable native tool archive and its relative executable path.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolDownload {
    pub url: String,
    pub sha256: String,
    pub executable: String,
}

impl ToolDownload {
    pub(super) fn validate(&self) -> Result<()> {
        let host = self
            .url
            .strip_prefix("https://")
            .and_then(|url| url.split(['/', '?', '#']).next())
            .unwrap_or("");
        ensure!(
            !host.is_empty()
                && !host.contains(['@', '\\'])
                && self.url.len() <= 2048
                && self.url.bytes().all(|byte| byte.is_ascii_graphic()),
            "Native tool download requires an HTTPS URL without credentials"
        );
        validate_digest(&self.sha256)?;
        portable_path(&self.executable)?;
        Ok(())
    }
}

/// Fixed startup arguments for a service that can rebuild from document snapshots.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolBackground {
    #[serde(default)]
    pub args: Vec<String>,
}

/// Package-owned localized display text. Empty fields fall back individually.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Translation {
    pub name: String,
    pub description: String,
}

/// Files and static contributions referenced by the version-one manifest.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub name: String,
    pub id: String,
    pub version: Version,
    pub engines: Engines,
    #[serde(default = "api_version")]
    pub api: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub platforms: BTreeSet<String>,
    #[serde(default)]
    pub permissions: BTreeSet<Permission>,
    /// Exact external files requested by conditional JavaScript. Approval is host-local.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub rule_paths: BTreeSet<String>,
    #[serde(default)]
    pub experiments: BTreeMap<String, String>,
    #[serde(default)]
    pub modules: Vec<Module>,
    #[serde(default)]
    pub settings: BTreeMap<String, SettingDecl>,
    #[serde(default)]
    pub commands: Vec<CommandDecl>,
    #[serde(default)]
    pub tools: BTreeMap<String, ToolDecl>,
    #[serde(default)]
    pub public_services: BTreeMap<String, String>,
    #[serde(default)]
    pub translations: BTreeMap<String, Translation>,
}

fn api_version() -> String {
    API.into()
}

impl Manifest {
    /// Return the canonical identity, independent of locale.
    pub fn identity(&self) -> Identity {
        Identity {
            name: self.name.clone(),
            id: self.id.clone(),
        }
    }

    /// Per-field fallback to canonical manifest text.
    pub fn display(&self, language: &str) -> (&str, &str) {
        let translation = self.translations.get(language);
        (
            translation
                .map(|value| value.name.as_str())
                .filter(|value| !value.is_empty())
                .unwrap_or(&self.name),
            translation
                .map(|value| value.description.as_str())
                .filter(|value| !value.is_empty())
                .unwrap_or(&self.description),
        )
    }

    /// Validate cross-references without running any package code.
    fn validate(&self, entries: &BTreeMap<String, String>) -> Result<()> {
        Identity::new(self.name.clone(), self.id.clone())?;
        rule_paths::validate(
            &self.rule_paths,
            self.permissions.contains(&Permission::ReadRulePaths),
        )?;
        let mut modules = BTreeSet::new();
        for module in &self.modules {
            ensure!(
                module.document_extensions.iter().all(|extension| {
                    !extension.is_empty()
                        && extension
                            .bytes()
                            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
                }),
                "Document extensions must use lowercase ASCII letters or digits"
            );
            ensure!(
                !module.provides.contains(&Provider::Document)
                    || module.wasm.is_some()
                        && self.permissions.contains(&Permission::ReadDocument),
                "Document presentation requires a WASM module with document access"
            );
            ensure!(
                !module.rust_analyzer
                    || module.wasm.is_some()
                        && module.document_extensions == BTreeSet::from(["rs".into()])
                        && self.permissions.contains(&Permission::ReadDocument)
                        && self.permissions.contains(&Permission::RunTool)
                        && [
                            Provider::Complete,
                            Provider::Diagnose,
                            Provider::Definition,
                            Provider::Hover,
                        ]
                        .iter()
                        .any(|provider| module.provides.contains(provider)),
                "rust-analyzer requires a Rust language module with document and native tool access"
            );
            ensure!(
                module.language_server.is_none()
                    || !module.rust_analyzer
                        && module.wasm.is_some()
                        && !module.document_extensions.is_empty()
                        && self.permissions.contains(&Permission::ReadDocument)
                        && self.permissions.contains(&Permission::RunTool)
                        && [
                            Provider::Complete,
                            Provider::Diagnose,
                            Provider::Definition,
                            Provider::Hover,
                        ]
                        .iter()
                        .any(|provider| module.provides.contains(provider)),
                "Language servers require a document module with native tool access"
            );
            ensure!(
                !module.provides.contains(&Provider::Hover)
                    || module.rust_analyzer
                    || module.language_server.is_some(),
                "Hover details require a native language server"
            );
            ensure!(
                !module.settings_ui || module.interactive && module.wasm.is_some(),
                "Custom settings require an interactive module"
            );
            ensure!(
                !module.credential_events
                    || module.wasm.is_some() && self.permissions.contains(&Permission::Credentials),
                "Credential notifications require a WASM module with credential access"
            );
            ensure!(
                valid_id(&module.id) && modules.insert(&module.id),
                "Invalid or duplicate module ID"
            );
            for (path, suffix) in [
                (module.wasm.as_ref(), ".wasm"),
                (module.rule.as_ref(), ".js"),
            ] {
                if let Some(path) = path {
                    ensure!(
                        path.ends_with(suffix) && entries.contains_key(path),
                        "Missing package file: {path}"
                    );
                }
            }
        }
        let mut commands = BTreeSet::new();
        for command in &self.commands {
            ensure!(
                valid_id(&command.id)
                    && commands.insert(&command.id)
                    && modules.contains(&command.module),
                "Invalid command or missing command module: {}",
                command.id
            );
        }
        for (name, module) in &self.public_services {
            ensure!(
                valid_id(name)
                    && self
                        .modules
                        .iter()
                        .any(|item| item.id == *module && item.wasm.is_some()),
                "Invalid public service or missing service module"
            );
        }
        ensure!(
            self.tools.is_empty() || self.permissions.contains(&Permission::RunTool),
            "Native tools require run_tool permission"
        );
        for (id, tool) in &self.tools {
            ensure!(
                valid_id(id)
                    && (tool.executable.is_some()
                        || tool.path_setting.is_some()
                        || !tool.downloads.is_empty()),
                "Invalid native tool declaration"
            );
            if let Some(path) = &tool.executable {
                ensure!(
                    entries.contains_key(path),
                    "Native tool package file is missing"
                );
            }
            if let Some(key) = &tool.path_setting {
                ensure!(
                    self.settings
                        .get(key)
                        .is_some_and(|setting| !setting.secret && setting.default.is_string()),
                    "Native tool path setting must be ordinary text"
                );
            }
            ensure!(
                tool.downloads.is_empty() || self.permissions.contains(&Permission::Network),
                "Managed tool downloads require network permission"
            );
            ensure!(
                tool.downloads.len() <= 16,
                "Too many download platforms for native tool"
            );
            for (platform, download) in &tool.downloads {
                ensure!(
                    platform.len() <= 40
                        && platform.split_once('-').is_some_and(|(os, arch)| {
                            valid_id(os)
                                && !arch.is_empty()
                                && arch.bytes().all(|byte| {
                                    byte.is_ascii_lowercase()
                                        || byte.is_ascii_digit()
                                        || byte == b'_'
                                })
                        }),
                    "Invalid native tool platform"
                );
                download.validate()?;
            }
            if let Some(background) = &tool.background {
                super::tools::check_args(&background.args)?;
            }
        }
        for (key, setting) in &self.settings {
            ensure!(valid_id(key), "Invalid setting ID: {key}");
            ensure!(
                !setting.secret || (!setting.project && setting.default.is_null()),
                "Secret settings must have a null default and local scope"
            );
            ensure!(
                setting
                    .renamed_from
                    .iter()
                    .all(|previous| valid_id(previous) && !self.settings.contains_key(previous))
                    && (!setting.secret || setting.renamed_from.is_empty()),
                "Invalid prior setting name"
            );
        }
        Ok(())
    }
}

/// The host's environment. Tests can supply a precise compatibility baseline.
#[derive(Clone, Debug)]
pub struct HostCompatibility {
    pub version: Version,
    pub platform: String,
    pub experiments: BTreeMap<String, String>,
}

impl HostCompatibility {
    /// The compiled editor's version and native platform. Experiments are opt-in.
    pub fn current(development: bool) -> Self {
        Self {
            version: Version::parse(env!("CARGO_PKG_VERSION")).expect("Cargo package version"),
            platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            experiments: if development {
                BTreeMap::from([("ui".into(), UI_CONTRACT.into())])
            } else {
                BTreeMap::new()
            },
        }
    }

    /// Explain compatibility failures before any package code is started.
    pub fn check(&self, manifest: &Manifest) -> Result<()> {
        if !manifest.engines.nanobug.matches(&self.version) {
            let too_old = manifest
                .engines
                .nanobug
                .comparators
                .iter()
                .any(|constraint| {
                    let mut floor = Version::new(
                        constraint.major,
                        constraint.minor.unwrap_or(0),
                        constraint.patch.unwrap_or(0),
                    );
                    floor.pre = constraint.pre.clone();
                    match constraint.op {
                        Op::Greater if constraint.minor.is_none() => {
                            self.version.major <= constraint.major
                        }
                        Op::Greater if constraint.patch.is_none() => {
                            (self.version.major, self.version.minor) <= (floor.major, floor.minor)
                        }
                        Op::Greater => self.version <= floor,
                        Op::GreaterEq | Op::Exact | Op::Tilde | Op::Caret | Op::Wildcard => {
                            self.version < floor
                        }
                        _ => false,
                    }
                });
            if too_old {
                bail!(
                    "Editor version too old. Upgrade Nanobug (installed {}, requires {}).",
                    self.version,
                    manifest.engines.nanobug
                );
            }
            bail!(
                "Plugin does not support Nanobug {} (requires {}).",
                self.version,
                manifest.engines.nanobug
            );
        }
        ensure!(
            manifest.api == API,
            "Unsupported plugin API {}",
            manifest.api
        );
        ensure!(
            manifest.platforms.is_empty()
                || manifest.platforms.contains(&self.platform)
                || self
                    .platform
                    .split_once('-')
                    .is_some_and(|(os, _)| manifest.platforms.contains(os)),
            "Plugin does not support platform {}",
            self.platform
        );
        for (name, contract) in &manifest.experiments {
            match self.experiments.get(name) {
                Some(supported) => ensure!(
                    supported == contract,
                    "Experimental interface {name} does not match this build"
                ),
                None => bail!("Experimental interface {name} is unavailable in this profile"),
            }
        }
        ensure!(
            !manifest.modules.iter().any(|module| module.interactive)
                || manifest.experiments.contains_key("ui"),
            "Interactive modules must declare the UI experiment"
        );
        Ok(())
    }
}

/// Immutable package data in the registry. Directory names never come directly from the archive.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Release {
    pub manifest: Manifest,
    pub digest: String,
    pub files: BTreeMap<String, String>,
    pub source: PathBuf,
    #[serde(default)]
    pub development: bool,
    /// Cloning a live release keeps its verified files available to work already queued.
    #[serde(skip)]
    _lease: Option<Arc<File>>,
}

/// Current install with a compatible previous release retained for a failed takeover.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Installed {
    pub release: Release,
    pub grants: BTreeSet<Permission>,
    #[serde(default, skip_serializing_if = "ApprovedRulePaths::is_empty")]
    pub(crate) rule_paths: ApprovedRulePaths,
    pub previous: Option<Box<Installed>>,
    pub enabled: bool,
    /// An update keeps the last confirmed version until the new runtime opens successfully.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pending_takeover: bool,
    /// Preserve the order in which default keys were introduced, including across updates.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub shortcut_order: BTreeMap<String, u64>,
}

/// Temporary verified bytes and a reviewable manifest. Dropping it aborts installation.
pub struct Prepared {
    pub release: Release,
    /// Exact names confirmed for this staged package. Inspection leaves this empty.
    pub(super) reviewed_rule_paths: BTreeSet<String>,
    staging: Staging,
}

/// A separate lease stays in the staging directory when the verified contents move out.
struct Staging {
    path: PathBuf,
    _lease: File,
    directory: TempDir,
}

impl Staging {
    fn path(&self) -> &Path {
        &self.path
    }
}

/// An editor's installation registry, package files, and local plugin state.
#[derive(Clone)]
pub struct InstallStore {
    /// Installation index, verified packages, and package locks live here.
    root: PathBuf,
    /// Local settings, credential references, and other plugin state live here.
    state_root: Arc<PathBuf>,
    compatibility: HostCompatibility,
}

impl InstallStore {
    /// Initialize the default compatibility policy without doing any disk I/O.
    pub fn new(root: PathBuf) -> Self {
        Self::with_compatibility(root, HostCompatibility::current(false))
    }

    /// Use `root` for both installation and state, with the supplied `compatibility` policy.
    pub fn with_compatibility(root: PathBuf, compatibility: HostCompatibility) -> Self {
        Self {
            state_root: Arc::new(root.clone()),
            root,
            compatibility,
        }
    }

    /// Directory containing the installation index and verified packages.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Use `state_root` for plugin state while keeping packages in the current store.
    pub(super) fn with_state_root(mut self, state_root: PathBuf) -> Self {
        self.state_root = Arc::new(state_root);
        self
    }

    /// Locate state data for the session, including references to the OS credential store.
    pub(super) fn state_root(&self) -> &Path {
        self.state_root.as_path()
    }

    pub fn compatibility(&self) -> &HostCompatibility {
        &self.compatibility
    }

    /// Read a small index. This does not scan packages or start workers.
    pub fn list(&self) -> Result<Vec<Installed>> {
        Ok(self.read_index()?.into_values().collect())
    }

    /// Copy already approved releases from `previous` into this fresh installation once.
    pub(super) fn import_installations(&self, previous: &Self) -> Result<()> {
        let destination = self.root.join(INSTALLED_INDEX);
        if destination.try_exists()? {
            return Ok(());
        }
        let _target = self.lock_index()?;
        if destination.try_exists()? {
            return Ok(());
        }
        if !previous.root.join(INSTALLED_INDEX).try_exists()? {
            return write_registry(&destination, &BTreeMap::<String, Installed>::new());
        }
        let _source = previous.lock_index()?;
        let index = previous.read_index()?;
        let mut imported = BTreeSet::new();
        for record in index.values() {
            for item in std::iter::once(record).chain(record.previous.as_deref()) {
                let release = &item.release;
                if imported.insert(&release.digest) {
                    self.import_package(previous, release)?;
                }
            }
        }
        // Readers only see the index once every referenced package is present.
        write_registry(&destination, &index)
    }

    /// Publish a verified package from `previous`, or check the `release` left by a partial import.
    fn import_package(&self, previous: &Self, release: &Release) -> Result<()> {
        let object = self.object_path(release)?;
        if object.try_exists()? {
            for path in release.files.keys() {
                self.verified_asset(release, path)?;
            }
            return Ok(());
        }
        let packages = object.parent().expect("object parent");
        fs::create_dir_all(packages)?;
        ensure!(
            packages.canonicalize()? == self.root.canonicalize()?.join("packages"),
            "Plugin package directory escaped the store"
        );
        let staged = self.stage_locked()?;
        for path in release.files.keys() {
            let (source, expected) = previous.asset_location(release, path)?;
            let bytes = read_limited(&source, MAX_FILE)?;
            ensure!(
                hash_bytes(&bytes) == expected,
                "Plugin asset changed after installation"
            );
            let path = staged.path().join(path);
            write_bytes(&path, &bytes)?;
            #[cfg(unix)]
            fs::set_permissions(path, source.metadata()?.permissions())?;
        }
        fs::rename(staged.path(), object)?;
        Ok(())
    }

    /// Read the index and reserve its active files in one transaction across editor instances.
    pub(super) fn list_leased(&self) -> Result<Vec<Installed>> {
        if !self.root.join(INSTALLED_INDEX).try_exists()? {
            return Ok(Vec::new());
        }
        let _index = self.lock_index()?;
        let mut installed = self.list()?;
        for record in &mut installed {
            let lease = self.package_lock(&record.release.digest)?;
            lease.lock_shared()?;
            record.release._lease = Some(Arc::new(lease));
        }
        Ok(installed)
    }

    /// Metadata for locally retained settings, credentials and drafts of removed plugins.
    pub(super) fn retained(&self) -> Result<BTreeMap<String, Release>> {
        Ok(read_json(&self.state_root.join("retained.json"), MAX_INDEX)?.unwrap_or_default())
    }

    /// Change activation without mutating the installed package.
    pub(super) fn set_enabled(&self, key: &str, enabled: bool) -> Result<()> {
        let _lock = self.lock_index()?;
        let mut index = self.read_index()?;
        index
            .get_mut(key)
            .context("Plugin is not installed")?
            .enabled = enabled;
        write_registry(&self.root.join(INSTALLED_INDEX), &index)
    }

    /// Remove an installation from the index after its supervisor drains active calls.
    pub(super) fn uninstall(&self, key: &str) -> Result<()> {
        let _lock = self.lock_index()?;
        let mut index = self.read_index()?;
        let removed = index.remove(key).context("Plugin is not installed")?;
        // Retained metadata shares the state directory with the settings it describes.
        let _retained = self.lock_retained()?;
        let mut retained = self.retained()?;
        retained.insert(key.to_owned(), removed.release.clone());
        write_registry(&self.state_root.join("retained.json"), &retained)?;
        write_registry(&self.root.join(INSTALLED_INDEX), &index)?;
        for release in std::iter::once(&removed.release)
            .chain(removed.previous.as_deref().map(|record| &record.release))
        {
            if index.values().all(|record| {
                record.release.digest != release.digest
                    && record
                        .previous
                        .as_ref()
                        .is_none_or(|previous| previous.release.digest != release.digest)
            }) {
                self.remove_object(&release.digest)
                    .context("Uninstalled; cached package could not be removed")?;
            }
        }
        Ok(())
    }

    /// Restore a compatible previous install only if the failed release still owns this entry.
    pub(super) fn rollback(&self, key: &str, digest: &str) -> Result<()> {
        let lock = self.open_index_lock()?;
        lock.try_lock().context("Plugin index is busy")?;
        let mut index = self.read_index()?;
        let current = index.get_mut(key).context("Plugin is not installed")?;
        ensure!(
            current.release.digest == digest,
            "Installed release changed before recovery"
        );
        let previous = current
            .previous
            .take()
            .context("No previous plugin release")?;
        self.compatibility.check(&previous.release.manifest)?;
        *current = *previous;
        write_registry(&self.root.join(INSTALLED_INDEX), &index)
    }

    /// Confirm this exact candidate before allowing it to take over user operations.
    pub(super) fn confirm_takeover(&self, key: &str, digest: &str) -> Result<()> {
        let lock = self.open_index_lock()?;
        lock.try_lock().context("Plugin index is busy")?;
        let mut index = self.read_index()?;
        let current = index.get_mut(key).context("Plugin is not installed")?;
        ensure!(
            current.release.digest == digest,
            "Installed release changed before takeover"
        );
        if current.pending_takeover {
            current.pending_takeover = false;
            write_registry(&self.root.join(INSTALLED_INDEX), &index)?;
        }
        Ok(())
    }

    fn read_index(&self) -> Result<BTreeMap<String, Installed>> {
        let index: BTreeMap<String, Installed> =
            read_json(&self.root.join(INSTALLED_INDEX), MAX_INDEX)?.unwrap_or_default();
        for (key, record) in &index {
            ensure!(
                record.previous.as_ref().is_none_or(
                    |previous| previous.previous.is_none() && !previous.pending_takeover
                ) && (!record.pending_takeover || record.previous.is_some()),
                "Invalid plugin rollback history"
            );
            for release in std::iter::once(record).chain(record.previous.as_deref()) {
                ensure!(
                    key == &release.release.manifest.identity().namespace(),
                    "Invalid plugin index identity"
                );
                validate_digest(&release.release.digest)?;
                release.release.manifest.validate(&release.release.files)?;
                ensure!(
                    release.grants == release.release.manifest.permissions,
                    "Invalid plugin index permissions"
                );
                release
                    .rule_paths
                    .validate_against(&release.release.manifest.rule_paths)?;
            }
        }
        Ok(index)
    }

    /// Stage exactly one archive from the selected local path. The source stays untouched.
    pub fn prepare(&self, path: &Path) -> Result<Prepared> {
        let input = read_limited(path, MAX_ARCHIVE)?;
        let digest = hash_bytes(&input);
        let staging = self.stage()?;
        let files = extract(&input, staging.path())?;
        let data = read_limited(&staging.path().join(MANIFEST), MAX_MANIFEST)?;
        let manifest: Manifest =
            toml::from_str(std::str::from_utf8(&data)?).context("Invalid plugin manifest")?;
        manifest.validate(&files)?;
        self.compatibility.check(&manifest)?;
        let release = Release {
            manifest,
            digest,
            files,
            source: path.to_path_buf(),
            development: false,
            _lease: None,
        };
        let package = Prepared {
            release,
            reviewed_rule_paths: BTreeSet::new(),
            staging,
        };
        self.verify_prepared(&package)?;
        Ok(package)
    }

    /// Prepare an explicitly selected local build output in a development profile.
    pub fn prepare_directory(&self, path: &Path) -> Result<Prepared> {
        ensure!(
            self.compatibility.experiments.contains_key("ui"),
            "A development profile is required"
        );
        let root = path.canonicalize()?;
        ensure!(root.is_dir(), "Select a plugin build output directory");
        ensure!(
            root.join(MANIFEST).is_file(),
            "Build output must contain {MANIFEST}"
        );
        let staging = self.stage()?;
        ensure!(
            !staging.path().canonicalize()?.starts_with(&root),
            "Build output must be separate from the plugin profile"
        );
        let mut files = BTreeMap::new();
        let mut pending = vec![root.clone()];
        let mut count = 0;
        let mut total = 0;
        while let Some(directory) = pending.pop() {
            for entry in fs::read_dir(directory)? {
                let entry = entry?;
                count += 1;
                ensure!(count <= 2048, "Build output has too many files");
                let path = entry.path();
                let ty = entry.file_type()?;
                ensure!(
                    !ty.is_symlink() && path.canonicalize()?.starts_with(&root),
                    "Build output must not contain linked files"
                );
                let relative = path
                    .strip_prefix(&root)?
                    .to_str()
                    .context("Build output name is not UTF-8")?
                    .replace('\\', "/");
                let relative = portable_path(&relative)?;
                if ty.is_dir() {
                    pending.push(path);
                    continue;
                }
                ensure!(ty.is_file(), "Build output entry must be a regular file");
                let bytes = read_limited(&path, MAX_FILE)?;
                total += bytes.len() as u64;
                ensure!(
                    total <= MAX_UNPACKED,
                    "Build output exceeds the package budget"
                );
                write_bytes(&staging.path().join(relative), &bytes)?;
                files.insert(relative.to_owned(), hash_bytes(&bytes));
            }
        }
        let bytes = read_limited(&staging.path().join(MANIFEST), MAX_MANIFEST)?;
        let manifest: Manifest =
            toml::from_str(std::str::from_utf8(&bytes)?).context("Invalid plugin manifest")?;
        manifest.validate(&files)?;
        self.compatibility.check(&manifest)?;
        let digest = hash_bytes(&serde_json::to_vec(&files)?);
        let package = Prepared {
            release: Release {
                manifest,
                digest,
                files,
                source: root,
                development: true,
                _lease: None,
            },
            reviewed_rule_paths: BTreeSet::new(),
            staging,
        };
        self.verify_prepared(&package)?;
        Ok(package)
    }

    /// Publish the directory and its lease together with respect to the cache collector.
    fn stage(&self) -> Result<Staging> {
        let _index = self.lock_index()?;
        self.stage_locked()
    }

    /// Create a staging directory while the caller holds this store's install lock.
    fn stage_locked(&self) -> Result<Staging> {
        let root = self.root.canonicalize()?.join(STAGING_DIRECTORY);
        fs::create_dir_all(&root)?;
        ensure!(
            root.canonicalize()? == root,
            "Package staging points outside the plugin store"
        );
        let directory = tempfile::Builder::new()
            .prefix(STAGING_PREFIX)
            .rand_bytes(STAGING_ID_LEN)
            .tempdir_in(root)?;
        let lease = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(directory.path().join("live.lock"))?;
        lease.lock()?;
        let path = directory.path().join("contents");
        fs::create_dir(&path)?;
        Ok(Staging {
            path,
            _lease: lease,
            directory,
        })
    }

    /// Verify and compile while the previous version can still serve requests.
    fn verify_prepared(&self, package: &Prepared) -> Result<()> {
        let scripts = package
            .release
            .manifest
            .modules
            .iter()
            .filter_map(|module| module.rule.as_ref())
            .map(|path| {
                read_limited(&package.staging.path().join(path), 128 * 1024)
                    .and_then(|bytes| Ok(String::from_utf8(bytes)?))
            })
            .collect::<Result<Vec<_>>>()?;
        check_scripts(scripts)?;
        #[cfg(unix)]
        for tool in package.release.manifest.tools.values() {
            if let Some(path) = &tool.executable {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(
                    package.staging.path().join(portable_path(path)?),
                    fs::Permissions::from_mode(0o700),
                )?;
            }
        }
        let modules: Vec<_> = package
            .release
            .manifest
            .modules
            .iter()
            .filter_map(|module| module.wasm.as_ref())
            .map(|path| package.staging.path().join(path))
            .collect();
        if !modules.is_empty() {
            let cache = self.cache_config().ok();
            let worker = Worker::start("wasm", cache.as_deref())?;
            worker.post(Request::Inspect { id: 1, modules })?;
            match worker.wait(Duration::from_secs(30))? {
                Reply::Inspected { id: 1 } => {}
                Reply::Error { id: 1, message, .. } => bail!("{message}"),
                _ => bail!("Invalid install verification reply"),
            }
            worker.stop();
        }
        Ok(())
    }

    /// Publish a package whose permissions and declared external paths the caller has approved.
    pub fn install(
        &self,
        mut package: Prepared,
        grants: &BTreeSet<Permission>,
    ) -> Result<Installed> {
        package.reviewed_rule_paths = package.release.manifest.rule_paths.clone();
        self.install_checked(package, grants, |_| Ok(()))
    }

    /// Allow a local development build to recheck its source and installed base at publication.
    pub(super) fn install_checked(
        &self,
        package: Prepared,
        grants: &BTreeSet<Permission>,
        before_publish: impl FnOnce(Option<&Installed>) -> Result<()>,
    ) -> Result<Installed> {
        ensure!(
            grants == &package.release.manifest.permissions,
            "Plugin permissions were not approved"
        );
        self.compatibility.check(&package.release.manifest)?;
        let declared = &package.release.manifest.rule_paths;
        ensure!(
            package.reviewed_rule_paths.is_subset(declared),
            "Invalid external rule path approval"
        );
        // Confirmed paths may reside on a slow share. Resolve them off the registry lock.
        let reviewed = ApprovedRulePaths::approve(&package.reviewed_rule_paths)?;
        let lock = self.lock_index()?;
        let mut index = self.read_index()?;
        let key = package.release.manifest.identity().namespace();
        let rule_paths = reviewed.inherit(declared, index.get(&key).map(|old| &old.rule_paths))?;
        let object = self.object_path(&package.release)?;
        let created = !object.exists();
        if !created {
            for path in package.release.files.keys() {
                self.verified_asset(&package.release, path)
                    .context("Stored plugin files failed validation")?;
            }
        } else {
            let parent = object.parent().expect("object parent");
            fs::create_dir_all(parent)?;
            let root = self.root.canonicalize()?;
            let stage = package.staging.path().canonicalize()?;
            ensure!(
                parent.canonicalize()? == root.join("packages")
                    && stage == package.staging.directory.path().join("contents")
                    && stage.parent().and_then(Path::parent)
                        == Some(root.join(STAGING_DIRECTORY).as_path()),
                "Plugin staging directory escaped the store"
            );
            fs::rename(stage, &object)?;
        }
        let ready = before_publish(index.get(&key));
        let previous = index.remove(&key);
        let enabled = previous.as_ref().is_none_or(|old| old.enabled);
        let mut last_order = index
            .values()
            .chain(previous.as_ref())
            .flat_map(|record| std::iter::once(record).chain(record.previous.as_deref()))
            .flat_map(|record| record.shortcut_order.values().copied())
            .max()
            .unwrap_or(0);
        let mut shortcut_order = BTreeMap::new();
        for command in &package.release.manifest.commands {
            let Some(key) = command.key.as_ref() else {
                continue;
            };
            let order = previous
                .as_ref()
                .filter(|record| {
                    record.release.manifest.commands.iter().any(|old| {
                        old.id == command.id
                            && old
                                .key
                                .as_ref()
                                .is_some_and(|prior| prior.eq_ignore_ascii_case(key))
                            && old.contexts == command.contexts
                    })
                })
                .map_or_else(
                    || {
                        last_order = last_order.saturating_add(1);
                        last_order
                    },
                    |record| record.shortcut_order.get(&command.id).copied().unwrap_or(0),
                );
            shortcut_order.insert(command.id.clone(), order);
        }
        let (previous, pending_takeover) = match previous {
            None => (None, false),
            Some(old) if old.release.digest == package.release.digest => {
                (old.previous, old.pending_takeover)
            }
            Some(mut old) => {
                let previous = if old.pending_takeover {
                    old.previous
                } else {
                    old.previous = None;
                    Some(Box::new(old))
                };
                let pending = previous.is_some()
                    && package
                        .release
                        .manifest
                        .modules
                        .iter()
                        .any(|module| module.wasm.is_some());
                (previous, pending)
            }
        };
        let record = Installed {
            release: package.release,
            grants: grants.clone(),
            rule_paths,
            previous,
            enabled,
            pending_takeover,
            shortcut_order,
        };
        index.insert(key, record.clone());
        if let Err(error) =
            ready.and_then(|()| write_registry(&self.root.join(INSTALLED_INDEX), &index))
        {
            // Only this transaction's unpublished directory can be removed here.
            if created && let Err(cleanup) = self.remove_object(&record.release.digest) {
                return Err(
                    error.context(format!("Unpublished package cleanup failed: {cleanup:#}"))
                );
            }
            return Err(error);
        }
        drop(lock);
        Ok(record)
    }

    /// Validate content again before sending a module or rule path to a worker.
    pub fn verified_asset(&self, release: &Release, path: &str) -> Result<PathBuf> {
        let (asset, expected) = self.asset_location(release, path)?;
        ensure!(
            hash_bytes(&read_limited(&asset, MAX_FILE)?) == expected,
            "Plugin asset changed after installation"
        );
        Ok(asset)
    }

    /// Read bounded package data and verify those exact bytes before using them in the host.
    pub(super) fn verified_asset_bytes(
        &self,
        release: &Release,
        path: &str,
        limit: u64,
    ) -> Result<Vec<u8>> {
        let (asset, expected) = self.asset_location(release, path)?;
        let bytes = read_limited(&asset, limit.min(MAX_FILE))?;
        ensure!(
            hash_bytes(&bytes) == expected,
            "Plugin asset changed after installation"
        );
        Ok(bytes)
    }

    /// Resolve a declared asset and its recorded content hash within the package directory.
    fn asset_location<'a>(&self, release: &'a Release, path: &str) -> Result<(PathBuf, &'a str)> {
        let path = portable_path(path)?;
        let expected = release
            .files
            .get(path)
            .context("undeclared package asset")?;
        let object = self.object_path(release)?.canonicalize()?;
        let packages = self.root.join("packages").canonicalize()?;
        ensure!(
            object.parent() == Some(&packages)
                && object
                    .file_name()
                    .is_some_and(|name| name == release.digest.as_str()),
            "Plugin package directory escaped the store"
        );
        let asset = object.join(path).canonicalize()?;
        ensure!(
            asset.starts_with(&object),
            "package asset escaped the store"
        );
        Ok((asset, expected))
    }

    /// Create a private Wasmtime cache configuration outside extracted packages.
    pub(super) fn cache_config(&self) -> Result<PathBuf> {
        let directory = self.state_root.join("compiled");
        fs::create_dir_all(&directory)?;
        let config = self.state_root.join("wasmtime-cache.toml");
        let cache = toml::Table::from_iter([(
            "directory".into(),
            toml::Value::String(directory.canonicalize()?.to_string_lossy().into_owned()),
        )]);
        let settings = toml::Table::from_iter([("cache".into(), toml::Value::Table(cache))]);
        let expected = toml::to_string(&settings)?;
        let matches = || {
            read_limited(&config, expected.len() as u64)
                .is_ok_and(|bytes| bytes == expected.as_bytes())
        };
        // Concurrent starters can reuse the same atomically published file.
        if !matches()
            && let Err(error) = write_bytes(&config, expected.as_bytes())
            && !matches()
        {
            return Err(error);
        }
        Ok(config)
    }

    fn object_path(&self, release: &Release) -> Result<PathBuf> {
        validate_digest(&release.digest)?;
        Ok(self.root.join("packages").join(&release.digest))
    }

    /// Delete an object only after resolving its exact name under the profile's packages folder.
    fn remove_object(&self, digest: &str) -> Result<()> {
        let lease = self.package_lock(digest)?;
        match lease.try_lock() {
            Ok(()) => {}
            Err(fs::TryLockError::WouldBlock) => return Ok(()),
            Err(error) => return Err(error.into()),
        }
        let object = self.root.join("packages").join(digest);
        let resolved = match object.canonicalize() {
            Ok(resolved) => resolved,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let root = self.root.canonicalize()?;
        let packages = root.join("packages");
        ensure!(
            resolved == packages.join(digest),
            "Package directory points outside the plugin store"
        );
        fs::remove_dir_all(resolved)?;
        Ok(())
    }

    /// Remove unclaimed staging and published objects, preserving live and rollback files.
    pub(super) fn prune_unreferenced(&self) -> Result<()> {
        if !self.root.try_exists()? {
            return Ok(());
        }
        let _index = self.lock_index()?;
        self.prune_staging()?;
        let package_root = self.root.join("packages");
        if !package_root.try_exists()? {
            return Ok(());
        }
        let installed = self.list()?;
        let retained: BTreeSet<&str> = installed
            .iter()
            .flat_map(|record| {
                std::iter::once(record.release.digest.as_str()).chain(
                    record
                        .previous
                        .as_deref()
                        .map(|prior| prior.release.digest.as_str()),
                )
            })
            .collect();
        for entry in fs::read_dir(package_root)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(digest) = name.to_str() else {
                continue;
            };
            if !retained.contains(digest)
                && validate_digest(digest).is_ok()
                && entry.file_type()?.is_dir()
            {
                self.remove_object(digest)?;
            }
        }
        Ok(())
    }

    /// A crashed process releases the lease. Only private, resolved staging directories qualify.
    fn prune_staging(&self) -> Result<()> {
        let root = self.root.canonicalize()?.join(STAGING_DIRECTORY);
        if !root.try_exists()? {
            return Ok(());
        }
        ensure!(
            root.canonicalize()? == root,
            "Package staging points outside the plugin store"
        );
        for entry in fs::read_dir(&root)? {
            let entry = entry?;
            let name = entry.file_name();
            if !name
                .to_str()
                .and_then(|name| name.strip_prefix(STAGING_PREFIX))
                .is_some_and(|suffix| {
                    suffix.len() == STAGING_ID_LEN
                        && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
                })
                || !entry.file_type()?.is_dir()
            {
                continue;
            }
            let directory = root.join(name);
            match directory.canonicalize() {
                Ok(path) if path == directory => {}
                Ok(_) => continue,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            }
            let marker = directory.join("live.lock");
            match fs::symlink_metadata(&marker) {
                Ok(meta) if meta.is_file() => {
                    let lease = match OpenOptions::new().read(true).write(true).open(marker) {
                        Ok(lease) => lease,
                        Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                        Err(error) => return Err(error.into()),
                    };
                    match lease.try_lock() {
                        Ok(()) => {}
                        Err(fs::TryLockError::WouldBlock) => continue,
                        Err(error) => return Err(error.into()),
                    }
                }
                Ok(_) => continue,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            match fs::remove_dir_all(directory) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    fn package_lock(&self, digest: &str) -> Result<File> {
        validate_digest(digest)?;
        let directory = self.root.join("package-leases");
        fs::create_dir_all(&directory)?;
        ensure!(
            directory.canonicalize()? == self.root.canonicalize()?.join("package-leases"),
            "Package lease directory points outside the plugin store"
        );
        Ok(OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(directory.join(digest))?)
    }

    fn lock_index(&self) -> Result<File> {
        let lock = self.open_index_lock()?;
        lock.lock()?;
        Ok(lock)
    }

    /// Synchronize retained metadata across sessions that use this state directory.
    fn lock_retained(&self) -> Result<Option<File>> {
        if self.state_root() == self.root() {
            return Ok(None);
        }
        let lock = Self::open_registry_lock(self.state_root())?;
        lock.lock()?;
        Ok(Some(lock))
    }

    /// Open the common install lock for either a background wait or a supervisor try-lock.
    fn open_index_lock(&self) -> Result<File> {
        Self::open_registry_lock(&self.root)
    }

    /// Open the lock for `root`'s installation or retained metadata without modifying its contents.
    fn open_registry_lock(root: &Path) -> Result<File> {
        fs::create_dir_all(root)?;
        Ok(OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join("installed.lock"))?)
    }
}

pub(super) fn validate_digest(value: &str) -> Result<()> {
    ensure!(
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
        "Invalid package digest"
    );
    Ok(())
}

/// Stable SHA-256 string for package and state keys.
pub(super) fn hash_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Read and cap data from a regular file, detecting growth after the metadata check.
pub(super) fn read_limited(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let file = File::open(path).with_context(|| format!("Open {}", path.display()))?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.len() <= limit,
        "Invalid file or file exceeds size limit"
    );
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= limit, "File exceeds size limit");
    Ok(bytes)
}

pub(super) fn read_json<T: DeserializeOwned>(path: &Path, limit: u64) -> Result<Option<T>> {
    match File::open(path) {
        Ok(file) => {
            ensure!(file.metadata()?.len() <= limit, "State file is too large");
            Ok(Some(serde_json::from_reader(BufReader::new(
                file.take(limit + 1),
            ))?))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn write_bytes(path: &Path, data: &[u8]) -> Result<()> {
    let parent = path.parent().context("state file has no parent")?;
    fs::create_dir_all(parent)?;
    let mut file = NamedTempFile::new_in(parent)?;
    file.write_all(data)?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}

pub(super) fn write_json(path: &Path, data: &impl Serialize) -> Result<()> {
    write_bytes(path, &serde_json::to_vec_pretty(data)?)
}

/// Never publish an index that a later session would reject for size.
fn write_registry(path: &Path, data: &impl Serialize) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(data)?;
    ensure!(
        bytes.len() as u64 <= MAX_INDEX,
        "Plugin registry exceeds its size budget"
    );
    write_bytes(path, &bytes)
}

/// Portable archive paths cannot reference links, alternate streams, or device names.
pub(super) fn portable_path(path: &str) -> Result<&str> {
    ensure!(
        !path.is_empty() && path.len() <= 240,
        "Invalid package path"
    );
    for part in path.split('/') {
        ensure!(
            !part.is_empty()
                && part != "."
                && part != ".."
                && !part.ends_with('.')
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.')),
            "Invalid package path: {path}"
        );
        let base = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        ensure!(
            !matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                && !(base.len() == 4
                    && (base.starts_with("COM") || base.starts_with("LPT"))
                    && base.as_bytes()[3].is_ascii_digit()),
            "Reserved device name in package: {path}"
        );
    }
    Ok(path)
}

/// Extract to a new private staging directory with size and name collision checks.
fn extract(bytes: &[u8], staging: &Path) -> Result<BTreeMap<String, String>> {
    let files = extract_zip(Cursor::new(bytes), staging)?;
    ensure!(
        files.contains_key(MANIFEST),
        "Missing root nanobug-plugin.toml"
    );
    Ok(files)
}

/// Extract checked content into a fresh private directory without assuming it is a plugin.
pub(super) fn extract_zip(
    reader: impl Read + Seek,
    staging: &Path,
) -> Result<BTreeMap<String, String>> {
    let mut zip = ZipArchive::new(reader).context("Invalid ZIP archive")?;
    ensure!(zip.len() <= 4096, "Too many ZIP entries");
    let mut seen = BTreeSet::new();
    let mut components = BTreeMap::<String, String>::new();
    let mut files = BTreeMap::new();
    let mut total = 0;
    for index in 0..zip.len() {
        let entry = zip.by_index(index)?;
        ensure!(
            !entry.is_symlink() && !entry.encrypted() && entry.enclosed_name().is_some(),
            "Unsupported ZIP entry"
        );
        let raw = std::str::from_utf8(entry.name_raw())?;
        let path = portable_path(raw.strip_suffix('/').unwrap_or(raw))?.to_owned();
        ensure!(
            seen.insert(path.to_ascii_lowercase()),
            "Duplicate ZIP entry: {path}"
        );
        for prefix in path
            .match_indices('/')
            .map(|(i, _)| &path[..i])
            .chain(std::iter::once(path.as_str()))
        {
            let actual = components
                .entry(prefix.to_ascii_lowercase())
                .or_insert_with(|| prefix.into());
            ensure!(actual == prefix, "Case collision in ZIP: {path}");
        }
        let destination = staging.join(&path);
        if entry.is_dir() {
            fs::create_dir_all(destination)?;
            continue;
        }
        let limit = if path == MANIFEST {
            MAX_MANIFEST
        } else {
            MAX_FILE
        };
        let size = entry.size();
        ensure!(
            size <= limit && total + size <= MAX_UNPACKED,
            "Plugin ZIP exceeds unpacking limits"
        );
        fs::create_dir_all(destination.parent().expect("staged parent"))?;
        let mut source = entry.take(size + 1);
        let mut out = BufWriter::new(
            OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&destination)?,
        );
        let mut hasher = Sha256::new();
        let mut count = 0;
        let mut buf = [0; 16 * 1024];
        loop {
            let read = source.read(&mut buf)?;
            if read == 0 {
                break;
            }
            count += read as u64;
            ensure!(count <= size, "Invalid ZIP entry size");
            hasher.update(&buf[..read]);
            out.write_all(&buf[..read])?;
        }
        ensure!(count == size, "Incomplete ZIP entry");
        out.flush()?;
        out.get_ref().sync_all()?;
        total += count;
        files.insert(path, format!("{:x}", hasher.finalize()));
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zip::{ZipWriter, write::SimpleFileOptions};

    #[test]
    fn package_paths_are_portable_relative_names() -> Result<()> {
        assert_eq!(portable_path("assets/icon.svg")?, "assets/icon.svg");
        for name in [
            "",
            ".",
            "..",
            "/",
            "/assets",
            "assets//icon.svg",
            "assets/..",
        ] {
            assert!(portable_path(name).is_err(), "{name:?}");
        }
        Ok(())
    }

    #[test]
    fn staging_cleanup_leaves_live_installers_and_reclaims_abandoned_bytes() -> Result<()> {
        let profile = tempfile::tempdir()?;
        let store = InstallStore::new(profile.path().to_path_buf());
        let current = store.stage()?;
        let Staging {
            directory, _lease, ..
        } = store.stage()?;
        let abandoned = directory.keep();
        // A terminated process releases its lease without dropping the staging directory.
        drop(_lease);
        store.prune_unreferenced()?;
        assert!(current.path().is_dir());
        assert!(!abandoned.exists());
        Ok(())
    }

    #[test]
    fn package_collection_preserves_live_and_rollback_files() -> Result<()> {
        let folder = tempfile::tempdir()?;
        let archive = folder.path().join("fixture.zip");
        let store = InstallStore::new(folder.path().join("profile"));
        let install = |version: &str| -> Result<Installed> {
            let mut zip = ZipWriter::new(File::create(&archive)?);
            zip.start_file(MANIFEST, SimpleFileOptions::default())?;
            write!(
                zip,
                "name='Fixture'\nid='fixture'\nversion={version:?}\n[engines]\nnanobug='*'\n"
            )?;
            zip.finish()?;
            store.install(store.prepare(&archive)?, &BTreeSet::new())
        };
        let first = install("1.0.0")?;
        let live = store.list_leased()?;
        let rollback = install("1.1.0")?;
        let current = install("1.2.0")?;
        store.prune_unreferenced()?;
        assert!(store.object_path(&first.release)?.is_dir());
        drop(live);
        store.prune_unreferenced()?;
        assert!(!store.object_path(&first.release)?.exists());
        assert!(store.object_path(&rollback.release)?.is_dir());
        assert!(store.object_path(&current.release)?.is_dir());
        assert!(archive.is_file());
        Ok(())
    }
}
