//! Own plugin children, live project instances, install transitions and editor replies.

mod canvas;
pub(super) mod config;
mod credential_control;
pub(super) mod developer;
mod drafts;
mod language;
mod resource;
mod startup;
mod tool_control;

use super::{
    Action, CallResult, CatalogEntry, InstallReview, Key, Project, Projects, RuntimeStatus,
    SharedDrafts, SharedProjects, SharedView, State, TextMeasurement, View,
    rules::{Rules, Source},
};
use crate::plugins::{
    credentials::Credentials,
    drawing::{self, PreparedCanvas},
    edits,
    package::{
        ApprovedRulePaths, InstallStore, Installed, Module, Prepared, Release, read_json,
        write_json,
    },
    pipe::{Worker, check_scripts},
    query, tools,
};
use anyhow::Context;
use drafts::Drafts;
use nanobug_plugin_sdk::{
    CredentialChange, DocumentSnapshot, GridSurface,
    canvas::{EventKind, MeasureText, Viewport},
    document_wire::{DocumentDelta, MAX_DOCUMENT_BYTES},
};
use nanobug_plugin_sdk::{ErrorKind, Input, Instance, Permission, PluginEvent, Reply, Request};
use nanobug_plugin_sdk::{Service, ServiceRequest, ServiceResult};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

const TICK: Duration = Duration::from_millis(20);
const DRAFT_INTERVAL: Duration = Duration::from_millis(500);
const MAX_DRAFT: usize = 64 * 1024;
const MAX_DOC: usize = MAX_DOCUMENT_BYTES;
const PREPARE_TIMEOUT: Duration = Duration::from_secs(15);
const PEER_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_PEER_CALLS: usize = 10;
const MAX_RUNTIME_CALLS: usize = 16;
const RUNTIME_PRIVATE_LIMIT: u64 = 384 * 1024 * 1024;

/// A bounded off-thread work queue. Filesystem tasks remain counted until the thread returns.
pub(super) struct Pool(async_channel::Sender<Box<dyn FnOnce() + Send>>);

impl Pool {
    pub(super) fn new(threads: usize, capacity: usize) -> Self {
        let (send, receive) = async_channel::bounded::<Box<dyn FnOnce() + Send>>(capacity);
        let supervisor = std::thread::current();
        for _ in 0..threads {
            let receive = receive.clone();
            let supervisor = supervisor.clone();
            std::thread::spawn(move || {
                while let Ok(job) = receive.recv_blocking() {
                    job();
                    supervisor.unpark();
                }
            });
        }
        Self(send)
    }

    pub(super) fn submit(&self, task: impl FnOnce() + Send + 'static) -> bool {
        self.0.try_send(Box::new(task)).is_ok()
    }
}

/// Sticky pause after three scheduled restarts within five minutes.
#[derive(Default)]
pub(super) struct Backoff {
    attempts: VecDeque<Instant>,
    resume: Option<Instant>,
    paused: bool,
}

impl Backoff {
    pub(super) fn ready(&self, now: Instant) -> bool {
        !self.paused && self.resume.is_none_or(|resume| now >= resume)
    }

    pub(super) fn paused(&self) -> bool {
        self.paused
    }

    pub(super) fn failed(&mut self, now: Instant) {
        self.attempts
            .retain(|start| now.saturating_duration_since(*start) < Duration::from_secs(300));
        if let Some(delay) = [1, 3, 10].get(self.attempts.len()) {
            let due = now + Duration::from_secs(*delay);
            self.attempts.push_back(due);
            self.resume = Some(due);
        } else {
            self.paused = true;
        }
    }
}

type Answer = async_channel::Sender<Result<CallResult, String>>;
type Version = Option<(u64, u64)>;
type Signature = (
    Version,
    String,
    BTreeMap<String, serde_json::Value>,
    Option<u64>,
    [u64; 3],
);

struct Call {
    instance: Instance,
    version: Version,
    documents: Vec<(u64, u64)>,
    deadline: Instant,
    started: Instant,
    kind: CallKind,
    reply: Option<Answer>,
    credential_read: Option<CredentialChange>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CallKind {
    Open,
    Refresh,
    Explicit,
    User,
    CanvasPassive(EventKind),
    Close,
    Service,
    /// Completes a version-bound invocation that is waiting for a read-only service.
    Continued,
    CredentialChange,
    CredentialReply,
    PeerService,
}

impl CallKind {
    fn is_service(self) -> bool {
        matches!(
            self,
            Self::Service | Self::Continued | Self::CredentialReply | Self::PeerService
        )
    }

    fn timeout_message(self) -> &'static str {
        if self == Self::PeerService {
            "Optional plugin service timed out"
        } else {
            "Plugin request timed out"
        }
    }
}

/// An optional request never owns or activates the target instance.
struct PeerCall {
    caller: Key,
    caller_generation: u64,
    peer: Key,
    peer_generation: u64,
    token: u32,
    deadline: Instant,
    response: async_channel::Receiver<Result<CallResult, String>>,
}

struct Refresh {
    observed: Signature,
    document: bool,
    settings: bool,
}

struct Live {
    module: Module,
    opened: bool,
    last: Option<Signature>,
    refresh: Option<Refresh>,
    force_document_refresh: bool,
    document_base: Option<Arc<DocumentSnapshot>>,
    pending_document: Option<Arc<DocumentSnapshot>>,
    failure: Option<String>,
    settings_error: Option<String>,
    credential_seen: BTreeMap<String, u64>,
    startup_frame: Option<GridSurface>,
    retry_settings: bool,
    open_job: bool,
}

#[derive(Default)]
struct Runtime {
    worker: Option<Worker>,
    starting: Option<u64>,
    generation: u64,
    takeover_retry: Option<Instant>,
    backoff: Backoff,
    instances: BTreeMap<Instance, Live>,
    closing: BTreeSet<u64>,
    calls: BTreeMap<u64, Call>,
    completed: u64,
    errors: u64,
    last_ms: u64,
    last_worker_timing: Option<nanobug_plugin_sdk::WorkerTiming>,
    memory: MemoryProtection,
}

#[derive(Default)]
struct MemoryProtection {
    next: Option<Instant>,
    private_bytes: Option<u64>,
    tried_trim: bool,
    paused: bool,
}

enum ResultEvent {
    Catalog(anyhow::Result<Vec<CatalogEntry>>),
    Prepared(u64, anyhow::Result<Prepared>),
    Installed(String, anyhow::Result<Installed>),
    Started(String, u64, anyhow::Result<Worker>),
    Asset(Key, u64, anyhow::Result<PathBuf>),
    Frame(
        Key,
        u64,
        u64,
        GridSurface,
        anyhow::Result<PreparedCanvas>,
        u64,
    ),
    TextPrepared(
        Option<u64>,
        TextMeasurement,
        anyhow::Result<Vec<Option<serde_json::Value>>>,
    ),
    Config(config::ConfigScope, u64, anyhow::Result<()>),
    Credential(
        String,
        Option<String>,
        anyhow::Result<crate::plugins::credentials::DeleteOutcome>,
    ),
    CredentialChanged(String, String),
    CredentialAccess(credential_control::AccessResult),
    Service(Key, u64, ServiceResult),
    Syntax(Key, u64, Version, ServiceResult),
    LanguageService(language::Pending, Result<serde_json::Value, String>),
    Tool(Key, u64, ServiceResult),
    BackgroundToolStarted(
        Key,
        String,
        u64,
        anyhow::Result<tools::background::BackgroundProcess>,
    ),
    BackgroundToolStatus(Key, u64, u32, String),
    ToolOperations(u64, String, OperationsReview),
    Development(String, String, anyhow::Result<developer::BuildOutcome>),
}

struct OperationsReview {
    tools: anyhow::Result<Vec<tools::ToolOperation>>,
    files: anyhow::Result<Vec<crate::plugins::file_operations::FileOperationReceipt>>,
}

/// A confirmed editor-side write keeps its plugin version until the background task drops its lease.
struct FileWrite {
    owner: Key,
    ended: async_channel::Receiver<()>,
}

enum Change {
    Install(Box<Prepared>),
    Development(Box<Prepared>, developer::Candidate),
    Remove(String),
}

impl Change {
    fn namespace(&self) -> String {
        match self {
            Self::Install(package) | Self::Development(package, _) => {
                package.release.manifest.identity().namespace()
            }
            Self::Remove(key) => key.clone(),
        }
    }

    /// Bind the paths displayed in this review to the exact staged package.
    fn approve_rule_paths(&mut self, paths: BTreeSet<String>) {
        match self {
            Self::Install(package) | Self::Development(package, _) => {
                package.reviewed_rule_paths = paths;
            }
            Self::Remove(_) => unreachable!("no permission review for removal"),
        }
    }
}

/// The pending change owns the exact staged package and the external paths shown with it.
struct PackageReview {
    token: u64,
    change: Change,
    rule_paths: BTreeSet<String>,
}

struct Engine {
    store: InstallStore,
    enabled: bool,
    startup: Option<startup::Startup>,
    view: View,
    records: BTreeMap<String, Arc<Installed>>,
    releases: BTreeMap<String, Arc<Release>>,
    projects: BTreeMap<u64, Project>,
    active: Option<u64>,
    rules: Rules,
    runtimes: BTreeMap<String, Runtime>,
    wanted: BTreeMap<Key, Module>,
    config: config::Configurations,
    config_applies: BTreeMap<config::ConfigScope, u64>,
    draft: Drafts,
    draft_write: Instant,
    package_prune_due: Instant,
    layouts: BTreeMap<Key, Viewport>,
    frames: canvas::Frames,
    /// The last Canvas revision that received its single next-frame callback.
    frame_callbacks: BTreeMap<Key, u64>,
    ready_candidates: BTreeSet<String>,
    pending_change: Option<Change>,
    mutating: Option<String>,
    review: Option<PackageReview>,
    inspecting: Option<u64>,
    serial: u64,
    finish: mpsc::Sender<ResultEvent>,
    events: mpsc::Receiver<ResultEvent>,
    io: Pool,
    launch: Pool,
    install: Pool,
    credentials: Credentials,
    credential_io: Pool,
    credential_events: credential_control::CredentialEvents,
    resource_sample_due: Instant,
    last_canvas_prepare_us: BTreeMap<String, u64>,
    pending_measurements: BTreeMap<u64, (Key, usize)>,
    service_pending: BTreeMap<String, usize>,
    file_writes: Vec<FileWrite>,
    syntax: Arc<Mutex<Syntax>>,
    syntax_io: Pool,
    peer_calls: Vec<PeerCall>,
    tool_io: Pool,
    tool_writes: BTreeMap<Key, usize>,
    close_drains: BTreeMap<Option<u64>, tool_control::CloseDrain>,
    backgrounds: BTreeMap<(Key, String), tool_control::BackgroundJob>,
    tool_review: Option<(u64, String)>,
    project_execution: BTreeMap<String, bool>,
    project_flags: BTreeMap<u64, Arc<AtomicBool>>,
    developer_io: Pool,
    developer_job: Option<(String, Arc<AtomicBool>)>,
    developer_ready: Option<Change>,
    developer_install: Option<(String, Arc<AtomicBool>)>,
    developer_stamps: BTreeMap<String, String>,
    developer_build: BTreeSet<String>,
    developer_last: String,
    developer_due: Instant,
    changed: bool,
}

pub(super) fn run(
    store: InstallStore,
    enabled: bool,
    requests: mpsc::Receiver<Action>,
    projects: SharedProjects,
    view: SharedView,
    notify: async_channel::Sender<()>,
    drafts: SharedDrafts,
) {
    let mut engine = Engine::new(store, enabled);
    engine.read_catalog();
    let mut last_projects = Arc::default();
    loop {
        // Take the newest editor snapshots before accepting any asynchronous output or action.
        let current = Arc::clone(&projects.lock().unwrap());
        if !Arc::ptr_eq(&current, &last_projects) {
            engine.projects(current.as_ref().clone());
            last_projects = current;
        }
        let confirmed = std::mem::take(&mut *drafts.lock().unwrap());
        for (key, value) in confirmed {
            engine.set_draft(key, value);
        }
        for _ in 0..32 {
            match requests.try_recv() {
                Ok(action) => engine.action(action),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    for cancelled in engine.project_flags.values() {
                        cancelled.store(true, Ordering::Release);
                    }
                    if let Some((_, cancelled)) = engine
                        .developer_job
                        .as_ref()
                        .or(engine.developer_install.as_ref())
                    {
                        cancelled.store(true, Ordering::Release);
                    }
                    let _ = engine.flush_drafts(true, None);
                    return;
                }
            }
        }
        engine.tick();
        if engine.changed {
            engine.changed = false;
            *view.lock().unwrap() = Arc::new(engine.view.clone());
            let _ = notify.try_send(());
        }
        let working = engine.rules.working(Instant::now())
            || engine.runtimes.values().any(|runtime| {
                runtime.starting.is_some()
                    || !runtime.calls.is_empty()
                    || runtime.instances.values().any(|live| live.open_job)
            });
        std::thread::park_timeout(if working {
            Duration::from_millis(5)
        } else {
            TICK
        });
    }
}

