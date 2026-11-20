//! Drafts and committed targets for shared project configuration and local defaults.

use crate::plugins::{
    package::{Identity, Installed, Manifest, SettingDecl, hash_bytes, read_limited, write_bytes},
    query::{project_key, project_path},
    session::{Key, Project},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

mod replacement;
use replacement::{Displaced, Replacement, sync_parent};

const LIMIT: u64 = 512 * 1024;
const JOURNAL_LIMIT: u64 = 5 * LIMIT + 256 * 1024;
const PROJECT_CONFIG: &str = ".nb/plugins.json";

/// The selected rules and ordinary settings scope.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ConfigScope {
    AllProjects,
    Project(u64),
    ProjectLocal(u64),
}

/// An explicit module override. Absence means inheritance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Rule {
    Always,
    Never,
    Script(String),
}

// Use a boolean on disk for an explicit enable/disable, including false.
mod rules {
    use super::*;
    pub(super) fn serialize<S: serde::Serializer>(
        rules: &BTreeMap<String, Rule>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let values: BTreeMap<_, _> = rules
            .iter()
            .map(|(key, rule)| {
                (
                    key,
                    match rule {
                        Rule::Always => Value::Bool(true),
                        Rule::Never => Value::Bool(false),
                        Rule::Script(source) => Value::String(source.clone()),
                    },
                )
            })
            .collect();
        values.serialize(serializer)
    }
    pub(super) fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<BTreeMap<String, Rule>, D::Error> {
        let values = BTreeMap::<String, Value>::deserialize(deserializer)?;
        values
            .into_iter()
            .map(|(key, value)| {
                let rule = match value {
                    Value::Bool(true) => Rule::Always,
                    Value::Bool(false) => Rule::Never,
                    Value::String(source) => Rule::Script(source),
                    _ => {
                        return Err(serde::de::Error::custom(
                            "Rule must be true, false or JS source",
                        ));
                    }
                };
                Ok((key, rule))
            })
            .collect()
    }
}

/// Configurable items for one canonical plugin name and ID.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginValues {
    pub identity: Identity,
    #[serde(default, with = "rules")]
    pub rules: BTreeMap<String, Rule>,
    #[serde(default)]
    pub settings: BTreeMap<String, Value>,
}

/// Shared configuration keeps identity visible even for missing plugins.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub plugins: Vec<PluginValues>,
    /// Per-file-type formatter identity and opt-in save behavior.
    pub formatters: BTreeMap<String, Formatting>,
    /// User mappings are local to the profile and shared by all projects.
    pub shortcuts: BTreeMap<String, BTreeMap<String, String>>,
}

/// Independent overrides for one file extension. Inheritance is represented by absence.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Formatting {
    pub provider: Option<Identity>,
    pub on_save: Option<bool>,
}

/// A settings panel edits individual fields of the latest draft.
pub enum ConfigEdit {
    Rule {
        owner: Identity,
        module: String,
        value: Option<Rule>,
    },
    Setting {
        owner: Identity,
        id: String,
        renamed_from: Vec<String>,
        original: Option<Value>,
        value: Option<Value>,
    },
    Shortcut {
        namespace: String,
        command: String,
        value: Option<String>,
    },
    Formatter {
        extension: String,
        provider: Identity,
    },
    ToggleFormatOnSave(String),
    Restore {
        owner: Identity,
        rules: bool,
        revision: u64,
    },
}

impl Config {
    /// Find the values for an identity, using the same key as the installed index.
    pub fn plugin(&self, namespace: &str) -> Option<&PluginValues> {
        self.plugins
            .iter()
            .find(|plugin| plugin.identity.namespace() == namespace)
    }

    /// Edit or add values without changing any other plugin's draft.
    pub fn plugin_mut(&mut self, identity: Identity) -> &mut PluginValues {
        if let Some(index) = self
            .plugins
            .iter()
            .position(|plugin| plugin.identity == identity)
        {
            return &mut self.plugins[index];
        }
        self.plugins.push(PluginValues {
            identity,
            rules: BTreeMap::new(),
            settings: BTreeMap::new(),
        });
        self.plugins.last_mut().expect("added plugin")
    }
}

/// The draft and an independently observed external version, with neither applied yet.
#[derive(Clone, Debug, Default)]
pub struct ConfigView {
    pub revision: u64,
    pub applied: Config,
    pub draft: Option<Config>,
    pub external: Option<Config>,
    pub error: Option<String>,
    pub apply_issue: Option<&'static str>,
    pub previous: Option<Config>,
    /// Raw external bytes retained by a concurrent Apply, including unreadable configurations.
    pub preserved_external: Option<PathBuf>,
    pub replacement_conflict: bool,
}

/// Persist a host diagnostic without copying arbitrary plugin or script output into the journal.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ApplyIssue {
    Validation,
    Save,
}

impl ApplyIssue {
    fn description(self) -> &'static str {
        match self {
            Self::Validation => {
                "The last Apply could not validate this draft. Edit it and try again."
            }
            Self::Save => "The last Apply could not save this draft. Review it and try again.",
        }
    }
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct Journal {
    applied: Config,
    draft: Option<Config>,
    base: Option<String>,
    pending: Option<Config>,
    /// Rebase unrelated in-progress manager edits only if a native page write reaches the project file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pending_rebased_draft: Option<Config>,
    #[serde(skip_serializing_if = "Option::is_none")]
    replacement: Option<Replacement>,
    #[serde(skip_serializing_if = "Option::is_none")]
    displaced: Option<Replacement>,
    previous: Option<Config>,
    #[serde(skip_serializing_if = "Option::is_none")]
    apply_issue: Option<ApplyIssue>,
    /// Local markers tie adoption failures to an exact release and effective values.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    settings_rejections: BTreeMap<String, String>,
}

struct Scope {
    local: PathBuf,
    root: Option<PathBuf>,
    data: Journal,
    journal_digest: Option<String>,
    external: Option<(String, Config)>,
    preserved_external: Option<PathBuf>,
    error: Option<String>,
    unreadable: bool,
    revision: u64,
    applied_revision: u64,
}

struct ExternalConfiguration {
    parsed: Option<(String, Config)>,
    preserved: Option<PathBuf>,
    error: Option<String>,
}

impl Scope {
    fn load(local: PathBuf, root: Option<PathBuf>) -> Self {
        let mut scope = Self {
            local,
            root,
            data: Journal::default(),
            journal_digest: None,
            external: None,
            preserved_external: None,
            error: None,
            unreadable: false,
            revision: 0,
            applied_revision: 0,
        };
        match read_optional(&scope.local, JOURNAL_LIMIT).and_then(|bytes| {
            bytes
                .map(|bytes| {
                    Ok((
                        serde_json::from_slice::<Journal>(&bytes)?,
                        hash_bytes(&bytes),
                    ))
                })
                .transpose()
        }) {
            Ok(Some((data, digest))) => {
                scope.data = data;
                scope.journal_digest = Some(digest);
            }
            Ok(None) if scope.root.is_some() => match scope.disk() {
                Ok((hash, current)) => {
                    scope.data.base = hash;
                    scope.data.applied = current;
                    if let Err(error) = scope.save() {
                        scope.error = Some(format!("{error:#}"));
                    }
                }
                Err(error) => scope.error = Some(format!("{error:#}")),
            },
            Ok(None) => {}
            Err(error) => {
                scope.error = Some(format!("Local configuration could not be read: {error:#}"));
                scope.unreadable = true;
            }
        }
        scope.refresh();
        scope
    }

    fn disk(&self) -> Result<(Option<String>, Config)> {
        let Some(root) = &self.root else {
            return Ok((None, Config::default()));
        };
        let path = project_path(root, PROJECT_CONFIG)?;
        read_project(&path)
    }

    fn project_file(&self) -> Result<PathBuf> {
        project_path(
            self.root.as_deref().context("Project is closed")?,
            PROJECT_CONFIG,
        )
    }

