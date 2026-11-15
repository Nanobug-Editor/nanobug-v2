//! Awaited language services and revision-bound document presentation.

use super::{Answer, Call, CallKind, Engine, Key, ResultEvent, Version};
use crate::plugins::{package::Provider, query, tools};
use anyhow::{Context, ensure};
use nanobug_plugin_sdk::{
    DocumentSnapshot, Permission, PluginEvent, Service, ServiceResult,
    language::{ByteRange, DocumentFeatures},
};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};

/// Retain the originating call's identity and deadline while its service runs off-thread.
pub(super) struct Pending {
    key: Key,
    generation: u64,
    version: Version,
    deadline: Instant,
    reply: Option<Answer>,
    kind: CallKind,
    format_path: Option<PathBuf>,
    /// Reject callbacks if the active file was renamed without a text edit.
    document_path: Option<std::path::PathBuf>,
    /// Prepared project settings used by the first half of the call.
    settings_revision: [u64; 3],
    token: u32,
}

impl Pending {
    /// Bind `token` to this provider's `call` and worker `generation`.
    pub(super) fn new(
        key: Key,
        generation: u64,
        call: Call,
        token: u32,
        settings_revision: [u64; 3],
    ) -> Self {
        Self {
            key,
            generation,
            version: call.version,
            deadline: call.deadline,
            reply: call.reply,
            kind: call.kind,
            format_path: call.format_path,
            token,
            document_path: None,
            settings_revision,
        }
    }

    /// Return a continuation failure to the original caller, if it was waiting.
    fn fail(self, message: &str) {
        if let Some(reply) = self.reply {
            let _ = reply.try_send(Err(message.into()));
        }
    }
}

impl Engine {
    /// Queue a bounded service whose reply returns to the original invocation.
    pub(super) fn begin_language_service(
        &mut self,
        mut pending: Pending,
        operation: Service,
    ) -> bool {
        let id = self.next();
        let key = &pending.key;
        let namespace = key.namespace.clone();
        let document = self.active_document(key).cloned();
        if self
            .requests
            .service_pending
            .get(&namespace)
            .copied()
            .unwrap_or_default()
            >= super::MAX_RUNTIME_CALLS
        {
            let retry = pending.reply.is_none();
            pending.fail("Plugin service queue is busy");
            return !retry;
        }
        pending.document_path = document.as_ref().and_then(|doc| doc.path.clone());
        let finish = self.finish.clone();
        let deadline = pending.deadline;
        let notify_failure = pending.reply.clone();
        let submitted = match operation {
            Service::QuerySyntax { language, query } => {
                let syntax = Arc::clone(&self.syntax);
                self.syntax_io.submit(move || {
                    let result = (|| -> anyhow::Result<_> {
                        anyhow::ensure!(Instant::now() < deadline, "Syntax request timed out");
                        anyhow::ensure!(query.len() <= 8 * 1024, "Syntax query exceeds size limit");
                        let doc =
                            document.ok_or_else(|| anyhow::anyhow!("No readable document"))?;
                        syntax.lock().unwrap().parse(
                            id,
                            language,
                            doc.as_ref().clone(),
                            Some(query),
                        )
                    })();
                    let _ = finish.send(ResultEvent::LanguageService(
                        pending,
                        result.map_err(|error| error.to_string()),
                    ));
                })
            }
            Service::ReadProjectAncestors { names } => {
                let project = &self.projects[&key.instance.project];
                let record = &self.records[&namespace];
                let context = query::ContextData {
                    root: project.root.clone(),
                    grants: record.grants.clone(),
                    rule_paths: Default::default(),
                    document,
                    open_documents: Vec::new(),
                    deadline: deadline.min(Instant::now() + Duration::from_secs(2)),
                    cancelled: self
                        .project_flags
                        .get(&project.id)
                        .cloned()
                        .unwrap_or_else(|| Arc::new(AtomicBool::new(false))),
                };
                self.io.submit(move || {
                    let result = query::read_ancestors(&context, names)
                        .and_then(|files| Ok(serde_json::to_value(files)?));
                    let _ = finish.send(ResultEvent::LanguageService(
                        pending,
                        result.map_err(|error| error.to_string()),
                    ));
                })
            }
            Service::Rustfmt | Service::FormatLanguage { .. } => {
                let tool = match operation {
                    Service::FormatLanguage { tool } => tool,
                    _ => "rustfmt".to_owned(),
                };
                let native = tool != "sqlformat";
                let (root, path, document) =
                    match self.format_source(&pending, document, &tool, native) {
                        Ok(source) => source,
                        Err(error) => {
                            pending.fail(&error.to_string());
                            return true;
                        }
                    };
                let closed =
                    Arc::clone(self.project_flags.entry(key.instance.project).or_default());
                self.tool_io.submit(move || {
                    let result =
                        tools::format_language(&tool, &root, &path, &document, deadline, &closed);
                    let _ = finish.send(ResultEvent::LanguageService(
                        pending,
                        result
                            .map(serde_json::Value::String)
                            .map_err(|error| error.to_string()),
                    ));
                })
            }
            _ => {
                pending.fail("Unsupported awaited service");
                return true;
            }
        };
        if submitted {
            *self.requests.service_pending.entry(namespace).or_default() += 1;
        } else if let Some(reply) = notify_failure {
            let _ = reply.try_send(Err("Plugin service queue is busy".into()));
        }
        submitted
    }

