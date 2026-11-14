//! Plugin supervisor services.

use super::*;

/// State retained by the requests workflow across supervisor ticks.
#[derive(Default)]
pub(super) struct Requests {
    /// Native text measurements awaiting a version-bound editor reply.
    pub(super) pending_measurements: BTreeMap<u64, (Key, usize)>,
    /// Outstanding host requests counted per package before replacement.
    pub(super) service_pending: BTreeMap<String, usize>,
    /// Confirmed editor writes holding their plugin version until completion.
    pub(super) file_writes: Vec<FileWrite>,
    /// Optional peer requests retaining both caller and provider generations.
    pub(super) peer_calls: Vec<PeerCall>,
}

impl Engine {
    pub(super) fn can_use_credential(&self, namespace: &str, slot: &str) -> bool {
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

    pub(super) fn service(
        &mut self,
        key: Key,
        generation: u64,
        user_event: bool,
        request: ServiceRequest,
    ) {
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
                                .lifecycle
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
                    .requests
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
            Service::QuerySyntax { .. }
            | Service::ReadProjectAncestors { .. }
            | Service::Rustfmt => {
                self.message("This service requires an awaited invocation");
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
            && !self.installation.busy(&key.namespace)
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
            *self
                .requests
                .service_pending
                .entry(key.namespace)
                .or_default() += 1;
        } else {
            *self
                .requests
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
    pub(super) fn prepare_text_measurement(
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
            .lifecycle
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
                self.requests
                    .pending_measurements
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
    pub(super) fn text_measurement_live(&self, request: &TextMeasurement) -> bool {
        self.lifecycle.wanted.contains_key(&request.owner)
            && self
                .lifecycle
                .runtimes
                .get(&request.owner.namespace)
                .is_some_and(|runtime| {
                    runtime.generation == request.generation && runtime.worker.is_some()
                })
    }

    /// Send the result through the existing host service response path for the same live module.
    pub(super) fn reply_text_measurement(
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

    pub(super) fn read_project_text(
        &mut self,
        key: Key,
        generation: u64,
        token: u32,
        path: String,
    ) {
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
            .requests
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

    pub(super) fn parse_document(
        &mut self,
        key: Key,
        generation: u64,
        token: u32,
        language: String,
    ) {
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
            .requests
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

    pub(super) fn call_plugin(
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
            let namespace = crate::plugins::package::Identity::new(name, id)?.namespace();
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
                self.lifecycle.wanted.contains_key(&key)
                    && !self.installation.busy(&key.namespace)
                    && !self.installation.ready_candidates.contains(&key.namespace)
                    && !self.is_draining(key.instance.project),
                "Optional plugin service is not ready"
            );
            let runtime = self
                .lifecycle
                .runtimes
                .get(&key.namespace)
                .filter(|runtime| runtime.worker.is_some())
                .context("Optional plugin service is not ready")?;
            anyhow::ensure!(
                self.requests.peer_calls.len() < MAX_PEER_CALLS,
                "Optional plugin service queue is busy"
            );
            Ok((key, runtime.generation))
        })();
        *self
            .requests
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
        self.requests.peer_calls.push(PeerCall {
            caller,
            caller_generation: generation,
            peer,
            peer_generation,
            token,
            deadline: Instant::now() + PEER_TIMEOUT,
            response,
        });
    }

    pub(super) fn finish_service(&mut self, namespace: &str) {
        if let Some(count) = self.requests.service_pending.get_mut(namespace) {
            *count -= 1;
            if *count == 0 {
                self.requests.service_pending.remove(namespace);
            }
        }
    }

    /// Deliver only to the caller that requested the operation, without blocking either worker.
    pub(super) fn advance_peer_calls(&mut self) {
        let now = Instant::now();
        let mut pending = std::mem::take(&mut self.requests.peer_calls);
        pending.retain(|call| {
            let caller = self
                .lifecycle
                .runtimes
                .get(&call.caller.namespace)
                .filter(|runtime| {
                    runtime.generation == call.caller_generation && runtime.worker.is_some()
                });
            let Some(caller) = caller.filter(|_| {
                self.lifecycle.wanted.contains_key(&call.caller)
                    && !self.installation.busy(&call.caller.namespace)
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
            let peer_ready = self.lifecycle.wanted.contains_key(&call.peer)
                && !self.installation.busy(&call.peer.namespace)
                && self
                    .lifecycle
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
        self.requests.peer_calls = pending;
    }
}