    fn external_configuration(&self) -> ExternalConfiguration {
        if let Some(displaced) = &self.data.displaced {
            let backup = self
                .root
                .as_deref()
                .context("Project is closed")
                .and_then(|root| displaced.recorded_project(root))
                .and_then(|project| displaced.backup(&project));
            let mut result = ExternalConfiguration {
                parsed: None,
                preserved: None,
                error: None,
            };
            match backup {
                Ok(backup) => {
                    match read_preserved_project(&backup) {
                        Ok((Some(hash), config)) => result.parsed = Some((hash, config)),
                        Ok((None, _)) => {
                            result.error =
                                Some("The preserved external configuration is missing".into())
                        }
                        Err(error) => {
                            result.error = Some(format!(
                                "Preserved external configuration could not be read: {error:#}"
                            ))
                        }
                    }
                    result.preserved = Some(backup);
                }
                Err(error) => {
                    result.error = Some(format!(
                        "Could not locate the preserved external configuration: {error:#}"
                    ))
                }
            }
            return result;
        }
        let (parsed, error) = match self.disk() {
            Ok((hash, content)) if hash != self.data.base => {
                (Some((hash.unwrap_or_default(), content)), None)
            }
            Ok(_) => (None, None),
            Err(error) => (None, Some(format!("{error:#}"))),
        };
        ExternalConfiguration {
            parsed,
            preserved: None,
            error,
        }
    }

    fn refresh(&mut self) -> bool {
        if self.root.is_none() || self.unreadable {
            return false;
        }
        let mut recovered = false;
        if self.data.pending.is_some() {
            match self.recover() {
                Ok(_) => recovered = true,
                Err(error) => {
                    let reason = format!("Could not recover interrupted Apply: {error:#}");
                    if self.error.as_ref() == Some(&reason) {
                        return false;
                    }
                    self.error = Some(reason);
                    return true;
                }
            }
        }
        let observed = self.external_configuration();
        if observed.parsed == self.external
            && observed.preserved == self.preserved_external
            && observed.error == self.error
        {
            return recovered;
        }
        self.external = observed.parsed;
        self.preserved_external = observed.preserved;
        self.error = observed.error;
        self.revision += 1;
        true
    }

    fn recover(&mut self) -> Result<bool> {
        let lock = self.journal_lock()?;
        self.recover_locked(&lock)
    }

    fn recover_locked(&mut self, lock: &File) -> Result<bool> {
        self.verify_journal()?;
        let prior = self.data.clone();
        let Some(target) = &prior.pending else {
            return Ok(false);
        };
        let target_digest = hash_bytes(&encode_project(target)?);
        let project = if let Some(replacement) = &prior.replacement {
            replacement.recorded_project(self.root.as_deref().context("Project is closed")?)?
        } else {
            self.project_file()?
        };
        let displaced = prior
            .replacement
            .as_ref()
            .map(|replacement| {
                let base = prior.base.as_deref().context("Missing replacement base")?;
                replacement.recover(&project, base)
            })
            .transpose()?;
        let conflict = displaced.as_ref().is_some_and(Displaced::must_preserve);
        let visible = self.disk();
        let published = !conflict
            && !matches!(displaced, Some(Displaced::None))
            && visible
                .as_ref()
                .is_ok_and(|(hash, _)| hash.as_deref() == Some(&target_digest));
        if published {
            self.data.previous = Some(std::mem::replace(&mut self.data.applied, target.clone()));
            if let Some(draft) = self.data.pending_rebased_draft.take() {
                self.data.draft = Some(draft);
            } else if self.data.draft == prior.pending {
                self.data.draft = None;
                self.data.apply_issue = None;
            }
            self.data.base = Some(target_digest.clone());
        } else if conflict {
            self.data.displaced = prior.replacement.clone();
            self.data.apply_issue = Some(ApplyIssue::Save);
            if matches!(displaced, Some(Displaced::Conflict))
                && visible
                    .as_ref()
                    .is_ok_and(|(hash, _)| hash.as_deref() == Some(&target_digest))
            {
                // This path contains our own publication. A newer external version is detected
                // independently once the displaced copy has been reviewed.
                self.data.base = Some(target_digest);
            }
        }
        self.data.pending = None;
        self.data.pending_rebased_draft = None;
        self.data.replacement = None;
        if let Err(error) = self.save_locked(lock) {
            self.data = prior;
            return Err(error);
        }
        self.revision += u64::from(published || conflict);
        self.applied_revision += u64::from(self.data.applied != prior.applied);
        if !conflict
            && let Some(replacement) = &prior.replacement
            && sync_parent(&self.local).is_ok()
        {
            // Keep the sidecar if the final journal rename could not be made durable.
            replacement.cleanup(&project);
        }
        Ok(published)
    }

    fn journal_lock(&self) -> Result<File> {
        ensure!(
            !self.unreadable,
            "Existing local configuration is unreadable and was kept"
        );
        fs::create_dir_all(
            self.local
                .parent()
                .context("Missing configuration directory")?,
        )?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.local.with_extension("lock"))?;
        lock.try_lock()
            .context("Local configuration is busy in another editor")?;
        Ok(lock)
    }

    fn save(&mut self) -> Result<()> {
        let lock = self.journal_lock()?;
        self.save_locked(&lock)
    }

    /// The same lock covers the pending record, project publication and final journal update.
    fn save_locked(&mut self, _lock: &File) -> Result<()> {
        let bytes = serde_json::to_vec(&self.data)?;
        ensure!(
            bytes.len() as u64 <= JOURNAL_LIMIT,
            "Local configuration is too large"
        );
        self.verify_journal()?;
        write_bytes(&self.local, &bytes)?;
        self.journal_digest = Some(hash_bytes(&bytes));
        Ok(())
    }

    fn verify_journal(&self) -> Result<()> {
        let actual = read_optional(&self.local, JOURNAL_LIMIT)?.map(|bytes| hash_bytes(&bytes));
        ensure!(
            actual == self.journal_digest,
            "Local configuration changed in another editor. Keep this draft and reopen to review the saved version."
        );
        Ok(())
    }

    fn commit(&mut self, revision: u64) -> Result<()> {
        self.commit_with(revision, || {})
    }

    fn commit_with(&mut self, revision: u64, before_publish: impl FnOnce()) -> Result<()> {
        let target = self.data.draft.clone().context("No changes to apply")?;
        self.publish_target(revision, target, None, before_publish)
    }

    /// Publish a validated target while keeping unrelated plugin-manager edits in the journal.
    fn publish_target(
        &mut self,
        revision: u64,
        target: Config,
        remaining: Option<Config>,
        before_publish: impl FnOnce(),
    ) -> Result<()> {
        self.refresh();
        ensure!(
            self.revision == revision,
            "Configuration changed during validation. Review it and Apply again."
        );
        ensure!(
            self.external.is_none() && self.data.displaced.is_none(),
            "Resolve the external change before applying"
        );
        ensure!(
            self.error.is_none() && self.data.pending.is_none(),
            "Configuration cannot be written in its current state"
        );
        if let Some(root) = &self.root {
            // Recover the local commit from the shared target if the process stops between writes.
            let path = project_path(root, PROJECT_CONFIG)?;
            let bytes = encode_project(&target)?;
            let parent = path
                .parent()
                .context("Missing project configuration directory")?;
            fs::create_dir_all(parent)?;
            let lock = self.journal_lock()?;
            let replacement = if self.data.base.is_some() {
                Some(Replacement::stage(&path, &bytes)?)
            } else {
                None
            };
            let staged = if replacement.is_none() {
                let mut staged = tempfile::NamedTempFile::new_in(parent)?;
                staged.write_all(&bytes)?;
                staged.as_file().sync_all()?;
                Some(staged)
            } else {
                None
            };
            self.data.pending = Some(target.clone());
            self.data.replacement = replacement.clone();
            self.data.pending_rebased_draft = remaining;
            if let Err(error) = self.save_locked(&lock) {
                self.data.pending_rebased_draft = None;
                self.data.pending = None;
                self.data.replacement = None;
                if let Some(replacement) = &replacement {
                    replacement.cleanup(&path);
                }
                return Err(error);
            }
            sync_parent(&self.local)?;
            let write = (|| {
                ensure!(
                    project_path(
                        self.root.as_deref().context("Project is closed")?,
                        PROJECT_CONFIG
                    )? == path
                        && self.disk()?.0 == self.data.base,
                    "Project configuration changed during Apply"
                );
                before_publish();
                if let Some(replacement) = &replacement {
                    replacement.publish(&path)?;
                } else {
                    // A file created by another writer after the check must stay in place.
                    staged
                        .expect("staged initial configuration")
                        .persist_noclobber(&path)?;
                    sync_parent(&path)?;
                }
                Ok(())
            })();
            let published = self.recover_locked(&lock)?;
            if !published {
                self.refresh();
                write?;
                anyhow::bail!("Project configuration changed during Apply");
            }
            return Ok(());
        }
        let prior = self.data.clone();
        self.data.previous = Some(std::mem::replace(&mut self.data.applied, target));
        self.data.draft = remaining;
        self.data.pending = None;
        self.data.apply_issue = None;
        if let Err(error) = self.save() {
            self.data = prior;
            return Err(error);
        }
        self.revision += 1;
        self.applied_revision += u64::from(self.data.applied != prior.applied);
        Ok(())
    }

    fn view(&self) -> ConfigView {
        ConfigView {
            revision: self.revision,
            applied: self.data.applied.clone(),
            draft: self.data.draft.clone(),
            external: self.external.as_ref().map(|(_, value)| value.clone()),
            error: self.error.clone(),
            apply_issue: self.data.apply_issue.map(ApplyIssue::description),
            previous: self.data.previous.clone(),
            preserved_external: self.preserved_external.clone(),
            replacement_conflict: self.data.displaced.is_some(),
        }
    }

    /// Clear this identity's settings from every retained version of a local scope.
    fn clean_settings(&mut self, namespace: &str) -> Result<bool> {
        ensure!(!self.unreadable, "Local settings could not be read");
        let mut next = self.data.clone();
        let before = next.settings_rejections.len();
        next.settings_rejections.retain(|name, _| {
            name.split_once('/')
                .is_none_or(|(owner, _)| owner != namespace)
        });
        let mut changed = before != next.settings_rejections.len();
        for config in std::iter::once(&mut next.applied)
            .chain(next.draft.iter_mut())
            .chain(next.previous.iter_mut())
        {
            if let Some(plugin) = config
                .plugins
                .iter_mut()
                .find(|plugin| plugin.identity.namespace() == namespace)
            {
                changed |= !plugin.settings.is_empty();
                plugin.settings.clear();
            }
        }
        if changed {
            let prior = std::mem::replace(&mut self.data, next);
            if let Err(error) = self.save() {
                self.data = prior;
                return Err(error);
            }
            self.revision += 1;
            self.applied_revision += u64::from(self.data.applied != prior.applied);
        }
        Ok(changed)
    }
}

