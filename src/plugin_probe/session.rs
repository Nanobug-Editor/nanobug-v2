//! Reconcile live projects, restart the sample runtime, and accept only current frames.

use super::{Options, Trace};
use nanobug_probe_wire::{
    Draft, Event, Failure, MAX_FLAG, Owner, RULE_LANES, Request, Rule, receive, send,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs::{self, File},
    io::{self, BufReader, Read},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, SyncSender},
    },
    time::{Duration, Instant},
};

/// Host snapshot for one open directory and its acknowledged input.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Project {
    /// Unique ID for this opening, independent of its tab position.
    pub(crate) scope: u64,
    /// Host-selected root for the fixture's only file read.
    pub(crate) root: PathBuf,
    /// Complete latest draft, suitable for reconnecting a fresh instance.
    pub(crate) draft: Draft,
}

/// Current complete sample frame, verified against a live project and its input.
#[derive(Clone)]
pub(crate) struct Surface {
    /// Identifies the live worker and project instance.
    pub(crate) owner: Owner,
    /// UTF-8 draw text and the revision requested by the guest.
    pub(crate) draft: Draft,
    /// Heading decoded from the WASM draw request.
    pub(crate) title: String,
    /// Background RGBA for the grid surface.
    pub(crate) color: u32,
}

/// Latest renderable state, published with a coalescing notification.
#[derive(Clone, Default)]
pub(crate) struct Scene {
    /// Status to show while a frame is unavailable.
    pub(crate) status: &'static str,
    /// Complete accepted frames by project scope.
    pub(crate) surfaces: BTreeMap<u64, Surface>,
}

/// Explicit fault and workload commands for the experiment.
pub(crate) enum Control {
    /// Terminate the active WASM process, exercising ordinary recovery.
    Terminate,
    /// Start the bounded business job for this scope.
    Work(u64),
    /// Start a rule which exhausts its execution allowance.
    BusyRule,
}

/// Single-slot state shared with the UI. Never held across I/O or process waits.
#[derive(Default)]
struct Shared {
    /// Latest desired project snapshots, replacing older snapshots as a unit.
    projects: Arc<Vec<Project>>,
    /// Latest accepted view for each live scope.
    scene: Scene,
    /// Set on UI destruction, independent of channel sender lifetimes.
    stopped: bool,
}

/// Nonblocking UI connection to the supervisor.
pub(crate) struct Client {
    /// Latest projects and renderable scene.
    shared: Arc<Mutex<Shared>>,
    /// Bounded queue for test controls, timestamps and worker events.
    sender: SyncSender<Message>,
    /// Coalesced notifications for the current scene.
    notify: async_channel::Receiver<()>,
}

impl Client {
    /// Create the handoff channels and run the supervisor off the UI thread.
    pub(super) fn start(options: Options, ready: Instant, editable: bool) -> Self {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let (sender, receiver) = mpsc::sync_channel(128);
        let (updates, notify) = async_channel::bounded(1);
        let client = Self {
            shared: Arc::clone(&shared),
            sender: sender.clone(),
            notify,
        };
        std::thread::spawn(move || {
            let Ok(mut trace) = Trace::open(&options) else {
                shared.lock().unwrap().scene.status = "Cannot create probe trace";
                let _ = updates.try_send(());
                return;
            };
            for (name, at) in &options.milestones {
                trace.mark(name, *at, Value::Null);
            }
            trace.mark(
                "core_ready",
                ready,
                json!({ "editable": editable, "baseline": options.baseline }),
            );
            Supervisor::new(shared, sender, updates, trace, &options).run(receiver);
        });
        client
    }

    /// Publish new `projects` without waiting for a worker. Drafts remain acknowledged on the host.
    pub(crate) fn projects(&self, projects: Vec<Project>) {
        let mut shared = self.shared.lock().unwrap();
        if *shared.projects != projects {
            shared.projects = Arc::new(projects);
            drop(shared);
            let _ = self.sender.try_send(Message::Wake);
        }
    }

    /// Read the current view after an asynchronous notification.
    pub(crate) fn scene(&self) -> Scene {
        self.shared.lock().unwrap().scene.clone()
    }

