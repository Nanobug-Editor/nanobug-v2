//! Workspace adapter for session link.

use super::*;
use crate::grid::workspace::layout::Focus;
use nanobug::plugins::session::LanguageServerPhase;

impl Workspace {
    /// Return the server phase for the visible document or its open project.
    pub(in crate::grid::workspace) fn active_language_server_status(
        &self,
    ) -> Option<(&'static str, LanguageServerPhase)> {
        let project = self.projects.active_project()?;
        if project.hidden_pane == Some(Focus::Editor) {
            return None;
        }
        let document = &project.editor.vim().doc;
        let status = self
            .plugins
            .view
            .language_servers
            .get(&project.plugin_scope)?;
        status
            .document
            .as_ref()
            .is_none_or(|(opening, path)| {
                project.focus == Focus::Editor
                    && document.opening == *opening
                    && document.path.as_ref() == Some(path)
            })
            .then_some((status.name, status.phase))
    }

    /// Drop retired atlas entries only after this frame replaces the one that could use them.
    pub(in crate::grid::workspace) fn retire_canvas_images_after_paint(
        &mut self,
        window: &Window,
        cx: &Context<Self>,
    ) {
        if self.plugins.retired_images.is_empty() || self.plugins.image_retirement_queued {
            return;
        }
        let retiring = std::mem::take(&mut self.plugins.retired_images);
        self.plugins.image_retirement_queued = true;
        let weak = cx.weak_entity();
        window.on_next_frame(move |window, cx| {
            let _ = weak.update(cx, |workspace, cx| {
                workspace.plugins.image_retirement_queued = false;
                for (id, image) in retiring {
                    if !workspace.plugins.images.contains_key(&id) {
                        let _ = window.drop_image(image);
                    }
                }
                if !workspace.plugins.retired_images.is_empty() {
                    cx.notify();
                }
            });
        });
    }

    /// Record the document revision after the workspace has emitted its scene primitives.
    pub(in crate::grid::workspace) fn trace_document_scene(&mut self) {
        let Some(trace) = &self.plugins.trace else {
            return;
        };
        let Some(project) = self.projects.active_project() else {
            return;
        };
        let document = &project.editor.vim().doc;
        let revision = (document.opening, document.revision);
        if self.plugins.traced_scene.replace(revision) != Some(revision) {
            let body = project
                .editor
                .text_area(self.active_editor_area())
                .pixels(self.metrics);
            trace.mark(
                "document_scene",
                serde_json::json!({
                    "revision": revision.1, "opening": revision.0,
                    "text_origin": [f32::from(body.origin.x), f32::from(body.origin.y)],
                    "cell": [self.metrics.cw, self.metrics.lh],
                }),
            );
        }
    }

