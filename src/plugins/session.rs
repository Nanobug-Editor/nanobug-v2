//! Editor-facing plugin session. Child processes and filesystem work stay off the UI thread.

mod engine;
mod profile;
mod rules;
mod trace;
pub use super::pipe::PROCESS_MEMORY_LABEL;
pub use super::tools::rust_analyzer::ScanOverride;
pub use super::tools::{
    ToolOperation, ToolOperationState,
    background::{BackgroundToolPhase, BackgroundToolStatus},
};
pub use engine::config::{
    Config, ConfigEdit, ConfigScope, ConfigView, Formatting, PluginValues, Rule,
};
pub use engine::developer::DeveloperSettings;
pub use engine::sql_dialect::{is_generic_sql, is_sql_dialect};
pub use trace::Trace;

use super::{
    package::{Identity, Installed, Manifest, Module, Release},
    tools::rust_analyzer::Server,
};
use nanobug_plugin_sdk::{
    DocumentSnapshot, GridSurface, Instance, Output, Permission, PluginEvent,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Range,
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};

pub use profile::Options;

/// A live project with its most recent open document snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Project {
    pub id: u64,
    pub root: PathBuf,
    pub document: Option<Arc<DocumentSnapshot>>,
    pub open_documents: Vec<Arc<DocumentSnapshot>>,
}

/// An instance address assigned by the host, never supplied by plugin output.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Key {
    pub namespace: String,
    pub instance: Instance,
}

/// The editor's current knowledge of an installed module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    Disabled,
    Checking,
    Unmatched,
    Starting,
    Ready,
    Recovering,
    Failed(String),
}

impl State {
    /// Short status for the module list and plugin details.
    pub fn label(&self) -> &str {
        match self {
            Self::Disabled => "Disabled",
            Self::Checking => "Checking rule",
            Self::Unmatched => "Condition not met",
            Self::Starting => "Starting",
            Self::Ready => "Ready",
            Self::Recovering => "Recovering",
            Self::Failed(reason) => reason,
        }
    }
}

/// Permission review is bound to the prepared bytes by its session token.
#[derive(Clone, Debug)]
pub struct InstallReview {
    pub token: u64,
    pub manifest: Manifest,
    pub additional: BTreeSet<Permission>,
    pub additional_rule_paths: Vec<PathBuf>,
}

/// One catalog entry, loaded only at startup and on opening Marketplace.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogEntry {
    pub identity: Identity,
    pub version: semver::Version,
    #[serde(default)]
    pub description: String,
    pub zip: PathBuf,
}

/// One complete surface and the last host-confirmed input for its owner.
#[derive(Clone, Debug)]
pub struct Surface {
    pub generation: u64,
    pub revision: u64,
    pub frame: Arc<GridSurface>,
    pub drawing: Option<Arc<super::drawing::PreparedCanvas>>,
    pub draft: String,
}

/// Inexpensive counters collected while a plugin runtime is active.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RuntimeStatus {
    pub generation: u64,
    pub ready_modules: usize,
    pub pending: usize,
    pub completed: u64,
    pub errors: u64,
    pub last_ms: u64,
    pub private_bytes: Option<u64>,
    /// Prepared drawing allocations owned by the current host surfaces.
    pub canvas_bytes: usize,
    /// Exclusive serialized request capacity and queued reply size on the wire.
    pub request_bytes: usize,
    pub reply_wire_bytes: usize,
    pub peak_request_bytes: usize,
    pub peak_reply_wire_bytes: usize,
    pub last_write_wait_us: usize,
    pub last_worker_timing: Option<nanobug_plugin_sdk::WorkerTiming>,
    pub last_canvas_prepare_us: Option<u64>,
}

/// Estimated model/geometry memory attributed to an open project and module.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HostScopeStatus {
    pub model_bytes: usize,
    pub drawing_bytes: usize,
    pub layout_queue_bytes: usize,
    pub shared_document_bytes: usize,
}

/// Work for the native text layout system, restricted to a live experimental module.
#[derive(Clone, Debug)]
pub struct TextMeasurement {
    pub lines: Vec<nanobug_plugin_sdk::canvas::MeasureText>,
    /// Package-font results by line index. System-font lines are measured in the native window.
    pub prepared: Arc<[Option<serde_json::Value>]>,
    pub(super) owner: Key,
    pub(super) generation: u64,
    pub(super) token: u32,
}