/// Configuration is committed by the same supervisor that schedules rules.
pub(super) struct Configurations {
    root: PathBuf,
    scopes: BTreeMap<ConfigScope, Scope>,
    next_read: Instant,
    dirty: bool,
}

impl Configurations {
    /// Snapshot applied values without exposing an unrelated manager draft.
    pub(super) fn committed_settings(&self) -> BTreeMap<ConfigScope, Config> {
        self.scopes
            .iter()
            .map(|(&scope, state)| (scope, state.data.applied.clone()))
            .collect()
    }

    /// Patch one manifest page against the latest committed config; unrelated draft entries survive.
    pub(super) fn patch_settings(
        &mut self,
        scope: ConfigScope,
        manifest: &Manifest,
        owner_was_absent: bool,
        patches: &[crate::plugins::session::SettingPatch],
    ) -> Result<()> {
        let state = self.scopes.get_mut(&scope).context("Project is closed")?;
        self.dirty |= state.refresh();
        let mut target = state.data.applied.clone();
        let mut remaining = state.data.draft.clone();
        for patch in patches {
            let decl = manifest
                .settings
                .get(&patch.id)
                .context("Plugin setting is no longer declared")?;
            ensure!(
                !decl.secret
                    && (scope == ConfigScope::AllProjects
                        || decl.project == matches!(scope, ConfigScope::Project(_))),
                "Setting does not belong to this scope"
            );
            ensure!(
                patch.after.iter().all(|(key, value)| (key == &patch.id
                    || decl.renamed_from.contains(key))
                    && (patch.restore_snapshot || decl.accepts_value(value))),
                "Invalid value for {}",
                patch.id
            );
            let apply = |config: &mut Config| -> Result<()> {
                let settings = &mut config.plugin_mut(manifest.identity()).settings;
                let keys = std::iter::once(&patch.id).chain(&decl.renamed_from);
                let current: BTreeMap<_, _> = keys
                    .clone()
                    .filter_map(|id| settings.get(id).map(|v| (id.clone(), v.clone())))
                    .collect();
                ensure!(
                    current == patch.before,
                    "Setting {} changed in another view. Reopen settings.",
                    patch.id
                );
                for key in keys {
                    settings.remove(key);
                }
                settings.extend(patch.after.clone());
                Ok(())
            };
            apply(&mut target)?;
            if let Some(draft) = &mut remaining {
                apply(draft)?;
            }
        }
        if owner_was_absent {
            let identity = manifest.identity();
            for config in std::iter::once(&mut target).chain(remaining.iter_mut()) {
                config.plugins.retain(|plugin| {
                    plugin.identity != identity
                        || !plugin.settings.is_empty()
                        || !plugin.rules.is_empty()
                });
            }
        }
        if target == state.data.applied && remaining == state.data.draft {
            return Ok(());
        }
        if !matches!(scope, ConfigScope::Project(_)) {
            ensure!(
                serde_json::to_vec(&target)?.len() as u64 <= LIMIT,
                "Local configuration is too large"
            );
        }
        let result = state.publish_target(state.revision, target, remaining, || {});
        self.dirty = true;
        result
    }
    pub(super) fn new(root: PathBuf) -> Self {
        let global = Scope::load(root.join("config").join("global.json"), None);
        Self {
            root,
            scopes: BTreeMap::from([(ConfigScope::AllProjects, global)]),
            next_read: Instant::now(),
            dirty: true,
        }
    }

