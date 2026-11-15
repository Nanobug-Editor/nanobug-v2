//! Editor-facing plugin session. Child processes and filesystem work stay off the UI thread.

mod engine;
mod profile;
mod rules;
mod trace;
pub use super::pipe::PROCESS_MEMORY_LABEL;
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

use super::package::{Identity, Installed, Manifest, Release};
use nanobug_plugin_sdk::{
    DocumentSnapshot, GridSurface, Instance, Output, Permission, PluginEvent,
};
use std::{
    collections::{BTreeMap, BTreeSet},
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

/// Live diagnostics for one version of the current document.
#[derive(Clone)]
pub struct DocumentDiagnostics {
    pub version: (u64, u64),
    /// File identity, because a rename may leave the editor revision unchanged.
    pub path: PathBuf,
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
    /// Revision-matched diagnostics from the project language server.
    pub document_diagnostics: BTreeMap<Key, Arc<DocumentDiagnostics>>,
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

/// Result and exact input version of an explicit editor request.
#[derive(Debug)]
pub struct CallResult {
    pub generation: u64,
    pub version: Option<(u64, u64)>,
    pub output: Output,
}

/// User and editor events queued on the supervisor.
pub enum Action {
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

type Projects = (Option<u64>, Vec<Project>, BTreeMap<u64, String>);
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
        *self.projects.lock().unwrap() = Arc::new((active, projects, dialects));
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