    /// Authorize formatting for the exact snapshot, applying native execution grants when needed.
    fn format_source(
        &self,
        pending: &Pending,
        document: Option<Arc<DocumentSnapshot>>,
        tool: &str,
        native: bool,
    ) -> anyhow::Result<(PathBuf, PathBuf, Arc<DocumentSnapshot>)> {
        let key = &pending.key;
        let document = document.context("Open a file to format it")?;
        let path = pending
            .format_path
            .as_deref()
            .or(document.path.as_deref())
            .context("Open a file to format it")?;
        ensure!(
            pending.kind == CallKind::Format
                && pending.reply.is_some()
                && pending.version == Some((document.opening, document.revision))
                && (tool != "rustfmt"
                    || Path::extension(path).is_some_and(|ext| ext.eq_ignore_ascii_case("rs")))
                && self.lifecycle.wanted.get(key).is_some_and(|module| {
                    module.provides.contains(&Provider::Format)
                        && module.accepts_document(Some(path))
                }),
            "The formatter requires an active request for a supported file"
        );
        let record = self
            .records
            .get(&key.namespace)
            .context("Plugin is unavailable")?;
        ensure!(
            record.enabled
                && record.grants.contains(&Permission::ReadDocument)
                && record.grants.contains(&Permission::EditDocument)
                && (!native || record.grants.contains(&Permission::RunTool)),
            "The formatter is missing required permissions"
        );
        let project = self
            .projects
            .get(&key.instance.project)
            .context("Project was closed")?;
        ensure!(
            !native
                || self
                    .project_execution
                    .get(&query::project_key(&project.root))
                    == Some(&true),
            "Project execution is blocked. Choose Allow project execution in Modules."
        );
        Ok((project.root.clone(), path.to_owned(), document))
    }