/// A language provider's validated result bound to one opening and revision.
#[derive(Clone)]
pub struct DocumentPresentation {
    /// Document opening and text revision.
    pub version: (u64, u64),
    /// Worker generation which produced this result.
    pub generation: u64,
    /// The validated foreground spans, fold ranges and outline entries.
    pub features: nanobug_plugin_sdk::language::DocumentFeatures,
}

/// Resolve the bundled grammar that can color this file before a plugin or language server replies.
pub fn builtin_syntax_grammar(
    path: Option<&std::path::Path>,
    filetype: Option<&str>,
) -> Option<&'static str> {
    let extension = filetype.or_else(|| path?.extension()?.to_str())?;
    Some(match extension.to_ascii_lowercase().as_str() {
        "json" | "jsonc" => "json",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => "cpp",
        "lua" => "lua",
        "rs" | "rust" => "rust",
        _ => return None,
    })
}

/// Additional highlighting tied to the exact file and revision that was analyzed.
#[derive(Clone)]
pub struct DocumentSyntax {
    /// Bundled grammar for lexical captures. Language-server overlays carry no grammar.
    pub grammar: Option<&'static str>,
    pub version: (u64, u64),
    pub path: PathBuf,
    pub highlights: Vec<nanobug_plugin_sdk::language::Highlight>,
}

/// Live diagnostics for a named or unsaved buffer at a specific revision.
#[derive(Clone)]
pub struct DocumentDiagnostics {
    /// Source opening and text revision supplied to the provider.
    pub version: (u64, u64),
    /// File identity, because a rename may leave the editor revision unchanged.
    pub path: Option<PathBuf>,
    /// Validated UTF-8 byte ranges, including point diagnostics.
    pub diagnostics: Vec<nanobug_plugin_sdk::Diagnostic>,
}

/// The native language server's lifecycle, independent of its plugin module.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LanguageServerPhase {
    /// The server is being launched or its LSP handshake is pending.
    Starting,
    /// The server reports that it is loading or indexing the project.
    Indexing,
    /// Server initialization and any reported initial indexing are complete.
    Ready,
    /// The native server could not start or its transport failed.
    Failed,
}

/// A language server associated with the current document or its open project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LanguageServerStatus {
    /// Opening identity and path of the selected file. A project startup has no document.
    pub document: Option<(u64, PathBuf)>,
    /// Display name of the server that owns the status.
    pub name: &'static str,
    /// Latest lifecycle phase reported by that server session.
    pub phase: LanguageServerPhase,
}

/// An editor request that changes a native language server's lifetime in one Session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LanguageServerOperation {
    /// Permit ordinary attachment and start the service when it is authorized and eligible.
    Enable,
    /// Stop the service and suppress automatic attachment for this Session.
    Disable,
    /// Stop the current instance until a new explicit request to start it.
    Stop,
    /// Wait for the current instance to stop, then attach a replacement.
    Restart,
}

/// Server selection captured from the editor before dispatching an asynchronous operation.
#[derive(Clone, Debug)]
pub struct LanguageServerTarget {
    /// Owning Session identity.
    pub project: u64,
    /// Originating document opening and physical path, if there is a file.
    pub document: Option<(u64, PathBuf)>,
    /// Exact plugin ID, plugin/module ID, or unique server name when explicitly supplied.
    pub name: Option<String>,
}

/// A declared native server and its stable host address from one installed manifest.
struct LanguageServerConfig<'a> {
    key: Key,
    plugin: &'a str,
    module: &'a Module,
    server: Server,
}

impl LanguageServerConfig<'_> {
    /// Resolve the short and qualified plugin names prior to trying a server executable name.
    fn matches_config(&self, selector: &str) -> bool {
        selector == self.plugin
            || selector.split_once('/').is_some_and(|(plugin, module)| {
                (plugin == self.plugin || plugin == self.key.namespace) && module == self.module.id
            })
    }
}

/// Use the configured language server or the original manifest flag for older Rust packages.
fn declared_server(module: &Module) -> Option<Server> {
    module
        .language_server
        .or(module.rust_analyzer.then_some(Server::RustAnalyzer))
}

