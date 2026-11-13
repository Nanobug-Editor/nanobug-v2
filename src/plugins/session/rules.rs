//! Coalesced rule scheduling, asynchronous data requests, and dependency validation.

use super::{
    Key, Project, State,
    engine::{Backoff, Pool},
};
use crate::plugins::{
    package::{ApprovedRulePaths, InstallStore, Release, read_limited},
    pipe::Worker,
    query::{self, ContextData, Dependency},
};
use nanobug_plugin_sdk::{ErrorKind, Permission, Reply, Request};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

const CONCURRENT: usize = 4;
const DEADLINE: Duration = Duration::from_secs(5);
const CHECK_INTERVAL: Duration = Duration::from_millis(350);
const SOURCE_LIMIT: u64 = 128 * 1024;

/// A configured rule. Package paths are verified against the installed release before reading.
#[derive(Clone)]
pub(super) enum Source {
    Package(Arc<Release>, String),
    Inline(String),
}

impl Source {
    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Package(a, path), Self::Package(b, other)) => {
                a.digest == b.digest && path == other
            }
            (Self::Inline(a), Self::Inline(b)) => a == b,
            _ => false,
        }
    }

    fn read(self, store: &InstallStore) -> anyhow::Result<String> {
        match self {
            Self::Package(release, path) => Ok(String::from_utf8(read_limited(
                &store.verified_asset(&release, &path)?,
                SOURCE_LIMIT,
            )?)?),
            Self::Inline(source) => Ok(source),
        }
    }
}

struct Entry {
    source: Source,
    grants: BTreeSet<Permission>,
    rule_paths: ApprovedRulePaths,
    allowed: Option<bool>,
    state: State,
    deps: Vec<Dependency>,
    dirty: Option<Instant>,
    in_flight: Option<u64>,
    compute_failures: u8,
    next_check: Instant,
    checking_files: Option<u64>,
}

impl Entry {
    fn invalidate(&mut self) {
        if self.compute_failures < 3 {
            self.dirty.get_or_insert_with(Instant::now);
        }
    }

    fn data_changed(&mut self) {
        self.allowed = None;
        if self.compute_failures < 3 {
            self.state = State::Checking;
            self.invalidate();
        }
    }
}

struct Evaluation {
    key: Key,
    input: ContextData,
    deps: Vec<Dependency>,
    sent: bool,
    cancel_sent: bool,
    pending_queries: usize,
    responses: VecDeque<Request>,
    judgment: Option<bool>,
    validating: bool,
}

impl Evaluation {
    fn new(key: Key, input: ContextData) -> Self {
        Self {
            key,
            input,
            deps: Vec::new(),
            sent: false,
            cancel_sent: false,
            pending_queries: 0,
            responses: VecDeque::new(),
            judgment: None,
            validating: false,
        }
    }
}

type Data = (Result<Value, String>, Option<Dependency>);

enum ResultEvent {
    Started(anyhow::Result<Worker>),
    Source(u64, anyhow::Result<String>),
    Data(u64, u32, Data),
    Validate(u64, bool, bool),
    Change(Key, u64, Vec<Dependency>, bool),
}

/// One rule process shared by all projects. Keys have at most one running evaluation.
pub(super) struct Rules {
    store: InstallStore,
    entries: BTreeMap<Key, Entry>,
    running: BTreeMap<u64, Evaluation>,
    worker: Option<Worker>,
    starting: bool,
    restart: Backoff,
    results: mpsc::Receiver<ResultEvent>,
    finish: mpsc::Sender<ResultEvent>,
    serial: u64,
    active: Option<u64>,
    foreground_turn: bool,
}

impl Rules {
    pub(super) fn new(store: InstallStore) -> Self {
        let (finish, results) = mpsc::channel();
        Self {
            store,
            entries: BTreeMap::new(),
            running: BTreeMap::new(),
            worker: None,
            starting: false,
            restart: Backoff::default(),
            results,
            finish,
            serial: 0,
            active: None,
            foreground_turn: true,
        }
    }

    /// Keep installed rules for live scopes. A changed source gets a fresh budget history.
    #[cfg(test)]
    pub(super) fn set(&mut self, key: Key, source: Source, grants: &BTreeSet<Permission>) {
        self.set_approved(key, source, grants, &ApprovedRulePaths::default());
    }