    pub(super) fn projects<'a>(&mut self, projects: impl Iterator<Item = &'a Project>) {
        let mut keep = BTreeSet::from([ConfigScope::AllProjects]);
        for project in projects {
            let scope = ConfigScope::Project(project.id);
            keep.insert(scope);
            if self
                .scopes
                .get(&scope)
                .is_none_or(|item| item.root.as_ref() != Some(&project.root))
            {
                let file = self
                    .root
                    .join("config")
                    .join(format!("{}.json", project_key(&project.root)));
                self.scopes
                    .insert(scope, Scope::load(file, Some(project.root.clone())));
                self.dirty = true;
            }
            let local_scope = ConfigScope::ProjectLocal(project.id);
            keep.insert(local_scope);
            let local_file = self
                .root
                .join("config")
                .join(format!("{}-local.json", project_key(&project.root)));
            if self
                .scopes
                .get(&local_scope)
                .is_none_or(|scope| scope.local != local_file)
            {
                self.scopes
                    .insert(local_scope, Scope::load(local_file, None));
                self.dirty = true;
            }
        }
        let count = self.scopes.len();
        self.scopes.retain(|key, _| keep.contains(key));
        self.dirty |= count != self.scopes.len();
    }

    pub(super) fn refresh(&mut self) {
        if Instant::now() < self.next_read {
            return;
        }
        self.next_read = Instant::now() + Duration::from_millis(750);
        for scope in self.scopes.values_mut() {
            self.dirty |= scope.refresh();
        }
    }

    pub(super) fn take_view(&mut self) -> Option<BTreeMap<ConfigScope, ConfigView>> {
        std::mem::take(&mut self.dirty).then(|| {
            self.scopes
                .iter()
                .map(|(&key, scope)| (key, scope.view()))
                .collect()
        })
    }

    pub(super) fn rule(&self, project: u64, key: &str, module: &str) -> Option<&Rule> {
        self.rule_with_draft(project, key, module, None)
    }

    fn config_at<'a>(
        &'a self,
        scope: ConfigScope,
        draft: Option<(ConfigScope, &'a Config)>,
    ) -> Option<&'a Config> {
        if let Some((target, draft)) = draft
            && target == scope
        {
            return Some(draft);
        }
        Some(&self.scopes.get(&scope)?.data.applied)
    }

    fn rule_with_draft<'a>(
        &'a self,
        project: u64,
        key: &str,
        module: &str,
        draft: Option<(ConfigScope, &'a Config)>,
    ) -> Option<&'a Rule> {
        [ConfigScope::Project(project), ConfigScope::AllProjects]
            .into_iter()
            .find_map(|scope| self.config_at(scope, draft)?.plugin(key)?.rules.get(module))
    }

    fn setting<'a>(
        &'a self,
        project: u64,
        key: &str,
        id: &str,
        decl: &'a SettingDecl,
        draft: Option<(ConfigScope, &'a Config)>,
    ) -> &'a Value {
        let project_scope = if decl.project {
            ConfigScope::Project(project)
        } else {
            ConfigScope::ProjectLocal(project)
        };
        [project_scope, ConfigScope::AllProjects]
            .into_iter()
            .find_map(|scope| {
                let settings = &self.config_at(scope, draft)?.plugin(key)?.settings;
                let value = settings.get(id).or_else(|| {
                    decl.renamed_from
                        .iter()
                        .find_map(|prior| settings.get(prior))
                })?;
                decl.accepts_value(value).then_some(value)
            })
            .unwrap_or(&decl.default)
    }

    pub(super) fn settings(
        &self,
        project: u64,
        key: &str,
        manifest: &Manifest,
    ) -> BTreeMap<String, Value> {
        manifest
            .settings
            .iter()
            .filter(|(_, setting)| !setting.secret)
            .map(|(id, decl)| {
                (
                    id.clone(),
                    self.setting(project, key, id, decl, None).clone(),
                )
            })
            .collect()
    }

    pub(super) fn settings_changed(
        &self,
        project: u64,
        namespace: &str,
        manifest: &Manifest,
        last: &BTreeMap<String, Value>,
    ) -> bool {
        let mut declared = manifest
            .settings
            .iter()
            .filter(|(_, setting)| !setting.secret);
        last.len() != declared.clone().count()
            || declared.any(|(id, decl)| {
                last.get(id) != Some(self.setting(project, namespace, id, decl, None))
            })
    }

    pub(super) fn rejected_settings(&self, key: &Key, release: &str, manifest: &Manifest) -> bool {
        let project = key.instance.project;
        let name = format!("{}/{}", key.namespace, key.instance.module);
        self.scopes
            .get(&ConfigScope::ProjectLocal(project))
            .and_then(|scope| scope.data.settings_rejections.get(&name))
            .is_some_and(|stamp| {
                settings_stamp(release, &self.settings(project, &key.namespace, manifest))
                    .is_some_and(|current| &current == stamp)
            })
    }

    /// Persist only a bounded marker. A guest's explanation can contain private text.
    pub(super) fn note_settings_rejection(
        &mut self,
        key: &Key,
        release: &str,
        values: &BTreeMap<String, Value>,
        rejected: bool,
    ) {
        let Some(scope) = self
            .scopes
            .get_mut(&ConfigScope::ProjectLocal(key.instance.project))
        else {
            return;
        };
        let name = format!("{}/{}", key.namespace, key.instance.module);
        let markers = &mut scope.data.settings_rejections;
        let previous = if rejected {
            let Some(stamp) = settings_stamp(release, values) else {
                return;
            };
            if markers.get(&name) == Some(&stamp)
                || (markers.len() >= 256 && !markers.contains_key(&name))
            {
                return;
            }
            markers.insert(name.clone(), stamp)
        } else {
            let Some(stamp) = markers.remove(&name) else {
                return;
            };
            Some(stamp)
        };
        if scope.save().is_err() {
            if let Some(stamp) = previous {
                scope.data.settings_rejections.insert(name, stamp);
            } else {
                scope.data.settings_rejections.remove(&name);
            }
        }
    }

    /// Poll the cheap committed revisions before resolving effective values for a live module.
    pub(super) fn settings_revision(&self, project: u64) -> [u64; 3] {
        [
            ConfigScope::AllProjects,
            ConfigScope::Project(project),
            ConfigScope::ProjectLocal(project),
        ]
        .map(|scope| {
            self.scopes
                .get(&scope)
                .map_or(0, |scope| scope.applied_revision)
        })
    }

    /// Only writes owned by a module with a changed effective rule or setting delay Apply.
    pub(super) fn affects_write(
        &self,
        scope: ConfigScope,
        revision: u64,
        owner: &Key,
        manifest: &Manifest,
    ) -> bool {
        let project = owner.instance.project;
        if matches!(scope, ConfigScope::Project(id) | ConfigScope::ProjectLocal(id) if id != project)
        {
            return false;
        }
        let Some(state) = self
            .scopes
            .get(&scope)
            .filter(|state| state.revision == revision)
        else {
            return false;
        };
        let Some(draft) = state.data.draft.as_ref() else {
            return false;
        };
        let draft = Some((scope, draft));
        let key = &owner.namespace;
        let module = &owner.instance.module;
        self.rule(project, key, module) != self.rule_with_draft(project, key, module, draft)
            || manifest
                .settings
                .iter()
                .filter(|(_, setting)| !setting.secret)
                .any(|(id, decl)| {
                    self.setting(project, key, id, decl, None)
                        != self.setting(project, key, id, decl, draft)
                })
    }

    pub(super) fn stage(&mut self, key: ConfigScope, value: Config) -> Result<()> {
        check_identities(&value)?;
        if matches!(key, ConfigScope::Project(_)) {
            encode_project(&value)?;
        } else {
            ensure!(
                serde_json::to_vec(&value)?.len() as u64 <= LIMIT,
                "Local configuration is too large"
            );
        }
        let scope = self.scopes.get_mut(&key).context("Project is closed")?;
        ensure!(
            key == ConfigScope::AllProjects || value.shortcuts.is_empty(),
            "Shortcut settings are global"
        );
        ensure!(
            !matches!(key, ConfigScope::ProjectLocal(_))
                || value.plugins.iter().all(|plugin| plugin.rules.is_empty())
                    && value.formatters.is_empty(),
            "Project-local storage is for local settings"
        );
        scope.data.draft = Some(value);
        scope.data.apply_issue = None;
        scope.revision += 1;
        self.dirty = true;
        scope.save()
    }

    pub(super) fn edit(&mut self, key: ConfigScope, edit: ConfigEdit) -> Result<()> {
        let scope = self.scopes.get(&key).context("Project is closed")?;
        let mut config = scope
            .data
            .draft
            .as_ref()
            .unwrap_or(&scope.data.applied)
            .clone();
        match edit {
            ConfigEdit::Rule {
                owner,
                module,
                value,
            } => {
                let rules = &mut config.plugin_mut(owner).rules;
                if let Some(value) = value {
                    rules.insert(module, value);
                } else {
                    rules.remove(&module);
                }
            }
            ConfigEdit::Setting {
                owner,
                id,
                renamed_from,
                original,
                value,
            } => {
                let settings = &mut config.plugin_mut(owner).settings;
                let latest = settings
                    .get(&id)
                    .or_else(|| renamed_from.iter().find_map(|old| settings.get(old)));
                ensure!(
                    latest == original.as_ref(),
                    "This setting changed while you were editing it. Open it again to review the current value."
                );
                if let Some(value) = value {
                    settings.insert(id, value);
                } else {
                    settings.remove(&id);
                    for old in renamed_from {
                        settings.remove(&old);
                    }
                }
            }
            ConfigEdit::Shortcut {
                namespace,
                command,
                value,
            } => {
                let shortcuts = config.shortcuts.entry(namespace).or_default();
                if let Some(value) = value {
                    shortcuts.insert(command, value);
                } else {
                    shortcuts.remove(&command);
                }
            }
            ConfigEdit::Formatter {
                extension,
                provider,
            } => {
                config.formatters.entry(extension).or_default().provider = Some(provider);
            }
            ConfigEdit::ToggleFormatOnSave(extension) => {
                let setting = &mut config.formatters.entry(extension).or_default().on_save;
                *setting = Some(!setting.unwrap_or(false));
            }
            ConfigEdit::Restore {
                owner,
                rules,
                revision,
            } => {
                ensure!(
                    scope.revision == revision,
                    "Configuration changed. Review the previous values again."
                );
                let previous = scope
                    .data
                    .previous
                    .as_ref()
                    .and_then(|config| config.plugin(&owner.namespace()));
                let target = config.plugin_mut(owner);
                if rules {
                    target.rules = previous.map(|old| old.rules.clone()).unwrap_or_default();
                } else {
                    target.settings = previous.map(|old| old.settings.clone()).unwrap_or_default();
                }
            }
        }
        self.stage(key, config)
    }

    /// Merge SDK settings into the same draft used by the native settings panel.
    pub(super) fn stage_sdk(
        &mut self,
        scope: ConfigScope,
        manifest: &Manifest,
        settings: BTreeMap<String, Value>,
    ) -> Result<()> {
        let state = self
            .scopes
            .get(&scope)
            .context("Settings scope is closed")?;
        let mut value = state
            .data
            .draft
            .as_ref()
            .unwrap_or(&state.data.applied)
            .clone();
        let target = value.plugin_mut(manifest.identity());
        for (id, setting) in settings {
            let decl = manifest
                .settings
                .get(&id)
                .context("Unknown plugin setting")?;
            ensure!(
                !decl.secret && decl.accepts_value(&setting),
                "Invalid plugin setting value"
            );
            ensure!(
                scope == ConfigScope::AllProjects
                    || decl.project == matches!(scope, ConfigScope::Project(_)),
                "Setting does not belong to this scope"
            );
            target.settings.insert(id, setting);
        }
        self.stage(scope, value)
    }

    pub(super) fn resolve(
        &mut self,
        key: ConfigScope,
        revision: u64,
        take_external: bool,
    ) -> Result<()> {
        let scope = self.scopes.get_mut(&key).context("Project is closed")?;
        self.dirty |= scope.refresh();
        ensure!(
            scope.revision == revision,
            "Configuration changed again. Compare the latest versions before choosing."
        );
        let displaced = scope.data.displaced.is_some();
        let external = scope.external.as_ref();
        ensure!(
            external.is_some() || displaced && !take_external,
            "No external version to resolve"
        );
        let prior = scope.data.clone();
        if take_external {
            let (_, candidate) = external.context("No readable external version to use")?;
            scope.data.draft = Some(candidate.clone());
            scope.data.apply_issue = None;
        }
        if displaced {
            ensure!(
                scope.preserved_external.is_some(),
                "Locate the preserved external file before resolving this conflict"
            );
            scope.data.displaced = None;
        } else if let Some((hash, _)) = external {
            scope.data.base = (!hash.is_empty()).then(|| hash.clone());
        }
        if let Err(error) = scope.save() {
            scope.data = prior;
            return Err(error);
        }
        scope.external = None;
        scope.refresh();
        scope.revision += 1;
        self.dirty = true;
        Ok(())
    }

    pub(super) fn prepare(
        &mut self,
        key: ConfigScope,
        installed: &BTreeMap<String, Arc<Installed>>,
    ) -> Result<(u64, Vec<String>)> {
        let scope = self.scopes.get_mut(&key).context("Project is closed")?;
        self.dirty |= scope.refresh();
        ensure!(
            scope.external.is_none() && scope.data.displaced.is_none(),
            "Configuration also changed on disk. Choose a version or merge before Apply."
        );
        ensure!(scope.error.is_none(), "Configuration is unreadable");
        let draft = scope.data.draft.as_ref().context("No changes to apply")?;
        let mut scripts = Vec::new();
        for plugin in &draft.plugins {
            for rule in plugin.rules.values() {
                if let Rule::Script(source) = rule {
                    ensure!(source.len() <= 128 * 1024, "Rule source is too large");
                    scripts.push(source.clone());
                }
            }
            let namespace = plugin.identity.namespace();
            let Some(record) = installed.get(&namespace) else {
                continue;
            };
            let applied = scope.data.applied.plugin(&namespace);
            for (id, value) in &plugin.settings {
                let Some(decl) = record.release.manifest.settings.get(id) else {
                    continue;
                };
                ensure!(!decl.secret, "Secret settings use the credentials store");
                // An upgrade can change the type or scope of an already committed value.
                // Leave that value available for review while applying unrelated changes.
                if applied.and_then(|old| old.settings.get(id)) == Some(value) {
                    continue;
                }
                ensure!(
                    key == ConfigScope::AllProjects
                        || decl.project == matches!(key, ConfigScope::Project(_)),
                    "{id} does not belong to this scope"
                );
                ensure!(decl.accepts_value(value), "Incorrect type for setting {id}");
            }
        }
        Ok((scope.revision, scripts))
    }

    pub(super) fn commit(&mut self, scope: ConfigScope, revision: u64) -> Result<()> {
        self.dirty = true;
        self.scopes
            .get_mut(&scope)
            .context("Project is closed")?
            .commit(revision)
    }

    /// Attach a failed Apply to the draft it actually checked. A save failure leaves it in memory.
    pub(super) fn note_apply_issue(
        &mut self,
        key: ConfigScope,
        revision: Option<u64>,
        issue: ApplyIssue,
    ) -> bool {
        let Some(scope) = self.scopes.get_mut(&key) else {
            return false;
        };
        if revision.is_some_and(|revision| revision != scope.revision) {
            return false;
        }
        if scope.data.draft.is_some() && scope.data.apply_issue != Some(issue) {
            scope.data.apply_issue = Some(issue);
            self.dirty = true;
            let _ = scope.save();
        }
        true
    }

    /// Remove only ordinary local settings for an uninstalled identity. Keep rules and shared files.
    pub(super) fn clean_settings(&mut self, namespace: &str) -> Result<()> {
        let mut open = BTreeSet::new();
        for (key, scope) in &mut self.scopes {
            if !matches!(key, ConfigScope::Project(_)) {
                self.dirty |= scope.clean_settings(namespace)?;
                open.insert(scope.local.clone());
            }
        }
        let entries = match std::fs::read_dir(self.root.join("config")) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name();
            let is_local_scope = name
                .to_str()
                .and_then(|name| name.strip_suffix("-local.json"))
                .is_some_and(|key| {
                    key.len() == 64
                        && key
                            .bytes()
                            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
                });
            let path = entry.path();
            if is_local_scope && entry.file_type()?.is_file() && !open.contains(&path) {
                Scope::load(path, None).clean_settings(namespace)?;
            }
        }
        Ok(())
    }
}