/// Enumerate the native language server declarations available in `project`.
fn language_server_configs<'a>(
    records: impl IntoIterator<Item = &'a Installed>,
    project: u64,
) -> Vec<LanguageServerConfig<'a>> {
    records
        .into_iter()
        .filter(|record| record.enabled)
        .flat_map(|record| {
            let manifest = &record.release.manifest;
            let namespace = manifest.identity().namespace();
            manifest.modules.iter().filter_map(move |module| {
                Some(LanguageServerConfig {
                    key: Key {
                        namespace: namespace.clone(),
                        instance: Instance {
                            project,
                            module: module.id.clone(),
                        },
                    },
                    plugin: &manifest.id,
                    module,
                    server: declared_server(module)?,
                })
            })
        })
        .collect()
}

/// Read-only state published as a whole. The native renderer never waits for a reply.
#[derive(Clone, Default)]
pub struct View {
    pub development: bool,
    pub developer_settings: BTreeMap<String, DeveloperSettings>,
    pub core_only: bool,
    pub startup_pending: bool,
    pub installed: Arc<Vec<Installed>>,
    pub catalog: Arc<Vec<CatalogEntry>>,
    pub retained: Arc<BTreeMap<String, Release>>,
    pub credential_owners: BTreeSet<String>,
    /// Authentication rejected the most recently read saved slot for this live module.
    pub credential_errors: BTreeMap<Key, BTreeSet<String>>,
    pub project_execution: BTreeMap<u64, bool>,
    pub background_tools: BTreeMap<Key, BTreeMap<String, BackgroundToolStatus>>,
    /// The latest explicitly opened operation history, keyed by plugin identity.
    pub tool_operations: Option<(String, Vec<ToolOperation>)>,
    pub file_operations: Option<(String, Vec<super::file_operations::FileOperationReceipt>)>,
    /// Host-only destination for the editor's file-operation receipts.
    pub operation_root: Option<PathBuf>,
    /// Directory of the last archive chosen in this plugin profile's local ZIP picker.
    pub local_zip_directory: Option<PathBuf>,
    pub modules: BTreeMap<Key, State>,
    /// Status from a fresh activation check when an earlier valid match is still in use.
    pub rule_warnings: BTreeMap<Key, String>,
    /// Reasons a live module rejected its saved settings.
    pub settings_errors: BTreeMap<Key, String>,
    pub surfaces: BTreeMap<Key, Surface>,
    /// The latest valid presentation of the current document from each provider.
    pub document_presentations: BTreeMap<Key, Arc<DocumentPresentation>>,
    /// Lexical results for visible documents, indexed by project scope and opening.
    pub builtin_syntax: BTreeMap<(u64, u64), Arc<DocumentSyntax>>,
    /// Revision-matched semantic roles and modifiers from authorized language servers.
    pub semantic_syntax: BTreeMap<Key, Arc<DocumentSyntax>>,
    /// Native server diagnostics indexed by module and document opening.
    pub document_diagnostics: BTreeMap<(Key, u64), Arc<DocumentDiagnostics>>,
    /// Guest Diagnose replies for documents awaiting, or without, a native result.
    pub guest_diagnostics: BTreeMap<(Key, u64), Arc<DocumentDiagnostics>>,
    /// Language server for each selected document, or a server started for the project.
    pub language_servers: BTreeMap<u64, LanguageServerStatus>,
    /// Latest language-server startup or transport failure for each interested module.
    pub analyzer_errors: BTreeMap<Key, String>,
    pub workers: BTreeMap<String, u32>,
    pub runtime_status: BTreeMap<String, RuntimeStatus>,
    pub host_resources: BTreeMap<Key, HostScopeStatus>,
    /// Allocations referenced by several scopes are counted here once.
    pub shared_plugin_resources: usize,
    pub sampling: BTreeSet<String>,
    pub text_measurements: BTreeMap<u64, TextMeasurement>,
    pub logs: BTreeMap<String, Vec<String>>,
    pub review: Option<InstallReview>,
    pub message: String,
    pub installing: bool,
    pub waiting_for_operations: bool,
    pub draft_pending: bool,
    pub configurations: Arc<BTreeMap<ConfigScope, ConfigView>>,
}