    pub(super) fn set_approved(
        &mut self,
        key: Key,
        source: Source,
        grants: &BTreeSet<Permission>,
        rule_paths: &ApprovedRulePaths,
    ) {
        if self.entries.get(&key).is_some_and(|entry| {
            entry.source.same(&source) && &entry.grants == grants && &entry.rule_paths == rule_paths
        }) {
            return;
        }
        let now = Instant::now();
        self.entries.insert(
            key,
            Entry {
                source,
                grants: grants.clone(),
                rule_paths: rule_paths.clone(),
                allowed: None,
                state: State::Checking,
                deps: Vec::new(),
                dirty: Some(now),
                in_flight: None,
                compute_failures: 0,
                next_check: now,
                checking_files: None,
            },
        );
    }

    pub(super) fn retain(&mut self, wanted: &BTreeSet<Key>) {
        self.entries.retain(|key, _| wanted.contains(key));
    }

    pub(super) fn state(&self, key: &Key) -> (bool, State) {
        self.entries
            .get(key)
            .map_or((false, State::Checking), |entry| {
                (entry.allowed.unwrap_or(false), entry.state.clone())
            })
    }

    pub(super) fn retry(&mut self, namespace: &str) {
        self.restart = Backoff::default();
        for (key, entry) in &mut self.entries {
            if key.namespace == namespace {
                entry.compute_failures = 0;
                entry.invalidate();
            }
        }
    }

    /// Faster polling is useful only during an evaluation or its pending launch.
    pub(super) fn working(&self, now: Instant) -> bool {
        !self.running.is_empty()
            || self.restart.ready(now) && self.entries.values().any(|entry| entry.dirty.is_some())
    }

    /// Poll already completed work, then schedule fairly without building a backlog.
    pub(super) fn tick(
        &mut self,
        active: Option<u64>,
        projects: &BTreeMap<u64, Project>,
        pool: &Pool,
    ) {
        let now = Instant::now();
        if self.active != active {
            self.active = active;
            self.foreground_turn = true;
        }
        for (&id, job) in &mut self.running {
            if self
                .entries
                .get(&job.key)
                .is_none_or(|entry| entry.in_flight != Some(id))
            {
                job.input.cancelled.store(true, Ordering::Release);
                if job.sent && !job.cancel_sent {
                    job.cancel_sent = self
                        .worker
                        .as_ref()
                        .is_some_and(|worker| worker.post(Request::CancelRule { id }).is_ok());
                }
            }
        }
        while let Ok(event) = self.results.try_recv() {
            self.finish_event(event, projects);
        }
        for _ in 0..64 {
            let Some(worker) = &self.worker else { break };
            match worker.poll() {
                Ok(Some(reply)) => self.reply(reply, pool),
                Ok(None) => break,
                Err(_) => {
                    self.lost();
                    break;
                }
            }
        }
        let cancelled = self
            .running
            .iter()
            .filter_map(|(&id, job)| {
                (!job.sent && job.input.cancelled.load(Ordering::Acquire)).then_some(id)
            })
            .collect::<Vec<_>>();
        for id in cancelled {
            self.reschedule(id);
        }
        self.advance_queries(pool);
        let expired = self
            .running
            .iter()
            .filter(|(_, job)| now >= job.input.deadline + Duration::from_millis(150))
            .map(|(&id, job)| (id, job.sent, job.judgment.is_some()))
            .collect::<Vec<_>>();
        if expired.iter().any(|&(_, sent, _)| sent) {
            self.lost();
        } else {
            for (id, _, judging) in expired {
                self.failed(
                    id,
                    ErrorKind::Deadline,
                    if judging {
                        "Rule validation exceeded five seconds"
                    } else {
                        "Rule preparation exceeded five seconds"
                    }
                    .into(),
                );
            }
        }
        if self.entries.is_empty() && self.running.is_empty() {
            self.worker.take();
            return;
        }
        for (key, entry) in &mut self.entries {
            let project = projects.get(&key.instance.project);
            if (entry.in_flight.is_none() || entry.allowed.is_some())
                && !query::document_current(&entry.deps, project)
            {
                entry.data_changed();
            }
            if now < entry.next_check
                || entry.checking_files.is_some()
                || entry.allowed.is_none() && (entry.in_flight.is_some() || entry.dirty.is_some())
                || entry.compute_failures >= 3 && entry.allowed.is_none()
                || !entry
                    .deps
                    .iter()
                    .any(|dep| matches!(dep, Dependency::File(..)))
            {
                continue;
            }
            let deps = entry.deps.clone();
            let key = key.clone();
            let finish = self.finish.clone();
            self.serial += 1;
            let id = self.serial;
            if !pool.submit(move || {
                let current = query::files_current(&deps);
                let _ = finish.send(ResultEvent::Change(key, id, deps, current));
            }) {
                break;
            }
            entry.checking_files = Some(id);
            entry.next_check = now + CHECK_INTERVAL;
        }
        let mut waiting = self
            .entries
            .iter()
            .filter_map(|(key, entry)| {
                entry
                    .dirty
                    .filter(|_| {
                        entry.in_flight.is_none()
                            && !self.running.values().any(|job| &job.key == key)
                    })
                    .map(|at| (at, key.clone()))
            })
            .collect::<Vec<_>>();
        if waiting.is_empty() || !self.restart.ready(now) {
            return;
        }
        if self.worker.is_none() {
            if !self.starting {
                let finish = self.finish.clone();
                let supervisor = std::thread::current();
                self.starting = pool.submit(move || {
                    let result = Worker::start_with_wake("rules", None, Some(supervisor));
                    let _ = finish.send(ResultEvent::Started(result));
                });
            }
            return;
        }
        waiting.sort_unstable();
        while !waiting.is_empty() && self.running.len() < CONCURRENT {
            let index = next_waiting(&waiting, active, self.foreground_turn);
            let (_, key) = waiting.remove(index);
            let Some(project) = projects.get(&key.instance.project) else {
                continue;
            };
            let entry = self.entries.get_mut(&key).expect("scheduled rule");
            let source = entry.source.clone();
            let store = self.store.clone();
            let finish = self.finish.clone();
            self.serial += 1;
            let id = self.serial;
            if !pool.submit(move || {
                let _ = finish.send(ResultEvent::Source(id, source.read(&store)));
            }) {
                break;
            }
            self.foreground_turn = Some(key.instance.project) != active;
            entry.dirty = None;
            entry.in_flight = Some(id);
            if entry.allowed.is_none() {
                entry.state = State::Checking;
            }
            self.running.insert(
                id,
                Evaluation::new(
                    key,
                    ContextData {
                        root: project.root.clone(),
                        grants: entry.grants.clone(),
                        rule_paths: entry.rule_paths.clone(),
                        document: project.document.clone(),
                        open_documents: project.open_documents.clone(),
                        deadline: Instant::now() + DEADLINE,
                        cancelled: Arc::new(AtomicBool::new(false)),
                    },
                ),
            );
        }
    }

