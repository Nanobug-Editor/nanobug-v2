//! Link native tool work to the installed plugin, project, and current module.

use super::{Backoff, CallKind, Engine, Key, ResultEvent};
use crate::plugins::{
    package::{Installed, ToolDecl},
    query,
    tools::{
        self, Run, Source,
        background::{BackgroundProcess, BackgroundToolPhase, BackgroundToolStatus, Executable},
    },
};
use anyhow::{Context, Result, ensure};
use nanobug_plugin_sdk::{Permission, ServiceResult};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const MAX_BACKGROUND_TOOLS: usize = 8;
const CLOSE_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

/// A close acknowledgement held until the scope's confirmed operations settle.
pub(super) struct CloseDrain {
    reply: async_channel::Sender<Result<(), String>>,
    deadline: Instant,
}

/// One rebuildable service and its independent recovery allowance.
pub(super) struct BackgroundJob {
    source: Source,
    running: Option<BackgroundProcess>,
    launching: Option<(u64, Arc<AtomicBool>)>,
    backoff: Backoff,
    recovering: bool,
    verified: Option<Executable>,
    memory_pause: Option<u64>,
}

impl BackgroundJob {
    fn new(source: Source) -> Self {
        Self {
            source,
            running: None,
            launching: None,
            backoff: Backoff::default(),
            recovering: false,
            verified: None,
            memory_pause: None,
        }
    }

    fn suspend(&mut self) {
        self.running.take();
        if let Some((_, cancelled)) = &self.launching {
            cancelled.store(true, Ordering::Release);
        }
    }

    fn fail(&mut self, now: Instant) {
        self.running.take();
        self.recovering = true;
        if self.verified.is_some() {
            self.backoff.failed(now);
        } else {
            // A process that never acknowledged the safe protocol must not be replayed.
            self.backoff.paused = true;
        }
    }

    fn paused(&self) -> bool {
        self.backoff.paused() || self.memory_pause.is_some()
    }

    fn retry(&mut self) {
        self.backoff = Backoff::default();
        self.recovering = false;
        self.verified = None;
        self.memory_pause = None;
    }

    fn status(&self) -> BackgroundToolStatus {
        let phase = if self.paused() {
            BackgroundToolPhase::Paused
        } else if self.recovering {
            BackgroundToolPhase::Recovering
        } else {
            BackgroundToolPhase::Starting
        };
        BackgroundToolStatus {
            memory_paused: self.memory_pause.is_some(),
            private_bytes: self.memory_pause,
            ..BackgroundToolStatus::idle(phase)
        }
    }
}

impl Drop for BackgroundJob {
    fn drop(&mut self) {
        self.suspend();
    }
}

impl Engine {
    pub(super) fn drain_for_close(
        &mut self,
        project: Option<u64>,
        reply: async_channel::Sender<Result<(), String>>,
    ) {
        if self.close_drains.contains_key(&project) {
            let _ = reply.try_send(Err(
                "This scope is already waiting for plugin operations".into()
            ));
            return;
        }
        self.close_drains.insert(
            project,
            CloseDrain {
                reply,
                deadline: Instant::now() + CLOSE_DRAIN_TIMEOUT,
            },
        );
    }

    pub(super) fn is_draining(&self, project: u64) -> bool {
        self.close_drains.contains_key(&None) || self.close_drains.contains_key(&Some(project))
    }

    pub(super) fn advance_close_drains(&mut self) {
        if self.close_drains.is_empty() {
            return;
        }
        let now = Instant::now();
        let mut drains = std::mem::take(&mut self.close_drains);
        drains.retain(|&project, drain| {
            if drain.reply.is_closed() {
                return false;
            }
            let pending = self.close_work_pending(project);
            if pending && now < drain.deadline {
                return true;
            }
            let result = if pending {
                Err(
                    "Plugin operations are still running. Try closing again after they settle."
                        .into(),
                )
            } else {
                Ok(())
            };
            let _ = drain.reply.try_send(result);
            false
        });
        self.close_drains = drains;
    }

    fn close_work_pending(&self, project: Option<u64>) -> bool {
        self.has_native_writes(project, None)
            || self.runtimes.iter().any(|(namespace, runtime)| {
                self.records.get(namespace).is_some_and(|record| {
                    record.grants.contains(&Permission::RunTool)
                        && record
                            .release
                            .manifest
                            .tools
                            .values()
                            .any(|tool| tool.background.is_none())
                }) && runtime.calls.values().any(|call| {
                    call.kind == CallKind::User
                        && project.is_none_or(|scope| scope == call.instance.project)
                })
            })
    }