impl View {
    /// List unambiguous native language-server selectors in `project` for command completion.
    pub fn language_server_targets(&self, project: u64) -> impl Iterator<Item = String> {
        let configs = language_server_configs(self.installed.iter(), project);
        let mut names: BTreeMap<String, usize> = BTreeMap::new();
        for config in configs {
            for name in BTreeSet::from([
                config.plugin.to_owned(),
                format!("{}/{}", config.plugin, config.module.id),
                config.server.name().to_owned(),
            ]) {
                *names.entry(name).or_default() += 1;
            }
        }
        names
            .into_iter()
            .filter_map(|(name, count)| (count == 1).then_some(name))
    }
}

/// Result and exact input version of an explicit editor request.
#[derive(Debug)]
pub struct CallResult {
    pub generation: u64,
    pub version: Option<(u64, u64)>,
    pub output: Output,
}

/// User and editor events queued on the supervisor.
pub enum Action {
    /// Capture installed manifests and committed configuration even before the first native frame update.
    SettingsCatalog {
        project: Option<u64>,
        reply: async_channel::Sender<Result<SettingsCatalog, String>>,
    },
    /// Commit one settings page with acknowledgement from the owning supervisor.
    WriteSettings(
        SettingsWrite,
        async_channel::Sender<Result<SettingsWrite, String>>,
    ),
    /// Apply a native language-server operation within an existing plugin Session.
    ControlLanguageServer {
        operation: LanguageServerOperation,
        target: LanguageServerTarget,
        /// Number of selected services once the engine accepts or completes the transition.
        reply: async_channel::Sender<Result<usize, String>>,
    },
    /// Read the named node ranges of an editor-owned document through the shared syntax worker.
    SyntaxNodes {
        language: &'static str,
        document: DocumentSnapshot,
        reply: async_channel::Sender<Result<Vec<Range<usize>>, String>>,
    },
    InspectZip(PathBuf),
    /// Remember the directory of the archive selected in the local picker and inspect it.
    InspectChosenZip(PathBuf),
    InspectDevelopment(PathBuf),
    SetDeveloperSettings(String, DeveloperSettings),
    BuildDevelopment(String),
    InspectCatalog(CatalogEntry),
    Approve(u64),
    CancelInstall,
    OpenMarketplace,
    BeginFileWrite {
        owner: Key,
        digest: String,
        generation: u64,
        ended: async_channel::Receiver<()>,
        reply: async_channel::Sender<Result<(), String>>,
    },
    Retry(String),
    RetrySettings(Key),
    SampleRuntime(String, bool),
    TextMeasured(u64, serde_json::Value),
    SetProjectExecution(u64, bool),
    /// Save or clear the preferred SQL language plugin for one project root.
    SetProjectSqlDialect {
        /// Open project scope to resolve to a profile-local preference file.
        project: u64,
        /// Manifest ID, or no value for automatic selection.
        dialect: Option<String>,
        /// Reports whether the preference reached disk.
        reply: async_channel::Sender<Result<(), String>>,
    },
    RetryBackgroundTool(Key, String),
    ReviewToolOperations(String),
    SetEnabled(String, bool),
    Uninstall(String),
    CleanLocalData {
        namespace: String,
        local: bool,
        credentials: bool,
    },
    SetDraft(Key, String),
    /// Wait for confirmed native work and dispatched commands in the closing scope.
    DrainForClose {
        project: Option<u64>,
        reply: async_channel::Sender<Result<(), String>>,
    },
    FlushDrafts {
        project: u64,
        reply: async_channel::Sender<Result<(), String>>,
    },
    FlushForExit(async_channel::Sender<Result<(), String>>),
    ResumePlugins,
    ExportDrafts {
        path: PathBuf,
        project: Option<u64>,
        reply: async_channel::Sender<Result<(), String>>,
    },
    DiscardDrafts(Option<u64>, async_channel::Sender<Result<(), String>>),
    Resize(Key, nanobug_plugin_sdk::canvas::Viewport),
    SurfaceAction {
        key: Key,
        revision: u64,
        action: String,
        argument: serde_json::Value,
    },
    /// A registered callback from a particular displayed Canvas and viewport.
    CanvasAction {
        key: Key,
        revision: u64,
        viewport: u64,
        kind: nanobug_plugin_sdk::canvas::EventKind,
        action: String,
        argument: serde_json::Value,
    },
    StageConfig(ConfigScope, Config),
    EditConfig(ConfigScope, ConfigEdit),
    ApplyConfig(ConfigScope),
    ResolveConfig(ConfigScope, u64, bool),
    SetCredential {
        namespace: String,
        slot: String,
        value: zeroize::Zeroizing<String>,
        reply: async_channel::Sender<Result<(), String>>,
    },
    DeleteCredential {
        namespace: String,
        slot: Option<String>,
    },
    Invoke {
        key: Key,
        event: PluginEvent,
        reply: Option<async_channel::Sender<Result<CallResult, String>>>,
        timeout: Duration,
    },
}