    /// Get a receiver for the native view's lifetime-bound update task.
    pub(crate) fn updates(&self) -> async_channel::Receiver<()> {
        self.notify.clone()
    }

    /// Request an experiment `control` without blocking the event loop.
    pub(crate) fn control(&self, control: Control) {
        let _ = self.sender.try_send(Message::Control(control));
    }

    /// Queue an observable host timestamp with `detail`, avoiding synchronous file writes.
    pub(crate) fn mark(&self, name: &'static str, detail: Value) {
        let _ = self
            .sender
            .try_send(Message::Mark(name, Instant::now(), detail));
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.shared.lock().unwrap().stopped = true;
        let _ = self.sender.try_send(Message::Wake);
    }
}

/// Pipe connection identity used to discard stale messages after disconnection.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Peer {
    /// The shared rule process.
    Rules,
    /// One generation of the WASM process.
    Runtime(u64),
}

/// Messages consumed by the supervisor, never by the UI thread.
enum Message {
    /// Desired projects may have changed.
    Wake,
    /// One decoded message from the identified pipe connection.
    Worker(Peer, Event),
    /// A worker pipe has closed or become invalid.
    Disconnected(Peer),
    /// A host file read completed for a rule evaluation.
    Flag(u64, Result<String, String>),
    /// A native test command.
    Control(Control),
    /// A host timestamp for the measurement trace.
    Mark(&'static str, Instant, Value),
}

/// Child and bounded outbox. Only the writer thread can block on the child's stdin.
struct Process {
    /// Child handle owned and reaped by the supervisor.
    child: Child,
    /// Messages to write to the child's pipe.
    requests: Option<SyncSender<Request>>,
}

impl Process {
    /// Launch `peer` from the same build directory and relay its messages to `sender`.
    fn spawn(peer: Peer, sender: SyncSender<Message>) -> io::Result<Self> {
        let binary = std::env::current_exe()?.with_file_name(format!(
            "nanobug-probe-worker{}",
            std::env::consts::EXE_SUFFIX
        ));
        let mut command = Command::new(binary);
        command
            .arg(match peer {
                Peer::Rules => "rules",
                Peer::Runtime(_) => "wasm",
            })
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if matches!(peer, Peer::Runtime(generation) if generation > 1) {
            command.arg("--inject-retired-frame");
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut child = command.spawn()?;
        let mut stdin = child.stdin.take().expect("piped worker stdin");
        let mut stdout = BufReader::new(child.stdout.take().expect("piped worker stdout"));
        let (requests, queue) = mpsc::sync_channel(16);
        std::thread::spawn(move || {
            while let Ok(request) = queue.recv() {
                if send(&mut stdin, &request).is_err() {
                    break;
                }
            }
        });
        std::thread::spawn(move || {
            while let Ok(Some(event)) = receive(&mut stdout) {
                if sender.send(Message::Worker(peer, event)).is_err() {
                    return;
                }
            }
            let _ = sender.send(Message::Disconnected(peer));
        });
        Ok(Self {
            child,
            requests: Some(requests),
        })
    }

    /// Try to queue `request`. The supervisor can retry a desired-state operation later.
    fn post(&self, request: Request) -> bool {
        self.requests
            .as_ref()
            .is_some_and(|queue| queue.try_send(request).is_ok())
    }

    /// Close stdin after queued requests and give the worker a bounded time to exit.
    fn stop(&mut self) -> bool {
        self.requests.take();
        let deadline = Instant::now() + Duration::from_millis(250);
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => return status.success(),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(4))
                }
                _ => return false,
            }
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A project that has yet to be judged or has a current rule result.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Match {
    /// Eligible to send to the rules worker.
    Pending,
    /// Waiting for the rule or its file query to complete.
    Evaluating,
    /// Passed the condition.
    Yes,
    /// Returned false or an error.
    No,
}

/// Runtime bookkeeping for an opening. The input itself remains host-owned.
struct Scope {
    /// Current desired project snapshot.
    project: Project,
    /// Latest rule decision for this opening.
    condition: Match,
    /// Last draft revision sent in the current runtime generation.
    sent: Option<u64>,
}

/// Off-thread coordinator for all processes and state of one experiment.
struct Supervisor {
    /// Latest desired state and published view.
    shared: Arc<Mutex<Shared>>,
    /// Return path for I/O and worker threads.
    sender: SyncSender<Message>,
    /// Notifies the UI when any renderable state changes.
    updates: async_channel::Sender<()>,
    /// Trace written in supervisor order, with original timestamps.
    trace: Trace,
    /// No worker startup in the comparison run.
    baseline: bool,
    /// Fault injection for the first opening's data read.
    delay_first_read: bool,
    /// Last observed host snapshot, compared by shared ownership.
    snapshot: Arc<Vec<Project>>,
    /// Openings the supervisor still recognizes.
    scopes: BTreeMap<u64, Scope>,
    /// Closed scopes pending a drop acknowledgment, with a flag when already sent.
    closing: BTreeMap<u64, bool>,
    /// Active data reads, retained until the I/O thread reports actual completion.
    reads: BTreeMap<u64, Option<Result<String, String>>>,
    /// Accepted evaluations retain a lane until the worker reports their completion.
    evaluating: BTreeSet<u64>,
    /// Shared JS process for active projects.
    rules: Option<Process>,
    /// Single WASM process for all matching scopes.
    runtime: Option<Process>,
    /// Counter incremented on each WASM process spawn.
    generation: u64,
    /// Recovery attempts retained across a short-lived successful restart.
    attempts: VecDeque<Instant>,
    /// Earliest next startup after a process failure.
    restart_at: Option<Instant>,
    /// Terminal failure shown without an automatic start loop.
    paused: bool,
}

impl Supervisor {
    /// Prepare state for `shared`, the two event channels, and this run's trace.
    fn new(
        shared: Arc<Mutex<Shared>>,
        sender: SyncSender<Message>,
        updates: async_channel::Sender<()>,
        trace: Trace,
        options: &Options,
    ) -> Self {
        Self {
            shared,
            sender,
            updates,
            trace,
            baseline: options.baseline,
            delay_first_read: options.delay_first_read,
            snapshot: Arc::default(),
            scopes: BTreeMap::new(),
            closing: BTreeMap::new(),
            reads: BTreeMap::new(),
            evaluating: BTreeSet::new(),
            rules: None,
            runtime: None,
            generation: 0,
            attempts: VecDeque::new(),
            restart_at: None,
            paused: false,
        }
    }