    fn reply(&mut self, reply: Reply, pool: &Pool) {
        match reply {
            Reply::Query {
                id,
                query: number,
                kind,
                args,
            } => {
                let Some(job) = self
                    .running
                    .get_mut(&id)
                    .filter(|job| job.sent && !job.input.cancelled.load(Ordering::Acquire))
                else {
                    return;
                };
                let context = job.input.clone();
                let finish = self.finish.clone();
                if pool.submit(move || {
                    let _ = finish.send(ResultEvent::Data(
                        id,
                        number,
                        query::run(&context, kind, &args),
                    ));
                }) {
                    job.pending_queries += 1;
                } else {
                    job.responses.push_back(Request::Data {
                        id,
                        query: number,
                        result: Err("Host data queue is busy".into()),
                    });
                }
            }
            Reply::Rule { id, matches } => {
                let Some(job) = self.running.get_mut(&id) else {
                    return;
                };
                job.sent = false;
                job.responses.clear();
                job.judgment = Some(matches);
            }
            Reply::Error { id, kind, message } => self.failed(id, kind, message),
            _ => self.lost(),
        }
    }

    fn finish_event(&mut self, event: ResultEvent, projects: &BTreeMap<u64, Project>) {
        match event {
            ResultEvent::Started(result) => {
                self.starting = false;
                if self.entries.is_empty() {
                    return;
                }
                match result {
                    Ok(worker) => self.worker = Some(worker),
                    Err(error) => {
                        self.lost();
                        for entry in self
                            .entries
                            .values_mut()
                            .filter(|entry| entry.compute_failures < 3)
                        {
                            entry.state =
                                State::Failed(format!("Rule worker unavailable: {error}"));
                        }
                    }
                }
            }
            ResultEvent::Source(id, source) => {
                let Some(job) = self.running.get_mut(&id) else {
                    return;
                };
                if job.input.cancelled.load(Ordering::Acquire) {
                    self.reschedule(id);
                    return;
                }
                let remaining = job.input.deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    self.failed(
                        id,
                        ErrorKind::Deadline,
                        "Rule preparation exceeded five seconds".into(),
                    );
                    return;
                }
                let source = match source {
                    Ok(source) => source,
                    Err(error) => {
                        self.failed(id, ErrorKind::Invalid, error.to_string());
                        return;
                    }
                };
                let context =
                    json!({ "project": job.input.root, "platform": std::env::consts::OS });
                if self.worker.as_ref().is_some_and(|worker| {
                    worker
                        .post(Request::Evaluate {
                            id,
                            source,
                            context,
                            remaining_ms: remaining.as_millis() as u64,
                        })
                        .is_ok()
                }) {
                    job.sent = true;
                } else {
                    self.reschedule(id);
                }
            }
            ResultEvent::Data(id, number, (result, dependency)) => {
                if let Some(job) = self
                    .running
                    .get_mut(&id)
                    .filter(|job| !job.input.cancelled.load(Ordering::Acquire))
                {
                    job.pending_queries -= 1;
                    job.deps.extend(dependency);
                    if job.sent {
                        job.responses.push_back(Request::Data {
                            id,
                            query: number,
                            result,
                        });
                    }
                }
            }
            ResultEvent::Validate(id, matches, files_current) => {
                if self
                    .running
                    .get(&id)
                    .is_some_and(|job| Instant::now() >= job.input.deadline)
                {
                    self.failed(
                        id,
                        ErrorKind::Deadline,
                        "Rule validation exceeded five seconds".into(),
                    );
                    return;
                }
                let Some(job) = self.running.remove(&id) else {
                    return;
                };
                let project = projects.get(&job.key.instance.project);
                let Some(entry) = self
                    .entries
                    .get_mut(&job.key)
                    .filter(|entry| entry.in_flight == Some(id))
                else {
                    return;
                };
                entry.in_flight = None;
                if files_current
                    && query::document_current(&job.deps, project)
                    && entry.dirty.is_none()
                {
                    entry.allowed = Some(matches);
                    entry.state = if matches {
                        State::Ready
                    } else {
                        State::Unmatched
                    };
                    entry.compute_failures = 0;
                    entry.deps = job.deps;
                } else {
                    entry.data_changed();
                }
            }
            ResultEvent::Change(key, id, deps, current) => {
                if let Some(entry) = self
                    .entries
                    .get_mut(&key)
                    .filter(|entry| entry.checking_files == Some(id))
                {
                    entry.checking_files = None;
                    if !current && entry.deps == deps {
                        entry.data_changed();
                    }
                }
            }
        }
    }

    /// Retain backpressured responses and wait for every issued read before publishing a judgment.
    fn advance_queries(&mut self, pool: &Pool) {
        let Some(worker) = &self.worker else { return };
        for (&id, job) in &mut self.running {
            if job.input.cancelled.load(Ordering::Acquire) || Instant::now() >= job.input.deadline {
                continue;
            }
            while let Some(request) = job.responses.front_mut() {
                match worker.post(&*request) {
                    Ok(()) => {
                        job.responses.pop_front();
                    }
                    Err(error)
                        if error
                            .downcast_ref::<io::Error>()
                            .is_some_and(|cause| cause.kind() == io::ErrorKind::InvalidData) =>
                    {
                        let Request::Data { result, .. } = request else {
                            unreachable!("queued data response")
                        };
                        *result = Err("Rule query result exceeds the protocol limit".into());
                    }
                    Err(_) => break,
                }
            }
            if job.pending_queries == 0
                && !job.validating
                && let Some(matches) = job.judgment
            {
                let deps = job.deps.clone();
                let finish = self.finish.clone();
                job.validating = pool.submit(move || {
                    let _ = finish.send(ResultEvent::Validate(
                        id,
                        matches,
                        query::files_current(&deps),
                    ));
                });
            }
        }
    }

    fn reschedule(&mut self, id: u64) {
        if let Some(job) = self.running.remove(&id)
            && let Some(entry) = self
                .entries
                .get_mut(&job.key)
                .filter(|entry| entry.in_flight == Some(id))
        {
            entry.in_flight = None;
            entry.invalidate();
        }
    }

    fn failed(&mut self, id: u64, kind: ErrorKind, message: String) {
        if kind == ErrorKind::Busy {
            self.reschedule(id);
            return;
        }
        let Some(job) = self.running.remove(&id) else {
            return;
        };
        let Some(entry) = self
            .entries
            .get_mut(&job.key)
            .filter(|entry| entry.in_flight == Some(id))
        else {
            return;
        };
        entry.in_flight = None;
        // Keep a still-valid previous result while reporting the failed new judgment.
        if entry.allowed.is_none() {
            entry.deps = job.deps;
        }
        entry.state = State::Failed(message);
        if kind == ErrorKind::Computation {
            entry.compute_failures += 1;
            if entry.compute_failures < 3 {
                entry.invalidate();
            } else {
                entry.state = State::Failed("Rule paused after three computation limits".into());
            }
        }
    }

    fn lost(&mut self) {
        self.worker.take();
        self.restart.failed(Instant::now());
        for (id, job) in std::mem::take(&mut self.running) {
            job.input.cancelled.store(true, Ordering::Release);
            if let Some(entry) = self
                .entries
                .get_mut(&job.key)
                .filter(|entry| entry.in_flight == Some(id))
            {
                entry.in_flight = None;
            }
        }
        for entry in self
            .entries
            .values_mut()
            .filter(|entry| entry.compute_failures < 3)
        {
            entry.invalidate();
            entry.state = if self.restart.paused() {
                State::Failed("Rule worker paused. Select Retry to resume.".into())
            } else {
                State::Recovering
            };
        }
    }
}

