//! Plugin supervisor events.

use super::*;
pub(super) enum ResultEvent {
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
    BuiltinSyntax(builtin_syntax::Stamp, Option<super::super::DocumentSyntax>),
    LanguageService(language::Pending, Result<serde_json::Value, String>),
    Analyzer(Key, u64, tools::rust_analyzer::Event),
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

pub(super) struct OperationsReview {
    pub(super) tools: anyhow::Result<Vec<tools::ToolOperation>>,
    pub(super) files: anyhow::Result<Vec<crate::plugins::file_operations::FileOperationReceipt>>,
}

/// A confirmed editor-side write keeps its plugin version until the background task drops its lease.
pub(super) struct FileWrite {
    pub(super) owner: Key,
    pub(super) ended: async_channel::Receiver<()>,
}

impl Engine {
    pub(super) fn event(&mut self, event: ResultEvent) {
        match event {
            ResultEvent::Catalog(result) => match result {
                Ok(entries) => {
                    self.view.catalog = Arc::new(entries);
                    self.changed = true;
                }
                Err(error) => self.message(format!("Could not read Marketplace: {error:#}")),
            },
            ResultEvent::Prepared(id, result) if self.installation.inspecting == Some(id) => {
                self.installation.inspecting = None;
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
                self.installation.mutating = None;
                self.view.installing = false;
                let development = self.development.developer_install.take().is_some();
                match result {
                    Ok(record) => {
                        let prior = self.lifecycle.runtimes.remove(&key);
                        if development && let Some(prior) = prior {
                            self.lifecycle.runtimes.insert(
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
                    .lifecycle
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
                    self.requests.pending_measurements.remove(&ticket);
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
            ResultEvent::BuiltinSyntax(stamp, data) => self.finish_builtin_syntax(stamp, data),
            ResultEvent::LanguageService(pending, result) => {
                self.language_service_finished(pending, result);
            }
            ResultEvent::Analyzer(key, generation, event) => {
                self.analyzer_event(&key, generation, event);
            }
            ResultEvent::Service(key, generation, result) => {
                self.finish_service(&key.namespace);
                if self.lifecycle.wanted.contains_key(&key)
                    && self
                        .lifecycle
                        .runtimes
                        .get(&key.namespace)
                        .is_some_and(|runtime| {
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
                let Some((job, cancelled)) = self.development.developer_job.take() else {
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
                        self.development
                            .developer_stamps
                            .insert(namespace.clone(), outcome.stamp.clone());
                        if let Some(result) = outcome.prepared {
                            match result {
                                Ok(package) if package.release.digest != digest => {
                                    self.development.developer_ready = Some(Change::Development(
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
                        self.development.developer_stamps.remove(&namespace);
                        self.log(&namespace, &format!("Development source: {error:#}"));
                        self.development.developer_due = Instant::now() + Duration::from_secs(3);
                    }
                }
            }
            _ => {}
        }
    }
}