    /// Process bounded messages and retry unsent snapshot updates until the UI exits.
    fn run(mut self, messages: Receiver<Message>) {
        loop {
            let (snapshot, stopped) = {
                let shared = self.shared.lock().unwrap();
                (Arc::clone(&shared.projects), shared.stopped)
            };
            if stopped {
                break;
            }
            if !self.baseline {
                self.reconcile(snapshot);
                self.schedule();
            }
            if let Ok(message) = messages.recv_timeout(Duration::from_millis(8)) {
                self.handle(message);
            }
        }
    }

    /// Apply a new desired `snapshot` and invalidate frames of closed project openings.
    fn reconcile(&mut self, snapshot: Arc<Vec<Project>>) {
        if Arc::ptr_eq(&self.snapshot, &snapshot) {
            return;
        }
        self.scopes.retain(|scope, state| {
            let retained = snapshot.iter().any(|project| project.scope == *scope);
            if !retained && state.sent.is_some() {
                self.closing.insert(*scope, false);
            }
            if !retained
                && self.evaluating.contains(scope)
                && let Some(rules) = &self.rules
            {
                rules.post(Request::FileResult {
                    id: *scope,
                    result: Err("project closed".into()),
                });
            }
            retained
        });
        for project in snapshot.iter() {
            self.scopes
                .entry(project.scope)
                .and_modify(|scope| scope.project = project.clone())
                .or_insert_with(|| Scope {
                    project: project.clone(),
                    condition: Match::Pending,
                    sent: None,
                });
        }
        self.snapshot = snapshot;
        let scopes = &self.scopes;
        self.publish(|scene| scene.surfaces.retain(|id, _| scopes.contains_key(id)));
    }