impl Engine {
    fn new(store: InstallStore, enabled: bool) -> Self {
        let (finish, events) = mpsc::channel();
        let (drafts, draft_error) = Drafts::load(store.root().to_path_buf());
        let project_execution = read_json(&store.root().join("execution.json"), 512 * 1024)
            .ok()
            .flatten()
            .unwrap_or_default();
        let mut engine = Self {
            rules: Rules::new(store.clone()),
            config: config::Configurations::new(store.root().to_path_buf()),
            config_applies: BTreeMap::new(),
            credentials: Credentials::new(store.root().to_path_buf()),
            credential_io: Pool::new(1, 4),
            credential_events: credential_control::CredentialEvents::default(),
            resource_sample_due: Instant::now(),
            last_canvas_prepare_us: BTreeMap::new(),
            pending_measurements: BTreeMap::new(),
            service_pending: BTreeMap::new(),
            file_writes: Vec::new(),
            syntax: Arc::new(Mutex::new(Syntax::default())),
            syntax_io: Pool::new(1, 8),
            peer_calls: Vec::new(),
            tool_io: Pool::new(2, 8),
            tool_writes: BTreeMap::new(),
            close_drains: BTreeMap::new(),
            backgrounds: BTreeMap::new(),
            tool_review: None,
            project_execution,
            project_flags: BTreeMap::new(),
            developer_io: Pool::new(1, 1),
            developer_job: None,
            developer_ready: None,
            developer_install: None,
            developer_stamps: BTreeMap::new(),
            developer_build: BTreeSet::new(),
            developer_last: String::new(),
            developer_due: Instant::now(),
            store,
            enabled,
            startup: None,
            view: View::default(),
            records: BTreeMap::new(),
            releases: BTreeMap::new(),
            projects: BTreeMap::new(),
            active: None,
            runtimes: BTreeMap::new(),
            wanted: BTreeMap::new(),
            draft: drafts,
            draft_write: Instant::now(),
            package_prune_due: Instant::now() + Duration::from_secs(5),
            layouts: BTreeMap::new(),
            frames: canvas::Frames::default(),
            frame_callbacks: BTreeMap::new(),
            ready_candidates: BTreeSet::new(),
            pending_change: None,
            mutating: None,
            review: None,
            inspecting: None,
            serial: 0,
            finish,
            events,
            io: Pool::new(4, 16),
            launch: Pool::new(2, 8),
            install: Pool::new(1, 2),
            changed: true,
        };
        engine.view.operation_root = Some(engine.store.root().to_path_buf());
        engine.reload();
        engine.view.development = engine.store.compatibility().experiments.contains_key("ui");
        if engine.view.development {
            engine.view.developer_settings =
                read_json(&engine.store.root().join("developer.json"), 512 * 1024)
                    .ok()
                    .flatten()
                    .unwrap_or_default();
        }
        if enabled && !engine.records.is_empty() {
            match startup::Startup::arm(engine.store.root()) {
                Ok(Some((startup, paused))) => {
                    engine.view.startup_pending = startup.pending();
                    engine.startup = Some(startup);
                    if paused {
                        engine.enabled = false;
                        engine.view.core_only = true;
                        engine.message("Plugins paused after repeated interrupted startups. Resume in Installed.");
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    engine.message(format!("Could not record startup recovery: {error:#}"))
                }
            }
        }
        engine.refresh_credentials();
        if !enabled {
            engine.message("Plugins are paused for this editor session");
        }
        if draft_error.is_some() {
            engine.message("Could not read saved plugin drafts. The draft file was kept.");
        }
        engine
    }

    fn next(&mut self) -> u64 {
        self.serial += 1;
        self.serial
    }

    fn message(&mut self, message: impl Into<String>) {
        self.view.message = message.into();
        self.changed = true;
    }

    fn log(&mut self, namespace: &str, message: &str) {
        let log = self.view.logs.entry(namespace.into()).or_default();
        if log.len() == 80 {
            log.remove(0);
        }
        log.push(
            message
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(240)
                .collect(),
        );
        self.changed = true;
    }

    fn reload(&mut self) {
        if let Ok(retained) = self.store.retained() {
            self.view.retained = Arc::new(retained);
            self.changed = true;
        }
        match self.store.list_leased() {
            Ok(list) => {
                self.records.clear();
                self.releases.clear();
                self.ready_candidates.clear();
                for record in &list {
                    let key = record.release.manifest.identity().namespace();
                    if record.pending_takeover {
                        self.ready_candidates.insert(key.clone());
                    }
                    self.releases
                        .insert(key.clone(), Arc::new(record.release.clone()));
                    self.records.insert(key, Arc::new(record.clone()));
                }
                self.view.installed = Arc::new(list);
                self.changed = true;
            }
            Err(error) => self.message(format!("Could not read installed plugins: {error:#}")),
        }
    }

    fn refresh_credentials(&mut self) {
        if let Ok(owners) = self.credentials.names() {
            self.view.credential_owners = owners;
            self.changed = true;
        }
    }

    fn projects(&mut self, (active, projects): Projects) {
        self.active = active;
        self.projects = projects
            .into_iter()
            .map(|project| (project.id, project))
            .collect();
        self.project_flags.retain(|id, flag| {
            let live = self.projects.contains_key(id);
            if !live {
                flag.store(true, Ordering::Release);
            }
            live
        });
        let execution = self
            .projects
            .iter()
            .filter_map(|(&id, project)| {
                self.project_execution
                    .get(&query::project_key(&project.root))
                    .copied()
                    .map(|allow| (id, allow))
            })
            .collect();
        if self.view.project_execution != execution {
            self.view.project_execution = execution;
            self.changed = true;
        }
        self.config.projects(self.projects.values());
    }

    fn read_catalog(&mut self) {
        let root = self.store.root().to_path_buf();
        let finish = self.finish.clone();
        if !self.install.submit(move || {
            let result = (|| {
                let mut entries = read_json::<Vec<CatalogEntry>>(
                    &root.join("marketplace.json"),
                    2 * 1024 * 1024,
                )?
                .unwrap_or_default();
                anyhow::ensure!(entries.len() <= 1000, "Too many marketplace entries");
                for entry in &mut entries {
                    super::super::package::Identity::new(
                        entry.identity.name.clone(),
                        entry.identity.id.clone(),
                    )?;
                    anyhow::ensure!(
                        entry.version.pre.is_empty(),
                        "Marketplace entry is not a stable release"
                    );
                    entry.zip = root.join(&entry.zip);
                }
                entries.sort_by(|a, b| {
                    a.identity
                        .cmp(&b.identity)
                        .then_with(|| b.version.cmp(&a.version))
                });
                Ok(entries)
            })();
            let _ = finish.send(ResultEvent::Catalog(result));
        }) {
            self.message("Marketplace is busy. Reopen Marketplace to read it.");
        }
    }

    fn inspect(&mut self, path: PathBuf, expected: Option<CatalogEntry>, development: bool) {
        if self.inspecting.is_some() || self.pending_change.is_some() || self.mutating.is_some() {
            return;
        }
        self.review.take();
        self.view.review = None;
        let id = self.next();
        let store = self.store.clone();
        let finish = self.finish.clone();
        let candidates = expected.as_ref().map(|entry| {
            let installed = self
                .records
                .get(&entry.identity.namespace())
                .map(|record| &record.release.manifest.version);
            self.view
                .catalog
                .iter()
                .filter(|candidate| {
                    candidate.identity == entry.identity
                        && installed.is_none_or(|version| candidate.version > *version)
                })
                .take(16)
                .cloned()
                .collect::<Vec<_>>()
        });
        if self.install.submit(move || {
            let result = match candidates {
                None if development => store.prepare_directory(&path),
                None => store.prepare(&path),
                Some(candidates) => prepare_catalog(&store, candidates),
            };
            let _ = finish.send(ResultEvent::Prepared(id, result));
        }) {
            self.inspecting = Some(id);
            self.view.installing = true;
            self.message("Checking ZIP…");
        } else {
            self.message("Installer is busy");
        }
    }

    fn action(&mut self, action: Action) {
        match action {
            Action::InspectZip(path) => self.inspect(path, None, false),
            Action::InspectDevelopment(path) => self.inspect(path, None, true),
            Action::SetDeveloperSettings(namespace, settings)
                if self.view.development
                    && self
                        .releases
                        .get(&namespace)
                        .is_some_and(|release| release.development) =>
            {
                let mut next = self.view.developer_settings.clone();
                next.insert(namespace.clone(), settings);
                match write_json(&self.store.root().join("developer.json"), &next) {
                    Ok(()) => {
                        self.view.developer_settings = next;
                        self.developer_stamps.remove(&namespace);
                        self.cancel_development(&namespace);
                        self.message("Development settings saved");
                    }
                    Err(error) => self.message(format!("{error:#}")),
                }
            }
            Action::BuildDevelopment(namespace)
                if self.view.development
                    && self
                        .releases
                        .get(&namespace)
                        .is_some_and(|release| release.development) =>
            {
                self.developer_build.insert(namespace);
                self.developer_due = Instant::now();
            }
            Action::InspectCatalog(entry) => self.inspect(entry.zip.clone(), Some(entry), false),
            Action::OpenMarketplace => self.read_catalog(),
            Action::BeginFileWrite {
                owner,
                digest,
                generation,
                ended,
                reply,
            } => {
                if self.config_apply_waits_for(&owner) {
                    let _ = reply.try_send(Err("Configuration is waiting for plugin writes to finish. Try again after Apply.".into()));
                    return;
                }
                let valid = !ended.is_closed()
                    && !self.busy(&owner.namespace)
                    && !self.ready_candidates.contains(&owner.namespace)
                    && self.wanted.contains_key(&owner)
                    && self.records.get(&owner.namespace).is_some_and(|record| {
                        record.release.digest == digest
                            && record.grants.contains(&Permission::ChangeFiles)
                    })
                    && self.runtimes.get(&owner.namespace).is_some_and(|runtime| {
                        runtime.generation == generation
                            && runtime.worker.is_some()
                            && runtime.instances.get(&owner.instance).is_some_and(|live| {
                                live.opened
                                    && live.failure.is_none()
                                    && live.settings_error.is_none()
                            })
                    });
                if !valid {
                    let _ = reply
                        .try_send(Err("Plugin changed after it prepared the file plan".into()));
                } else if reply.try_send(Ok(())).is_ok() {
                    *self
                        .service_pending
                        .entry(owner.namespace.clone())
                        .or_default() += 1;
                    self.file_writes.push(FileWrite { owner, ended });
                }
            }
            Action::Approve(token) => {
                if self
                    .review
                    .as_ref()
                    .is_some_and(|review| review.token == token)
                {
                    let PackageReview {
                        mut change,
                        rule_paths,
                        ..
                    } = self.review.take().expect("approved package");
                    change.approve_rule_paths(rule_paths);
                    if let Change::Development(_, candidate) = &mut change {
                        candidate.permissions_reviewed = true;
                    }
                    self.pending_change = Some(change);
                    self.view.review = None;
                    self.view.installing = true;
                    self.message("Waiting for plugin operations to finish…");
                }
            }
            Action::CancelInstall => {
                if self.mutating.is_none() {
                    self.inspecting.take();
                    self.review.take();
                    self.pending_change.take();
                    self.view.review = None;
                    self.view.installing = false;
                    self.message("Cancelled");
                }
            }
            Action::Uninstall(namespace)
                if self.records.contains_key(&namespace)
                    && self.mutating.is_none()
                    && self.pending_change.is_none()
                    && self.review.is_none()
                    && self.inspecting.is_none() =>
            {
                self.pending_change = Some(Change::Remove(namespace));
                self.view.installing = true;
                self.message("Waiting for plugin operations to finish…");
            }
            Action::SetEnabled(namespace, enabled)
                if self.mutating.is_none() && self.pending_change.is_none() =>
            {
                match self.store.set_enabled(&namespace, enabled) {
                    Ok(()) => {
                        self.reload();
                        self.message(if enabled {
                            "Plugin enabled"
                        } else {
                            "Plugin disabled"
                        });
                    }
                    Err(error) => self.message(error.to_string()),
                }
            }
            Action::Retry(namespace) => {
                if let Some(runtime) = self.runtimes.get_mut(&namespace) {
                    runtime.backoff = Backoff::default();
                    runtime.memory = MemoryProtection::default();
                    for module in runtime.instances.values_mut() {
                        module.failure = None;
                        module.last = None;
                        module.refresh = None;
                    }
                }
                self.rules.retry(&namespace);
            }
            Action::RetrySettings(key) => {
                if let Some(live) = self
                    .runtimes
                    .get_mut(&key.namespace)
                    .and_then(|runtime| runtime.instances.get_mut(&key.instance))
                    .filter(|live| live.settings_error.is_some())
                {
                    live.retry_settings = true;
                }
            }
            Action::SampleRuntime(namespace, enabled) => {
                if enabled {
                    self.view.sampling.insert(namespace);
                } else {
                    self.view.sampling.remove(&namespace);
                }
                self.changed = true;
            }
            Action::TextMeasured(id, value) => {
                if let Some(request) = self.view.text_measurements.remove(&id) {
                    self.changed = true;
                    self.reply_text_measurement(request, Ok(value));
                }
            }
            Action::RetryBackgroundTool(key, tool) => self.retry_background_tool(key, tool),
            Action::ReviewToolOperations(namespace) => self.review_tool_operations(namespace),
            Action::SetProjectExecution(id, allow) => {
                let Some(project) = self.projects.get(&id) else {
                    return;
                };
                let key = query::project_key(&project.root);
                let mut permissions = self.project_execution.clone();
                permissions.insert(key, allow);
                match write_json(&self.store.root().join("execution.json"), &permissions) {
                    Ok(()) => {
                        self.project_execution = permissions;
                        self.view.project_execution.insert(id, allow);
                        if !allow && let Some(flag) = self.project_flags.remove(&id) {
                            flag.store(true, Ordering::Release);
                        }
                        self.message(if allow {
                            "Project execution allowed"
                        } else {
                            "Project execution blocked"
                        });
                    }
                    Err(error) => self.message(format!(
                        "Could not save project execution preference: {error:#}"
                    )),
                }
            }
            Action::SetDraft(key, text) => self.set_draft(key, text),
            Action::DrainForClose { project, reply } => self.drain_for_close(project, reply),
            Action::FlushDrafts { project, reply } => {
                let result = self
                    .draft_scope(Some(project))
                    .and_then(|scope| self.flush_drafts(true, scope.as_deref()))
                    .map_err(|error| error.to_string());
                let _ = reply.try_send(result);
            }
            Action::FlushForExit(reply) => {
                let result = self
                    .flush_drafts(true, None)
                    .and_then(|()| {
                        if let Some(startup) = &mut self.startup
                            && startup.pending()
                        {
                            startup.clean()?;
                        }
                        Ok(())
                    })
                    .map_err(|error| error.to_string());
                let _ = reply.try_send(result);
            }
            Action::ResumePlugins if self.view.core_only => {
                if let Some(startup) = &mut self.startup
                    && let Err(error) = startup.clean()
                {
                    self.message(format!("Could not clear startup recovery: {error:#}"));
                    return;
                }
                self.view.core_only = false;
                self.enabled = true;
                self.message("Plugins resumed");
            }
            Action::ExportDrafts {
                path,
                project,
                reply,
            } => {
                let result = self
                    .draft_scope(project)
                    .and_then(|scope| self.draft.export(&path, scope.as_deref()))
                    .map_err(|error| error.to_string());
                let _ = reply.try_send(result);
            }
            Action::DiscardDrafts(scope, reply) => {
                let result = self
                    .draft_scope(scope)
                    .map(|scope| {
                        self.draft.discard(scope.as_deref());
                        self.publish_draft_state();
                    })
                    .map_err(|error| error.to_string());
                let _ = reply.try_send(result);
            }
            Action::CleanLocalData {
                namespace,
                local,
                credentials,
            } => {
                if self.records.contains_key(&namespace)
                    || !self.view.retained.contains_key(&namespace)
                {
                    self.message("Local cleanup requires an uninstalled plugin");
                    return;
                }
                if local {
                    if let Err(error) = self.clean_local(&namespace) {
                        self.message(format!("Could not clean local data: {error:#}"));
                        return;
                    }
                    self.message("Local settings and input drafts cleared");
                }
                if credentials {
                    self.action(Action::DeleteCredential {
                        namespace,
                        slot: None,
                    });
                }
            }
            Action::Resize(key, viewport) => {
                if self
                    .wanted
                    .get(&key)
                    .is_some_and(|module| module.interactive)
                    && viewport.width.is_finite()
                    && viewport.height.is_finite()
                    && viewport.scale.is_finite()
                    && viewport.width > 0.
                    && viewport.height > 0.
                    && viewport.scale > 0.
                {
                    self.resize_canvas(key, viewport);
                }
            }
            Action::SurfaceAction {
                key,
                revision,
                action,
                argument,
            } => {
                let available =
                    self.current_surface_revision(&key, revision)
                        .is_some_and(|surface| {
                            action == "submit"
                                && (surface.frame.input.is_some()
                                    || surface
                                        .frame
                                        .canvas
                                        .as_ref()
                                        .is_some_and(|canvas| canvas.has_input()))
                                || surface.frame.canvas.as_ref().is_some_and(|canvas| {
                                    canvas.hits.iter().any(|hit| hit.action == action)
                                })
                        });
                if available {
                    self.invoke(
                        key,
                        PluginEvent::Ui { action, argument },
                        None,
                        Duration::from_secs(2),
                    );
                }
            }
            Action::CanvasAction {
                key,
                revision,
                viewport,
                kind,
                action,
                argument,
            } => {
                if self
                    .layouts
                    .get(&key)
                    .is_some_and(|layout| layout.id == viewport)
                    && self
                        .current_surface_revision(&key, revision)
                        .is_some_and(|surface| {
                            surface
                                .frame
                                .canvas
                                .as_ref()
                                .is_some_and(|canvas| canvas.interaction.declares(kind, &action))
                                && (kind != EventKind::NextFrame
                                    || surface
                                        .drawing
                                        .as_ref()
                                        .is_some_and(|drawing| drawing.reports_underlines)
                                        == argument.get("underlines").is_some())
                        })
                    && (kind != EventKind::NextFrame
                        || self.frame_callbacks.get(&key) != Some(&revision))
                {
                    let call = if kind.is_passive() {
                        CallKind::CanvasPassive(kind)
                    } else {
                        CallKind::User
                    };
                    if self.invoke_with_kind(
                        key.clone(),
                        PluginEvent::Ui { action, argument },
                        None,
                        Duration::from_secs(2),
                        call,
                        None,
                    ) && kind == EventKind::NextFrame
                    {
                        self.frame_callbacks.insert(key, revision);
                    }
                }
            }
            Action::StageConfig(scope, value) => match self.config.stage(scope, value) {
                Ok(()) => self.message("Draft saved. Apply to use it."),
                Err(error) => self.message(format!("{error:#}")),
            },
            Action::EditConfig(scope, edit) => match self.config.edit(scope, edit) {
                Ok(()) => self.message("Draft saved. Apply to use it."),
                Err(error) => self.message(format!("{error:#}")),
            },
            Action::ResolveConfig(scope, revision, external) => {
                match self.config.resolve(scope, revision, external) {
                    Ok(()) if external => {
                        self.message("External version copied to draft. Apply to use it.")
                    }
                    Ok(()) => self.message("Draft kept. Apply to use it."),
                    Err(error) => self.message(format!("{error:#}")),
                }
            }
            Action::SetCredential {
                namespace,
                slot,
                value,
                reply,
            } => {
                let credential = self.credentials.clone();
                let answer = reply.clone();
                let finish = self.finish.clone();
                if !self.can_use_credential(&namespace, &slot) {
                    let _ = reply.try_send(Err("Credential access is not granted".into()));
                } else if !self.credential_io.submit(move || {
                    let result = credential.set(&namespace, &slot, &value);
                    if result.is_ok() {
                        let _ = finish.send(ResultEvent::CredentialChanged(namespace, slot));
                    }
                    let result = result
                        .and_then(crate::plugins::credentials::SaveOutcome::confirmed)
                        .map_err(|error| error.to_string());
                    let _ = answer.try_send(result);
                }) {
                    let _ = reply.try_send(Err("The system credential store is busy".into()));
                }
            }
            Action::DeleteCredential { namespace, slot } => {
                if slot
                    .as_ref()
                    .is_some_and(|slot| !self.can_use_credential(&namespace, slot))
                {
                    return;
                }
                let credential = self.credentials.clone();
                let finish = self.finish.clone();
                if !self.credential_io.submit(move || {
                    let result = credential.delete(&namespace, slot.as_deref());
                    let _ = finish.send(ResultEvent::Credential(namespace, slot, result));
                }) {
                    self.message("The system credential store is busy");
                }
            }
            Action::ApplyConfig(scope) => match self.config.prepare(scope, &self.records) {
                Ok((revision, scripts)) => {
                    let finish = self.finish.clone();
                    if self.install.submit(move || {
                        let result = check_scripts(scripts);
                        let _ = finish.send(ResultEvent::Config(scope, revision, result));
                    }) {
                        self.message("Checking configuration…");
                    } else {
                        self.message("Plugin manager is busy");
                    }
                }
                Err(error) => {
                    self.config
                        .note_apply_issue(scope, None, config::ApplyIssue::Validation);
                    self.message(format!("{error:#}"));
                }
            },
            Action::Invoke {
                key,
                event,
                reply,
                timeout,
            } => self.invoke(key, event, reply, timeout),
            _ => {}
        }
    }

    fn config_affects_write(&self, scope: config::ConfigScope, revision: u64, owner: &Key) -> bool {
        self.records.get(&owner.namespace).is_some_and(|record| {
            self.config
                .affects_write(scope, revision, owner, &record.release.manifest)
        })
    }

    pub(super) fn config_apply_waits_for(&self, owner: &Key) -> bool {
        self.config_applies
            .iter()
            .any(|(&scope, &revision)| self.config_affects_write(scope, revision, owner))
    }

    fn config_writes_pending(&self, scope: config::ConfigScope, revision: u64) -> bool {
        self.file_writes
            .iter()
            .map(|write| &write.owner)
            .chain(self.tool_writes.keys())
            .any(|owner| self.config_affects_write(scope, revision, owner))
    }

    fn advance_config_applies(&mut self) {
        for (scope, revision) in std::mem::take(&mut self.config_applies) {
            if self.config_writes_pending(scope, revision) {
                self.config_applies.insert(scope, revision);
            } else {
                match self.config.commit(scope, revision) {
                    Ok(()) => self.message("Configuration applied"),
                    Err(error) => {
                        self.config.note_apply_issue(
                            scope,
                            Some(revision),
                            config::ApplyIssue::Save,
                        );
                        self.message(format!("Apply failed: {error:#}"));
                    }
                }
            }
        }
    }

    fn tick(&mut self) {
        if Instant::now() >= self.package_prune_due {
            let store = self.store.clone();
            if self.io.submit(move || {
                let _ = store.prune_unreferenced();
            }) {
                self.package_prune_due = Instant::now() + Duration::from_secs(30);
            }
        }
        self.file_writes.retain(|write| {
            if !write.ended.is_closed() {
                return true;
            }
            let namespace = &write.owner.namespace;
            if let Some(count) = self.service_pending.get_mut(namespace) {
                *count -= 1;
                if *count == 0 {
                    self.service_pending.remove(namespace);
                }
            }
            false
        });
        if let Some(startup) = &mut self.startup {
            startup.tick();
            if self.view.startup_pending != startup.pending() {
                self.view.startup_pending = startup.pending();
                self.changed = true;
            }
        }
        while let Ok(event) = self.events.try_recv() {
            self.event(event);
        }
        let _ = self.flush_drafts(false, None);
        self.config.refresh();
        self.advance_config_applies();
        if let Some(configurations) = self.config.take_view() {
            self.view.configurations = Arc::new(configurations);
            self.changed = true;
        }
        self.reconcile();
        self.rules.tick(self.active, &self.projects, &self.io);
        let names = self.runtimes.keys().cloned().collect::<Vec<_>>();
        for name in names {
            let mut runtime = self.runtimes.remove(&name).expect("known runtime");
            self.advance_runtime(&name, &mut runtime);
            if runtime.worker.is_some()
                || runtime.starting.is_some()
                || !runtime.instances.is_empty()
                || (self.records.contains_key(&name) && runtime.backoff.paused())
                || (self.records.contains_key(&name) && runtime.memory.paused)
            {
                self.runtimes.insert(name, runtime);
            }
        }
        self.advance_peer_calls();
        self.advance_credential_events();
        self.advance_background_tools();
        self.advance_close_drains();
        self.advance_install();
        self.advance_development();
        self.publish_status();
    }

    fn event(&mut self, event: ResultEvent) {
        match event {
            ResultEvent::Catalog(result) => match result {
                Ok(entries) => {
                    self.view.catalog = Arc::new(entries);
                    self.changed = true;
                }
                Err(error) => self.message(format!("Could not read Marketplace: {error:#}")),
            },
            ResultEvent::Prepared(id, result) if self.inspecting == Some(id) => {
                self.inspecting = None;
                self.view.installing = false;
                match result {
                    Ok(package) => {
                        self.review_package(id, Change::Install(Box::new(package)));
                        self.message("ZIP ready for review");
                    }
                    Err(error) => self.message(format!("Install rejected: {error:#}")),
                }
            }
            ResultEvent::Installed(key, result) => {
                self.mutating = None;
                self.view.installing = false;
                let development = self.developer_install.take().is_some();
                match result {
                    Ok(record) => {
                        let prior = self.runtimes.remove(&key);
                        if development && let Some(prior) = prior {
                            self.runtimes.insert(
                                key.clone(),
                                Runtime {
                                    backoff: prior.backoff,
                                    memory: MemoryProtection {
                                        paused: prior.memory.paused,
                                        ..MemoryProtection::default()
                                    },
                                    ..Runtime::default()
                                },
                            );
                        }
                        self.view.surfaces.retain(|key, _| {
                            key.namespace != record.release.manifest.identity().namespace()
                        });
                        self.frames.remove_plugin(&key);
                        self.message(format!(
                            "Installed {} {}",
                            record.release.manifest.name, record.release.manifest.version
                        ));
                    }
                    Err(error) if development => {
                        self.log(&key, &format!("Development reload: {error:#}"));
                        self.message("Development reload skipped. The active version was kept.");
                    }
                    Err(error) => self.message(format!("Could not install plugin: {error:#}")),
                }
                self.reload();
            }
            ResultEvent::Started(key, id, result) => {
                let Some(runtime) = self
                    .runtimes
                    .get_mut(&key)
                    .filter(|runtime| runtime.starting == Some(id))
                else {
                    return;
                };
                runtime.starting = None;
                match result {
                    Ok(worker) => {
                        runtime.generation = id;
                        runtime.worker = Some(worker);
                    }
                    Err(error) => {
                        runtime.backoff.failed(Instant::now());
                        self.log(&key, &format!("Could not start plugin worker: {error:#}"));
                        self.recover_candidate(&key);
                    }
                }
            }
            ResultEvent::Asset(key, generation, result) => self.open_asset(key, generation, result),
            ResultEvent::Frame(key, generation, revision, frame, result, prepare_us) => {
                self.last_canvas_prepare_us
                    .insert(key.namespace.clone(), prepare_us);
                self.canvas_prepared(key, generation, revision, frame, result);
            }
            ResultEvent::TextPrepared(ticket, mut request, result) => {
                if let Some(ticket) = ticket {
                    self.pending_measurements.remove(&ticket);
                }
                if self.text_measurement_live(&request) {
                    match result {
                        Ok(prepared) if prepared.iter().all(Option::is_some) => {
                            let lines = prepared.into_iter().flatten().collect();
                            self.reply_text_measurement(
                                request,
                                Ok(serde_json::Value::Array(lines)),
                            );
                        }
                        Ok(prepared) if self.view.text_measurements.len() < 16 => {
                            request.prepared = prepared.into();
                            let id = self.next();
                            self.view.text_measurements.insert(id, request);
                            self.changed = true;
                        }
                        Ok(_) => self.reply_text_measurement(
                            request,
                            Err("Text measurement queue is busy".into()),
                        ),
                        Err(error) => self.reply_text_measurement(request, Err(error.to_string())),
                    }
                }
            }
            ResultEvent::Config(scope, revision, result) => match result {
                Ok(()) => {
                    if self.config_writes_pending(scope, revision) {
                        self.message("Apply is waiting for affected plugin writes to finish…");
                    }
                    let pending = self.config_applies.entry(scope).or_insert(revision);
                    *pending = (*pending).max(revision);
                }
                Err(error) => {
                    if self.config.note_apply_issue(
                        scope,
                        Some(revision),
                        config::ApplyIssue::Validation,
                    ) {
                        self.message(format!("Apply failed: {error:#}"));
                    }
                }
            },
            ResultEvent::Credential(namespace, slot, result) => {
                self.credential_removal_finished(&namespace, slot, result);
            }
            ResultEvent::CredentialChanged(namespace, slot) => {
                self.refresh_credentials();
                self.changed_credential(&namespace, Some(slot));
            }
            ResultEvent::CredentialAccess(access) => self.credential_access_finished(access),
            ResultEvent::Syntax(key, generation, version, mut result) => {
                let current = self
                    .projects
                    .get(&key.instance.project)
                    .and_then(|project| project.document.as_ref())
                    .map(|doc| (doc.opening, doc.revision));
                if version.is_some() && version != current {
                    result.value = Err("Document changed while parsing".into());
                }
                self.event(ResultEvent::Service(key, generation, result));
            }
            ResultEvent::LanguageService(pending, result) => {
                self.language_service_finished(pending, result);
            }
            ResultEvent::Service(key, generation, result) => {
                self.finish_service(&key.namespace);
                if self.wanted.contains_key(&key)
                    && self.runtimes.get(&key.namespace).is_some_and(|runtime| {
                        runtime.generation == generation && runtime.worker.is_some()
                    })
                {
                    self.invoke(
                        key,
                        PluginEvent::ServiceResult { result },
                        None,
                        Duration::from_secs(2),
                    );
                }
            }
            ResultEvent::Tool(key, generation, result) => {
                self.finish_tool_write(&key);
                if result.value.is_err() {
                    self.log(
                        &key.namespace,
                        "Native tool request failed. Review operation results.",
                    );
                }
                let reviewed = self
                    .tool_review
                    .as_ref()
                    .map(|(_, owner)| owner)
                    .or_else(|| self.view.tool_operations.as_ref().map(|(owner, _)| owner));
                if reviewed == Some(&key.namespace) {
                    self.review_tool_operations(key.namespace.clone());
                }
                self.event(ResultEvent::Service(key, generation, result));
            }
            ResultEvent::BackgroundToolStarted(key, tool, id, result) => {
                self.background_tool_started(key, tool, id, result);
            }
            ResultEvent::BackgroundToolStatus(key, generation, token, tool) => {
                let value = self.current_background_tool_status(&key, tool);
                self.event(ResultEvent::Service(
                    key,
                    generation,
                    ServiceResult { token, value },
                ));
            }
            ResultEvent::ToolOperations(id, namespace, result) => {
                if self.tool_review.as_ref() == Some(&(id, namespace.clone())) {
                    self.tool_review = None;
                    match result.tools {
                        Ok(operations) => {
                            self.view.tool_operations = Some((namespace.clone(), operations));
                            self.changed = true;
                        }
                        Err(error) => {
                            self.message(format!("Could not read tool operations: {error:#}"))
                        }
                    }
                    match result.files {
                        Ok(operations) => {
                            self.view.file_operations = Some((namespace, operations));
                            self.changed = true;
                        }
                        Err(error) => {
                            self.message(format!("Could not read file operations: {error:#}"))
                        }
                    }
                }
            }
            ResultEvent::Development(namespace, digest, result) => {
                let Some((job, cancelled)) = self.developer_job.take() else {
                    return;
                };
                if job != namespace
                    || cancelled.load(Ordering::Acquire)
                    || self
                        .releases
                        .get(&namespace)
                        .is_none_or(|release| release.digest != digest)
                {
                    return;
                }
                match result {
                    Ok(outcome) => {
                        self.developer_stamps
                            .insert(namespace.clone(), outcome.stamp.clone());
                        if let Some(result) = outcome.prepared {
                            match result {
                                Ok(package) if package.release.digest != digest => {
                                    self.developer_ready = Some(Change::Development(
                                        Box::new(package),
                                        developer::Candidate {
                                            source: outcome.source,
                                            stamp: outcome.stamp,
                                            base: digest,
                                            cancelled,
                                            permissions_reviewed: false,
                                        },
                                    ));
                                }
                                Ok(_) => self.message(
                                    "Local build completed. The active version is unchanged.",
                                ),
                                Err(error) => {
                                    self.log(&namespace, &format!("Development build: {error:#}"))
                                }
                            }
                        }
                    }
                    Err(error) => {
                        self.developer_stamps.remove(&namespace);
                        self.log(&namespace, &format!("Development source: {error:#}"));
                        self.developer_due = Instant::now() + Duration::from_secs(3);
                    }
                }
            }
            _ => {}
        }
    }

    fn busy(&self, namespace: &str) -> bool {
        self.mutating.as_deref() == Some(namespace)
            || self
                .pending_change
                .as_ref()
                .is_some_and(|change| change.namespace() == namespace)
    }

    fn review_package(&mut self, token: u64, change: Change) {
        let package = match &change {
            Change::Install(package) | Change::Development(package, _) => package,
            Change::Remove(_) => unreachable!("no permission review for removal"),
        };
        let manifest = &package.release.manifest;
        let previous = self.records.get(&manifest.identity().namespace());
        let mut additional: BTreeSet<_> = previous.map_or_else(
            || manifest.permissions.clone(),
            |old| {
                manifest
                    .permissions
                    .difference(&old.grants)
                    .copied()
                    .collect()
            },
        );
        // A deliberate reinstall of the same package also lets the user reapprove moved paths.
        let rule_paths = if matches!(change, Change::Install(_))
            && previous.is_some_and(|old| old.release.digest == package.release.digest)
        {
            manifest.rule_paths.clone()
        } else {
            ApprovedRulePaths::additional(&manifest.rule_paths, previous.map(|old| &old.rule_paths))
        };
        if !rule_paths.is_empty() {
            additional.insert(Permission::ReadRulePaths);
        }
        self.view.review = Some(InstallReview {
            token,
            manifest: manifest.clone(),
            additional,
            additional_rule_paths: rule_paths.iter().map(PathBuf::from).collect(),
        });
        self.review = Some(PackageReview {
            token,
            change,
            rule_paths,
        });
    }

    fn cancel_development(&mut self, namespace: &str) {
        for (key, cancelled) in self
            .developer_job
            .iter()
            .chain(self.developer_install.iter())
        {
            if key == namespace {
                cancelled.store(true, Ordering::Release);
            }
        }
        let matches = |change: &Change| {
            matches!(change, Change::Development(..)) && change.namespace() == namespace
        };
        if self.developer_ready.as_ref().is_some_and(matches) {
            self.developer_ready.take();
        }
        if self.pending_change.as_ref().is_some_and(matches) {
            self.pending_change.take();
            self.view.installing = false;
        }
        if self
            .review
            .as_ref()
            .is_some_and(|review| matches(&review.change))
        {
            self.review.take();
            self.view.review = None;
        }
    }

    fn advance_development(&mut self) {
        if !self.view.development
            || !self.enabled
            || self.developer_job.is_some()
            || self.pending_change.is_some()
            || self.mutating.is_some()
            || self.inspecting.is_some()
            || self.review.is_some()
        {
            return;
        }
        if let Some(change) = self.developer_ready.take() {
            if let Change::Development(package, _) = &change
                && let Some(previous) = self.records.get(&change.namespace())
                && (!package
                    .release
                    .manifest
                    .permissions
                    .is_subset(&previous.grants)
                    || !ApprovedRulePaths::additional(
                        &package.release.manifest.rule_paths,
                        Some(&previous.rule_paths),
                    )
                    .is_empty())
            {
                let token = self.next();
                self.review_package(token, change);
                self.message("Review the new permissions before reloading this plugin.");
                return;
            }
            self.pending_change = Some(change);
            self.view.installing = true;
            self.message("Local build ready. Waiting for plugin operations to finish…");
            return;
        }
        if Instant::now() < self.developer_due {
            return;
        }
        let manual = self.developer_build.pop_first();
        let force = manual.is_some();
        let namespace = manual.or_else(|| {
            let mut choices = self
                .view
                .developer_settings
                .iter()
                .filter_map(|(key, settings)| {
                    (settings.automatic
                        && settings.source.is_some()
                        && self
                            .releases
                            .get(key)
                            .is_some_and(|release| release.development))
                    .then_some(key)
                })
                .collect::<Vec<_>>();
            if let Some(index) = choices
                .iter()
                .position(|key| key.as_str() == self.developer_last.as_str())
            {
                let count = choices.len();
                choices.rotate_left((index + 1) % count);
            }
            choices.first().map(|key| (*key).clone())
        });
        let Some(namespace) = namespace else {
            return;
        };
        self.developer_due = Instant::now() + Duration::from_millis(750);
        self.developer_last.clone_from(&namespace);
        let Some(release) = self.releases.get(&namespace).cloned() else {
            return;
        };
        let settings = self
            .view
            .developer_settings
            .get(&namespace)
            .cloned()
            .unwrap_or_default();
        let cancelled = Arc::new(AtomicBool::new(false));
        let build = developer::Build {
            settings,
            prior: self.developer_stamps.get(&namespace).cloned(),
            force,
            cancelled: Arc::clone(&cancelled),
        };
        let store = self.store.clone();
        let finish = self.finish.clone();
        let key = namespace.clone();
        if self.developer_io.submit(move || {
            let digest = release.digest.clone();
            let result = developer::run(store, release, build);
            let _ = finish.send(ResultEvent::Development(key, digest, result));
        }) {
            self.developer_job = Some((namespace, cancelled));
        }
    }

    fn advance_install(&mut self) {
        let Some(change) = &self.pending_change else {
            return;
        };
        let key = change.namespace();
        if self.service_pending.contains_key(&key) {
            return;
        }
        if self
            .runtimes
            .get(&key)
            .is_some_and(|runtime| !runtime.calls.is_empty())
        {
            return;
        }
        let change = self.pending_change.take().expect("pending change");
        match change {
            Change::Install(_) | Change::Development(..) => {
                let (package, candidate) = match change {
                    Change::Install(package) => (package, None),
                    Change::Development(package, candidate) => (package, Some(candidate)),
                    Change::Remove(_) => unreachable!("installation candidate"),
                };
                let cancelled = candidate.as_ref().map(|build| Arc::clone(&build.cancelled));
                let store = self.store.clone();
                let finish = self.finish.clone();
                let namespace = key.clone();
                if self.install.submit(move || {
                    let grants = package.release.manifest.permissions.clone();
                    let result = store.install_checked(*package, &grants, |installed| {
                        candidate.map_or(Ok(()), |build| {
                            build.verify(installed)?;
                            if !build.permissions_reviewed {
                                let prior =
                                    installed.context("Development plugin is not installed")?;
                                anyhow::ensure!(
                                    grants.is_subset(&prior.grants),
                                    "Review the new development permissions before installing"
                                );
                            }
                            Ok(())
                        })
                    });
                    let _ = finish.send(ResultEvent::Installed(namespace, result));
                }) {
                    self.developer_install = cancelled.map(|cancelled| (key.clone(), cancelled));
                    self.mutating = Some(key);
                    self.message("Preparing and installing plugin…");
                } else {
                    self.view.installing = false;
                    self.message("Installer is busy. Select the ZIP again.");
                }
            }
            Change::Remove(namespace) => {
                self.view.installing = false;
                self.runtimes.remove(&namespace);
                self.view
                    .surfaces
                    .retain(|key, _| key.namespace != namespace);
                let result = self.store.uninstall(&namespace);
                self.reload();
                match result {
                    Ok(()) => self.message("Plugin uninstalled. Local data kept."),
                    Err(error) => self.message(format!("{error:#}")),
                }
            }
        }
    }

    fn recover_candidate(&mut self, namespace: &str) -> bool {
        if !self.ready_candidates.contains(namespace) {
            return false;
        }
        let Some(release) = self.releases.get(namespace) else {
            return false;
        };
        match self.store.rollback(namespace, &release.digest) {
            Ok(()) => {
                self.runtimes.remove(namespace);
                self.view
                    .surfaces
                    .retain(|key, _| key.namespace != namespace);
                self.reload();
                self.message("Plugin could not start. Previous version restored.");
                true
            }
            Err(error) => {
                self.message(format!(
                    "Plugin could not start. Could not restore previous version: {error:#}"
                ));
                false
            }
        }
    }

    /// Reconcile permission and rule results without starting modules for manager visibility.
    fn reconcile(&mut self) {
        let mut rules = BTreeSet::new();
        let mut wanted = BTreeMap::new();
        let mut states = BTreeMap::new();
        let mut rule_warnings = BTreeMap::new();
        for (namespace, record) in &self.records {
            let compatible = self.store.compatibility().check(&record.release.manifest);
            for project in self.projects.values() {
                for module in &record.release.manifest.modules {
                    let key = Key {
                        namespace: namespace.clone(),
                        instance: Instance {
                            project: project.id,
                            module: module.id.clone(),
                        },
                    };
                    let (allowed, state) = if !self.enabled || !record.enabled {
                        (false, State::Disabled)
                    } else if let Err(error) = &compatible {
                        (false, State::Failed(error.to_string()))
                    } else {
                        let configured = self.config.rule(project.id, namespace, &module.id);
                        let source = match configured {
                            Some(config::Rule::Always) => None,
                            Some(config::Rule::Never) => {
                                states.insert(key, State::Unmatched);
                                continue;
                            }
                            Some(config::Rule::Script(source)) => {
                                Some(Source::Inline(source.clone()))
                            }
                            None => module.rule.as_ref().map(|path| {
                                Source::Package(Arc::clone(&self.releases[namespace]), path.clone())
                            }),
                        };
                        if let Some(source) = source {
                            self.rules.set_approved(
                                key.clone(),
                                source,
                                &record.grants,
                                &record.rule_paths,
                            );
                            rules.insert(key.clone());
                            let (allowed, state) = self.rules.state(&key);
                            if matches!(state, State::Failed(_)) || allowed && state != State::Ready
                            {
                                rule_warnings.insert(key.clone(), state.label().into());
                            }
                            (allowed, state)
                        } else {
                            (true, State::Ready)
                        }
                    };
                    if allowed && module.wasm.is_some() {
                        wanted.insert(key.clone(), module.clone());
                    }
                    states.insert(key, state);
                }
            }
        }
        self.rules.retain(&rules);
        for key in self.wanted.keys().filter(|key| !wanted.contains_key(*key)) {
            if let Some(live) = self
                .runtimes
                .get_mut(&key.namespace)
                .and_then(|runtime| runtime.instances.get_mut(&key.instance))
            {
                live.force_document_refresh = true;
                live.refresh = None;
            }
        }
        self.wanted = wanted;
        self.layouts
            .retain(|key, _| self.projects.contains_key(&key.instance.project));
        self.frames.retain(|key| self.wanted.contains_key(key));
        self.frame_callbacks
            .retain(|key, _| self.wanted.contains_key(key));
        for key in self.wanted.keys() {
            if self.busy(&key.namespace) {
                continue;
            }
            let module = self.wanted[key].clone();
            self.runtimes
                .entry(key.namespace.clone())
                .or_default()
                .instances
                .entry(key.instance.clone())
                .or_insert_with(|| Live {
                    module,
                    opened: false,
                    last: None,
                    refresh: None,
                    force_document_refresh: false,
                    document_base: None,
                    pending_document: None,
                    failure: None,
                    settings_error: self
                        .config
                        .rejected_settings(
                            key,
                            &self.releases[&key.namespace].digest,
                            &self.records[&key.namespace].release.manifest,
                        )
                        .then(|| "Module previously rejected these saved settings".into()),
                    credential_seen: BTreeMap::new(),
                    startup_frame: None,
                    retry_settings: false,
                    open_job: false,
                });
        }
        let before = self.view.surfaces.len();
        self.view
            .surfaces
            .retain(|key, _| self.wanted.contains_key(key));
        self.changed |= before != self.view.surfaces.len();
        self.retain_presentations();
        let before_measurements = self.view.text_measurements.len();
        self.view.text_measurements.retain(|_, request| {
            self.wanted.contains_key(&request.owner)
                && self
                    .runtimes
                    .get(&request.owner.namespace)
                    .is_some_and(|runtime| {
                        runtime.generation == request.generation && runtime.worker.is_some()
                    })
        });
        self.changed |= before_measurements != self.view.text_measurements.len();
        // Runtime status replaces "Ready" from a matching rule until its guest is open.
        let mut settings_errors = BTreeMap::new();
        for key in self.wanted.keys() {
            let runtime = self.runtimes.get(&key.namespace);
            let state = if runtime.is_some_and(|runtime| runtime.memory.paused) {
                State::Failed("Runtime paused after exceeding its memory budget".into())
            } else if runtime.is_some_and(|runtime| runtime.backoff.paused()) {
                State::Failed("Runtime paused. Select Retry to resume.".into())
            } else if runtime.is_some_and(|runtime| !runtime.backoff.ready(Instant::now())) {
                State::Recovering
            } else if let Some(live) =
                runtime.and_then(|runtime| runtime.instances.get(&key.instance))
            {
                if let Some(error) = &live.failure {
                    State::Failed(error.clone())
                } else if let Some(error) = &live.settings_error {
                    settings_errors.insert(key.clone(), error.clone());
                    State::Failed(error.clone())
                } else if live.opened && !self.ready_candidates.contains(&key.namespace) {
                    State::Ready
                } else {
                    State::Starting
                }
            } else {
                State::Starting
            };
            states.insert(key.clone(), state);
        }
        if self.view.modules != states {
            self.view.modules = states;
            self.changed = true;
        }
        if self.view.rule_warnings != rule_warnings {
            self.view.rule_warnings = rule_warnings;
            self.changed = true;
        }
        if self.view.settings_errors != settings_errors {
            self.view.settings_errors = settings_errors;
            self.changed = true;
        }
        let credential_errors = self.view.credential_errors.len();
        self.view
            .credential_errors
            .retain(|key, _| self.wanted.contains_key(key));
        self.changed |= credential_errors != self.view.credential_errors.len();
    }

    fn publish_status(&mut self) {
        let waiting = self.pending_change.is_some();
        if self.view.waiting_for_operations != waiting {
            self.view.waiting_for_operations = waiting;
            self.changed = true;
        }
        let workers = self
            .runtimes
            .iter()
            .filter_map(|(key, runtime)| {
                runtime
                    .worker
                    .as_ref()
                    .map(|worker| (key.clone(), worker.pid()))
            })
            .collect();
        if self.view.workers != workers {
            self.view.workers = workers;
            self.changed = true;
        }
        self.publish_host_resources();
        self.last_canvas_prepare_us
            .retain(|namespace, _| self.runtimes.contains_key(namespace));
        let mut canvas_bytes = BTreeMap::new();
        for (key, resource) in &self.view.host_resources {
            *canvas_bytes.entry(key.namespace.as_str()).or_insert(0) += resource.drawing_bytes;
        }
        let stats = self
            .runtimes
            .iter()
            .filter_map(|(name, runtime)| {
                let queue = runtime.worker.as_ref()?.queue_status();
                Some((
                    name.clone(),
                    RuntimeStatus {
                        generation: runtime.generation,
                        ready_modules: runtime
                            .instances
                            .values()
                            .filter(|live| live.opened && live.settings_error.is_none())
                            .count(),
                        pending: runtime.calls.len()
                            + self.service_pending.get(name).copied().unwrap_or(0),
                        completed: runtime.completed,
                        errors: runtime.errors,
                        last_ms: runtime.last_ms,
                        private_bytes: runtime.memory.private_bytes,
                        canvas_bytes: canvas_bytes.get(name.as_str()).copied().unwrap_or(0),
                        request_bytes: queue.request_bytes,
                        reply_wire_bytes: queue.reply_wire_bytes,
                        peak_request_bytes: queue.peak_request_bytes,
                        peak_reply_wire_bytes: queue.peak_reply_wire_bytes,
                        last_write_wait_us: queue.last_write_wait_us,
                        last_worker_timing: runtime.last_worker_timing,
                        last_canvas_prepare_us: self.last_canvas_prepare_us.get(name).copied(),
                    },
                ))
            })
            .collect();
        if self.view.runtime_status != stats {
            self.view.runtime_status = stats;
            self.changed = true;
        }
    }

    fn advance_runtime(&mut self, namespace: &str, runtime: &mut Runtime) {
        let now = Instant::now();
        if self.sample_memory(namespace, runtime, now) {
            return;
        }
        let mut failed = false;
        for _ in 0..32 {
            let Some(worker) = &runtime.worker else {
                break;
            };
            match worker.poll() {
                Ok(Some(reply)) => self.runtime_reply(namespace, runtime, reply),
                Ok(None) => break,
                Err(_) => {
                    failed = true;
                    break;
                }
            }
        }
        let mut expired = Vec::new();
        for (&id, call) in &runtime.calls {
            if now >= call.deadline {
                expired.push(id);
            }
        }
        for id in expired {
            let call = runtime.calls.get_mut(&id).expect("outstanding call");
            if let Some(reply) = call.reply.take() {
                let _ = reply.try_send(Err(call.kind.timeout_message().into()));
            }
            // Keep timed out operations counted until the runtime actually returns.
            failed |= now.saturating_duration_since(call.deadline) >= PREPARE_TIMEOUT;
        }
        if failed {
            self.runtime_lost(namespace, runtime);
            return;
        }
        if self.ready_candidates.contains(namespace)
            && self.advance_candidate(namespace, runtime, now)
        {
            return;
        }
        let disabled = !self.enabled
            || self
                .records
                .get(namespace)
                .is_none_or(|record| !record.enabled);
        let closing = runtime
            .instances
            .keys()
            .filter_map(|instance| {
                (disabled || !self.projects.contains_key(&instance.project))
                    .then_some(instance.project)
            })
            .collect::<BTreeSet<_>>();
        for project in closing {
            if runtime.closing.contains(&project) {
                continue;
            }
            if runtime.worker.is_none() {
                runtime
                    .instances
                    .retain(|instance, _| instance.project != project);
                continue;
            }
            let id = self.next();
            if runtime
                .worker
                .as_ref()
                .expect("live worker")
                .post(Request::Close { id, project })
                .is_ok()
            {
                runtime.closing.insert(project);
                runtime.calls.insert(
                    id,
                    Call {
                        instance: Instance {
                            project,
                            module: String::new(),
                        },
                        version: None,
                        documents: Vec::new(),
                        deadline: now + PREPARE_TIMEOUT,
                        started: now,
                        kind: CallKind::Close,
                        reply: None,
                        credential_read: None,
                    },
                );
            }
        }
        if runtime.instances.is_empty() && runtime.calls.is_empty() {
            if let Some(worker) = runtime.worker.take() {
                self.log(namespace, "Last project closed. Released plugin worker.");
                drop(worker);
            }
            return;
        }
        if self.busy(namespace) {
            return;
        }
        let mut wanted = self
            .wanted
            .keys()
            .filter(|key| key.namespace == namespace)
            .cloned()
            .collect::<Vec<_>>();
        if wanted.is_empty() {
            return;
        }
        wanted.sort_by_key(|key| Some(key.instance.project) != self.active);
        if runtime.worker.is_none()
            && runtime.starting.is_none()
            && runtime.backoff.ready(now)
            && !runtime.memory.paused
        {
            let store = self.store.clone();
            let finish = self.finish.clone();
            let namespace = namespace.to_owned();
            let id = self.next();
            let supervisor = std::thread::current();
            if self.launch.submit(move || {
                let cache = store.cache_config().ok();
                let result = Worker::start_with_wake("wasm", cache.as_deref(), Some(supervisor));
                let _ = finish.send(ResultEvent::Started(namespace, id, result));
            }) {
                runtime.starting = Some(id);
            }
        }
        if runtime.worker.is_none() {
            return;
        }
        for key in wanted {
            if runtime.closing.contains(&key.instance.project) || runtime.calls.len() >= 12 {
                continue;
            }
            let Some(live) = runtime.instances.get_mut(&key.instance) else {
                continue;
            };
            if live.failure.is_some() {
                continue;
            }
            if !live.opened {
                if !live.open_job
                    && let Some(path) = live.module.wasm.clone()
                {
                    let store = self.store.clone();
                    let release = Arc::clone(&self.releases[namespace]);
                    let finish = self.finish.clone();
                    let generation = runtime.generation;
                    live.open_job = self.launch.submit(move || {
                        let asset = store.verified_asset(&release, &path);
                        let _ = finish.send(ResultEvent::Asset(key, generation, asset));
                    });
                }
                continue;
            }
            if runtime
                .calls
                .values()
                .any(|call| call.instance == key.instance)
            {
                continue;
            }
            let Some(event) = self.refresh_event(&key, live) else {
                continue;
            };
            let refreshed_document = matches!(event, PluginEvent::DocumentChanged);
            let refreshed_settings = matches!(event, PluginEvent::SettingsChanged);
            let resync_canvas = matches!(event, PluginEvent::CanvasResync);
            let document = self.active_document(&key);
            let delta = if refreshed_document {
                live.document_base
                    .as_deref()
                    .zip(document.map(Arc::as_ref))
                    .and_then(|(base, target)| DocumentDelta::between(base, target))
            } else {
                None
            };
            let Some(input) =
                self.input_with_document(&key, event, false, document.filter(|_| delta.is_none()))
            else {
                continue;
            };
            let mut current =
                signature(&input, self.config.settings_revision(key.instance.project));
            let version = document.map(|doc| (doc.opening, doc.revision));
            current.0 = version;
            let observed = refresh_observed(live.last.as_ref(), current, &input.event);
            let pending_document = if refreshed_document {
                document.cloned()
            } else {
                None
            };
            let id = self.next();
            let input = Box::new(input);
            let request = if refreshed_document {
                Request::RefreshDocument {
                    id,
                    instance: key.instance.clone(),
                    input,
                    interactive: live.module.interactive,
                    delta,
                }
            } else {
                Request::Call {
                    id,
                    instance: key.instance.clone(),
                    input,
                    interactive: live.module.interactive,
                }
            };
            if runtime
                .worker
                .as_ref()
                .expect("live worker")
                .post(request)
                .is_ok()
            {
                if resync_canvas {
                    self.frames.resync_sent(&key);
                }
                live.refresh = Some(Refresh {
                    observed,
                    document: refreshed_document,
                    settings: refreshed_settings,
                });
                live.pending_document = pending_document;
                runtime.calls.insert(
                    id,
                    Call {
                        instance: key.instance,
                        version,
                        documents: Vec::new(),
                        deadline: now + PREPARE_TIMEOUT,
                        started: now,
                        kind: CallKind::Refresh,
                        reply: None,
                        credential_read: None,
                    },
                );
            }
        }
    }

    /// Keep candidate frames and settings isolated until every wanted instance accepts startup.
    fn advance_candidate(&mut self, namespace: &str, runtime: &mut Runtime, now: Instant) -> bool {
        let active = self
            .wanted
            .keys()
            .filter(|key| key.namespace == namespace)
            .collect::<Vec<_>>();
        let manifest = &self.records[namespace].release.manifest;
        let mut ready = !active.is_empty();
        for key in &active {
            let Some(live) = runtime.instances.get(&key.instance) else {
                ready = false;
                continue;
            };
            let settings_current = live.last.as_ref().is_some_and(|last| {
                !self
                    .config
                    .settings_changed(key.instance.project, namespace, manifest, &last.2)
            });
            if live.failure.is_some() || settings_current && live.settings_error.is_some() {
                self.runtime_lost(namespace, runtime);
                return true;
            }
            ready &= live.opened && settings_current;
        }
        if !ready
            || runtime
                .takeover_retry
                .is_some_and(|deadline| now < deadline)
        {
            return false;
        }
        match self
            .store
            .confirm_takeover(namespace, &self.releases[namespace].digest)
        {
            Ok(()) => {
                self.ready_candidates.remove(namespace);
                runtime.takeover_retry = None;
                if let Some(record) = self.records.get_mut(namespace) {
                    Arc::make_mut(record).pending_takeover = false;
                }
                if let Some(record) = Arc::make_mut(&mut self.view.installed)
                    .iter_mut()
                    .find(|record| record.release.digest == self.releases[namespace].digest)
                {
                    record.pending_takeover = false;
                }
                self.changed = true;
                let frames = active
                    .into_iter()
                    .filter_map(|key| {
                        let frame = runtime
                            .instances
                            .get_mut(&key.instance)?
                            .startup_frame
                            .take()?;
                        Some((key.clone(), frame))
                    })
                    .collect::<Vec<_>>();
                for (key, frame) in frames {
                    self.set_surface(&key, runtime.generation, frame);
                }
                for live in runtime.instances.values_mut() {
                    live.startup_frame = None;
                }
            }
            Err(error) => {
                runtime.takeover_retry = Some(now + Duration::from_secs(1));
                self.log(
                    namespace,
                    &format!("Could not confirm updated plugin: {error:#}"),
                );
            }
        }
        false
    }

    fn refresh_event(&self, key: &Key, live: &mut Live) -> Option<PluginEvent> {
        let project = self.projects.get(&key.instance.project)?;
        let record = self.records.get(&key.namespace)?;
        let interactive = live.module.interactive;
        let presents_document = live
            .module
            .provides
            .contains(&crate::plugins::package::Provider::Document);
        if live.retry_settings {
            return Some(PluginEvent::SettingsChanged);
        }
        let Some(prior) = &mut live.last else {
            return Some(
                if (interactive || presents_document) && live.settings_error.is_none() {
                    PluginEvent::DocumentChanged
                } else {
                    PluginEvent::SettingsChanged
                },
            );
        };
        let settings_revision = self.config.settings_revision(project.id);
        if settings_revision != prior.4 {
            if self.config.settings_changed(
                project.id,
                &key.namespace,
                &record.release.manifest,
                &prior.2,
            ) {
                return Some(PluginEvent::SettingsChanged);
            }
            prior.4 = settings_revision;
        }
        if (!interactive && !presents_document)
            || live.settings_error.is_some() && !live.module.settings_ui
        {
            return None;
        }
        if !interactive {
            let document = input_document(project, record);
            let version = document.map(|doc| (doc.opening, doc.revision));
            let path = document.and_then(|doc| doc.path.as_ref());
            let prior_path = live
                .document_base
                .as_ref()
                .and_then(|doc| doc.path.as_ref());
            return (live.force_document_refresh || version != prior.0 || path != prior_path)
                .then_some(PluginEvent::DocumentChanged);
        }
        if self.draft_for(key).map_or("", String::as_str) != prior.1 {
            return Some(PluginEvent::DraftChanged);
        }
        if self.layouts.get(key).map(|viewport| viewport.id) != prior.3 {
            return Some(PluginEvent::Layout);
        }
        if self.frames.resync_pending(key) {
            return Some(PluginEvent::CanvasResync);
        }
        let version = input_document(project, record).map(|doc| (doc.opening, doc.revision));
        (live.force_document_refresh || version != prior.0).then_some(PluginEvent::DocumentChanged)
    }

    fn sample_memory(&mut self, namespace: &str, runtime: &mut Runtime, now: Instant) -> bool {
        let Some(worker) = &runtime.worker else {
            return false;
        };
        if runtime.memory.next.is_some_and(|next| now < next) {
            return false;
        }
        runtime.memory.next = Some(now + Duration::from_secs(1));
        runtime.memory.private_bytes = worker.private_bytes();
        if runtime
            .memory
            .private_bytes
            .is_none_or(|bytes| bytes <= RUNTIME_PRIVATE_LIMIT)
        {
            runtime.memory.tried_trim = false;
            return false;
        }
        if !runtime.memory.tried_trim {
            runtime.memory.tried_trim = worker.post(Request::TrimCache).is_ok();
            self.log(
                namespace,
                "Memory budget exceeded. Releasing rebuildable runtime cache.",
            );
            return false;
        }
        let candidate = self.ready_candidates.contains(namespace);
        self.runtime_lost(namespace, runtime);
        runtime.memory.paused = !candidate;
        self.log(
            namespace,
            "Runtime stopped after its private memory remained above the budget",
        );
        true
    }

    fn input(&self, key: &Key, event: PluginEvent, user: bool) -> Option<Input> {
        self.input_with_document(key, event, user, self.active_document(key))
    }

    fn active_document(&self, key: &Key) -> Option<&Arc<DocumentSnapshot>> {
        input_document(
            self.projects.get(&key.instance.project)?,
            self.records.get(&key.namespace)?,
        )
    }

    fn input_with_document(
        &self,
        key: &Key,
        event: PluginEvent,
        user: bool,
        document: Option<&Arc<DocumentSnapshot>>,
    ) -> Option<Input> {
        let project = self.projects.get(&key.instance.project)?;
        let record = self.records.get(&key.namespace)?;
        let document = document.map(|snapshot| snapshot.as_ref().clone());
        let settings = self
            .config
            .settings(project.id, &key.namespace, &record.release.manifest);
        let draft = self.draft_for(key).cloned().unwrap_or_default();
        let mut space = MAX_DOC.saturating_sub(document.as_ref().map_or(0, |doc| doc.text.len()));
        let open_documents = if user && record.grants.contains(&Permission::ReadDocument) {
            project
                .open_documents
                .iter()
                .filter_map(|snapshot| {
                    if snapshot.text.len() > space
                        || document
                            .as_ref()
                            .is_some_and(|active| active.opening == snapshot.opening)
                    {
                        return None;
                    }
                    space -= snapshot.text.len();
                    Some(snapshot.as_ref().clone())
                })
                .take(16)
                .collect()
        } else {
            Vec::new()
        };
        let canvas_sync = self.canvas_sync_for(key, &event);
        Some(Input {
            event,
            document,
            settings,
            draft,
            viewport: self.layouts.get(key).copied(),
            canvas_sync,
            open_documents,
        })
    }

    fn open_asset(&mut self, key: Key, generation: u64, result: anyhow::Result<PathBuf>) {
        let id = self.next();
        let input = self.input(&key, PluginEvent::Open, false);
        let document = self.active_document(&key).cloned();
        let settings_revision = self.config.settings_revision(key.instance.project);
        let busy = self.busy(&key.namespace);
        let Some(runtime) = self
            .runtimes
            .get_mut(&key.namespace)
            .filter(|runtime| runtime.generation == generation)
        else {
            return;
        };
        let Some(live) = runtime.instances.get_mut(&key.instance) else {
            return;
        };
        live.open_job = false;
        let Some(input) = input.filter(|_| {
            !busy
                && self.wanted.contains_key(&key)
                && !runtime.closing.contains(&key.instance.project)
        }) else {
            return;
        };
        let path = match result {
            Ok(path) => path,
            Err(error) => {
                live.failure = Some(format!("{error:#}"));
                return;
            }
        };
        let Some(worker) = &runtime.worker else {
            return;
        };
        let version = signature(&input, settings_revision);
        let request = Request::Open {
            id,
            instance: key.instance.clone(),
            wasm: path,
            input: Box::new(input),
            interactive: live.module.interactive,
        };
        if worker.post(request).is_ok() {
            live.last = Some(version.clone());
            live.force_document_refresh = false;
            if live.module.interactive
                || live
                    .module
                    .provides
                    .contains(&crate::plugins::package::Provider::Document)
            {
                live.pending_document = document;
            }
            live.open_job = true;
            runtime.calls.insert(
                id,
                Call {
                    instance: key.instance,
                    version: version.0,
                    documents: Vec::new(),
                    deadline: Instant::now() + PREPARE_TIMEOUT,
                    started: Instant::now(),
                    kind: CallKind::Open,
                    reply: None,
                    credential_read: None,
                },
            );
        }
    }

    fn invoke(&mut self, key: Key, event: PluginEvent, reply: Option<Answer>, timeout: Duration) {
        let kind = match event {
            PluginEvent::ServiceResult { .. } => CallKind::Service,
            PluginEvent::ServiceCall { .. } => CallKind::PeerService,
            PluginEvent::Command { .. } | PluginEvent::Ui { .. } => CallKind::User,
            _ => CallKind::Explicit,
        };
        self.invoke_with_kind(key, event, reply, timeout, kind, None);
    }

    /// Limit in-flight background Canvas callbacks and keep their outputs non-authoritative.
    fn invoke_with_kind(
        &mut self,
        key: Key,
        event: PluginEvent,
        reply: Option<Answer>,
        timeout: Duration,
        kind: CallKind,
        credential: Option<CredentialChange>,
    ) -> bool {
        let id = self.next();
        let allowed = !self.busy(&key.namespace)
            && self.wanted.contains_key(&key)
            && (matches!(kind, CallKind::Service | CallKind::Continued)
                || !self.ready_candidates.contains(&key.namespace));
        let quiet = matches!(kind, CallKind::CanvasPassive(_) | CallKind::Continued);
        let passive = matches!(kind, CallKind::CanvasPassive(_));
        if passive
            && self.runtimes.get(&key.namespace).is_some_and(|runtime| {
                runtime
                    .calls
                    .values()
                    .any(|call| call.kind == kind && call.instance == key.instance)
            })
        {
            return false;
        }
        let closing = (passive
            || matches!(
                kind,
                CallKind::User
                    | CallKind::Explicit
                    | CallKind::Continued
                    | CallKind::PeerService
                    | CallKind::CredentialChange
            ))
            && self.is_draining(key.instance.project);
        let input = self.input(&key, event, kind == CallKind::User);
        let send = || -> anyhow::Result<()> {
            anyhow::ensure!(
                !closing,
                "Project is waiting for confirmed plugin commands to finish"
            );
            anyhow::ensure!(allowed, "Module is not available");
            let input = input.context("Project or plugin was closed")?;
            let runtime = self
                .runtimes
                .get_mut(&key.namespace)
                .context("Plugin is starting")?;
            let live = runtime
                .instances
                .get_mut(&key.instance)
                .filter(|live| live.opened && live.failure.is_none())
                .context("Module is not ready")?;
            let settings_ui = live.module.settings_ui
                && (matches!(input.event, PluginEvent::Ui { .. })
                    || matches!(kind, CallKind::CanvasPassive(_)));
            if !settings_ui
                && !matches!(
                    kind,
                    CallKind::Service | CallKind::CredentialReply | CallKind::CredentialChange
                )
            {
                anyhow::ensure!(
                    live.settings_error.is_none(),
                    "Module could not use its saved settings"
                );
                anyhow::ensure!(
                    live.last
                        .as_ref()
                        .is_none_or(|last| last.2 == input.settings),
                    "Module is adopting updated settings"
                );
            }
            let worker = runtime.worker.as_ref().context("Plugin is recovering")?;
            anyhow::ensure!(
                runtime.calls.len()
                    < MAX_RUNTIME_CALLS + usize::from(kind == CallKind::CredentialReply),
                "Plugin is busy"
            );
            let version = if matches!(kind, CallKind::CredentialChange | CallKind::CredentialReply)
            {
                None
            } else {
                input
                    .document
                    .as_ref()
                    .map(|doc| (doc.opening, doc.revision))
            };
            let documents = input
                .open_documents
                .iter()
                .map(|doc| (doc.opening, doc.revision))
                .chain(version)
                .collect();
            worker.post(Request::Call {
                id,
                instance: key.instance.clone(),
                input: Box::new(input),
                interactive: live.module.interactive,
            })?;
            if kind == CallKind::CredentialChange
                && let Some(change) = &credential
            {
                live.credential_seen
                    .insert(change.slot.clone(), change.revision);
            }
            runtime.calls.insert(
                id,
                Call {
                    instance: key.instance.clone(),
                    version,
                    documents,
                    deadline: Instant::now() + timeout,
                    started: Instant::now(),
                    kind,
                    reply: reply.clone(),
                    credential_read: credential.filter(|_| kind == CallKind::CredentialReply),
                },
            );
            Ok(())
        };
        if let Err(error) = send() {
            if let Some(reply) = reply {
                let _ = reply.try_send(Err(error.to_string()));
            } else if !quiet
                && kind != CallKind::CredentialChange
                && kind != CallKind::CredentialReply
            {
                self.message(error.to_string());
            }
            return false;
        }
        true
    }

    fn runtime_reply(&mut self, namespace: &str, runtime: &mut Runtime, reply: Reply) {
        let id = match &reply {
            Reply::Result { id, .. }
            | Reply::Closed { id, .. }
            | Reply::Error { id, .. }
            | Reply::DocumentResync { id } => *id,
            _ => return,
        };
        let Some(call) = runtime.calls.remove(&id) else {
            return;
        };
        if matches!(reply, Reply::DocumentResync { .. }) {
            if call.kind == CallKind::Refresh
                && let Some(live) = runtime.instances.get_mut(&call.instance)
            {
                live.document_base = None;
                live.pending_document = None;
                live.refresh = None;
                live.force_document_refresh = true;
                if self.view.sampling.contains(namespace) {
                    self.log(namespace, "Document base missing. Sending a full snapshot.");
                }
            }
            return;
        }
        if matches!(call.kind, CallKind::Open | CallKind::Refresh)
            && let Some(live) = runtime.instances.get_mut(&call.instance)
        {
            let pending = live.pending_document.take();
            let tracks_document = call.kind == CallKind::Open
                || live
                    .refresh
                    .as_ref()
                    .is_some_and(|refresh| refresh.document);
            if tracks_document && matches!(reply, Reply::Result { .. }) {
                live.document_base = pending;
            }
        }
        runtime.last_ms = call.started.elapsed().as_millis() as u64;
        let timing = match &reply {
            Reply::Result { timing, .. } => *timing,
            _ => None,
        };
        if timing.is_some() {
            runtime.last_worker_timing = timing;
        }
        if matches!(reply, Reply::Error { .. }) {
            runtime.errors += 1;
        } else {
            runtime.completed += 1;
        }
        if self.view.sampling.contains(namespace) {
            let mut message = format!(
                "Module {} · response in {} ms",
                call.instance.module, runtime.last_ms
            );
            if let Some(timing) = timing {
                message.push_str(&format!(
                    " · project {} · worker queue {} µs · compute {} µs",
                    call.instance.project, timing.queue_us, timing.compute_us
                ));
                if timing.compile_us != 0 || timing.initialize_us != 0 {
                    message.push_str(&format!(
                        " · compile {} µs · initialize {} µs",
                        timing.compile_us, timing.initialize_us
                    ));
                }
            }
            self.log(namespace, &message);
        }
        if call.kind == CallKind::PeerService && Instant::now() >= call.deadline {
            if let Some(reply) = call.reply {
                let _ = reply.try_send(Err(call.kind.timeout_message().into()));
            }
            return;
        }
        let key = Key {
            namespace: namespace.to_owned(),
            instance: call.instance.clone(),
        };
        let mut settings_reply = call.kind == CallKind::Open;
        if call.kind == CallKind::Refresh
            && let Some(live) = runtime.instances.get_mut(&call.instance)
            && let Some(refresh) = live.refresh.take()
            && !matches!(
                &reply,
                Reply::Error {
                    kind: ErrorKind::Busy,
                    ..
                }
            )
        {
            live.last = Some(refresh.observed);
            live.force_document_refresh &= !refresh.document;
            live.retry_settings &= !refresh.settings;
            settings_reply = refresh.settings;
        }
        let mut rejected_settings = false;
        let settings_current = settings_reply
            && Instant::now() <= call.deadline
            && self.wanted.contains_key(&key)
            && runtime
                .instances
                .get(&call.instance)
                .and_then(|live| live.last.as_ref())
                .is_some_and(|last| {
                    !self.config.settings_changed(
                        call.instance.project,
                        namespace,
                        &self.records[namespace].release.manifest,
                        &last.2,
                    )
                });
        if settings_current && let Some(live) = runtime.instances.get_mut(&call.instance) {
            match &reply {
                Reply::Result { output, .. } => {
                    let issue = output.settings_error.as_deref().map(|error| {
                        if error.trim().is_empty() {
                            "Plugin could not use the saved settings".into()
                        } else {
                            error.trim().chars().take(300).collect()
                        }
                    });
                    // Keep a restored rejection only while the same saved values are in use.
                    let restored = call.kind == CallKind::Open
                        && issue.is_none()
                        && live.settings_error.is_some()
                        && self.config.rejected_settings(
                            &key,
                            &self.releases[namespace].digest,
                            &self.records[namespace].release.manifest,
                        );
                    if !restored {
                        live.settings_error = issue;
                    }
                    rejected_settings = live.settings_error.is_some();
                }
                Reply::Error { .. } if call.kind == CallKind::Refresh => {
                    live.settings_error = Some("Plugin could not read the saved settings".into());
                }
                _ => {}
            }
            if let Some(last) = &live.last {
                self.config.note_settings_rejection(
                    &key,
                    &self.releases[namespace].digest,
                    &last.2,
                    live.settings_error.is_some(),
                );
            }
        }
        match reply {
            Reply::Closed { project, .. }
                if call.kind == CallKind::Close && call.instance.project == project =>
            {
                runtime
                    .instances
                    .retain(|instance, _| instance.project != project);
                runtime.closing.remove(&project);
                self.view
                    .surfaces
                    .retain(|key, _| key.namespace != namespace || key.instance.project != project);
                self.changed = true;
            }
            Reply::Error { kind, message, .. } => {
                if call.kind == CallKind::Open
                    && let Some(live) = runtime.instances.get_mut(&call.instance)
                {
                    live.open_job = false;
                    if kind != ErrorKind::Busy {
                        live.failure = Some(message.clone());
                    }
                }
                if let Some(reply) = call.reply {
                    let _ = reply.try_send(Err(message.clone()));
                }
                if !call.kind.is_service() {
                    // Provider errors can carry values from an invocation. Log its outcome only.
                    self.log(
                        namespace,
                        &format!(
                            "Module {} · request failed ({kind:?})",
                            call.instance.module
                        ),
                    );
                }
            }
            Reply::Result { output, .. } => {
                let Some(live) = runtime.instances.get_mut(&call.instance) else {
                    return;
                };
                if call.kind == CallKind::Open {
                    live.open_job = false;
                    live.opened = true;
                    self.log(namespace, &format!("Module {} ready", call.instance.module));
                }
                let current = self
                    .projects
                    .get(&call.instance.project)
                    .and_then(|project| {
                        project
                            .document
                            .as_ref()
                            .map(|doc| (doc.opening, doc.revision))
                    });
                let valid = self.wanted.contains_key(&key)
                    && self.projects.contains_key(&call.instance.project)
                    && (call.version.is_none() || call.version == current)
                    && Instant::now() <= call.deadline;
                if !valid || settings_reply && !settings_current {
                    if let Some(reply) = call.reply {
                        let _ = reply.try_send(Err("Plugin result is out of date".into()));
                    }
                    return;
                }
                let mut output = *output;
                if let Some(change) = &call.credential_read {
                    self.credential_check_finished(&key, change, output.credential_rejected);
                    // Never publish free-form text from a callback which received a credential.
                    return;
                }
                if matches!(call.kind, CallKind::CanvasPassive(_))
                    || call.version.is_none()
                    || !self.records[namespace]
                        .grants
                        .contains(&Permission::EditDocument)
                {
                    output.edits.clear();
                    output.document_edits.clear();
                }
                if call.kind != CallKind::User {
                    output.document_edits.clear();
                }
                if call.kind != CallKind::User
                    || !self.records[namespace]
                        .grants
                        .contains(&Permission::ReadProject)
                    || !self.records[namespace]
                        .grants
                        .contains(&Permission::EditDocument)
                {
                    output.project_file_edits.clear();
                }
                if call.kind != CallKind::User
                    || !self.records[namespace]
                        .grants
                        .contains(&Permission::ChangeFiles)
                {
                    output.file_operations.clear();
                }
                if output
                    .document_edits
                    .iter()
                    .any(|group| !call.documents.contains(&(group.opening, group.revision)))
                {
                    if let Some(reply) = call.reply {
                        let _ = reply.try_send(Err(
                            "Plugin referred to a document version it was not given".into(),
                        ));
                    }
                    return;
                }
                if output.await_service {
                    let request = (output.services.len() == 1)
                        .then(|| output.services.pop().expect("one service"));
                    if !call.kind.is_service()
                        && !rejected_settings
                        && live.settings_error.is_none()
                        && let Some(request) = request
                    {
                        let settings_revision = live.last.as_ref().map_or_else(
                            || self.config.settings_revision(key.instance.project),
                            |last| last.4,
                        );
                        let pending = language::Pending::new(
                            key,
                            runtime.generation,
                            call,
                            request.token,
                            settings_revision,
                        );
                        if !self.begin_language_service(pending, request.operation) {
                            live.force_document_refresh = true;
                        }
                    } else if let Some(reply) = call.reply {
                        let _ = reply
                            .try_send(Err("Expected one read-only service per invocation".into()));
                    }
                    return;
                }
                if let Some(features) = output.document_features.take()
                    && live.settings_error.is_none()
                    && live
                        .module
                        .provides
                        .contains(&crate::plugins::package::Provider::Document)
                    && !rejected_settings
                    && !matches!(
                        call.kind,
                        CallKind::CredentialChange | CallKind::PeerService
                    )
                {
                    self.publish_presentation(&key, runtime.generation, call.version, features);
                }
                let surface = output.surface.take();
                let delta = output.canvas_delta.take();
                if live.module.interactive && (!rejected_settings || live.module.settings_ui) {
                    match (surface, delta) {
                        (Some(frame), None) if self.ready_candidates.contains(namespace) => {
                            live.startup_frame = Some(frame);
                        }
                        (Some(frame), None) => self.set_surface(&key, runtime.generation, frame),
                        (None, Some(delta)) if !self.ready_candidates.contains(namespace) => {
                            self.set_canvas_delta(&key, runtime.generation, delta);
                        }
                        (Some(_), Some(_)) => self.reject_canvas_delta(&key, runtime.generation),
                        _ => {}
                    }
                }
                if !call.kind.is_service() && !rejected_settings {
                    for request in std::mem::take(&mut output.services).into_iter().take(4) {
                        self.service(
                            key.clone(),
                            runtime.generation,
                            call.kind == CallKind::User,
                            request,
                        );
                    }
                }
                if !output.message.is_empty()
                    && (matches!(call.kind, CallKind::Explicit | CallKind::User)
                        || call.kind == CallKind::Continued && call.reply.is_some())
                {
                    self.message(output.message.chars().take(300).collect::<String>());
                }
                if let Some(reply) = call.reply {
                    let _ = reply.try_send(Ok(CallResult {
                        generation: runtime.generation,
                        version: call.version,
                        output,
                    }));
                }
            }
            _ => {}
        }
    }

    fn runtime_lost(&mut self, namespace: &str, runtime: &mut Runtime) {
        runtime.worker.take();
        runtime.closing.clear();
        self.last_canvas_prepare_us.remove(namespace);
        self.frames.remove_plugin(namespace);
        self.view
            .surfaces
            .retain(|key, _| key.namespace != namespace);
        let before = self.view.document_presentations.len();
        self.view
            .document_presentations
            .retain(|key, _| key.namespace != namespace);
        self.changed |= before != self.view.document_presentations.len();
        for (_, call) in std::mem::take(&mut runtime.calls) {
            if let Some(reply) = call.reply {
                let _ = reply.try_send(Err("Plugin worker stopped".into()));
            }
        }
        for instance in runtime.instances.values_mut() {
            instance.opened = false;
            instance.open_job = false;
            instance.last = None;
            instance.refresh = None;
            instance.document_base = None;
            instance.pending_document = None;
            instance.startup_frame = None;
            instance.credential_seen.clear();
        }
        runtime.backoff.failed(Instant::now());
        self.log(namespace, "Plugin worker stopped");
        if self.ready_candidates.contains(namespace) {
            if self.recover_candidate(namespace) {
                runtime.instances.clear();
            } else {
                for live in runtime.instances.values_mut() {
                    live.failure = None;
                }
            }
        }
    }

    fn can_use_credential(&self, namespace: &str, slot: &str) -> bool {
        self.records.get(namespace).is_some_and(|record| {
            record.grants.contains(&Permission::Credentials)
                && record
                    .release
                    .manifest
                    .settings
                    .get(slot)
                    .is_some_and(|setting| setting.secret)
        })
    }

    fn service(&mut self, key: Key, generation: u64, user_event: bool, request: ServiceRequest) {
        let ServiceRequest { token, operation } = request;
        match operation {
            Service::ParseDocument { language } => {
                self.parse_document(key, generation, token, language);
                return;
            }
            Service::ReadProjectText { path } => {
                self.read_project_text(key, generation, token, path);
                return;
            }
            Service::MeasureText { lines } => {
                self.prepare_text_measurement(key, generation, token, lines);
                return;
            }
            Service::StageSettings { scope, values } => {
                use nanobug_plugin_sdk::SettingsScope;
                let scope = match scope {
                    SettingsScope::AllProjects => config::ConfigScope::AllProjects,
                    SettingsScope::CurrentProject => {
                        config::ConfigScope::Project(key.instance.project)
                    }
                    SettingsScope::CurrentProjectLocal => {
                        config::ConfigScope::ProjectLocal(key.instance.project)
                    }
                };
                let value = (|| -> anyhow::Result<serde_json::Value> {
                    anyhow::ensure!(
                        user_event
                            && self
                                .wanted
                                .get(&key)
                                .is_some_and(|module| module.settings_ui),
                        "Settings drafts require a user action in an approved settings module"
                    );
                    let record = self
                        .records
                        .get(&key.namespace)
                        .context("Plugin is not installed")?;
                    self.config
                        .stage_sdk(scope, &record.release.manifest, values)?;
                    self.message("Settings draft saved. Apply to use it.");
                    Ok(serde_json::Value::Null)
                })()
                .map_err(|error| error.to_string());
                *self
                    .service_pending
                    .entry(key.namespace.clone())
                    .or_default() += 1;
                let _ = self.finish.send(ResultEvent::Service(
                    key,
                    generation,
                    ServiceResult { token, value },
                ));
                return;
            }
            Service::RunTool { tool, args } => {
                self.run_tool(key, generation, token, user_event, tool, args);
                return;
            }
            Service::StartBackgroundTool { tool } => {
                self.start_background_tool(key, generation, token, user_event, tool);
                return;
            }
            Service::BackgroundToolStatus { tool } => {
                self.background_tool_status(key, generation, token, tool);
                return;
            }
            Service::CallPlugin {
                name,
                id,
                method,
                argument,
            } => {
                self.call_plugin(key, generation, token, (name, id, method, argument));
                return;
            }
            _ => {}
        }
        let slot = match &operation {
            Service::ReadCredential { key }
            | Service::SaveCredential { key, .. }
            | Service::DeleteCredential { key } => key,
            Service::QuerySyntax { .. } | Service::ReadProjectAncestors { .. } => {
                self.message("This read-only service requires an awaited invocation");
                return;
            }
            Service::ParseDocument { .. }
            | Service::ReadProjectText { .. }
            | Service::MeasureText { .. }
            | Service::StageSettings { .. }
            | Service::RunTool { .. }
            | Service::StartBackgroundTool { .. }
            | Service::BackgroundToolStatus { .. }
            | Service::CallPlugin { .. } => {
                unreachable!("request dispatched")
            }
        };
        let allowed = self.can_use_credential(&key.namespace, slot)
            && !self.busy(&key.namespace)
            && (user_event || matches!(operation, Service::ReadCredential { .. }));
        let read = matches!(operation, Service::ReadCredential { .. });
        let slot = slot.clone();
        let revision = self.credential_revision(&key.namespace, &slot);
        let operation = match operation {
            Service::ReadCredential { .. } => credential_control::Access::Read,
            Service::SaveCredential { secret, .. } => {
                credential_control::Access::Save(zeroize::Zeroizing::new(secret))
            }
            Service::DeleteCredential { .. } => credential_control::Access::Delete,
            _ => unreachable!("request dispatched"),
        };
        let credential = self.credentials.clone();
        let finish = self.finish.clone();
        let owner = key.clone();
        if self.credential_io.submit(move || {
            let completion = if allowed {
                operation.run(&credential, &owner.namespace, &slot)
            } else {
                credential_control::AccessCompletion {
                    changed: false,
                    value: Err("Credential access is not granted for this event".into()),
                }
            };
            let _ = finish.send(ResultEvent::CredentialAccess(
                credential_control::AccessResult {
                    owner,
                    generation,
                    slot,
                    revision,
                    read,
                    changed: completion.changed,
                    result: ServiceResult {
                        token,
                        value: completion.value,
                    },
                },
            ));
        }) {
            *self.service_pending.entry(key.namespace).or_default() += 1;
        } else {
            *self
                .service_pending
                .entry(key.namespace.clone())
                .or_default() += 1;
            let _ = self.finish.send(ResultEvent::Service(
                key,
                generation,
                ServiceResult {
                    token,
                    value: Err("The system credential store is busy".into()),
                },
            ));
        }
    }

    /// Resolve package fonts on the bounded worker queue and defer reply until this runtime is live.
    fn prepare_text_measurement(
        &mut self,
        owner: Key,
        generation: u64,
        token: u32,
        lines: Vec<MeasureText>,
    ) {
        let request = TextMeasurement {
            lines,
            prepared: Arc::default(),
            owner,
            generation,
            token,
        };
        let result = if !self
            .wanted
            .get(&request.owner)
            .is_some_and(|module| module.interactive)
            || request.lines.is_empty()
            || request.lines.len() > 8
            || !request.lines.iter().all(MeasureText::valid)
        {
            Err(anyhow::anyhow!("Invalid or ungranted text measurement"))
        } else if request.lines.iter().all(|line| line.package_font.is_none()) {
            Ok(vec![None; request.lines.len()])
        } else if let Some(release) = self.releases.get(&request.owner.namespace).cloned() {
            let store = self.store.clone();
            let finish = self.finish.clone();
            let pending = request.clone();
            let ticket = self.next();
            if self.io.submit(move || {
                let result = drawing::prepare_text_measurements(&store, &release, &pending.lines);
                let _ = finish.send(ResultEvent::TextPrepared(Some(ticket), pending, result));
            }) {
                let bytes = resource::measurement_bytes(&request.lines, &[]);
                self.pending_measurements
                    .insert(ticket, (request.owner, bytes));
                return;
            }
            Err(anyhow::anyhow!("Text measurement queue is busy"))
        } else {
            Err(anyhow::anyhow!("Plugin is unavailable"))
        };
        let _ = self
            .finish
            .send(ResultEvent::TextPrepared(None, request, result));
    }

    /// Ignore measurements from a replaced, stopped, or deactivated module instance.
    fn text_measurement_live(&self, request: &TextMeasurement) -> bool {
        self.wanted.contains_key(&request.owner)
            && self
                .runtimes
                .get(&request.owner.namespace)
                .is_some_and(|runtime| {
                    runtime.generation == request.generation && runtime.worker.is_some()
                })
    }

    /// Send the result through the existing host service response path for the same live module.
    fn reply_text_measurement(
        &mut self,
        request: TextMeasurement,
        value: Result<serde_json::Value, String>,
    ) {
        if self.text_measurement_live(&request) {
            self.invoke(
                request.owner,
                PluginEvent::ServiceResult {
                    result: ServiceResult {
                        token: request.token,
                        value,
                    },
                },
                None,
                Duration::from_secs(2),
            );
        }
    }

    fn read_project_text(&mut self, key: Key, generation: u64, token: u32, path: String) {
        let root = self
            .projects
            .get(&key.instance.project)
            .filter(|_| {
                self.records
                    .get(&key.namespace)
                    .is_some_and(|record| record.grants.contains(&Permission::ReadProject))
            })
            .map(|project| project.root.clone());
        let finish = self.finish.clone();
        let owner = key.clone();
        let submitted = self.io.submit(move || {
            let value = (|| -> anyhow::Result<serde_json::Value> {
                let root = root.context("Project file access is not granted")?;
                Ok(serde_json::to_value(edits::read_project_text(
                    &root, path,
                )?)?)
            })()
            .map_err(|error| error.to_string());
            let _ = finish.send(ResultEvent::Service(
                owner,
                generation,
                ServiceResult { token, value },
            ));
        });
        *self
            .service_pending
            .entry(key.namespace.clone())
            .or_default() += 1;
        if !submitted {
            let _ = self.finish.send(ResultEvent::Service(
                key,
                generation,
                ServiceResult {
                    token,
                    value: Err("Project file read queue is busy".into()),
                },
            ));
        }
    }

    fn parse_document(&mut self, key: Key, generation: u64, token: u32, language: String) {
        let id = self.next();
        let document = self
            .projects
            .get(&key.instance.project)
            .and_then(|project| project.document.clone())
            .filter(|_| {
                self.records
                    .get(&key.namespace)
                    .is_some_and(|record| record.grants.contains(&Permission::ReadDocument))
            });
        let syntax = Arc::clone(&self.syntax);
        let finish = self.finish.clone();
        let owner = key.clone();
        let version = document.as_ref().map(|doc| (doc.opening, doc.revision));
        *self
            .service_pending
            .entry(key.namespace.clone())
            .or_default() += 1;
        if !self.syntax_io.submit(move || {
            let value = document
                .ok_or_else(|| anyhow::anyhow!("No readable document"))
                .and_then(|doc| {
                    syntax
                        .lock()
                        .unwrap()
                        .parse(id, language, doc.as_ref().clone(), None)
                })
                .map_err(|error| error.to_string());
            let _ = finish.send(ResultEvent::Syntax(
                owner,
                generation,
                version,
                ServiceResult { token, value },
            ));
        }) {
            let _ = self.finish.send(ResultEvent::Service(
                key,
                generation,
                ServiceResult {
                    token,
                    value: Err("Syntax worker is busy".into()),
                },
            ));
        }
    }

    fn call_plugin(
        &mut self,
        caller: Key,
        generation: u64,
        token: u32,
        request: (String, String, String, serde_json::Value),
    ) {
        let (name, id, service, argument) = request;
        let peer = (|| -> anyhow::Result<(Key, u64)> {
            anyhow::ensure!(
                self.records
                    .get(&caller.namespace)
                    .is_some_and(|record| record.grants.contains(&Permission::Collaborate)),
                "Plugin collaboration is not granted"
            );
            let namespace = super::super::package::Identity::new(name, id)?.namespace();
            anyhow::ensure!(
                namespace != caller.namespace,
                "Select another plugin's public service"
            );
            let record = self
                .records
                .get(&namespace)
                .context("Optional plugin is not installed")?;
            anyhow::ensure!(
                self.enabled && record.enabled,
                "Optional plugin is disabled"
            );
            let module = record
                .release
                .manifest
                .public_services
                .get(&service)
                .context("Optional plugin service is unavailable")?;
            let key = Key {
                namespace,
                instance: Instance {
                    project: caller.instance.project,
                    module: module.clone(),
                },
            };
            match self.view.modules.get(&key) {
                Some(State::Ready) => {}
                Some(State::Unmatched) => {
                    anyhow::bail!("Optional plugin service is inactive in this project")
                }
                Some(State::Disabled) => anyhow::bail!("Optional plugin is disabled"),
                _ => anyhow::bail!("Optional plugin service is not ready"),
            }
            anyhow::ensure!(
                self.wanted.contains_key(&key)
                    && !self.busy(&key.namespace)
                    && !self.ready_candidates.contains(&key.namespace)
                    && !self.is_draining(key.instance.project),
                "Optional plugin service is not ready"
            );
            let runtime = self
                .runtimes
                .get(&key.namespace)
                .filter(|runtime| runtime.worker.is_some())
                .context("Optional plugin service is not ready")?;
            anyhow::ensure!(
                self.peer_calls.len() < MAX_PEER_CALLS,
                "Optional plugin service queue is busy"
            );
            Ok((key, runtime.generation))
        })();
        *self
            .service_pending
            .entry(caller.namespace.clone())
            .or_default() += 1;
        let (peer, peer_generation) = match peer {
            Ok(peer) => peer,
            Err(error) => {
                let _ = self.finish.send(ResultEvent::Service(
                    caller,
                    generation,
                    ServiceResult {
                        token,
                        value: Err(error.to_string()),
                    },
                ));
                return;
            }
        };
        let (reply, response) = async_channel::bounded(1);
        self.invoke(
            peer.clone(),
            PluginEvent::ServiceCall {
                name: service,
                argument,
            },
            Some(reply),
            PEER_TIMEOUT,
        );
        self.peer_calls.push(PeerCall {
            caller,
            caller_generation: generation,
            peer,
            peer_generation,
            token,
            deadline: Instant::now() + PEER_TIMEOUT,
            response,
        });
    }

    fn finish_service(&mut self, namespace: &str) {
        if let Some(count) = self.service_pending.get_mut(namespace) {
            *count -= 1;
            if *count == 0 {
                self.service_pending.remove(namespace);
            }
        }
    }

    /// Deliver only to the caller that requested the operation, without blocking either worker.
    fn advance_peer_calls(&mut self) {
        let now = Instant::now();
        let mut pending = std::mem::take(&mut self.peer_calls);
        pending.retain(|call| {
            let caller = self.runtimes.get(&call.caller.namespace).filter(|runtime| {
                runtime.generation == call.caller_generation && runtime.worker.is_some()
            });
            let Some(caller) = caller.filter(|_| {
                self.wanted.contains_key(&call.caller)
                    && !self.busy(&call.caller.namespace)
                    && !self.is_draining(call.caller.instance.project)
            }) else {
                self.finish_service(&call.caller.namespace);
                return false;
            };
            if caller.calls.len() >= MAX_RUNTIME_CALLS {
                if now.saturating_duration_since(call.deadline) < PEER_TIMEOUT {
                    return true;
                }
                self.finish_service(&call.caller.namespace);
                self.log(
                    &call.caller.namespace,
                    "Optional plugin result expired while the caller was busy",
                );
                return false;
            }
            let peer_ready = self.wanted.contains_key(&call.peer)
                && !self.busy(&call.peer.namespace)
                && self
                    .runtimes
                    .get(&call.peer.namespace)
                    .is_some_and(|runtime| {
                        runtime.generation == call.peer_generation && runtime.worker.is_some()
                    });
            let value = if !peer_ready {
                Err("Optional plugin service is no longer ready".into())
            } else {
                match call.response.try_recv() {
                    Ok(result) => result.map(|answer| {
                        answer
                            .output
                            .service_value
                            .unwrap_or(serde_json::Value::Null)
                    }),
                    Err(async_channel::TryRecvError::Empty) if now < call.deadline => return true,
                    Err(async_channel::TryRecvError::Empty) => {
                        Err(CallKind::PeerService.timeout_message().into())
                    }
                    Err(async_channel::TryRecvError::Closed) => {
                        Err("Optional plugin service stopped".into())
                    }
                }
            };
            self.event(ResultEvent::Service(
                call.caller.clone(),
                call.caller_generation,
                ServiceResult {
                    token: call.token,
                    value,
                },
            ));
            false
        });
        self.peer_calls = pending;
    }

    fn draft_for(&self, key: &Key) -> Option<&String> {
        let project = self.projects.get(&key.instance.project)?;
        self.draft.get(
            &key.namespace,
            &query::project_key(&project.root),
            &key.instance.module,
        )
    }

    fn set_draft(&mut self, key: Key, text: String) {
        if text.len() > MAX_DRAFT {
            self.message("Plugin input is too long");
            return;
        }
        let Some(project) = self.projects.get(&key.instance.project) else {
            return;
        };
        if !self.records.get(&key.namespace).is_some_and(|record| {
            record
                .release
                .manifest
                .modules
                .iter()
                .any(|module| module.id == key.instance.module && module.interactive)
        }) {
            return;
        }
        self.draft.record(
            &key.namespace,
            query::project_key(&project.root),
            &key.instance.module,
            text.clone(),
        );
        if let Some(surface) = self.view.surfaces.get_mut(&key) {
            surface.draft = text;
        }
        self.publish_draft_state();
    }

    fn draft_scope(&self, scope: Option<u64>) -> anyhow::Result<Option<String>> {
        scope
            .map(|id| {
                self.projects
                    .get(&id)
                    .map(|project| query::project_key(&project.root))
                    .context("Project was closed")
            })
            .transpose()
    }

    fn publish_draft_state(&mut self) {
        self.view.draft_pending = self.draft.pending();
        self.changed = true;
    }

    fn flush_drafts(&mut self, force: bool, scope: Option<&str>) -> anyhow::Result<()> {
        if !self.draft.pending() || (!force && self.draft_write.elapsed() < DRAFT_INTERVAL) {
            return Ok(());
        }
        self.draft_write = Instant::now();
        let result = self.draft.flush(scope);
        if let Err(error) = &result {
            self.message(format!("Could not save plugin input: {error:#}"));
        }
        self.publish_draft_state();
        result
    }

    fn clean_local(&mut self, namespace: &str) -> anyhow::Result<()> {
        self.config.clean_settings(namespace)?;
        self.draft.clean(namespace)?;
        self.publish_draft_state();
        Ok(())
    }
}

fn signature(input: &Input, settings_revision: [u64; 3]) -> Signature {
    (
        input
            .document
            .as_ref()
            .map(|doc| (doc.opening, doc.revision)),
        input.draft.clone(),
        input.settings.clone(),
        input.viewport.map(|viewport| viewport.id),
        settings_revision,
    )
}

fn input_document<'a>(
    project: &'a Project,
    record: &Installed,
) -> Option<&'a Arc<DocumentSnapshot>> {
    project.document.as_ref().filter(|doc| {
        doc.text.len() <= MAX_DOC && record.grants.contains(&Permission::ReadDocument)
    })
}

