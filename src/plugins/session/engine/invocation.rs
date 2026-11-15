//! Plugin supervisor invocation.

use super::*;

impl Engine {
    /// Route an explicit `event` to the available analyzer or its ordinary plugin worker.
    /// Preserve `reply` and `timeout` for the selected execution path.
    pub(super) fn dispatch_invocation(
        &mut self,
        key: Key,
        event: PluginEvent,
        reply: Option<Answer>,
        timeout: Duration,
    ) {
        if !self.invoke_analyzer(&key, &event, reply.as_ref(), timeout) {
            self.invoke(key, event, reply, timeout);
        }
    }

    pub(super) fn input(&self, key: &Key, event: PluginEvent, user: bool) -> Option<Input> {
        self.input_with_document(key, event, user, self.active_document(key))
    }

    pub(super) fn active_document(&self, key: &Key) -> Option<&Arc<DocumentSnapshot>> {
        input_document(
            self.projects.get(&key.instance.project)?,
            self.records.get(&key.namespace)?,
        )
    }

    pub(super) fn input_with_document(
        &self,
        key: &Key,
        event: PluginEvent,
        user: bool,
        document: Option<&Arc<DocumentSnapshot>>,
    ) -> Option<Input> {
        let project = self.projects.get(&key.instance.project)?;
        let record = self.records.get(&key.namespace)?;
        let document = document.map(|snapshot| snapshot.as_ref().clone());
        let settings = self
            .config
            .settings(project.id, &key.namespace, &record.release.manifest);
        let draft = self.draft_for(key).cloned().unwrap_or_default();
        let mut space = MAX_DOC.saturating_sub(document.as_ref().map_or(0, |doc| doc.text.len()));
        let open_documents = if user && record.grants.contains(&Permission::ReadDocument) {
            project
                .open_documents
                .iter()
                .filter_map(|snapshot| {
                    if snapshot.text.len() > space
                        || document
                            .as_ref()
                            .is_some_and(|active| active.opening == snapshot.opening)
                    {
                        return None;
                    }
                    space -= snapshot.text.len();
                    Some(snapshot.as_ref().clone())
                })
                .take(16)
                .collect()
        } else {
            Vec::new()
        };
        let canvas_sync = self.canvas_sync_for(key, &event);
        Some(Input {
            event,
            document,
            settings,
            draft,
            viewport: self.layouts.get(key).copied(),
            canvas_sync,
            open_documents,
        })
    }

    pub(super) fn invoke(
        &mut self,
        key: Key,
        event: PluginEvent,
        reply: Option<Answer>,
        timeout: Duration,
    ) {
        let kind = match event {
            PluginEvent::ServiceResult { .. } => CallKind::Service,
            PluginEvent::ServiceCall { .. } => CallKind::PeerService,
            PluginEvent::Command { .. } | PluginEvent::Ui { .. } => CallKind::User,
            PluginEvent::Format { .. } => CallKind::Format,
            _ => CallKind::Explicit,
        };
        self.invoke_with_kind(key, event, reply, timeout, kind, None);
    }