    /// Schedule rules, closed-scope drops, and current drafts on live connections.
    fn schedule(&mut self) {
        let need_rules = self
            .scopes
            .values()
            .any(|scope| scope.condition == Match::Pending);
        if need_rules && self.rules.is_none() {
            match Process::spawn(Peer::Rules, self.sender.clone()) {
                Ok(process) => {
                    self.mark("rules_started", json!({"pid": process.child.id()}));
                    self.rules = Some(process);
                    self.publish(|scene| scene.status = "Checking project rule");
                }
                Err(error) => {
                    self.mark("worker_error", json!({ "message": error.to_string() }));
                    self.publish(|scene| scene.status = "Cannot start rule worker");
                    for scope in self.scopes.values_mut() {
                        scope.condition = Match::No;
                    }
                }
            }
        }
        let mut active = self.rule_lanes_in_use();
        if let Some(rules) = &self.rules {
            for (id, scope) in &mut self.scopes {
                if active >= RULE_LANES {
                    break;
                }
                if scope.condition == Match::Pending
                    && rules.post(Request::Evaluate {
                        id: *id,
                        rule: Rule::Flag,
                    })
                {
                    scope.condition = Match::Evaluating;
                    self.evaluating.insert(*id);
                    active += 1;
                }
            }
            self.reads.retain(|id, result| {
                result.as_ref().is_none_or(|value| {
                    !rules.post(Request::FileResult {
                        id: *id,
                        result: value.clone(),
                    })
                })
            });
        }
        let needed = self
            .scopes
            .values()
            .any(|scope| scope.condition == Match::Yes);
        if !needed && self.closing.is_empty() {
            if let Some(mut runtime) = self.runtime.take() {
                let pid = runtime.child.id();
                let graceful = runtime.stop();
                drop(runtime);
                self.mark(
                    "runtime_stopped",
                    json!({ "pid": pid, "generation": self.generation, "graceful": graceful }),
                );
            }
            self.restart_at = None;
            return;
        }
        if needed
            && self.runtime.is_none()
            && !self.paused
            && self.restart_at.is_none_or(|at| Instant::now() >= at)
        {
            if self.restart_at.take().is_some() {
                self.attempts.push_back(Instant::now());
            }
            self.generation += 1;
            match Process::spawn(Peer::Runtime(self.generation), self.sender.clone()) {
                Ok(process) => {
                    self.mark(
                        "runtime_started",
                        json!({ "pid": process.child.id(), "generation": self.generation }),
                    );
                    self.runtime = Some(process);
                    self.publish(|scene| scene.status = "Preparing WASM surface");
                }
                Err(error) => {
                    self.mark("worker_error", json!({ "message": error.to_string() }));
                    self.paused = true;
                    self.publish(|scene| scene.status = "Cannot start WASM worker");
                }
            }
        }
        if let Some(runtime) = &self.runtime {
            let generation = self.generation;
            for (scope, sent) in &mut self.closing {
                if !*sent {
                    *sent = runtime.post(Request::Close {
                        owner: Owner {
                            scope: *scope,
                            generation,
                        },
                    });
                }
            }
            for (id, scope) in &mut self.scopes {
                if scope.condition != Match::Yes || scope.sent == Some(scope.project.draft.revision)
                {
                    continue;
                }
                let owner = Owner {
                    scope: *id,
                    generation,
                };
                let draft = scope.project.draft.clone();
                let request = if scope.sent.is_none() {
                    Request::Open { owner, draft }
                } else {
                    Request::Input { owner, draft }
                };
                if runtime.post(request) {
                    scope.sent = Some(scope.project.draft.revision);
                }
            }
        }
    }