    /// Check accepted one-shot writes in an Apply or close scope.
    pub(super) fn has_native_writes(&self, project: Option<u64>, namespace: Option<&str>) -> bool {
        self.tool_writes.keys().any(|owner| {
            project.is_none_or(|scope| scope == owner.instance.project)
                && namespace.is_none_or(|name| name == owner.namespace)
        })
    }

    pub(super) fn finish_tool_write(&mut self, owner: &Key) {
        if let Some(count) = self.tool_writes.get_mut(owner) {
            *count -= 1;
            if *count == 0 {
                self.tool_writes.remove(owner);
            }
        }
    }

    fn configured_tool_path(
        &self,
        key: &Key,
        record: &Installed,
        declaration: &ToolDecl,
    ) -> Option<String> {
        let setting = declaration.path_setting.as_ref()?;
        let settings = self.config.settings(
            key.instance.project,
            &key.namespace,
            &record.release.manifest,
        );
        settings
            .get(setting)?
            .as_str()
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    }

    fn tool_source(&mut self, key: &Key, tool: &str, user_event: bool) -> Result<Source> {
        ensure!(
            user_event && !self.busy(&key.namespace) && self.wanted.contains_key(key),
            "Native tools require an explicit command for a permitted module"
        );
        let record = Arc::clone(
            self.records
                .get(&key.namespace)
                .context("Plugin is unavailable")?,
        );
        ensure!(
            record.enabled && record.grants.contains(&Permission::RunTool),
            "Native tool access is not granted"
        );
        let declaration = record
            .release
            .manifest
            .tools
            .get(tool)
            .cloned()
            .context("Undeclared native tool")?;
        let project = self
            .projects
            .get(&key.instance.project)
            .context("Project was closed")?;
        ensure!(
            self.project_execution
                .get(&query::project_key(&project.root))
                == Some(&true),
            "Project execution is blocked. Review the project execution setting in Modules."
        );
        let configured_path = self.configured_tool_path(key, &record, &declaration);
        Ok(Source {
            store: self.store.clone(),
            record,
            declaration,
            configured_path,
            project: project.root.clone(),
            closed: Arc::clone(self.project_flags.entry(key.instance.project).or_default()),
        })
    }

    pub(super) fn run_tool(
        &mut self,
        key: Key,
        generation: u64,
        token: u32,
        user_event: bool,
        tool: String,
        args: Vec<String>,
    ) {
        if self.config_apply_waits_for(&key) {
            self.tool_reply(
                key,
                generation,
                token,
                Err(
                    "Configuration is waiting for plugin writes to finish. Try again after Apply."
                        .into(),
                ),
            );
            return;
        }
        let source = self
            .tool_source(&key, &tool, user_event)
            .and_then(|source| {
                tools::check_args(&args)?;
                ensure!(
                    source.declaration.background.is_none(),
                    "Use StartBackgroundTool for a declared background service"
                );
                Ok(source)
            });
        let source = match source {
            Ok(source) => source,
            Err(error) => {
                self.tool_reply(key, generation, token, Err(format!("{error:#}")));
                return;
            }
        };
        let finish = self.finish.clone();
        let owner = key.clone();
        if self.tool_io.submit(move || {
            let value = source
                .executable()
                .and_then(|executable| {
                    let result = tools::execute(Run {
                        profile: source.store.root().to_path_buf(),
                        namespace: owner.namespace.clone(),
                        tool,
                        project: source.project,
                        executable: executable.path().to_path_buf(),
                        args,
                        closed: source.closed,
                        timeout: Duration::from_secs(30),
                    });
                    drop(executable);
                    result
                })
                .map_err(|error| format!("{error:#}"));
            let _ = finish.send(ResultEvent::Tool(
                owner,
                generation,
                ServiceResult { token, value },
            ));
        }) {
            *self.tool_writes.entry(key.clone()).or_default() += 1;
            *self.service_pending.entry(key.namespace).or_default() += 1;
        } else {
            self.tool_reply(
                key,
                generation,
                token,
                Err("Native tool queue is busy".into()),
            );
        }
    }