    /// Limit in-flight background Canvas callbacks and keep their outputs non-authoritative.
    pub(super) fn invoke_with_kind(
        &mut self,
        key: Key,
        event: PluginEvent,
        reply: Option<Answer>,
        timeout: Duration,
        kind: CallKind,
        credential: Option<CredentialChange>,
    ) -> bool {
        let id = self.next();
        let allowed = !self.installation.busy(&key.namespace)
            && self.lifecycle.wanted.contains_key(&key)
            && (matches!(kind, CallKind::Service | CallKind::Continued)
                || !self.installation.ready_candidates.contains(&key.namespace));
        let quiet = matches!(kind, CallKind::CanvasPassive(_) | CallKind::Continued);
        let passive = matches!(kind, CallKind::CanvasPassive(_));
        if passive
            && self
                .lifecycle
                .runtimes
                .get(&key.namespace)
                .is_some_and(|runtime| {
                    runtime
                        .calls
                        .values()
                        .any(|call| call.kind == kind && call.instance == key.instance)
                })
        {
            return false;
        }
        let closing = (passive
            || matches!(
                kind,
                CallKind::User
                    | CallKind::Explicit
                    | CallKind::Format
                    | CallKind::Continued
                    | CallKind::PeerService
                    | CallKind::CredentialChange
            ))
            && self.is_draining(key.instance.project);
        let input = self.input(&key, event, kind == CallKind::User);
        let send = || -> anyhow::Result<()> {
            anyhow::ensure!(
                !closing,
                "Project is waiting for confirmed plugin commands to finish"
            );
            anyhow::ensure!(allowed, "Module is not available");
            let input = input.context("Project or plugin was closed")?;
            let runtime = self
                .lifecycle
                .runtimes
                .get_mut(&key.namespace)
                .context("Plugin is starting")?;
            let live = runtime
                .instances
                .get_mut(&key.instance)
                .filter(|live| live.opened && live.failure.is_none())
                .context("Module is not ready")?;
            let settings_ui = live.module.settings_ui
                && (matches!(input.event, PluginEvent::Ui { .. })
                    || matches!(kind, CallKind::CanvasPassive(_)));
            if !settings_ui
                && !matches!(
                    kind,
                    CallKind::Service | CallKind::CredentialReply | CallKind::CredentialChange
                )
            {
                anyhow::ensure!(
                    live.settings_error.is_none(),
                    "Module could not use its saved settings"
                );
                anyhow::ensure!(
                    live.last
                        .as_ref()
                        .is_none_or(|last| last.2 == input.settings),
                    "Module is adopting updated settings"
                );
            }
            let worker = runtime.worker.as_ref().context("Plugin is recovering")?;
            anyhow::ensure!(
                runtime.calls.len()
                    < MAX_RUNTIME_CALLS + usize::from(kind == CallKind::CredentialReply),
                "Plugin is busy"
            );
            let version = if matches!(kind, CallKind::CredentialChange | CallKind::CredentialReply)
            {
                None
            } else {
                input
                    .document
                    .as_ref()
                    .map(|doc| (doc.opening, doc.revision))
            };
            let documents = input
                .open_documents
                .iter()
                .map(|doc| (doc.opening, doc.revision))
                .chain(version)
                .collect();
            let format_path = match &input.event {
                PluginEvent::Format { path, .. } => path.clone(),
                _ => None,
            };
            worker.post(Request::Call {
                id,
                instance: key.instance.clone(),
                input: Box::new(input),
                interactive: live.module.interactive,
            })?;
            if kind == CallKind::CredentialChange
                && let Some(change) = &credential
            {
                live.credential_seen
                    .insert(change.slot.clone(), change.revision);
            }
            runtime.calls.insert(
                id,
                Call {
                    instance: key.instance.clone(),
                    version,
                    documents,
                    format_path,
                    deadline: Instant::now() + timeout,
                    started: Instant::now(),
                    kind,
                    reply: reply.clone(),
                    credential_read: credential.filter(|_| kind == CallKind::CredentialReply),
                },
            );
            Ok(())
        };
        if let Err(error) = send() {
            if let Some(reply) = reply {
                let _ = reply.try_send(Err(error.to_string()));
            } else if !quiet
                && kind != CallKind::CredentialChange
                && kind != CallKind::CredentialReply
            {
                self.message(error.to_string());
            }
            return false;
        }
        true
    }