    /// The supervisor starts on the first frame boundary, even on a start page with no input.
    pub(in crate::grid::workspace) fn plugin_after_frame(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(options) = self.plugins.options.take() else {
            return;
        };
        let view = cx.weak_entity();
        window.on_next_frame(move |window, cx| {
            let _ = view.update(cx, |workspace, cx| {
                if let Some(trace) = &workspace.plugins.trace {
                    trace.mark("core_ready", serde_json::json!({ "editable": workspace.projects.active_project().is_some() }));
                }
                let (session, changes) = Session::start(options);
                workspace.plugins.session = Some(session);
                workspace.sync_plugin_projects();
                if workspace.plugins.open {
                    workspace.plugin_refresh(window, cx);
                }
                cx.spawn_in(window, async move |view, cx| {
                    while changes.recv().await.is_ok() {
                        if cx
                            .update(|window, cx| {
                                view.update(cx, |workspace, cx| {
                                    let previous_server = workspace.active_language_server_status();
                                    workspace.plugins.update_view();
                                    let restored_surface = workspace.reattach_project_surface();
                                    let server_changed =
                                        previous_server != workspace.active_language_server_status();
                                    if let Some(session) = &workspace.plugins.session {
                                        for (&id, request) in &workspace.plugins.view.text_measurements {
                                            let value = request.lines.iter().zip(request.prepared.iter()).map(|(line, prepared)| {
                                                prepared.clone().unwrap_or_else(|| canvas::measure(window, line, style(cx).font))
                                            }).collect();
                                            let _ = session.send(Action::TextMeasured(id, serde_json::Value::Array(value)));
                                        }
                                    }
                                    if let Some(trace) = &workspace.plugins.trace {
                                        let view = &workspace.plugins.view;
                                        let ready = view.modules.values().filter(|state| **state == nanobug::plugins::session::State::Ready).count();
                                        trace.mark("session_update", serde_json::json!({
                                            "installed": view.installed.len(), "ready": ready, "workers": view.workers.len(),
                                            "worker_pids": view.workers.values().copied().collect::<Vec<_>>(),
                                            "modules": view.modules.len(), "surfaces": view.surfaces.len(),
                                            "drawing_revision": workspace.plugins.surface.key.as_ref()
                                                .and_then(|key| view.surfaces.get(key))
                                                .map(|surface| surface.revision),
                                            "review": view.review.is_some(), "installing": view.installing,
                                            "unmatched": view.modules.values().filter(|state| **state == nanobug::plugins::session::State::Unmatched).count()
                                        }));
                                    }
                                    workspace.plugins.collect_retired_images();
                                    if workspace.refresh_document_presentation() {
                                        workspace.repaint([nanobug::animation::scroll::ScrollInput::Advance; 3], window, cx);
                                    } else {
                                        workspace.sync_plugin_projects();
                                        if server_changed {
                                            workspace.refresh_footer(cx);
                                            cx.notify();
                                        }
                                    }
                                    if workspace.plugins.open || workspace.plugins.surface.key.is_some()
                                        || !workspace.plugins.retired_images.is_empty()
                                    {
                                        if restored_surface {
                                            workspace.sync_text_input(window, cx);
                                        }
                                        workspace.update_cursor(window, cx);
                                        cx.notify();
                                    }
                                })
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                })
                .detach();
            });
        });
    }

    /// Publish project identity and document changes once per version, without filesystem I/O.
    pub(in crate::grid::workspace) fn sync_plugin_projects(&mut self) {
        if self.plugins.session.is_none() {
            return;
        }
        self.sync_project_plugin_windows();
        let active = self
            .projects
            .active_project()
            .map(|project| project.plugin_scope);
        if let Some(trace) = &self.plugins.trace
            && let Some(doc) = self
                .projects
                .active_project()
                .map(|project| &project.editor.vim().doc)
            && self.plugins.traced_doc != Some((doc.opening, doc.revision))
        {
            self.plugins.traced_doc = Some((doc.opening, doc.revision));
            trace.mark(
                "document_revision",
                serde_json::json!({ "revision": doc.revision, "bytes": doc.text.len() }),
            );
        }
        let document_readers = self
            .plugins
            .view
            .installed
            .iter()
            .any(|record| record.enabled && record.grants.contains(&Permission::ReadDocument));
        let projects = self
            .projects
            .ordered()
            .map(|project| {
                let id = project.plugin_scope;
                let doc = &project.editor.vim().doc;
                let document = (document_readers && doc.text.len() <= 120 * 1024).then(|| {
                    let snapshot = self
                        .plugins
                        .snapshots
                        .entry(id)
                        .or_insert_with(|| Arc::new(snapshot_from(doc)));
                    if snapshot.opening != doc.opening
                        || snapshot.revision != doc.revision
                        || snapshot.path != doc.path
                    {
                        *snapshot = Arc::new(snapshot_from(doc));
                    }
                    Arc::clone(snapshot)
                });
                let mut remaining = 512 * 1024;
                let open_documents = if document_readers {
                    project
                        .editor
                        .buffers()
                        .iter()
                        .filter_map(|buffer| {
                            let other = &buffer.vim.doc;
                            if other.opening == doc.opening
                                || other.text.len() > 120 * 1024
                                || other.text.len() > remaining
                            {
                                return None;
                            }
                            remaining -= other.text.len();
                            let snapshot = self
                                .plugins
                                .open_snapshots
                                .entry((id, other.opening))
                                .or_insert_with(|| Arc::new(snapshot_from(other)));
                            if snapshot.revision != other.revision || snapshot.path != other.path {
                                *snapshot = Arc::new(snapshot_from(other));
                            }
                            Some(Arc::clone(snapshot))
                        })
                        .take(32)
                        .collect()
                } else {
                    Vec::new()
                };
                Project {
                    id,
                    root: project.root.clone(),
                    document,
                    open_documents,
                }
            })
            .collect::<Vec<_>>();
        self.plugins
            .snapshots
            .retain(|id, _| document_readers && projects.iter().any(|project| project.id == *id));
        self.plugins.open_snapshots.retain(|(id, opening), _| {
            projects.iter().any(|project| {
                project.id == *id
                    && project
                        .open_documents
                        .iter()
                        .any(|snapshot| snapshot.opening == *opening)
            })
        });
        let dialects = self
            .projects
            .ordered()
            .filter_map(|project| {
                let dialect = project.editor.buffer().sql_dialect()?;
                Some((project.plugin_scope, dialect.to_owned()))
            })
            .collect();
        let filetypes = self
            .projects
            .ordered()
            .flat_map(|project| {
                project.editor.buffers().iter().filter_map(|buffer| {
                    let filetype = buffer.vim.filetype_override()?;
                    Some(((project.plugin_scope, buffer.vim.doc.opening), filetype))
                })
            })
            .collect();
        if self.plugins.active != active
            || self.plugins.projects != projects
            || self.plugins.sql_dialects != dialects
            || self.plugins.document_filetypes != filetypes
        {
            if self
                .plugins
                .surface
                .key
                .as_ref()
                .is_some_and(|key| Some(key.instance.project) != active)
            {
                self.plugins.surface.clear_attachment();
                self.plugins.composition.clear();
            }
            self.plugins.active = active;
            self.plugins.projects = projects;
            self.plugins.sql_dialects = dialects;
            self.plugins.document_filetypes = filetypes;
            self.plugins
                .session
                .as_ref()
                .expect("plugin session")
                .projects_with_languages(
                    active,
                    self.plugins.projects.clone(),
                    self.plugins.sql_dialects.clone(),
                    self.plugins.document_filetypes.clone(),
                );
        }
        self.reattach_project_surface();
    }

    /// Observe native composition length without recording text or credential metadata.
    pub(in crate::grid::workspace) fn trace_plugin_preedit(&self, utf16_units: usize) {
        if self.plugins.accepts_text()
            && !self
                .plugins
                .manager
                .prompt
                .as_ref()
                .is_some_and(|input| matches!(input.kind, Prompt::Credential { .. }))
            && let Some(trace) = &self.plugins.trace
        {
            trace.mark(
                "plugin_preedit",
                serde_json::json!({
                    "units": utf16_units,
                    "surface": self.plugins.surface.focused,
                }),
            );
        }
    }
}