    /// Dispatch one supervisor `message`, validating the connection before reading its payload.
    fn handle(&mut self, message: Message) {
        match message {
            Message::Wake => {}
            Message::Worker(Peer::Rules, event) if self.rules.is_some() => self.rule_event(event),
            Message::Worker(Peer::Runtime(generation), event)
                if self.runtime.is_some() && generation == self.generation =>
            {
                self.runtime_event(event)
            }
            Message::Disconnected(Peer::Rules) => {
                self.rules.take();
                self.evaluating.clear();
                for scope in self
                    .scopes
                    .values_mut()
                    .filter(|scope| scope.condition == Match::Evaluating)
                {
                    scope.condition = Match::No;
                }
                self.publish(|scene| scene.status = "Rule worker disconnected");
            }
            Message::Disconnected(Peer::Runtime(generation))
                if self.runtime.is_some() && generation == self.generation =>
            {
                self.recover()
            }
            Message::Flag(id, result) => {
                let waiting = self
                    .scopes
                    .get(&id)
                    .is_some_and(|scope| scope.condition == Match::Evaluating);
                self.mark(
                    "read_completed",
                    json!({ "scope": id, "expired": !waiting }),
                );
                if waiting {
                    self.reads.insert(id, Some(result));
                } else {
                    self.reads.remove(&id);
                }
            }
            Message::Mark(name, at, detail) => self.trace.mark(name, at, detail),
            Message::Control(control) => self.control(control),
            _ => {}
        }
    }

    /// Count unfinished evaluations and I/O still running after its evaluation ended.
    fn rule_lanes_in_use(&self) -> usize {
        self.evaluating.len()
            + self
                .reads
                .keys()
                .filter(|id| !self.evaluating.contains(id))
                .count()
    }

    /// Handle a host-data request or boolean result from the shared rules process.
    fn rule_event(&mut self, event: Event) {
        match event {
            Event::ReadFlag { id } => {
                let Some(scope) = self.scopes.get(&id) else {
                    return;
                };
                if scope.condition != Match::Evaluating || self.reads.contains_key(&id) {
                    return;
                }
                if self.reads.len() >= RULE_LANES {
                    if let Some(rules) = &self.rules {
                        rules.post(Request::FileResult {
                            id,
                            result: Err("host read lanes are occupied".into()),
                        });
                    }
                    return;
                }
                let path = scope.project.root.join("probe-enabled.txt");
                let sender = self.sender.clone();
                self.reads.insert(id, None);
                let delayed = self.delay_first_read && id == 1;
                self.mark("read_started", json!({ "scope": id, "delayed": delayed }));
                std::thread::spawn(move || {
                    if delayed {
                        std::thread::sleep(Duration::from_millis(5500));
                    }
                    let _ = sender.send(Message::Flag(
                        id,
                        read_flag(&path).map_err(|error| error.to_string()),
                    ));
                });
            }
            Event::RuleDone {
                id,
                result,
                js_us,
                total_us,
            } => {
                if !self.evaluating.remove(&id) {
                    return;
                }
                self.mark(
                    "rule_done",
                    json!({ "scope": id, "result": result, "js_us": js_us, "total_us": total_us, "read_active": self.reads.get(&id).is_some_and(Option::is_none) }),
                );
                if let Some(scope) = self.scopes.get_mut(&id) {
                    scope.condition = match result {
                        Ok(true) => Match::Yes,
                        Err(Failure::Busy) => Match::Pending,
                        _ => Match::No,
                    };
                }
                if !self
                    .scopes
                    .values()
                    .any(|scope| scope.condition != Match::No)
                {
                    self.publish(|scene| scene.status = "No matching project rule");
                }
            }
            _ => {}
        }
    }