    pub(super) fn runtime_reply(&mut self, namespace: &str, runtime: &mut Runtime, reply: Reply) {
        let id = match &reply {
            Reply::Result { id, .. }
            | Reply::Closed { id, .. }
            | Reply::Error { id, .. }
            | Reply::DocumentResync { id } => *id,
            _ => return,
        };
        let Some(call) = runtime.calls.remove(&id) else {
            return;
        };
        if matches!(reply, Reply::DocumentResync { .. }) {
            if call.kind == CallKind::Refresh
                && let Some(live) = runtime.instances.get_mut(&call.instance)
            {
                live.document_base = None;
                live.pending_document = None;
                live.refresh = None;
                live.force_document_refresh = true;
                if self.view.sampling.contains(namespace) {
                    self.log(namespace, "Document base missing. Sending a full snapshot.");
                }
            }
            return;
        }
        if matches!(call.kind, CallKind::Open | CallKind::Refresh)
            && let Some(live) = runtime.instances.get_mut(&call.instance)
        {
            let pending = live.pending_document.take();
            let tracks_document = call.kind == CallKind::Open
                || live
                    .refresh
                    .as_ref()
                    .is_some_and(|refresh| refresh.document);
            if tracks_document && matches!(reply, Reply::Result { .. }) {
                live.document_base = pending;
            }
        }
        runtime.last_ms = call.started.elapsed().as_millis() as u64;
        let timing = match &reply {
            Reply::Result { timing, .. } => *timing,
            _ => None,
        };
        if timing.is_some() {
            runtime.last_worker_timing = timing;
        }
        if matches!(reply, Reply::Error { .. }) {
            runtime.errors += 1;
        } else {
            runtime.completed += 1;
        }
        if self.view.sampling.contains(namespace) {
            let mut message = format!(
                "Module {} · response in {} ms",
                call.instance.module, runtime.last_ms
            );
            if let Some(timing) = timing {
                message.push_str(&format!(
                    " · project {} · worker queue {} µs · compute {} µs",
                    call.instance.project, timing.queue_us, timing.compute_us
                ));
                if timing.compile_us != 0 || timing.initialize_us != 0 {
                    message.push_str(&format!(
                        " · compile {} µs · initialize {} µs",
                        timing.compile_us, timing.initialize_us
                    ));
                }
            }
            self.log(namespace, &message);
        }
        if call.kind == CallKind::PeerService && Instant::now() >= call.deadline {
            if let Some(reply) = call.reply {
                let _ = reply.try_send(Err(call.kind.timeout_message().into()));
            }
            return;
        }
        let key = Key {
            namespace: namespace.to_owned(),
            instance: call.instance.clone(),
        };
        let mut settings_reply = call.kind == CallKind::Open;
        if call.kind == CallKind::Refresh
            && let Some(live) = runtime.instances.get_mut(&call.instance)
            && let Some(refresh) = live.refresh.take()
            && !matches!(
                &reply,
                Reply::Error {
                    kind: ErrorKind::Busy,
                    ..
                }
            )
        {
            live.last = Some(refresh.observed);
            live.force_document_refresh &= !refresh.document;
            live.retry_settings &= !refresh.settings;
            settings_reply = refresh.settings;
        }
        let mut rejected_settings = false;
        let settings_current = settings_reply
            && Instant::now() <= call.deadline
            && self.lifecycle.wanted.contains_key(&key)
            && runtime
                .instances
                .get(&call.instance)
                .and_then(|live| live.last.as_ref())
                .is_some_and(|last| {
                    !self.config.settings_changed(
                        call.instance.project,
                        namespace,
                        &self.records[namespace].release.manifest,
                        &last.2,
                    )
                });
        if settings_current && let Some(live) = runtime.instances.get_mut(&call.instance) {
            match &reply {
                Reply::Result { output, .. } => {
                    let issue = output.settings_error.as_deref().map(|error| {
                        if error.trim().is_empty() {
                            "Plugin could not use the saved settings".into()
                        } else {
                            error.trim().chars().take(300).collect()
                        }
                    });
                    // Keep a restored rejection only while the same saved values are in use.
                    let restored = call.kind == CallKind::Open
                        && issue.is_none()
                        && live.settings_error.is_some()
                        && self.config.rejected_settings(
                            &key,
                            &self.releases[namespace].digest,
                            &self.records[namespace].release.manifest,
                        );
                    if !restored {
                        live.settings_error = issue;
                    }
                    rejected_settings = live.settings_error.is_some();
                }
                Reply::Error { .. } if call.kind == CallKind::Refresh => {
                    live.settings_error = Some("Plugin could not read the saved settings".into());
                }
                _ => {}
            }
            if let Some(last) = &live.last {
                self.config.note_settings_rejection(
                    &key,
                    &self.releases[namespace].digest,
                    &last.2,
                    live.settings_error.is_some(),
                );
            }
        }
        match reply {
            Reply::Closed { project, .. }
                if call.kind == CallKind::Close && call.instance.project == project =>
            {
                runtime
                    .instances
                    .retain(|instance, _| instance.project != project);
                runtime.closing.remove(&project);
                self.view
                    .surfaces
                    .retain(|key, _| key.namespace != namespace || key.instance.project != project);
                self.changed = true;
            }
            Reply::Error { kind, message, .. } => {
                if call.kind == CallKind::Open
                    && let Some(live) = runtime.instances.get_mut(&call.instance)
                {
                    live.open_job = false;
                    if kind != ErrorKind::Busy {
                        live.failure = Some(message.clone());
                    }
                }
                if let Some(reply) = call.reply {
                    let _ = reply.try_send(Err(message.clone()));
                }
                if !call.kind.is_service() {
                    // Provider errors can carry values from an invocation. Log its outcome only.
                    self.log(
                        namespace,
                        &format!(
                            "Module {} · request failed ({kind:?})",
                            call.instance.module
                        ),
                    );
                }
            }
            Reply::Result { output, .. } => {
                let Some(live) = runtime.instances.get_mut(&call.instance) else {
                    return;
                };
                if call.kind == CallKind::Open {
                    live.open_job = false;
                    live.opened = true;
                    self.log(namespace, &format!("Module {} ready", call.instance.module));
                }
                let current = self
                    .projects
                    .get(&call.instance.project)
                    .and_then(|project| {
                        project
                            .document
                            .as_ref()
                            .map(|doc| (doc.opening, doc.revision))
                    });
                let valid = self.lifecycle.wanted.contains_key(&key)
                    && self.projects.contains_key(&call.instance.project)
                    && (call.version.is_none() || call.version == current)
                    && Instant::now() <= call.deadline;
                if !valid || settings_reply && !settings_current {
                    if let Some(reply) = call.reply {
                        let _ = reply.try_send(Err("Plugin result is out of date".into()));
                    }
                    return;
                }
                let mut output = *output;
                // Definition jumps can open files outside the project. Only the native language
                // server, under the project's execution grant, supplies locations for that action.
                output.definitions.clear();
                // Hover details are returned by the native language-server path for this revision.
                output.hover = None;
                if let Some(change) = &call.credential_read {
                    self.credential_check_finished(&key, change, output.credential_rejected);
                    // Never publish free-form text from a callback which received a credential.
                    return;
                }
                if matches!(call.kind, CallKind::CanvasPassive(_))
                    || call.version.is_none()
                    || !self.records[namespace]
                        .grants
                        .contains(&Permission::EditDocument)
                {
                    output.edits.clear();
                    output.document_edits.clear();
                }
                if call.kind != CallKind::User {
                    output.document_edits.clear();
                }
                if call.kind != CallKind::User
                    || !self.records[namespace]
                        .grants
                        .contains(&Permission::ReadProject)
                    || !self.records[namespace]
                        .grants
                        .contains(&Permission::EditDocument)
                {
                    output.project_file_edits.clear();
                }
                if call.kind != CallKind::User
                    || !self.records[namespace]
                        .grants
                        .contains(&Permission::ChangeFiles)
                {
                    output.file_operations.clear();
                }
                if output
                    .document_edits
                    .iter()
                    .any(|group| !call.documents.contains(&(group.opening, group.revision)))
                {
                    if let Some(reply) = call.reply {
                        let _ = reply.try_send(Err(
                            "Plugin referred to a document version it was not given".into(),
                        ));
                    }
                    return;
                }
                if output.await_service {
                    let request = (output.services.len() == 1)
                        .then(|| output.services.pop().expect("one service"));
                    if !call.kind.is_service()
                        && !rejected_settings
                        && live.settings_error.is_none()
                        && let Some(request) = request
                    {
                        let settings_revision = live.last.as_ref().map_or_else(
                            || self.config.settings_revision(key.instance.project),
                            |last| last.4,
                        );
                        let pending = language::Pending::new(
                            key,
                            runtime.generation,
                            call,
                            request.token,
                            settings_revision,
                        );
                        if !self.begin_language_service(pending, request.operation) {
                            live.force_document_refresh = true;
                        }
                    } else if let Some(reply) = call.reply {
                        let _ = reply
                            .try_send(Err("Expected one awaited service per invocation".into()));
                    }
                    return;
                }
                if let Some(features) = output.document_features.take()
                    && live.settings_error.is_none()
                    && live
                        .module
                        .provides
                        .contains(&crate::plugins::package::Provider::Document)
                    && !rejected_settings
                    && !matches!(
                        call.kind,
                        CallKind::CredentialChange | CallKind::PeerService
                    )
                {
                    self.publish_presentation(&key, runtime.generation, call.version, features);
                }
                let surface = output.surface.take();
                let delta = output.canvas_delta.take();
                if live.module.interactive && (!rejected_settings || live.module.settings_ui) {
                    match (surface, delta) {
                        (Some(frame), None)
                            if self.installation.ready_candidates.contains(namespace) =>
                        {
                            live.startup_frame = Some(frame);
                        }
                        (Some(frame), None) => self.set_surface(&key, runtime.generation, frame),
                        (None, Some(delta))
                            if !self.installation.ready_candidates.contains(namespace) =>
                        {
                            self.set_canvas_delta(&key, runtime.generation, delta);
                        }
                        (Some(_), Some(_)) => self.reject_canvas_delta(&key, runtime.generation),
                        _ => {}
                    }
                }
                if !call.kind.is_service() && !rejected_settings {
                    for request in std::mem::take(&mut output.services).into_iter().take(4) {
                        self.service(
                            key.clone(),
                            runtime.generation,
                            call.kind == CallKind::User,
                            request,
                        );
                    }
                }
                if !output.message.is_empty()
                    && (matches!(
                        call.kind,
                        CallKind::Explicit | CallKind::Format | CallKind::User
                    ) || call.kind == CallKind::Continued && call.reply.is_some())
                {
                    self.message(output.message.chars().take(300).collect::<String>());
                }
                if let Some(reply) = call.reply {
                    let _ = reply.try_send(Ok(CallResult {
                        generation: runtime.generation,
                        version: call.version,
                        output,
                    }));
                }
            }
            _ => {}
        }
    }
}

pub(super) fn input_document<'a>(
    project: &'a Project,
    record: &Installed,
) -> Option<&'a Arc<DocumentSnapshot>> {
    project.document.as_ref().filter(|doc| {
        doc.text.len() <= MAX_DOC && record.grants.contains(&Permission::ReadDocument)
    })
}