    pub(super) fn start_background_tool(
        &mut self,
        key: Key,
        generation: u64,
        token: u32,
        user_event: bool,
        tool: String,
    ) {
        let value = (|| -> Result<Value> {
            let source = self.tool_source(&key, &tool, user_event)?;
            ensure!(
                source.declaration.background.is_some(),
                "Native tool is not a declared background service"
            );
            let address = (key.clone(), tool);
            ensure!(
                self.backgrounds.contains_key(&address)
                    || self.backgrounds.len() < MAX_BACKGROUND_TOOLS,
                "Background tool limit reached"
            );
            let job = self
                .backgrounds
                .entry(address)
                .or_insert_with(|| BackgroundJob::new(source));
            if job.paused() {
                job.retry();
            }
            let status = job.running.as_ref().map_or_else(
                || job.status(),
                |process| {
                    process
                        .current(
                            &self.projects[&key.instance.project],
                            job.source.record.grants.contains(&Permission::ReadDocument),
                        )
                        .0
                },
            );
            Ok(json!({ "status": status }))
        })()
        .map_err(|error| error.to_string());
        self.tool_reply(key, generation, token, value);
    }

    pub(super) fn background_tool_status(
        &mut self,
        key: Key,
        generation: u64,
        token: u32,
        tool: String,
    ) {
        *self
            .service_pending
            .entry(key.namespace.clone())
            .or_default() += 1;
        // Evaluate the result when the reply is dispatched using the latest host snapshots.
        let _ = self.finish.send(ResultEvent::BackgroundToolStatus(
            key, generation, token, tool,
        ));
    }

    pub(super) fn current_background_tool_status(
        &self,
        key: &Key,
        tool: String,
    ) -> std::result::Result<Value, String> {
        (|| -> Result<Value> {
            let job = self
                .backgrounds
                .get(&(key.clone(), tool))
                .context("Background tool was not started")?;
            ensure!(
                self.background_is_authorized(key, job),
                "Background tool is no longer permitted"
            );
            let (status, result) = if !self.wanted.contains_key(key) || self.busy(&key.namespace) {
                (
                    BackgroundToolStatus::idle(BackgroundToolPhase::Suspended),
                    None,
                )
            } else if let Some(process) = &job.running {
                process.current(
                    &self.projects[&key.instance.project],
                    job.source.record.grants.contains(&Permission::ReadDocument),
                )
            } else {
                (job.status(), None)
            };
            Ok(json!({ "status": status, "result": result }))
        })()
        .map_err(|error| error.to_string())
    }

    fn tool_reply(
        &mut self,
        key: Key,
        generation: u64,
        token: u32,
        value: std::result::Result<Value, String>,
    ) {
        if let Err(error) = &value {
            self.log(&key.namespace, error);
        }
        *self
            .service_pending
            .entry(key.namespace.clone())
            .or_default() += 1;
        let _ = self.finish.send(ResultEvent::Service(
            key,
            generation,
            ServiceResult { token, value },
        ));
    }

    fn background_is_authorized(&self, key: &Key, job: &BackgroundJob) -> bool {
        self.enabled
            && !job.source.closed.load(Ordering::Acquire)
            && self.records.get(&key.namespace).is_some_and(|record| {
                record.enabled
                    && record.release.digest == job.source.record.release.digest
                    && record.grants == job.source.record.grants
                    && self.configured_tool_path(key, record, &job.source.declaration)
                        == job.source.configured_path
            })
            && self
                .projects
                .get(&key.instance.project)
                .is_some_and(|project| {
                    project.root == job.source.project
                        && self
                            .project_execution
                            .get(&query::project_key(&project.root))
                            == Some(&true)
                })
    }

    pub(super) fn retry_background_tool(&mut self, key: Key, tool: String) {
        let address = (key, tool);
        if !self.wanted.contains_key(&address.0) || self.busy(&address.0.namespace) {
            self.message("Background tool is not available for retry");
            return;
        }
        if let Some(job) = self.backgrounds.get_mut(&address)
            && job.paused()
        {
            job.retry();
        }
    }

    pub(super) fn advance_background_tools(&mut self) {
        let mut statuses = BTreeMap::<Key, BTreeMap<String, BackgroundToolStatus>>::new();
        for ((key, tool), mut job) in std::mem::take(&mut self.backgrounds) {
            if !self.background_is_authorized(&key, &job) {
                continue;
            }
            let status = if !self.wanted.contains_key(&key) || self.busy(&key.namespace) {
                job.suspend();
                BackgroundToolStatus::idle(BackgroundToolPhase::Suspended)
            } else {
                self.advance_background_tool(&key, &tool, &mut job)
            };
            statuses
                .entry(key.clone())
                .or_default()
                .insert(tool.clone(), status);
            self.backgrounds.insert((key, tool), job);
        }
        if self.view.background_tools != statuses {
            self.view.background_tools = statuses;
            self.changed = true;
        }
    }