    /// Handle a complete frame, release acknowledgment, or business result from the active runtime.
    fn runtime_event(&mut self, event: Event) {
        match event {
            Event::Frame {
                owner,
                draft,
                title,
                color,
                instances,
            } => {
                if owner.generation != self.generation {
                    self.mark("frame_discarded", json!({ "scope": owner.scope, "generation": owner.generation, "reason": "retired_generation" }));
                    return;
                }
                // Consult the latest UI snapshot even if the supervisor has not reconciled it yet.
                let current = self
                    .shared
                    .lock()
                    .unwrap()
                    .projects
                    .iter()
                    .any(|project| project.scope == owner.scope && project.draft == draft);
                if !current {
                    return;
                }
                let first = !self
                    .shared
                    .lock()
                    .unwrap()
                    .scene
                    .surfaces
                    .contains_key(&owner.scope);
                self.mark("guest_frame", json!({ "scope": owner.scope, "generation": owner.generation, "revision": draft.revision, "bytes": draft.text.len(), "instances": instances }));
                self.publish(|scene| {
                    scene.status = "Ready";
                    scene.surfaces.insert(
                        owner.scope,
                        Surface {
                            owner,
                            draft,
                            title,
                            color,
                        },
                    );
                });
                if first && let Some(runtime) = &self.runtime {
                    runtime.post(Request::Work { owner, spin: false });
                }
            }
            Event::Closed { owner, instances } if owner.generation == self.generation => {
                self.closing.remove(&owner.scope);
                self.mark("scope_closed", json!({ "scope": owner.scope, "generation": owner.generation, "instances": instances }));
            }
            Event::WorkStarted { owner }
                if owner.generation == self.generation
                    && self.scopes.contains_key(&owner.scope) =>
            {
                self.mark(
                    "business_started",
                    json!({ "scope": owner.scope, "generation": owner.generation }),
                );
            }
            Event::WorkDone {
                owner,
                spin,
                result,
                elapsed_us,
            } if owner.generation == self.generation && self.scopes.contains_key(&owner.scope) => {
                self.mark("business_result", json!({ "scope": owner.scope, "generation": owner.generation, "spin": spin, "result": result, "elapsed_us": elapsed_us }));
            }
            Event::ScopeError { owner, message } => {
                self.closing.remove(&owner.scope);
                self.mark(
                    "worker_error",
                    json!({ "scope": owner.scope, "message": message }),
                );
                self.publish(|scene| scene.status = "Sample worker error");
            }
            _ => {}
        }
    }

    /// Invalidate the dead generation and schedule at most three attempts in five minutes.
    fn recover(&mut self) {
        let Some(runtime) = self.runtime.take() else {
            return;
        };
        let pid = runtime.child.id();
        drop(runtime);
        self.mark(
            "runtime_exited",
            json!({ "pid": pid, "generation": self.generation }),
        );
        self.closing.clear();
        for scope in self.scopes.values_mut() {
            scope.sent = None;
        }
        self.attempts
            .retain(|attempt| attempt.elapsed() < Duration::from_secs(300));
        self.restart_at = [1, 3, 10]
            .get(self.attempts.len())
            .map(|&seconds| Instant::now() + Duration::from_secs(seconds));
        self.paused = self.restart_at.is_none();
        let status = if self.paused {
            "Recovery paused"
        } else {
            "Recovering plugin"
        };
        self.publish(|scene| {
            scene.surfaces.clear();
            scene.status = status;
        });
    }

    /// Run a test-only `control` without touching editor text or configuration.
    fn control(&mut self, control: Control) {
        match control {
            Control::Terminate => self.recover(),
            Control::Work(scope) => {
                if let Some(runtime) = &self.runtime {
                    runtime.post(Request::Work {
                        owner: Owner {
                            scope,
                            generation: self.generation,
                        },
                        spin: true,
                    });
                }
            }
            Control::BusyRule => {
                if self.rule_lanes_in_use() < RULE_LANES
                    && !self.evaluating.contains(&0)
                    && let Some(rules) = &self.rules
                    && rules.post(Request::Evaluate {
                        id: 0,
                        rule: Rule::Busy,
                    })
                {
                    self.evaluating.insert(0);
                }
            }
        }
    }

    /// Change the latest scene and wake the native view at most once per pending notification.
    fn publish(&self, update: impl FnOnce(&mut Scene)) {
        update(&mut self.shared.lock().unwrap().scene);
        let _ = self.updates.try_send(());
    }

    /// Record a supervisor event with metadata `detail` at the current time.
    fn mark(&mut self, name: &'static str, detail: Value) {
        self.trace.mark(name, Instant::now(), detail);
    }
}

/// Read the fixture's `path` on an I/O thread, limiting returned bytes and rejecting special files.
fn read_flag(path: &Path) -> io::Result<String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "probe flag is not a regular file",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(String::new()),
        Err(error) => return Err(error),
    }
    let mut text = String::new();
    File::open(path)?
        .take((MAX_FLAG + 1) as u64)
        .read_to_string(&mut text)?;
    if text.len() > MAX_FLAG {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "probe flag exceeds its result budget",
        ));
    }
    Ok(text)
}
