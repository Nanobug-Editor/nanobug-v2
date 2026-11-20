//! Plugin supervisor actions.

use super::*;

impl Engine {
    pub(super) fn action(&mut self, action: Action) {
        match action {
            Action::SettingsCatalog { project, reply } => {
                let result = self.settings_catalog(project);
                let _ = reply.try_send(result.map_err(|error| format!("{error:#}")));
            }
            Action::WriteSettings(request, reply) => {
                let result = self.write_settings_page(&request);
                let _ = reply.try_send(
                    result
                        .map(|()| request)
                        .map_err(|error| format!("{error:#}")),
                );
            }
            Action::ControlLanguageServer {
                operation,
                target,
                reply,
            } => {
                self.control_language_server(operation, target, reply);
            }
            Action::SyntaxNodes {
                language,
                document,
                reply,
            } => {
                let id = self.next();
                let syntax = Arc::clone(&self.syntax);
                let on_busy = reply.clone();
                if !self.syntax_io.submit(move || {
                    if !reply.is_closed() {
                        let result = syntax
                            .lock()
                            .unwrap()
                            .node_ranges(id, language, document)
                            .map_err(|error| error.to_string());
                        let _ = reply.try_send(result);
                    }
                }) {
                    let _ = on_busy.try_send(Err("Syntax worker is busy".into()));
                }
            }
            Action::InspectZip(path) => self.inspect(path, None, false),
            Action::InspectChosenZip(path) => self.inspect_chosen_zip(path),
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
                match write_json(&self.store.state_root().join("developer.json"), &next) {
                    Ok(()) => {
                        self.view.developer_settings = next;
                        self.development.developer_stamps.remove(&namespace);
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
                self.development.developer_build.insert(namespace);
                self.development.developer_due = Instant::now();
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
                    && !self.installation.busy(&owner.namespace)
                    && !self
                        .installation
                        .ready_candidates
                        .contains(&owner.namespace)
                    && self.lifecycle.wanted.contains_key(&owner)
                    && self.records.get(&owner.namespace).is_some_and(|record| {
                        record.release.digest == digest
                            && record.grants.contains(&Permission::ChangeFiles)
                    })
                    && self
                        .lifecycle
                        .runtimes
                        .get(&owner.namespace)
                        .is_some_and(|runtime| {
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
                        .requests
                        .service_pending
                        .entry(owner.namespace.clone())
                        .or_default() += 1;
                    self.requests.file_writes.push(FileWrite { owner, ended });
                }
            }
            Action::Approve(token) => {
                if self.installation.approve(token) {
                    self.view.review = None;
                    self.view.installing = true;
                    self.message("Waiting for plugin operations to finish…");
                }
            }
            Action::CancelInstall => {
                if self.installation.cancel() {
                    self.view.review = None;
                    self.view.installing = false;
                    self.message("Cancelled");
                }
            }
            Action::Uninstall(namespace)
                if self.records.contains_key(&namespace)
                    && self.installation.mutating.is_none()
                    && self.installation.pending_change.is_none()
                    && self.installation.review.is_none()
                    && self.installation.inspecting.is_none() =>
            {
                self.installation.pending_change = Some(Change::Remove(namespace));
                self.view.installing = true;
                self.message("Waiting for plugin operations to finish…");
            }
            Action::SetEnabled(namespace, enabled)
                if self.installation.mutating.is_none()
                    && self.installation.pending_change.is_none() =>
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
                if let Some(runtime) = self.lifecycle.runtimes.get_mut(&namespace) {
                    runtime.backoff = Backoff::default();
                    runtime.memory = MemoryProtection::default();
                    for module in runtime.instances.values_mut() {
                        module.failure = None;
                        module.last = None;
                        module.refresh = None;
                    }
                }
                // Re-resolve newly installed language-server dependencies on an explicit retry.
                self.retire_plugin_servers(&namespace);
                self.rules.retry(&namespace);
            }
            Action::RetrySettings(key) => {
                if let Some(live) = self
                    .lifecycle
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
            Action::SetProjectSqlDialect {
                project,
                dialect,
                reply,
            } => {
                let result = self
                    .sql_dialects
                    .set(project, dialect)
                    .map_err(|error| error.to_string());
                if result.is_ok() {
                    self.reconcile();
                }
                let _ = reply.try_send(result);
            }
            Action::SetProjectExecution(id, allow) => {
                let Some(project) = self.projects.get(&id) else {
                    return;
                };
                let key = query::project_key(&project.root);
                let mut permissions = self.project_execution.clone();
                permissions.insert(key, allow);
                match write_json(
                    &self.store.state_root().join("execution.json"),
                    &permissions,
                ) {
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
                    .lifecycle
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
            } => {
                self.dispatch_invocation(key, event, reply, timeout);
            }
            _ => {}
        }
    }
}