    fn advance_background_tool(
        &mut self,
        key: &Key,
        tool: &str,
        job: &mut BackgroundJob,
    ) -> BackgroundToolStatus {
        let now = Instant::now();
        if let Some(process) = &mut job.running {
            if let Some(bytes) = process.sample_memory(now) {
                return self.pause_tool_memory(key, tool, job, bytes);
            }
            let revision = self.next();
            let project = &self.projects[&key.instance.project];
            let read_document = job.source.record.grants.contains(&Permission::ReadDocument);
            match process.advance(project, read_document, revision, now) {
                Ok(()) => {
                    if job.verified.is_none() && process.acknowledged_snapshot() {
                        job.verified = Some(process.executable.clone());
                    }
                    return process.current(project, read_document).0;
                }
                Err(error) => {
                    if let Some(bytes) = process.reclaiming() {
                        return self.pause_tool_memory(key, tool, job, bytes);
                    }
                    job.fail(now);
                    self.log(&key.namespace, &format!("Background tool {tool}: {error}"));
                }
            }
        }
        if job.launching.is_none() && !job.paused() && job.backoff.ready(now) {
            let id = self.next();
            let cancelled = Arc::new(AtomicBool::new(false));
            let mut source = job.source.clone();
            source.closed = Arc::clone(&cancelled);
            let args = source
                .declaration
                .background
                .as_ref()
                .expect("background declaration")
                .args
                .clone();
            let finish = self.finish.clone();
            let owner = key.clone();
            let name = tool.to_owned();
            let expected = job.verified.clone();
            if self.tool_io.submit(move || {
                let result = BackgroundProcess::start(&source, &args, expected.as_ref());
                let _ = finish.send(ResultEvent::BackgroundToolStarted(owner, name, id, result));
            }) {
                job.launching = Some((id, cancelled));
                *self
                    .service_pending
                    .entry(key.namespace.clone())
                    .or_default() += 1;
            }
        }
        job.status()
    }

    fn pause_tool_memory(
        &mut self,
        key: &Key,
        tool: &str,
        job: &mut BackgroundJob,
        bytes: u64,
    ) -> BackgroundToolStatus {
        job.suspend();
        job.memory_pause = Some(bytes);
        self.log(
            &key.namespace,
            &format!("Background tool {tool} exceeded its memory budget and was paused"),
        );
        job.status()
    }

    pub(super) fn background_tool_started(
        &mut self,
        key: Key,
        tool: String,
        id: u64,
        result: Result<BackgroundProcess>,
    ) {
        if let Some(count) = self.service_pending.get_mut(&key.namespace) {
            *count -= 1;
            if *count == 0 {
                self.service_pending.remove(&key.namespace);
            }
        }
        let Some(job) = self.backgrounds.get_mut(&(key.clone(), tool.clone())) else {
            return;
        };
        if job
            .launching
            .as_ref()
            .is_none_or(|(current, _)| *current != id)
        {
            return;
        }
        let (_, cancelled) = job.launching.take().expect("matching tool launch");
        if cancelled.load(Ordering::Acquire) {
            return;
        }
        match result {
            Ok(process) => job.running = Some(process),
            Err(error) => {
                job.fail(Instant::now());
                self.log(
                    &key.namespace,
                    &format!("Could not start background tool {tool}: {error:#}"),
                );
            }
        }
    }

    pub(super) fn review_tool_operations(&mut self, namespace: String) {
        if !self.records.contains_key(&namespace) && !self.view.retained.contains_key(&namespace) {
            self.message("Plugin operation history is unavailable");
            return;
        }
        let id = self.next();
        let owner = namespace.clone();
        let profile = self.store.root().to_path_buf();
        let finish = self.finish.clone();
        if self.tool_io.submit(move || {
            let result = super::OperationsReview {
                tools: tools::review(&profile, &owner),
                files: crate::plugins::file_operations::review(&profile, &owner),
            };
            let _ = finish.send(ResultEvent::ToolOperations(id, owner, result));
        }) {
            self.tool_review = Some((id, namespace));
        } else {
            self.message("Native tool queue is busy. Try opening operation history again.");
        }
    }
}