fn read_optional(path: &Path, limit: u64) -> Result<Option<Vec<u8>>> {
    match read_limited(path, limit) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error)
            if error
                .downcast_ref::<io::Error>()
                .is_some_and(|error| error.kind() == io::ErrorKind::NotFound) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn read_project(path: &Path) -> Result<(Option<String>, Config)> {
    let Some(bytes) = read_optional(path, LIMIT)? else {
        return Ok((None, Config::default()));
    };
    let value: Config =
        serde_json::from_slice(&bytes).context("Invalid project plugin configuration")?;
    check_identities(&value)?;
    Ok((Some(hash_bytes(&bytes)), value))
}

fn read_preserved_project(path: &Path) -> Result<(Option<String>, Config)> {
    ensure!(
        fs::symlink_metadata(path)?.is_file(),
        "Displaced configuration is not a regular file"
    );
    read_project(path)
}

fn settings_stamp(release: &str, values: &BTreeMap<String, Value>) -> Option<String> {
    serde_json::to_vec(&(release, values))
        .ok()
        .map(|bytes| hash_bytes(&bytes))
}

fn encode_project(config: &Config) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec_pretty(config)?;
    ensure!(
        bytes.len() as u64 <= LIMIT,
        "Project configuration is too large"
    );
    Ok(bytes)
}

