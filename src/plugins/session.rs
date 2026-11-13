//! Editor-facing plugin session. Child processes and filesystem work stay off the UI thread.

mod engine;
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
pub use trace::Trace;

use super::package::{HostCompatibility, Identity, InstallStore, Installed, Manifest, Release};
use nanobug_plugin_sdk::{
    DocumentSnapshot, GridSurface, Instance, Output, Permission, PluginEvent,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};

/// Plugin data and proposal policy selected for this editor invocation.
pub struct Options {
    root: Option<PathBuf>,
    development: bool,
    pub enabled: bool,
    trace: Option<PathBuf>,
}

impl Options {
    /// Consume plugin flags without treating a profile path as a project.
    pub fn take(args: &mut Vec<OsString>, cwd: &Path) -> anyhow::Result<Self> {
        let mut root = None;
        let mut development = false;
        let mut enabled = true;
        let mut trace = None;
        let mut index = 0;
        while index < args.len() {
            match args[index].to_str() {
                Some("--plugin-profile" | "--plugin-dev-profile") => {
                    anyhow::ensure!(root.is_none(), "Specify only one plugin profile");
                    development = args.remove(index) == "--plugin-dev-profile";
                    anyhow::ensure!(index < args.len(), "Expected a plugin profile directory");
                    root = Some(cwd.join(args.remove(index)));
                }
                Some("--no-plugins") => {
                    args.remove(index);
                    enabled = false;
                }
                Some("--plugin-trace") => {
                    args.remove(index);
                    anyhow::ensure!(index < args.len(), "Expected a plugin trace file");
                    trace = Some(cwd.join(args.remove(index)));
                }
                Some("--") => {
                    args.remove(index);
                    break;
                }
                _ => index += 1,
            }
        }
        Ok(Self {
            root,
            development,
            enabled,
            trace,
        })
    }

    /// Explicit profiles keep editor session history out of the ordinary environment.
    pub fn isolated(&self) -> bool {
        self.root.is_some()
    }

    /// Take the opt-in performance destination before opening the workspace.
    pub fn start_trace(&mut self) -> Option<Trace> {
        self.trace.take().map(Trace::new)
    }

    /// Resolve the per-user data directory after core startup.
    pub fn store(self) -> anyhow::Result<InstallStore> {
        let default = dirs::data_local_dir().map(|path| path.join("Nanobug").join("plugins-v1"));
        let root = self
            .root
            .or_else(|| default.clone())
            .ok_or_else(|| anyhow::anyhow!("Plugin data directory is unavailable"))?;
        if self.development {
            // Comparing resolved paths also prevents aliases into the ordinary profile.
            let development = super::query::resolve_prefix(&root)?;
            if let Some(default) = default {
                let default = super::query::resolve_prefix(&default)?;
                anyhow::ensure!(
                    !development.starts_with(&default) && !default.starts_with(&development),
                    "Choose a development directory separate from the regular plugin profile"
                );
            }
        }
        Ok(InstallStore::with_compatibility(
            bind_profile_mode(&root, self.development)?,
            HostCompatibility::current(self.development),
        ))
    }
}

/// Assign an unmarked profile on first use and keep its resolved directory in one mode.
fn bind_profile_mode(root: &Path, development: bool) -> anyhow::Result<PathBuf> {
    fs::create_dir_all(root)?;
    let root = root.canonicalize()?;
    let path = root.join("profile.mode");
    let expected: &[u8] = if development {
        b"development\n"
    } else {
        b"regular\n"
    };
    let read = || super::package::read_limited(&path, 32);
    let actual = match read() {
        Ok(mode) => mode,
        Err(error)
            if error
                .downcast_ref::<io::Error>()
                .is_some_and(|error| error.kind() == io::ErrorKind::NotFound) =>
        {
            let mut staged = tempfile::NamedTempFile::new_in(&root)?;
            staged.write_all(expected)?;
            staged.as_file().sync_all()?;
            match staged.persist_noclobber(&path) {
                Ok(_) => return Ok(root),
                Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => read()?,
                Err(error) => return Err(error.into()),
            }
        }
        Err(error) => return Err(error),
    };
    anyhow::ensure!(
        actual == expected,
        "Plugin profile belongs to another editor mode. Choose a separate profile directory."
    );
    Ok(root)
}

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
#[derive(Clone, Debug, serde::Deserialize)]
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
    pub modules: BTreeMap<Key, State>,
    /// Status from a fresh activation check when an earlier valid match is still in use.
    pub rule_warnings: BTreeMap<Key, String>,
    /// Reasons a live module rejected its saved settings.
    pub settings_errors: BTreeMap<Key, String>,
    pub surfaces: BTreeMap<Key, Surface>,
    /// The latest valid presentation of the current document from each provider.
    pub document_presentations: BTreeMap<Key, Arc<DocumentPresentation>>,
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

type Projects = (Option<u64>, Vec<Project>);
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
                        message: error.to_string(),
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
        *self.projects.lock().unwrap() = Arc::new((active, projects));
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
