//! Schedule read-only plugin diagnostics while a document has no native diagnostic result.

use super::{Backoff, Call, CallKind, CallResult, Engine, Key, State, language::Pending};
use crate::plugins::{package::Provider, session::DocumentDiagnostics};
use nanobug_plugin_sdk::{DocumentSnapshot, Output, Permission, PluginEvent, Service};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

/// One check per provider and opening, including completed results for retained buffers.
pub(super) type Checks = BTreeMap<(Key, u64), Check>;

/// A delayed or in-flight request and the identity of the input it may annotate.
pub(super) struct Check {
    /// Version and identity of the provider's input while the request is in flight.
    revision: u64,
    path: Option<PathBuf>,
    /// The package, file type and settings used to compute this result.
    generation: u64,
    filetype: Option<String>,
    settings: [u64; 3],
    phase: Phase,
    retry: Backoff,
}

/// Only completed, validated replies become editor decorations.
enum Phase {
    Waiting(Instant),
    Running(async_channel::Receiver<Result<CallResult, String>>),
    Published(Arc<DocumentDiagnostics>),
}

impl Check {
    /// Advance a submitted check against its still-current snapshot without blocking the host.
    fn collect(&mut self, doc: &DocumentSnapshot, now: Instant) {
        let Phase::Running(reply) = &self.phase else {
            return;
        };
        let result = match reply.try_recv() {
            Ok(Ok(result))
                if result.generation == self.generation
                    && result.version == Some((doc.opening, doc.revision)) =>
            {
                result
            }
            Err(async_channel::TryRecvError::Empty) => return,
            _ => {
                self.retry.failed(now);
                self.phase = Phase::Waiting(now);
                return;
            }
        };
        let diagnostics = result
            .output
            .diagnostics
            .into_iter()
            .take(512)
            .filter(|item| {
                item.start <= item.end
                    && doc.text.is_char_boundary(item.start)
                    && doc.text.is_char_boundary(item.end)
                    && !item.message.is_empty()
                    && item.message.len() <= 2048
            })
            .take(200)
            .collect();
        self.phase = Phase::Published(Arc::new(DocumentDiagnostics {
            version: (doc.opening, doc.revision),
            path: self.path.clone(),
            diagnostics,
        }));
    }
}

impl Engine {
    /// Check the active document once typing settles and retain current results for other
    /// open documents. A native publication, including an empty one, supersedes the guest.
    pub(super) fn sync_guest_diagnostics(&mut self) {
        let mut published = BTreeMap::new();
        let now = Instant::now();
        let mut checks = std::mem::take(&mut self.guest_diagnostics);
        let mut ready = Vec::new();
        for (key, module) in &self.lifecycle.wanted {
            if !module.provides.contains(&Provider::Diagnose)
                || self.view.modules.get(key) != Some(&State::Ready)
                || !self
                    .records
                    .get(&key.namespace)
                    .is_some_and(|record| record.grants.contains(&Permission::ReadDocument))
                || self.is_draining(key.instance.project)
            {
                continue;
            }
            let project = &self.projects[&key.instance.project];
            let Some(runtime) = self.lifecycle.runtimes.get(&key.namespace) else {
                continue;
            };
            let generation = runtime.generation;
            let settings = self.config.settings_revision(project.id);
            for doc in project.document.iter().chain(&project.open_documents) {
                let active = project
                    .document
                    .as_ref()
                    .is_some_and(|active| active.opening == doc.opening);
                let filetype = self.document_filetype(project.id, doc.opening);
                let id = (key.clone(), doc.opening);
                if doc.text.len() > super::MAX_DOC
                    || !module.accepts_filetype(doc.path.as_deref(), filetype)
                    || self.view.document_diagnostics.contains_key(&id)
                {
                    continue;
                }
                let previous = checks.remove(&id).filter(|check| {
                    check.revision == doc.revision
                        && check.path == doc.path
                        && check.generation == generation
                        && check.settings == settings
                        && check.filetype.as_deref() == filetype
                });
                if !active
                    && !previous
                        .as_ref()
                        .is_some_and(|check| matches!(check.phase, Phase::Published(_)))
                {
                    continue;
                }
                let mut check = previous.unwrap_or_else(|| Check {
                    revision: doc.revision,
                    path: doc.path.clone(),
                    generation,
                    settings,
                    filetype: filetype.map(str::to_owned),
                    phase: Phase::Waiting(now + Duration::from_millis(200)),
                    retry: Backoff::default(),
                });
                check.collect(doc, now);
                match &check.phase {
                    Phase::Published(data) => {
                        published.insert(id.clone(), Arc::clone(data));
                    }
                    Phase::Waiting(due) if now >= *due && check.retry.ready(now) => {
                        ready.push(id.clone())
                    }
                    _ => {}
                }
                self.guest_diagnostics.insert(id, check);
            }
        }
        self.changed |= published.len() != self.view.guest_diagnostics.len()
            || published.iter().any(|(id, data)| {
                self.view
                    .guest_diagnostics
                    .get(id)
                    .is_none_or(|old| !Arc::ptr_eq(old, data))
            });
        self.view.guest_diagnostics = published;
        for id in ready {
            let (reply, response) = async_channel::bounded(1);
            self.invoke_with_kind(
                id.0.clone(),
                PluginEvent::Diagnose,
                Some(reply),
                Duration::from_secs(3),
                CallKind::BackgroundDiagnose,
                None,
            );
            self.guest_diagnostics
                .get_mut(&id)
                .expect("selected diagnostic check")
                .phase = Phase::Running(response);
        }
    }

    /// Accept only diagnostic ranges or one awaited read-only service from an automatic check.
    /// Bind the continuation to the same provider, snapshot, settings and deadline as `call`.
    pub(super) fn background_diagnostic_reply(
        &mut self,
        key: Key,
        generation: u64,
        call: Call,
        mut output: Output,
    ) {
        if output.await_service {
            if call.kind == CallKind::BackgroundDiagnose
                && output.services.len() == 1
                && let Some(request) = output.services.pop()
                && matches!(
                    request.operation,
                    Service::QuerySyntax { .. } | Service::ReadProjectAncestors { .. }
                )
            {
                let settings = self.config.settings_revision(key.instance.project);
                let pending = Pending::new(key, generation, call, request.token, settings);
                self.begin_language_service(pending, request.operation);
            } else if let Some(reply) = call.reply {
                let _ = reply.try_send(Err("Unsupported background diagnostic service".into()));
            }
            return;
        }
        if let Some(reply) = call.reply {
            let _ = reply.try_send(Ok(CallResult {
                generation,
                version: call.version,
                output: Output {
                    diagnostics: output.diagnostics,
                    ..Default::default()
                },
            }));
        }
    }
}