fn check_identities(config: &Config) -> Result<()> {
    let mut known = BTreeSet::new();
    for plugin in &config.plugins {
        let identity = Identity::new(plugin.identity.name.clone(), plugin.identity.id.clone())?;
        ensure!(known.insert(identity), "Duplicate plugin configuration");
    }
    for (extension, format) in &config.formatters {
        ensure!(
            !extension.is_empty()
                && extension.len() <= 32
                && extension
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'),
            "Invalid formatter file extension"
        );
        if let Some(provider) = &format.provider {
            Identity::new(provider.name.clone(), provider.id.clone())?;
        }
    }
    for (namespace, shortcuts) in &config.shortcuts {
        ensure!(
            namespace.len() == 64 && namespace.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "Invalid shortcut owner"
        );
        ensure!(
            shortcuts
                .values()
                .all(|value| value.len() <= 64 && value.is_ascii()),
            "Invalid shortcut"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::package::{read_json, write_json};

    fn config(rule: Rule) -> Config {
        let mut config = Config::default();
        config
            .plugin_mut(Identity::new("Fixture".into(), "fixture".into()).unwrap())
            .rules
            .insert("module".into(), rule);
        config
    }

    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    #[test]
    fn replacement_keeps_exact_late_writer_and_never_overwrites_a_newer_file() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("project");
        let path = root.join(PROJECT_CONFIG);
        let profile = temp.path().join("profile");
        let initial = config(Rule::Never);
        let target = config(Rule::Always);
        let external = config(Rule::Script("return false;".into()));
        let latest = config(Rule::Script("return true;".into()));
        write_json(&path, &initial)?;
        let mut raw = b" \r\n".to_vec();
        raw.extend(serde_json::to_vec(&external)?);
        raw.extend(b"\r\n ");
        let project = Project {
            id: 1,
            root,
            document: None,
            open_documents: Vec::new(),
        };
        let key = ConfigScope::Project(1);
        let mut state = Configurations::new(profile.clone());
        state.projects(std::iter::once(&project));
        state.stage(key, target.clone())?;
        let (revision, _) = state.prepare(key, &BTreeMap::new())?;
        let scope = state.scopes.get_mut(&key).unwrap();
        let error = scope
            .commit_with(revision, || write_bytes(&path, &raw).unwrap())
            .unwrap_err();
        assert!(error.to_string().contains("changed during Apply"));
        assert_eq!(scope.data.applied, initial);
        assert_eq!(scope.data.draft, Some(target.clone()));
        let backup = scope.preserved_external.clone().unwrap();
        assert_eq!(fs::read(&backup)?, raw);
        assert_eq!(
            scope.external.as_ref().map(|(_, value)| value),
            Some(&external)
        );
        drop(state);

        // An additional external writer must survive reopening and resolving the first conflict.
        write_json(&path, &latest)?;
        let mut reopened = Configurations::new(profile);
        reopened.projects(std::iter::once(&project));
        let view = reopened.take_view().unwrap();
        assert_eq!(view[&key].applied, initial);
        assert_eq!(view[&key].draft, Some(target));
        assert_eq!(view[&key].external, Some(external.clone()));
        assert_eq!(view[&key].preserved_external.as_ref(), Some(&backup));
        reopened.resolve(key, view[&key].revision, true)?;
        let view = reopened.take_view().unwrap();
        assert_eq!(view[&key].draft, Some(external));
        assert_eq!(view[&key].external, Some(latest.clone()));
        assert_eq!(read_json::<Config>(&path, LIMIT)?, Some(latest));
        assert_eq!(fs::read(backup)?, raw);
        Ok(())
    }

    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    #[test]
    fn interrupted_replacement_recovers_the_displaced_raw_bytes() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("project");
        let path = root.join(PROJECT_CONFIG);
        let local = temp.path().join("profile/config.json");
        let initial = config(Rule::Never);
        let target = config(Rule::Always);
        write_json(&path, &initial)?;
        let mut scope = Scope::load(local.clone(), Some(root.clone()));
        let canonical = scope.project_file()?;
        let replacement = Replacement::stage(&canonical, &encode_project(&target)?)?;
        scope.data.draft = Some(target.clone());
        scope.data.pending = Some(target.clone());
        scope.data.replacement = Some(replacement.clone());
        scope.save()?;

        let raw = vec![b'x'; LIMIT as usize + 19];
        write_bytes(&path, &raw)?;
        replacement.publish(&canonical)?;
        let backup = replacement.backup(&canonical)?;
        drop(scope);

        let reopened = Scope::load(local, Some(root));
        assert_eq!(reopened.data.applied, initial);
        assert_eq!(reopened.data.draft, Some(target));
        assert!(reopened.data.pending.is_none());
        assert!(reopened.data.displaced.is_some());
        assert_eq!(reopened.preserved_external, Some(backup.clone()));
        assert!(reopened.external.is_none());
        assert!(reopened.error.is_some());
        assert_eq!(fs::read(backup)?, raw);
        Ok(())
    }

    #[test]
    fn written_size_limits_allow_exact_project_capacity_and_reject_pretty_overflow() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("project");
        fs::create_dir_all(&root)?;
        let project = Project {
            id: 1,
            root: root.clone(),
            document: None,
            open_documents: Vec::new(),
        };
        let profile = temp.path().join("profile");
        let mut state = Configurations::new(profile.clone());
        state.projects(std::iter::once(&project));
        let scope = ConfigScope::Project(project.id);
        let mut target = config(Rule::Always);
        target.plugins[0]
            .settings
            .insert("payload".into(), Value::String(String::new()));
        let payload_size = LIMIT as usize - encode_project(&target)?.len();
        for character in ['a', 'b', 'c'] {
            target.plugins[0].settings.insert(
                "payload".into(),
                Value::String(character.to_string().repeat(payload_size)),
            );
            state.stage(scope, target.clone())?;
            let (revision, _) = state.prepare(scope, &BTreeMap::new())?;
            state.commit(scope, revision)?;
            assert_eq!(fs::metadata(root.join(PROJECT_CONFIG))?.len(), LIMIT);
        }
        let mut oversized = target.clone();
        oversized.plugins[0].settings.insert(
            "payload".into(),
            serde_json::json!(vec![vec![vec![0; 128]; 16]; 16]),
        );
        assert!((serde_json::to_vec(&oversized)?.len() as u64) < LIMIT / 2);
        assert!((serde_json::to_vec_pretty(&oversized)?.len() as u64) > LIMIT);
        assert!(state.stage(scope, oversized).is_err());
        let mut reopened = Configurations::new(profile);
        reopened.projects(std::iter::once(&project));
        let view = reopened.take_view().unwrap();
        assert_eq!(view[&scope].applied, target);
        assert!(view[&scope].draft.is_none());
        assert!(view[&scope].error.is_none());
        Ok(())
    }

    #[test]
    fn competing_profile_session_cannot_overwrite_a_newer_journal() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let key = ConfigScope::AllProjects;
        let mut first = Configurations::new(temp.path().into());
        let mut stale = Configurations::new(temp.path().into());
        let committed_draft = config(Rule::Always);
        let attempted_draft = config(Rule::Never);
        first.stage(key, committed_draft.clone())?;
        assert!(stale.stage(key, attempted_draft.clone()).is_err());
        assert_eq!(
            stale.take_view().unwrap()[&key].draft,
            Some(attempted_draft.clone())
        );
        let (revision, _) = stale.prepare(key, &BTreeMap::new())?;
        assert!(stale.commit(key, revision).is_err());
        let mut reopened = Configurations::new(temp.path().into());
        let view = reopened.take_view().unwrap();
        assert_eq!(view[&key].draft, Some(committed_draft));
        assert_eq!(view[&key].applied, Config::default());
        assert_eq!(
            stale.take_view().unwrap()[&key].draft,
            Some(attempted_draft)
        );
        Ok(())
    }

    #[test]
    fn selecting_a_stale_external_version_does_not_resolve_the_next_change() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("project");
        let path = root.join(PROJECT_CONFIG);
        let initial = config(Rule::Never);
        write_json(&path, &initial)?;
        let project = Project {
            id: 1,
            root,
            document: None,
            open_documents: Vec::new(),
        };
        let scope = ConfigScope::Project(project.id);
        let mut state = Configurations::new(temp.path().join("profile"));
        state.projects(std::iter::once(&project));
        let draft = config(Rule::Script("return true;".into()));
        state.stage(scope, draft.clone())?;
        write_json(&path, &config(Rule::Always))?;
        assert!(state.prepare(scope, &BTreeMap::new()).is_err());
        let shown = state.take_view().unwrap()[&scope].revision;
        let newest = config(Rule::Script("return false;".into()));
        write_json(&path, &newest)?;
        assert!(state.resolve(scope, shown, true).is_err());
        let view = state.take_view().unwrap();
        assert_eq!(view[&scope].applied, initial);
        assert_eq!(view[&scope].draft, Some(draft));
        assert_eq!(view[&scope].external, Some(newest));
        Ok(())
    }

    #[test]
    fn settings_dialog_applies_project_field_without_committing_manager_draft() -> Result<()> {
        use crate::plugins::session::SettingPatch;
        use serde_json::json;
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("project");
        fs::create_dir_all(&root)?;
        let project = Project {
            id: 1,
            root,
            document: None,
            open_documents: vec![],
        };
        let profile = temp.path().join("profile");
        let scope = ConfigScope::Project(1);
        let manifest: Manifest = serde_json::from_value(json!({
            "name":"Example", "id":"example", "version":"1.0.0", "engines":{"nanobug":"*"},
            "settings":{"color":{"title":"Color","default":"green","project":true,"renamed_from":["old"]},
                "size":{"title":"Size","default":2,"project":true}}
        }))?;
        let key = manifest.identity().namespace();
        let mut state = Configurations::new(profile.clone());
        state.projects(std::iter::once(&project));
        let opening = BTreeMap::from([("old".into(), json!("blue"))]);
        let changed = BTreeMap::from([("color".into(), json!("red"))]);
        let mut initial = Config::default();
        initial.plugin_mut(manifest.identity()).settings = opening.clone();
        state.stage(scope, initial)?;
        let (revision, _) = state.prepare(scope, &BTreeMap::new())?;
        state.commit(scope, revision)?;
        state.stage_sdk(
            scope,
            &manifest,
            BTreeMap::from([("size".to_owned(), json!(9))]),
        )?;
        let apply = || SettingPatch {
            restore_snapshot: false,
            id: "color".into(),
            before: opening.clone(),
            after: changed.clone(),
        };
        state.patch_settings(scope, &manifest, false, &[apply()])?;
        assert!(
            state
                .patch_settings(scope, &manifest, false, &[apply()])
                .is_err()
        );
        let mut reopened = Configurations::new(profile);
        reopened.projects(std::iter::once(&project));
        let view = reopened.take_view().unwrap();
        assert_eq!(view[&scope].applied.plugin(&key).unwrap().settings, changed);
        let draft = &view[&scope]
            .draft
            .as_ref()
            .unwrap()
            .plugin(&key)
            .unwrap()
            .settings;
        assert_eq!(draft.get("size"), Some(&json!(9)));
        assert_eq!(draft.get("color"), Some(&json!("red")));
        reopened.patch_settings(
            scope,
            &manifest,
            false,
            &[SettingPatch {
                restore_snapshot: true,
                id: "color".into(),
                before: changed,
                after: opening.clone(),
            }],
        )?;
        assert_eq!(
            reopened.committed_settings()[&scope]
                .plugin(&key)
                .unwrap()
                .settings,
            opening
        );
        Ok(())
    }

    #[test]
    fn native_settings_edit_preserves_new_sdk_values() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut state = Configurations::new(temp.path().into());
        let key = ConfigScope::AllProjects;
        let manifest: Manifest = serde_json::from_value(serde_json::json!({
            "name": "Fixture", "id": "fixture", "version": "1.0.0", "engines": {"nanobug": "*"},
            "settings": {
                "count": {"title": "Count", "default": 0},
                "flag": {"title": "Flag", "default": true}
            }
        }))?;
        let owner = manifest.identity();
        let namespace = owner.namespace();
        let edit_count = |original, value| ConfigEdit::Setting {
            owner: owner.clone(),
            id: "count".into(),
            renamed_from: Vec::new(),
            original,
            value: Some(value),
        };
        state.stage_sdk(
            key,
            &manifest,
            BTreeMap::from([("flag".into(), Value::Bool(false))]),
        )?;
        state.edit(key, edit_count(None, Value::from(1)))?;
        state.stage_sdk(
            key,
            &manifest,
            BTreeMap::from([("count".into(), Value::from(2))]),
        )?;
        assert!(
            state
                .edit(key, edit_count(Some(Value::from(1)), Value::from(3)))
                .is_err()
        );
        let view = state.take_view().unwrap();
        let settings = &view[&key]
            .draft
            .as_ref()
            .unwrap()
            .plugin(&namespace)
            .unwrap()
            .settings;
        assert_eq!(settings["count"], 2);
        assert_eq!(settings["flag"], false);
        Ok(())
    }

    #[test]
    fn upgrade_keeps_legacy_settings_while_applying_unrelated_changes() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let project = Project {
            id: 1,
            root: temp.path().join("workspace"),
            document: None,
            open_documents: Vec::new(),
        };
        fs::create_dir(&project.root)?;
        let config_file = project.root.join(PROJECT_CONFIG);
        let mut state = Configurations::new(temp.path().join("profile"));
        state.projects(std::iter::once(&project));
        let scope = ConfigScope::Project(project.id);
        let updated: Manifest = serde_json::from_value(serde_json::json!({
            "name": "Fixture", "id": "fixture", "version": "1.1.0", "engines": {"nanobug": "*"},
            "settings": {
                "flag": {"title": "Flag", "default": true, "project": true, "renamed_from": ["old-flag"]},
                "count": {"title": "Count", "default": 2, "project": true},
                "absent": {"title": "Inherited", "default": 7, "project": true}
            }
        }))?;
        let owner = updated.identity();
        let namespace = owner.namespace();
        let mut old = Config::default();
        old.plugin_mut(owner.clone()).settings = BTreeMap::from([
            ("old-flag".into(), Value::Bool(false)),
            ("count".into(), Value::String("before the upgrade".into())),
            ("removed".into(), Value::from(0)),
        ]);
        state.stage(scope, old)?;
        let (revision, _) = state.prepare(scope, &BTreeMap::new())?;
        state.commit(scope, revision)?;
        let previous = fs::read(&config_file)?;
        let installed: Installed = serde_json::from_value(serde_json::json!({
            "release": {"manifest": updated, "digest": "fixture", "files": {}, "source": "plugin.zip"},
            "grants": [], "enabled": true
        }))?;
        let resolved = state.settings(project.id, &namespace, &installed.release.manifest);
        assert_eq!(resolved["flag"], false);
        assert_eq!(resolved["count"], 2);
        assert_eq!(resolved["absent"], 7);
        state.stage_sdk(
            scope,
            &installed.release.manifest,
            BTreeMap::from([("flag".into(), resolved["flag"].clone())]),
        )?;
        state.edit(
            scope,
            ConfigEdit::Rule {
                owner: owner.clone(),
                module: "test".into(),
                value: Some(Rule::Never),
            },
        )?;
        assert_eq!(fs::read(&config_file)?, previous);
        let records = BTreeMap::from([(namespace.clone(), Arc::new(installed))]);
        let (revision, _) = state.prepare(scope, &records)?;
        state.commit(scope, revision)?;
        let saved: Config = serde_json::from_slice(&fs::read(&config_file)?)?;
        let saved = &saved.plugin(&namespace).unwrap().settings;
        assert_eq!(saved["old-flag"], false);
        assert_eq!(saved["flag"], false);
        assert_eq!(saved["count"], "before the upgrade");
        assert_eq!(saved["removed"], 0);
        assert!(!saved.contains_key("absent"));
        state.edit(
            scope,
            ConfigEdit::Setting {
                owner,
                id: "count".into(),
                renamed_from: Vec::new(),
                original: Some(Value::String("before the upgrade".into())),
                value: Some(Value::String("another incorrect type".into())),
            },
        )?;
        assert!(state.prepare(scope, &records).is_err());
        Ok(())
    }

    #[test]
    fn project_apply_keeps_external_changes_and_reopens_the_last_target_and_draft() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("project");
        std::fs::create_dir_all(&root)?;
        let project = Project {
            id: 1,
            root: root.clone(),
            document: None,
            open_documents: Vec::new(),
        };
        let profile = temp.path().join("profile");
        let scope = ConfigScope::Project(project.id);
        let mut state = Configurations::new(profile.clone());
        state.projects(std::iter::once(&project));
        let disabled = config(Rule::Never);
        state.stage(scope, disabled.clone())?;
        let external = config(Rule::Always);
        write_json(&root.join(PROJECT_CONFIG), &external)?;
        assert!(state.prepare(scope, &BTreeMap::new()).is_err());
        let view = state.take_view().unwrap();
        assert_eq!(view[&scope].draft, Some(disabled.clone()));
        assert_eq!(view[&scope].external, Some(external.clone()));
        assert_eq!(view[&scope].applied, Config::default());
        state.resolve(scope, view[&scope].revision, false)?;
        let (revision, _) = state.prepare(scope, &BTreeMap::new())?;
        state.commit(scope, revision)?;
        assert_eq!(
            read_json::<Config>(&root.join(PROJECT_CONFIG), LIMIT)?,
            Some(disabled.clone())
        );
        state.stage(scope, external.clone())?;
        drop(state);
        let mut reopened = Configurations::new(profile);
        reopened.projects(std::iter::once(&project));
        let view = reopened.take_view().unwrap();
        assert_eq!(view[&scope].applied, disabled);
        assert_eq!(view[&scope].draft, Some(external));
        Ok(())
    }

    #[test]
    fn apply_checks_a_new_disk_version_and_keeps_explicit_project_overrides() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("project");
        let path = root.join(PROJECT_CONFIG);
        let initial = Config::default();
        write_json(&path, &initial)?;
        let project = Project {
            id: 1,
            root,
            document: None,
            open_documents: Vec::new(),
        };
        let mut state = Configurations::new(temp.path().join("profile"));
        state.projects(std::iter::once(&project));
        let manifest: Manifest = serde_json::from_value(serde_json::json!({
            "name": "Fixture", "id": "fixture", "version": "1.0.0", "engines": {"nanobug": "*"},
            "settings": {
                "flag": {"title": "Flag", "default": true, "project": true},
                "count": {"title": "Count", "default": 7, "project": true},
                "label": {"title": "Label", "default": "default", "project": true}
            }
        }))?;
        let owner = manifest.identity();
        let namespace = owner.namespace();
        let scope = ConfigScope::Project(project.id);
        let mut global = config(Rule::Always);
        global.plugin_mut(owner.clone()).settings = BTreeMap::from([
            ("flag".into(), true.into()),
            ("count".into(), 2.into()),
            ("label".into(), "global".into()),
        ]);
        state.stage(ConfigScope::AllProjects, global)?;
        let (revision, _) = state.prepare(ConfigScope::AllProjects, &BTreeMap::new())?;
        state.commit(ConfigScope::AllProjects, revision)?;
        let mut draft = config(Rule::Never);
        let explicit = BTreeMap::from([
            ("flag".into(), false.into()),
            ("count".into(), 0.into()),
            ("label".into(), "".into()),
        ]);
        draft.plugin_mut(owner).settings = explicit.clone();
        state.stage(scope, draft.clone())?;
        assert_eq!(
            state.rule(project.id, &namespace, "module"),
            Some(&Rule::Always)
        );
        assert_eq!(
            state.settings(project.id, &namespace, &manifest)["label"],
            "global"
        );

        let (validated, _) = state.prepare(scope, &BTreeMap::new())?;
        // Another editor can save while the rule check is running.
        let external = config(Rule::Script("return true;".into()));
        write_json(&path, &external)?;
        assert!(state.commit(scope, validated).is_err());
        let view = &state.take_view().unwrap()[&scope];
        assert_eq!(view.applied, initial);
        assert_eq!(view.draft.as_ref(), Some(&draft));
        assert_eq!(view.external.as_ref(), Some(&external));
        assert_eq!(read_json::<Config>(&path, LIMIT)?, Some(external));

        state.resolve(scope, view.revision, false)?;
        let (revision, _) = state.prepare(scope, &BTreeMap::new())?;
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            let locked = OpenOptions::new()
                .read(true)
                .share_mode(0x01 | 0x02)
                .open(&path)?;
            assert!(state.commit(scope, revision).is_err());
            let failed = &state.take_view().unwrap()[&scope];
            assert_eq!(failed.applied, initial);
            assert_eq!(failed.draft.as_ref(), Some(&draft));
            drop(locked);
        }
        state.commit(scope, revision)?;
        assert_eq!(state.settings(project.id, &namespace, &manifest), explicit);
        assert_eq!(
            state.rule(project.id, &namespace, "module"),
            Some(&Rule::Never)
        );
        assert_eq!(read_json::<Config>(&path, LIMIT)?, Some(draft));
        Ok(())
    }

    #[test]
    fn interrupted_apply_recovers_only_the_target_actually_written_to_the_project() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("project");
        let local = temp.path().join("profile/config.json");
        let disk = root.join(PROJECT_CONFIG);
        let initial = config(Rule::Never);
        let target = config(Rule::Always);
        write_json(&disk, &initial)?;
        let mut scope = Scope::load(local.clone(), Some(root.clone()));
        scope.data.draft = Some(target.clone());
        scope.data.pending = Some(target.clone());
        scope.save()?;

        // Simulate interruption before publishing the project file.
        let mut scope = Scope::load(local.clone(), Some(root.clone()));
        assert_eq!(scope.data.applied, initial);
        assert_eq!(scope.data.draft, Some(target.clone()));
        assert!(scope.data.pending.is_none());

        // Simulate interruption after publishing the shared target, before the local final write.
        scope.data.pending = Some(target.clone());
        scope.save()?;
        write_json(&disk, &target)?;
        let scope = Scope::load(local, Some(root));
        assert_eq!(scope.data.applied, target);
        assert_eq!(scope.data.previous, Some(initial));
        assert!(scope.data.draft.is_none());
        assert!(scope.data.pending.is_none());
        assert!(scope.external.is_none());
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn a_failed_conflict_choice_keeps_both_versions_available() -> Result<()> {
        use std::os::windows::fs::OpenOptionsExt;

        let temp = tempfile::tempdir()?;
        let root = temp.path().join("project");
        let profile = temp.path().join("profile");
        let path = root.join(PROJECT_CONFIG);
        write_json(&path, &config(Rule::Never))?;
        let project = Project {
            id: 1,
            root,
            document: None,
            open_documents: Vec::new(),
        };
        let key = ConfigScope::Project(project.id);
        let mut settings = Configurations::new(profile);
        settings.projects(std::iter::once(&project));
        let draft = config(Rule::Script("return false;".into()));
        settings.stage(key, draft.clone())?;
        let candidate = config(Rule::Always);
        write_json(&path, &candidate)?;
        assert!(settings.prepare(key, &BTreeMap::new()).is_err());
        let revision = settings.take_view().unwrap()[&key].revision;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0x01 | 0x02)
            .open(&settings.scopes[&key].local)?;
        assert!(settings.resolve(key, revision, true).is_err());
        let view = &settings.scopes[&key].view();
        assert_eq!(view.draft, Some(draft));
        assert_eq!(view.external, Some(candidate.clone()));
        drop(file);
        settings.resolve(key, revision, true)?;
        let (revision, _) = settings.prepare(key, &BTreeMap::new())?;
        settings.commit(key, revision)?;
        assert_eq!(settings.take_view().unwrap()[&key].applied, candidate);
        Ok(())
    }

    #[test]
    fn local_project_settings_stay_private_and_can_be_cleaned_after_close() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("project");
        std::fs::create_dir_all(&root)?;
        let project = Project {
            id: 1,
            root: root.clone(),
            document: None,
            open_documents: Vec::new(),
        };
        let manifest: Manifest = serde_json::from_value(serde_json::json!({
            "name": "Fixture", "id": "fixture", "version": "1.0.0", "engines": {"nanobug": "*"},
            "settings": {
                "local": {"title": "Local", "default": true},
                "shared": {"title": "Shared", "default": true, "project": true}
            }
        }))?;
        let namespace = manifest.identity().namespace();
        let scope = ConfigScope::ProjectLocal(project.id);
        let mut state = Configurations::new(temp.path().join("profile"));
        state.projects(std::iter::once(&project));
        let settings = BTreeMap::from([("local".into(), Value::Bool(false))]);
        assert!(
            state
                .stage_sdk(
                    ConfigScope::Project(project.id),
                    &manifest,
                    settings.clone()
                )
                .is_err()
        );
        state.stage_sdk(scope, &manifest, settings)?;
        assert_eq!(
            state.settings(project.id, &namespace, &manifest)["local"],
            true
        );
        let (revision, _) = state.prepare(scope, &BTreeMap::new())?;
        state.commit(scope, revision)?;
        assert_eq!(
            state.settings(project.id, &namespace, &manifest)["local"],
            false
        );
        assert!(!root.join(PROJECT_CONFIG).exists());
        state.projects(std::iter::empty());
        state.clean_settings(&namespace)?;
        state.projects(std::iter::once(&project));
        assert_eq!(
            state.settings(project.id, &namespace, &manifest)["local"],
            true
        );
        Ok(())
    }
}