/// Each successful notification acknowledges only the change that selected its event.
fn refresh_observed(
    prior: Option<&Signature>,
    current: Signature,
    event: &PluginEvent,
) -> Signature {
    let Some(mut observed) = prior.cloned() else {
        return current;
    };
    match event {
        PluginEvent::DocumentChanged => observed.0 = current.0,
        PluginEvent::DraftChanged => observed.1 = current.1,
        PluginEvent::SettingsChanged => {
            observed.2 = current.2;
            observed.4 = current.4;
        }
        PluginEvent::Layout => observed.3 = current.3,
        _ => {}
    }
    observed
}

#[derive(Default)]
struct Syntax {
    worker: Option<Worker>,
    restart: Backoff,
}

impl Syntax {
    fn parse(
        &mut self,
        id: u64,
        language: String,
        mut document: nanobug_plugin_sdk::DocumentSnapshot,
        query: Option<String>,
    ) -> anyhow::Result<serde_json::Value> {
        anyhow::ensure!(language.len() <= 32, "Invalid grammar name");
        // Parsing only needs bytes and version identity. Keep paths out of the worker message.
        document.path = None;
        anyhow::ensure!(
            self.restart.ready(Instant::now()),
            "Syntax worker is recovering"
        );
        if self.worker.is_none() {
            match Worker::start("syntax", None) {
                Ok(worker) => self.worker = Some(worker),
                Err(error) => {
                    self.restart.failed(Instant::now());
                    return Err(error);
                }
            }
        }
        let worker = self.worker.as_ref().expect("syntax worker started");
        let document = Box::new(document);
        let request = match query {
            Some(query) => Request::QuerySyntax {
                id,
                language,
                query,
                document,
            },
            None => Request::Parse {
                id,
                language,
                document,
            },
        };
        let response = worker
            .post(request)
            .and_then(|()| worker.wait(Duration::from_secs(2)));
        match response {
            Ok(Reply::Parsed { id: actual, value }) if actual == id => Ok(value),
            Ok(Reply::Error {
                id: actual,
                message,
                ..
            }) if actual == id => Err(anyhow::anyhow!(message)),
            _ => {
                self.worker.take();
                self.restart.failed(Instant::now());
                anyhow::bail!("Syntax worker stopped");
            }
        }
    }
}

