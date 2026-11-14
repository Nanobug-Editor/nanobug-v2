//! Own plugin children, live project instances, install transitions and editor replies.

mod actions;
mod assets;
mod events;
mod hot_reload;
mod installation;
mod invocation;
mod persistence;
mod queue;
mod runtime;
mod services;
mod syntax;
#[cfg(test)]
mod tests;
use events::{FileWrite, OperationsReview, ResultEvent};
use installation::Change;
use invocation::input_document;
pub(super) use queue::Pool;
pub(super) use runtime::Backoff;
use runtime::*;
use syntax::Syntax;

mod analyzer;
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
    ffi::OsString,
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
/// Last local ZIP directory, stored alongside the current profile's other state.
const LOCAL_ZIP_DIRECTORY: &str = "local-zip-directory.json";
/// Maximum JSON size for a native path, including the long Windows path form.
const LOCAL_ZIP_DIRECTORY_LIMIT: u64 = 512 * 1024;

struct Engine {
    /// Outstanding host services and instance-to-instance calls.
    requests: services::Requests,
    /// Live workers, project instances and desired runtime membership.
    lifecycle: runtime::Runtimes,
    /// Pending development builds and source-change polling.
    development: hot_reload::Development,
    /// Staged package review, installation and startup rollback candidates.
    installation: installation::Installations,
    store: InstallStore,
    enabled: bool,
    startup: Option<startup::Startup>,
    view: View,
    records: BTreeMap<String, Arc<Installed>>,
    releases: BTreeMap<String, Arc<Release>>,
    projects: BTreeMap<u64, Project>,
    active: Option<u64>,
    rules: Rules,
    config: config::Configurations,
    config_applies: BTreeMap<config::ConfigScope, u64>,
    draft: Drafts,
    draft_write: Instant,
    package_prune_due: Instant,
    layouts: BTreeMap<Key, Viewport>,
    frames: canvas::Frames,
    /// The last Canvas revision that received its single next-frame callback.
    frame_callbacks: BTreeMap<Key, u64>,
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
    syntax: Arc<Mutex<Syntax>>,
    syntax_io: Pool,
    tool_io: Pool,
    tool_writes: BTreeMap<Key, usize>,
    close_drains: BTreeMap<Option<u64>, tool_control::CloseDrain>,
    backgrounds: BTreeMap<(Key, String), tool_control::BackgroundJob>,
    tool_review: Option<(u64, String)>,
    project_execution: BTreeMap<String, bool>,
    project_flags: BTreeMap<u64, Arc<AtomicBool>>,
    analyzers: BTreeMap<Key, analyzer::Binding>,
    developer_io: Pool,
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
                        .development
                        .developer_job
                        .as_ref()
                        .or(engine.development.developer_install.as_ref())
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
            || engine.lifecycle.runtimes.values().any(|runtime| {
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
    /// Load the `store`'s installation and user state, starting modules when `enabled`.
    fn new(store: InstallStore, enabled: bool) -> Self {
        let (finish, events) = mpsc::channel();
        let state = store.state_root().to_path_buf();
        let (drafts, draft_error) = Drafts::load(state.clone());
        let project_execution = read_json(&state.join("execution.json"), 512 * 1024)
            .ok()
            .flatten()
            .unwrap_or_default();
        let mut engine = Self {
            requests: services::Requests::default(),
            lifecycle: runtime::Runtimes::default(),
            development: hot_reload::Development::default(),
            installation: installation::Installations::default(),
            rules: Rules::new(store.clone()),
            config: config::Configurations::new(state.clone()),
            config_applies: BTreeMap::new(),
            credentials: Credentials::new(state.clone()),
            credential_io: Pool::new(1, 4),
            credential_events: credential_control::CredentialEvents::default(),
            resource_sample_due: Instant::now(),
            last_canvas_prepare_us: BTreeMap::new(),
            syntax: Arc::new(Mutex::new(Syntax::default())),
            syntax_io: Pool::new(1, 8),
            tool_io: Pool::new(2, 8),
            tool_writes: BTreeMap::new(),
            close_drains: BTreeMap::new(),
            backgrounds: BTreeMap::new(),
            tool_review: None,
            project_execution,
            project_flags: BTreeMap::new(),
            analyzers: BTreeMap::new(),
            developer_io: Pool::new(1, 1),
            store,
            enabled,
            startup: None,
            view: View::default(),
            records: BTreeMap::new(),
            releases: BTreeMap::new(),
            projects: BTreeMap::new(),
            active: None,
            draft: drafts,
            draft_write: Instant::now(),
            package_prune_due: Instant::now() + Duration::from_secs(5),
            layouts: BTreeMap::new(),
            frames: canvas::Frames::default(),
            frame_callbacks: BTreeMap::new(),
            serial: 0,
            finish,
            events,
            io: Pool::new(4, 16),
            launch: Pool::new(2, 8),
            install: Pool::new(1, 2),
            changed: true,
        };
        engine.view.local_zip_directory =
            read_json::<OsString>(&state.join(LOCAL_ZIP_DIRECTORY), LOCAL_ZIP_DIRECTORY_LIMIT)
                .ok()
                .flatten()
                .map(PathBuf::from)
                .filter(|path| path.is_absolute());
        engine.view.operation_root = Some(state.clone());
        engine.reload();
        engine.view.development = engine.store.compatibility().experiments.contains_key("ui");
        if engine.view.development {
            engine.view.developer_settings = read_json(&state.join("developer.json"), 512 * 1024)
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
                self.installation.ready_candidates.clear();
                for record in &list {
                    let key = record.release.manifest.identity().namespace();
                    if record.pending_takeover {
                        self.installation.ready_candidates.insert(key.clone());
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
        self.requests
            .file_writes
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
        self.requests.file_writes.retain(|write| {
            if !write.ended.is_closed() {
                return true;
            }
            let namespace = &write.owner.namespace;
            if let Some(count) = self.requests.service_pending.get_mut(namespace) {
                *count -= 1;
                if *count == 0 {
                    self.requests.service_pending.remove(namespace);
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
        let names = self.lifecycle.runtimes.keys().cloned().collect::<Vec<_>>();
        for name in names {
            let mut runtime = self
                .lifecycle
                .runtimes
                .remove(&name)
                .expect("known runtime");
            self.advance_runtime(&name, &mut runtime);
            if runtime.worker.is_some()
                || runtime.starting.is_some()
                || !runtime.instances.is_empty()
                || (self.records.contains_key(&name) && runtime.backoff.paused())
                || (self.records.contains_key(&name) && runtime.memory.paused)
            {
                self.lifecycle.runtimes.insert(name, runtime);
            }
        }
        self.sync_analyzers();
        self.advance_peer_calls();
        self.advance_credential_events();
        self.advance_background_tools();
        self.advance_close_drains();
        self.advance_install();
        self.advance_development();
        self.publish_status();
    }
}
