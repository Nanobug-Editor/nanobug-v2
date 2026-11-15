//! Plugin supervisor runtime.

use super::*;
/// Sticky pause after three scheduled restarts within five minutes.
#[derive(Default)]
pub(in crate::plugins::session) struct Backoff {
    pub(super) attempts: VecDeque<Instant>,
    pub(super) resume: Option<Instant>,
    pub(super) paused: bool,
}

impl Backoff {
    pub(in crate::plugins::session) fn ready(&self, now: Instant) -> bool {
        !self.paused && self.resume.is_none_or(|resume| now >= resume)
    }

    pub(in crate::plugins::session) fn paused(&self) -> bool {
        self.paused
    }

    pub(in crate::plugins::session) fn failed(&mut self, now: Instant) {
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

pub(super) type Answer = async_channel::Sender<Result<CallResult, String>>;
pub(super) type Version = Option<(u64, u64)>;
pub(super) type Signature = (
    Version,
    String,
    BTreeMap<String, serde_json::Value>,
    Option<u64>,
    [u64; 3],
);

pub(super) struct Call {
    pub(super) instance: Instance,
    pub(super) version: Version,
    pub(super) documents: Vec<(u64, u64)>,
    /// Save destination for a format event, captured before entering the guest.
    pub(super) format_path: Option<PathBuf>,
    pub(super) deadline: Instant,
    pub(super) started: Instant,
    pub(super) kind: CallKind,
    pub(super) reply: Option<Answer>,
    pub(super) credential_read: Option<CredentialChange>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum CallKind {
    Open,
    Refresh,
    Explicit,
    /// A manual or save-triggered formatter request, bound to the active snapshot.
    Format,
    User,
    CanvasPassive(EventKind),
    Close,
    Service,
    /// Completes a version-bound invocation that is waiting for an awaited service.
    Continued,
    CredentialChange,
    CredentialReply,
    PeerService,
}

impl CallKind {
    pub(super) fn is_service(self) -> bool {
        matches!(
            self,
            Self::Service | Self::Continued | Self::CredentialReply | Self::PeerService
        )
    }

    pub(super) fn timeout_message(self) -> &'static str {
        if self == Self::PeerService {
            "Optional plugin service timed out"
        } else {
            "Plugin request timed out"
        }
    }
}

/// An optional request never owns or activates the target instance.
pub(super) struct PeerCall {
    pub(super) caller: Key,
    pub(super) caller_generation: u64,
    pub(super) peer: Key,
    pub(super) peer_generation: u64,
    pub(super) token: u32,
    pub(super) deadline: Instant,
    pub(super) response: async_channel::Receiver<Result<CallResult, String>>,
}

pub(super) struct Refresh {
    pub(super) observed: Signature,
    pub(super) document: bool,
    pub(super) settings: bool,
}

pub(super) struct Live {
    pub(super) module: Module,
    pub(super) opened: bool,
    pub(super) last: Option<Signature>,
    pub(super) refresh: Option<Refresh>,
    pub(super) force_document_refresh: bool,
    pub(super) document_base: Option<Arc<DocumentSnapshot>>,
    pub(super) pending_document: Option<Arc<DocumentSnapshot>>,
    pub(super) failure: Option<String>,
    pub(super) settings_error: Option<String>,
    pub(super) credential_seen: BTreeMap<String, u64>,
    pub(super) startup_frame: Option<GridSurface>,
    pub(super) retry_settings: bool,
    pub(super) open_job: bool,
}

#[derive(Default)]
pub(super) struct Runtime {
    pub(super) worker: Option<Worker>,
    pub(super) starting: Option<u64>,
    pub(super) generation: u64,
    pub(super) takeover_retry: Option<Instant>,
    pub(super) backoff: Backoff,
    pub(super) instances: BTreeMap<Instance, Live>,
    pub(super) closing: BTreeSet<u64>,
    pub(super) calls: BTreeMap<u64, Call>,
    pub(super) completed: u64,
    pub(super) errors: u64,
    pub(super) last_ms: u64,
    pub(super) last_worker_timing: Option<nanobug_plugin_sdk::WorkerTiming>,
    pub(super) memory: MemoryProtection,
}

#[derive(Default)]
pub(super) struct MemoryProtection {
    pub(super) next: Option<Instant>,
    pub(super) private_bytes: Option<u64>,
    pub(super) tried_trim: bool,
    pub(super) paused: bool,
}

/// State retained by the lifecycle workflow across supervisor ticks.
#[derive(Default)]
pub(super) struct Runtimes {
    /// Child workers and live module instances keyed by package namespace.
    pub(super) runtimes: BTreeMap<String, Runtime>,
    /// Modules selected by the current projects, permissions and activation rules.
    pub(super) wanted: BTreeMap<Key, Module>,
}

impl Engine {
    /// Reconcile permission and rule results without starting modules for manager visibility.
    pub(super) fn reconcile(&mut self) {
        let mut rules = BTreeSet::new();
        let mut wanted = BTreeMap::new();
        let mut states = BTreeMap::new();
        let mut rule_warnings = BTreeMap::new();
        let sql_providers: BTreeMap<_, _> = self
            .projects
            .values()
            .filter_map(|project| {
                let path = project.document.as_ref()?.path.as_deref()?;
                sql_dialect::is_generic_sql(path)
                    .then(|| (project.id, self.sql_provider(project.id).map(str::to_owned)))
            })
            .collect();
        for (namespace, record) in &self.records {
            let manifest = &record.release.manifest;
            let compatible = self.store.compatibility().check(manifest);
            for project in self.projects.values() {
                let project_dialect = self.sql_dialects.project_preference(project.id);
                let path = project
                    .document
                    .as_ref()
                    .and_then(|doc| doc.path.as_deref());
                for module in &manifest.modules {
                    let dialect_module = sql_dialect::is_dialect_module(&manifest.id, module);
                    // Evaluate only the explicitly selected SQL server before an SQL file is opened.
                    let project_sql_server = dialect_module
                        && module.language_server.is_some()
                        && project_dialect == Some(manifest.id.as_str());
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
                    } else if dialect_module
                        && ((!module.accepts_document(path) && !project_sql_server)
                            || sql_providers
                                .get(&project.id)
                                .is_some_and(|selected| selected.as_ref() != Some(namespace)))
                    {
                        (false, State::Unmatched)
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
                                if dialect_module {
                                    project_dialect
                                } else {
                                    None
                                },
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
        for key in self
            .lifecycle
            .wanted
            .keys()
            .filter(|key| !wanted.contains_key(*key))
        {
            if let Some(live) = self
                .lifecycle
                .runtimes
                .get_mut(&key.namespace)
                .and_then(|runtime| runtime.instances.get_mut(&key.instance))
            {
                live.force_document_refresh = true;
                live.refresh = None;
            }
        }
        self.lifecycle.wanted = wanted;
        self.layouts
            .retain(|key, _| self.projects.contains_key(&key.instance.project));
        self.frames
            .retain(|key| self.lifecycle.wanted.contains_key(key));
        self.frame_callbacks
            .retain(|key, _| self.lifecycle.wanted.contains_key(key));
        for key in self.lifecycle.wanted.keys() {
            if self.installation.busy(&key.namespace) {
                continue;
            }
            let module = self.lifecycle.wanted[key].clone();
            self.lifecycle
                .runtimes
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
            .retain(|key, _| self.lifecycle.wanted.contains_key(key));
        self.changed |= before != self.view.surfaces.len();
        self.retain_presentations();
        let before_measurements = self.view.text_measurements.len();
        self.view.text_measurements.retain(|_, request| {
            self.lifecycle.wanted.contains_key(&request.owner)
                && self
                    .lifecycle
                    .runtimes
                    .get(&request.owner.namespace)
                    .is_some_and(|runtime| {
                        runtime.generation == request.generation && runtime.worker.is_some()
                    })
        });
        self.changed |= before_measurements != self.view.text_measurements.len();
        // Runtime status replaces "Ready" from a matching rule until its guest is open.
        let mut settings_errors = BTreeMap::new();
        for key in self.lifecycle.wanted.keys() {
            let runtime = self.lifecycle.runtimes.get(&key.namespace);
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
                } else if live.opened
                    && !self.installation.ready_candidates.contains(&key.namespace)
                    && runtime
                        .is_some_and(|runtime| !runtime.closing.contains(&key.instance.project))
                {
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
            .retain(|key, _| self.lifecycle.wanted.contains_key(key));
        self.changed |= credential_errors != self.view.credential_errors.len();
    }

    pub(super) fn publish_status(&mut self) {
        let waiting = self.installation.pending_change.is_some();
        if self.view.waiting_for_operations != waiting {
            self.view.waiting_for_operations = waiting;
            self.changed = true;
        }
        let workers = self
            .lifecycle
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
            .retain(|namespace, _| self.lifecycle.runtimes.contains_key(namespace));
        let mut canvas_bytes = BTreeMap::new();
        for (key, resource) in &self.view.host_resources {
            *canvas_bytes.entry(key.namespace.as_str()).or_insert(0) += resource.drawing_bytes;
        }
        let stats = self
            .lifecycle
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
                            + self
                                .requests
                                .service_pending
                                .get(name)
                                .copied()
                                .unwrap_or(0),
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

    pub(super) fn advance_runtime(&mut self, namespace: &str, runtime: &mut Runtime) {
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
        if self.installation.ready_candidates.contains(namespace)
            && self.advance_candidate(namespace, runtime, now)
        {
            return;
        }
        let record = self.records.get(namespace);
        let disabled = !self.enabled || record.is_none_or(|record| !record.enabled);
        // Dedicated dialect packages release idle workers rather than keeping one per SQL choice.
        let sql_only = record.is_some_and(|record| {
            let manifest = &record.release.manifest;
            !manifest.modules.is_empty()
                && manifest
                    .modules
                    .iter()
                    .all(|module| sql_dialect::is_dialect_module(&manifest.id, module))
        });
        let closing = runtime
            .instances
            .keys()
            .filter_map(|instance| {
                let inactive_dialect = sql_only
                    && !self.lifecycle.wanted.keys().any(|key| {
                        key.namespace == namespace && key.instance.project == instance.project
                    });
                (disabled || inactive_dialect || !self.projects.contains_key(&instance.project))
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
                        format_path: None,
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
        if self.installation.busy(namespace) {
            return;
        }
        let mut wanted = self
            .lifecycle
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
                        format_path: None,
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
    pub(super) fn advance_candidate(
        &mut self,
        namespace: &str,
        runtime: &mut Runtime,
        now: Instant,
    ) -> bool {
        let active = self
            .lifecycle
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
                self.installation.ready_candidates.remove(namespace);
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

    pub(super) fn refresh_event(&self, key: &Key, live: &mut Live) -> Option<PluginEvent> {
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

    pub(super) fn sample_memory(
        &mut self,
        namespace: &str,
        runtime: &mut Runtime,
        now: Instant,
    ) -> bool {
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
        let candidate = self.installation.ready_candidates.contains(namespace);
        self.runtime_lost(namespace, runtime);
        runtime.memory.paused = !candidate;
        self.log(
            namespace,
            "Runtime stopped after its private memory remained above the budget",
        );
        true
    }

    pub(super) fn runtime_lost(&mut self, namespace: &str, runtime: &mut Runtime) {
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
        if self.installation.ready_candidates.contains(namespace) {
            if self.recover_candidate(namespace) {
                runtime.instances.clear();
            } else {
                for live in runtime.instances.values_mut() {
                    live.failure = None;
                }
            }
        }
    }
}

pub(super) fn signature(input: &Input, settings_revision: [u64; 3]) -> Signature {
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

/// Each successful notification acknowledges only the change that selected its event.
pub(super) fn refresh_observed(
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