fn prepare_catalog(
    store: &InstallStore,
    candidates: Vec<CatalogEntry>,
) -> anyhow::Result<Prepared> {
    let mut first_error = None;
    for entry in candidates {
        match store.prepare(&entry.zip).and_then(|package| {
            let manifest = &package.release.manifest;
            anyhow::ensure!(
                manifest.identity() == entry.identity && manifest.version == entry.version,
                "Marketplace entry does not match the ZIP"
            );
            Ok(package)
        }) {
            Ok(package) => return Ok(package),
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
    }
    Err(first_error.unwrap_or_else(|| anyhow::anyhow!("No newer marketplace release is available")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simultaneous_refreshes_keep_each_unsent_change_pending() {
        let prior = (
            Some((1, 1)),
            "previous".into(),
            BTreeMap::new(),
            None,
            [0; 3],
        );
        let current = (
            Some((1, 2)),
            "new draft".into(),
            BTreeMap::from([("flag".into(), true.into())]),
            Some(2),
            [1, 0, 0],
        );
        let observed =
            refresh_observed(Some(&prior), current.clone(), &PluginEvent::SettingsChanged);
        assert_eq!(
            (&observed.0, &observed.1, &observed.3),
            (&prior.0, &prior.1, &prior.3)
        );
        assert_eq!(observed.2, current.2);
        let observed =
            refresh_observed(Some(&observed), current.clone(), &PluginEvent::DraftChanged);
        assert_eq!(observed.1, current.1);
        assert_eq!((observed.0, observed.3), (prior.0, prior.3));
    }

    #[test]
    fn runtime_uses_all_three_restarts_and_pause_is_sticky() {
        let mut backoff = Backoff::default();
        let mut now = Instant::now();
        for delay in [1, 3, 10] {
            backoff.failed(now);
            assert!(!backoff.paused());
            assert!(!backoff.ready(now + Duration::from_millis(delay * 1000 - 1)));
            now += Duration::from_secs(delay);
            assert!(backoff.ready(now));
        }
        backoff.failed(now);
        assert!(backoff.paused());
        assert!(!backoff.ready(now + Duration::from_secs(301)));
        assert!(Backoff::default().ready(now + Duration::from_secs(301)));
    }

    #[test]
    #[ignore = "requires the real parser executable next to the test build"]
    fn editor_document_remains_writable_during_parser_recovery() -> anyhow::Result<()> {
        use crate::document::Document;
        use nanobug_plugin_sdk::DocumentSnapshot;

        let project = tempfile::tempdir()?;
        let path = project.path().join("data.json");
        std::fs::write(&path, "{\"value\": 1}\r\n")?;
        let mut document = Document::open(&path)?;
        let snapshot = |doc: &Document| DocumentSnapshot {
            opening: doc.opening,
            revision: doc.revision,
            path: doc.path.clone(),
            text: doc.text.clone(),
        };
        let mut parser = Syntax::default();
        let first = parser.parse(1, "json".into(), snapshot(&document), None)?;
        assert_eq!(first["has_error"], false);
        let first_pid = parser.worker.as_ref().unwrap().pid();
        parser.worker.as_mut().unwrap().kill_for_test()?;
        let error = parser
            .parse(2, "json".into(), snapshot(&document), None)
            .unwrap_err();
        assert_eq!(error.to_string(), "Syntax worker stopped");

        document.replace(10..11, "2", true);
        document.save(None)?;
        assert_eq!(std::fs::read_to_string(&path)?, "{\"value\": 2}\r\n");
        assert!(parser.worker.is_none());
        std::thread::sleep(Duration::from_millis(1_050));
        let recovered = parser.parse(3, "json".into(), snapshot(&document), None)?;
        assert_eq!(recovered["revision"], document.revision);
        assert_eq!(recovered["cached"], false);
        assert_ne!(parser.worker.as_ref().unwrap().pid(), first_pid);
        Ok(())
    }
}