    /// Drop late or replaced replies before invoking the same guest with the service data.
    pub(super) fn language_service_finished(
        &mut self,
        pending: Pending,
        value: Result<serde_json::Value, String>,
    ) {
        let key = &pending.key;
        self.finish_service(&key.namespace);
        let now = Instant::now();
        let current = self.active_document(key);
        let settings_changed =
            pending.settings_revision != self.config.settings_revision(key.instance.project);
        if now >= pending.deadline
            || self.installation.busy(&key.namespace)
            || !self.lifecycle.wanted.contains_key(key)
            || settings_changed
            || !self
                .lifecycle
                .runtimes
                .get(&key.namespace)
                .is_some_and(|runtime| runtime.generation == pending.generation)
            || pending.version != current.map(|doc| (doc.opening, doc.revision))
            || pending.document_path.as_ref() != current.and_then(|doc| doc.path.as_ref())
        {
            if settings_changed
                && pending.reply.is_none()
                && let Some(live) = self
                    .lifecycle
                    .runtimes
                    .get_mut(&key.namespace)
                    .and_then(|runtime| runtime.instances.get_mut(&key.instance))
            {
                live.force_document_refresh = true;
            }
            pending.fail("Plugin service result is out of date");
            return;
        }
        let key_namespace = key.namespace.clone();
        let instance = key.instance.clone();
        let reply = pending.reply.clone();
        if !self.invoke_with_kind(
            pending.key,
            PluginEvent::ServiceResult {
                result: ServiceResult {
                    token: pending.token,
                    value,
                },
            },
            pending.reply,
            pending.deadline - now,
            CallKind::Continued,
            None,
        ) && reply.is_none()
            && let Some(live) = self
                .lifecycle
                .runtimes
                .get_mut(&key_namespace)
                .and_then(|runtime| runtime.instances.get_mut(&instance))
        {
            live.force_document_refresh = true;
        }
    }

    /// Publish sorted, bounded ranges only for the document version and extension in the call.
    pub(super) fn publish_presentation(
        &mut self,
        key: &Key,
        generation: u64,
        version: Version,
        mut features: DocumentFeatures,
    ) {
        let Some((doc, version)) = self.active_document(key).zip(version) else {
            return;
        };
        if (doc.opening, doc.revision) != version
            || !self.lifecycle.wanted[key].accepts_document(doc.path.as_deref())
        {
            return;
        }
        let valid = |range: ByteRange| {
            range.start < range.end
                && doc.text.is_char_boundary(range.start)
                && doc.text.is_char_boundary(range.end)
        };
        features.highlights.truncate(6144);
        features.highlights.retain(|span| valid(span.range));
        features
            .highlights
            .sort_unstable_by_key(|span| (span.range.start, span.range.end));
        let mut end = 0;
        features.highlights.retain(|span| {
            if span.range.start < end {
                return false;
            }
            end = span.range.end;
            true
        });
        features.folds.truncate(2048);
        features.folds.retain(|range| valid(*range));
        features.symbols.truncate(2048);
        features.symbols.retain(|symbol| {
            doc.text.is_char_boundary(symbol.offset)
                && !symbol.name.is_empty()
                && symbol.name.len() <= 512
                && symbol.kind.len() <= 64
                && !symbol.name.chars().any(char::is_control)
                && !symbol.kind.chars().any(char::is_control)
        });
        features.symbols.sort_by_key(|symbol| symbol.offset);
        self.view.document_presentations.insert(
            key.clone(),
            Arc::new(super::super::DocumentPresentation {
                version,
                generation,
                features,
            }),
        );
        self.changed = true;
    }

    /// Remove contributions as soon as their authorization, module or document version changes.
    pub(super) fn retain_presentations(&mut self) {
        let before = self.view.document_presentations.len();
        self.view
            .document_presentations
            .retain(|key, presentation| {
                self.lifecycle
                    .wanted
                    .get(key)
                    .is_some_and(|module| module.provides.contains(&Provider::Document))
                    && self
                        .records
                        .get(&key.namespace)
                        .is_some_and(|record| record.grants.contains(&Permission::ReadDocument))
                    && self
                        .lifecycle
                        .runtimes
                        .get(&key.namespace)
                        .is_some_and(|runtime| {
                            runtime.generation == presentation.generation
                                && runtime.worker.is_some()
                                && runtime.instances.get(&key.instance).is_some_and(|live| {
                                    live.settings_error.is_none() && live.failure.is_none()
                                })
                        })
                    && self
                        .projects
                        .get(&key.instance.project)
                        .and_then(|project| project.document.as_ref())
                        .is_some_and(|doc| {
                            (doc.opening, doc.revision) == presentation.version
                                && self.lifecycle.wanted[key].accepts_document(doc.path.as_deref())
                        })
            });
        self.changed |= self.view.document_presentations.len() != before;
    }
}