/// Committed data from the supervisor used to populate the native settings tree.
pub struct SettingsCatalog {
    pub installed: Arc<Vec<Installed>>,
    pub scopes: BTreeMap<ConfigScope, Config>,
    /// Project-specific manual scan lists; a malformed plugin file leaves other pages usable.
    pub scans: BTreeMap<String, Result<ScanOverride, String>>,
}

/// A single manifest field, including its older names for lossless rollback.
pub struct SettingPatch {
    pub id: String,
    pub before: BTreeMap<String, serde_json::Value>,
    pub after: BTreeMap<String, serde_json::Value>,
    /// Restore storage captured before this edit, including an older release's incompatible type.
    pub restore_snapshot: bool,
}

/// Typed and scope-bound mutations supported by the settings dialog.
pub enum SettingsWrite {
    Enabled {
        namespace: String,
        digest: String,
        before: bool,
        after: bool,
    },
    Manifest {
        namespace: String,
        digest: String,
        scope: ConfigScope,
        owner_was_absent: bool,
        fields: Vec<SettingPatch>,
    },
    Scan {
        namespace: String,
        digest: String,
        project: u64,
        before: ScanOverride,
        after: ScanOverride,
    },
}

/// Explicit file types indexed by project ID and document opening.
pub type DocumentFiletypes = BTreeMap<(u64, u64), String>;

/// Last successful write per project and document opening, independent of the text revision.
pub type DocumentSaves = BTreeMap<(u64, u64), u64>;

type Projects = (
    Option<u64>,
    Vec<Project>,
    BTreeMap<u64, String>,
    DocumentFiletypes,
    Vec<Arc<DocumentSnapshot>>,
    DocumentSaves,
);
type SharedProjects = Arc<Mutex<Arc<Projects>>>;
type SharedView = Arc<Mutex<Arc<View>>>;
type SharedDrafts = Arc<Mutex<BTreeMap<Key, String>>>;

/// Editor-owned handle. Disconnecting it releases the session and its child workers.
pub struct Session {
    actions: mpsc::SyncSender<Action>,
    projects: SharedProjects,
    view: SharedView,
    drafts: SharedDrafts,
    wake: std::thread::Thread,
}

impl Session {
    /// Dispatch the native server `operation` and return its asynchronous acknowledgement.
    pub fn control_language_server(
        &self,
        operation: LanguageServerOperation,
        target: LanguageServerTarget,
    ) -> anyhow::Result<async_channel::Receiver<Result<usize, String>>> {
        let (reply, result) = async_channel::bounded(1);
        self.send(Action::ControlLanguageServer {
            operation,
            target,
            reply,
        })?;
        Ok(result)
    }

    /// Ask the shared Tree-sitter worker for the named ranges in `document`'s `language`.
    pub fn syntax_nodes(
        &self,
        language: &'static str,
        document: DocumentSnapshot,
    ) -> anyhow::Result<async_channel::Receiver<Result<Vec<Range<usize>>, String>>> {
        let (reply, result) = async_channel::bounded(1);
        self.send(Action::SyntaxNodes {
            language,
            document,
            reply,
        })?;
        Ok(result)
    }