/// The focused project gets first admission and alternates with oldest background work under load.
fn next_waiting(waiting: &[(Instant, Key)], active: Option<u64>, foreground: bool) -> usize {
    active
        .and_then(|active| {
            waiting
                .iter()
                .position(|(_, key)| (key.instance.project == active) == foreground)
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nanobug_plugin_sdk::Instance;
    use std::{fs, path::Path};

    fn scope(id: u64) -> Key {
        Key {
            namespace: "fixture".into(),
            instance: Instance {
                project: id,
                module: "module".into(),
            },
        }
    }

    fn projects(root: &Path) -> anyhow::Result<BTreeMap<u64, Project>> {
        (1..=2)
            .map(|id| {
                let root = root.join(id.to_string());
                fs::create_dir(&root)?;
                Ok((
                    id,
                    Project {
                        id,
                        root,
                        document: None,
                        open_documents: Vec::new(),
                    },
                ))
            })
            .collect()
    }

    fn tick_until(
        rules: &mut Rules,
        pool: &Pool,
        projects: &BTreeMap<u64, Project>,
        mut ready: impl FnMut(&Rules) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            rules.tick(Some(1), projects, pool);
            if ready(rules) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "first={:?}, second={:?}",
                rules.state(&scope(1)),
                rules.state(&scope(2))
            );
            std::thread::park_timeout(Duration::from_millis(5));
        }
    }

    #[test]
    fn focus_gets_first_then_alternates_with_oldest_background_work() {
        let now = Instant::now();
        let mut waiting = vec![
            (now, scope(2)),
            (now, scope(3)),
            (now, scope(1)),
            (now, scope(1)),
        ];
        let active = Some(1);
        let mut foreground = true;
        let mut ordered = Vec::new();
        while !waiting.is_empty() {
            let (_, key) = waiting.remove(next_waiting(&waiting, active, foreground));
            foreground = Some(key.instance.project) != active;
            ordered.push(key.instance.project);
        }
        assert_eq!(ordered, [1, 2, 1, 3]);
    }

    #[test]
    fn only_compute_limits_increment_the_counter_and_success_clears_it() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let projects = projects(temp.path())?;
        let first = scope(1);
        let mut rules = Rules::new(InstallStore::new(temp.path().join("profile")));
        rules.set(
            first.clone(),
            Source::Inline("return true;".into()),
            &BTreeSet::new(),
        );
        let running = |rules: &mut Rules, id| {
            let entry = rules.entries.get_mut(&first).unwrap();
            entry.in_flight = Some(id);
            entry.dirty = None;
            rules.running.insert(
                id,
                Evaluation::new(
                    first.clone(),
                    ContextData {
                        root: projects[&1].root.clone(),
                        grants: BTreeSet::new(),
                        rule_paths: ApprovedRulePaths::default(),
                        document: None,
                        open_documents: Vec::new(),
                        deadline: Instant::now() + DEADLINE,
                        cancelled: Arc::new(AtomicBool::new(false)),
                    },
                ),
            );
        };
        for (id, kind, count) in [
            (1, ErrorKind::Computation, 1),
            (2, ErrorKind::Invalid, 1),
            (3, ErrorKind::Deadline, 1),
            (4, ErrorKind::Computation, 2),
        ] {
            running(&mut rules, id);
            rules.failed(id, kind, "fixture error".into());
            assert_eq!(rules.entries[&first].compute_failures, count);
        }
        running(&mut rules, 5);
        rules.finish_event(ResultEvent::Validate(5, true, true), &projects);
        assert_eq!(rules.entries[&first].compute_failures, 0);
        assert_eq!(rules.state(&first), (true, State::Ready));
        running(&mut rules, 6);
        rules.failed(6, ErrorKind::Deadline, "fixture deadline".into());
        assert_eq!(
            rules.state(&first),
            (true, State::Failed("fixture deadline".into()))
        );
        Ok(())
    }

    #[test]
    #[ignore = "requires the bundled rule worker executable next to the test build"]
    fn validation_rejects_a_changed_read_and_only_rechecks_current_branches() -> anyhow::Result<()>
    {
        let temp = tempfile::tempdir()?;
        let projects = projects(temp.path())?;
        let pool = Pool::new(3, 16);
        let mut rules = Rules::new(InstallStore::new(temp.path().join("profile")));
        for project in projects.values() {
            for branch in ["a", "b"] {
                fs::create_dir(project.root.join(branch))?;
                fs::write(project.root.join(branch).join("ok"), "")?;
            }
            rules.set(
                scope(project.id),
                Source::Inline(
                    r#"
                const branch = await host.exists('alternate') ? 'b' : 'a';
                const {entries} = await host.list(branch);
                return entries.includes('ok');
            "#
                    .into(),
                ),
                &[Permission::ReadProject].into(),
            );
        }
        let first = scope(1);
        let second = scope(2);
        let root = &projects[&1].root;
        tick_until(&mut rules, &pool, &projects, |rules| {
            rules
                .running
                .values()
                .any(|job| job.key == first && job.deps.len() == 2 && job.judgment.is_none())
        });
        // The directory was read before this change and the host has not yet accepted a verdict.
        fs::remove_file(root.join("a/ok"))?;
        tick_until(&mut rules, &pool, &projects, |rules| {
            assert!(!rules.state(&first).0, "published a stale directory result");
            rules.state(&first).1 == State::Unmatched && rules.state(&second).0
        });

        fs::write(root.join("alternate"), "")?;
        tick_until(&mut rules, &pool, &projects, |rules| rules.state(&first).0);
        fs::write(root.join("a/ok"), "")?;
        let observed_until = Instant::now() + CHECK_INTERVAL * 2;
        tick_until(&mut rules, &pool, &projects, |rules| {
            assert!(
                rules.running.is_empty(),
                "unrelated branch or scope was re-evaluated"
            );
            Instant::now() >= observed_until
        });
        fs::remove_file(root.join("b/ok"))?;
        tick_until(&mut rules, &pool, &projects, |rules| {
            assert!(rules.state(&second).0 && rules.running.values().all(|job| job.key != second));
            rules.state(&first).1 == State::Unmatched
        });
        let grants = [Permission::ReadProject].into();
        rules.set(
            first.clone(),
            Source::Inline("await host.exists('a'); return true;".into()),
            &grants,
        );
        tick_until(&mut rules, &pool, &projects, |rules| {
            rules
                .running
                .values()
                .any(|job| job.key == first && !job.deps.is_empty())
        });
        for source in ["return true;", "return true && true;", "return false;"] {
            rules.set(first.clone(), Source::Inline(source.into()), &grants);
        }
        tick_until(&mut rules, &pool, &projects, |rules| {
            assert!(
                rules
                    .running
                    .values()
                    .filter(|job| job.key == first)
                    .count()
                    <= 1
            );
            assert!(
                !rules.state(&first).0,
                "published a superseded configuration"
            );
            rules.state(&first).1 == State::Unmatched && rules.entries[&first].deps.is_empty()
        });
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires the bundled rule worker executable next to the test build"]
    fn a_judgment_waits_for_issued_reads_and_access_recovery_rechecks() -> anyhow::Result<()> {
        use std::os::windows::fs::OpenOptionsExt;

        let temp = tempfile::tempdir()?;
        let projects = projects(temp.path())?;
        let pool = Pool::new(3, 16);
        let mut rules = Rules::new(InstallStore::new(temp.path().join("profile")));
        let first = scope(1);
        let root = &projects[&1].root;
        let file = root.join("blocked");
        fs::write(&file, "ready")?;
        let lock = || fs::OpenOptions::new().write(true).share_mode(0).open(&file);
        let held = lock()?;
        rules.set(
            first.clone(),
            Source::Inline(
                r#"
            void host.readFile('blocked').catch(() => null);
            await host.exists('quick');
            return true;
        "#
                .into(),
            ),
            &[Permission::ReadProject].into(),
        );
        tick_until(&mut rules, &pool, &projects, |rules| {
            assert!(!rules.state(&first).0);
            rules
                .running
                .values()
                .any(|job| job.judgment == Some(true) && job.pending_queries == 1)
        });
        drop(held);
        tick_until(&mut rules, &pool, &projects, |rules| rules.state(&first).0);

        let held = lock()?;
        rules.set(
            first.clone(),
            Source::Inline(
                r#"
            return await host.readFile('blocked').then(file => file?.text === 'ready', () => false);
        "#
                .into(),
            ),
            &[Permission::ReadProject].into(),
        );
        tick_until(&mut rules, &pool, &projects, |rules| {
            rules.state(&first).1 == State::Unmatched
        });
        drop(held);
        tick_until(&mut rules, &pool, &projects, |rules| rules.state(&first).0);
        Ok(())
    }

    #[test]
    #[ignore = "requires the bundled rule worker executable next to the test build"]
    fn rule_worker_restart_keeps_only_current_matches_and_does_not_clear_compute_pause()
    -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let projects = projects(temp.path())?;
        let pool = Pool::new(3, 16);
        let mut rules = Rules::new(InstallStore::new(temp.path().join("profile")));
        let first = scope(1);
        let second = scope(2);
        let path = projects[&1].root.join("match");
        fs::write(&path, "")?;
        rules.set(
            first.clone(),
            Source::Inline("return await host.exists('match');".into()),
            &[Permission::ReadProject].into(),
        );
        rules.set(
            second.clone(),
            Source::Inline("while (true) {}".into()),
            &BTreeSet::new(),
        );
        let paused = |rules: &Rules| matches!(rules.state(&second).1, State::Failed(reason) if reason.contains("three computation limits"));
        tick_until(&mut rules, &pool, &projects, |rules| {
            rules.state(&first).0 && paused(rules)
        });
        let worker = rules.worker.as_mut().unwrap();
        let pid = worker.pid();
        worker.kill_for_test()?;
        tick_until(&mut rules, &pool, &projects, |rules| {
            rules.state(&first) == (true, State::Recovering) && paused(rules)
        });
        fs::remove_file(&path)?;
        tick_until(&mut rules, &pool, &projects, |rules| {
            !rules.state(&first).0 && paused(rules)
        });
        tick_until(&mut rules, &pool, &projects, |rules| {
            rules.state(&first).1 == State::Unmatched
                && paused(rules)
                && rules
                    .worker
                    .as_ref()
                    .is_some_and(|worker| worker.pid() != pid)
        });
        Ok(())
    }

    #[test]
    fn a_paused_rule_invalidates_its_previous_match_without_restarting() {
        let mut entry = Entry {
            source: Source::Inline("return true;".into()),
            grants: BTreeSet::new(),
            rule_paths: ApprovedRulePaths::default(),
            allowed: Some(true),
            state: State::Failed("Rule paused".into()),
            deps: Vec::new(),
            dirty: None,
            in_flight: None,
            compute_failures: 3,
            next_check: Instant::now(),
            checking_files: None,
        };
        entry.data_changed();
        assert_eq!(entry.allowed, None);
        assert_eq!(entry.state, State::Failed("Rule paused".into()));
        assert!(entry.dirty.is_none());
    }
}