    /// Start the supervisor after the core window is drawable. The startup thread owns all I/O.
    pub fn start(options: Options) -> (Self, async_channel::Receiver<()>) {
        let (actions, requests) = mpsc::sync_channel(64);
        let (notify, updates) = async_channel::bounded(1);
        let projects = SharedProjects::default();
        let view = SharedView::default();
        let drafts = SharedDrafts::default();
        let worker_projects = Arc::clone(&projects);
        let worker_view = Arc::clone(&view);
        let worker_drafts = Arc::clone(&drafts);
        let supervisor = std::thread::spawn(move || {
            let enabled = options.enabled;
            match options.store() {
                Ok(store) => engine::run(
                    store,
                    enabled,
                    requests,
                    worker_projects,
                    worker_view,
                    notify,
                    worker_drafts,
                ),
                Err(error) => {
                    *worker_view.lock().unwrap() = Arc::new(View {
                        message: format!("{error:#}"),
                        ..View::default()
                    });
                    let _ = notify.try_send(());
                }
            }
        });
        (
            Self {
                actions,
                projects,
                view,
                drafts,
                wake: supervisor.thread().clone(),
            },
            updates,
        )
    }

    /// Replace the latest project state. Transient typing updates never build up in a queue.
    pub fn projects(&self, active: Option<u64>, projects: Vec<Project>) {
        self.projects_with_sql_dialects(active, projects, BTreeMap::new());
    }

    /// Publish project snapshots and the active buffer's SQL override for each project.
    pub fn projects_with_sql_dialects(
        &self,
        active: Option<u64>,
        projects: Vec<Project>,
        dialects: BTreeMap<u64, String>,
    ) {
        self.projects_with_languages(active, projects, dialects, DocumentFiletypes::new());
    }

    /// Publish `projects` with SQL `dialects` and per-opening explicit `filetypes`.
    pub fn projects_with_languages(
        &self,
        active: Option<u64>,
        projects: Vec<Project>,
        dialects: BTreeMap<u64, String>,
        filetypes: DocumentFiletypes,
    ) {
        self.projects_with_visible_syntax(active, projects, dialects, filetypes, Vec::new());
    }

    /// Publish the open projects and the active tabpage's additional lexical-only snapshots.
    pub fn projects_with_visible_syntax(
        &self,
        active: Option<u64>,
        projects: Vec<Project>,
        dialects: BTreeMap<u64, String>,
        filetypes: DocumentFiletypes,
        visible_syntax: Vec<Arc<DocumentSnapshot>>,
    ) {
        self.sync_documents(
            active,
            projects,
            dialects,
            filetypes,
            visible_syntax,
            DocumentSaves::new(),
        );
    }

    /// Coalesce editor overlays and their last successful saves in the same publication.
    pub fn sync_documents(
        &self,
        active: Option<u64>,
        projects: Vec<Project>,
        dialects: BTreeMap<u64, String>,
        filetypes: DocumentFiletypes,
        visible_syntax: Vec<Arc<DocumentSnapshot>>,
        saves: DocumentSaves,
    ) {
        *self.projects.lock().unwrap() =
            Arc::new((active, projects, dialects, filetypes, visible_syntax, saves));
        self.wake.unpark();
    }

    /// Read the latest static and runtime state without a worker round trip.
    pub fn view(&self) -> Arc<View> {
        Arc::clone(&self.view.lock().unwrap())
    }

    /// Coalesce host-confirmed input separately from optional worker calls.
    pub fn draft(&self, key: Key, value: String) {
        self.drafts.lock().unwrap().insert(key, value);
        self.wake.unpark();
    }

    /// Return an asynchronous acknowledgement before the caller closes this scope.
    pub fn drain_for_close(
        &self,
        project: Option<u64>,
    ) -> anyhow::Result<async_channel::Receiver<Result<(), String>>> {
        let (reply, result) = async_channel::bounded(1);
        self.send(Action::DrainForClose { project, reply })?;
        Ok(result)
    }

    /// Queue a user action. The caller can retain input if the queue is busy.
    pub fn send(&self, action: Action) -> anyhow::Result<()> {
        self.actions.try_send(action).map_err(|error| match error {
            mpsc::TrySendError::Full(_) => anyhow::anyhow!("Plugin manager is busy"),
            mpsc::TrySendError::Disconnected(_) => anyhow::anyhow!("Plugin manager is unavailable"),
        })?;
        self.wake.unpark();
        Ok(())
    }
}
