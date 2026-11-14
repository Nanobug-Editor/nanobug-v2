//! Workspace adapter for closing.

use super::*;

impl Workspace {
    /// Finish saving confirmed plugin inputs before starting a project/window close.
    pub(in crate::grid::workspace) fn prepare_plugin_close(
        &mut self,
        target: CloseTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.plugins.file_busy {
            self.plugins.file_close = Some(target);
            return true;
        }
        if std::mem::take(&mut self.plugins.skip_draft_close)
            || matches!(target, CloseTarget::Editor | CloseTarget::Buffer(_))
        {
            return false;
        }
        if let Some(input) = self.plugins.manager.prompt.take_if(
            |input| matches!(&input.kind, Prompt::Credential { value, .. } if !value.is_empty()),
        ) && let Prompt::Credential {
            namespace,
            slot,
            value,
        } = input.kind
        {
            self.review_plugin_credential(namespace, slot, value);
        }
        if self.plugins.credential_draft.is_some() || self.plugins.credential_waiting {
            self.plugins.open = true;
        }
        if self.plugins.credential_draft.is_some() || self.plugins.credential_waiting {
            self.plugins.credential_close = Some(target);
            self.plugin_refresh(window, cx);
            return true;
        }
        if self.plugins.close_draft.is_some() {
            return true;
        }
        let project = self
            .projects
            .active_project()
            .map(|project| project.plugin_scope);
        if self.plugins.session.is_none() || target == CloseTarget::Tab && project.is_none() {
            return false;
        }
        self.plugins.open = true;
        self.plugins.surface.focused = false;
        self.plugins.composition.clear();
        self.plugins.manager.prompt = None;
        self.plugins.manager.help = false;
        self.plugins.manager.selection = 0;
        self.plugins.close_draft = Some(DraftClose {
            target,
            project: project.filter(|_| target == CloseTarget::Tab),
            draining: true,
            waiting: false,
            error: None,
        });
        self.advance_plugin_close(window, cx);
        true
    }

    pub(in crate::grid::workspace) fn advance_plugin_close(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(close) = self
            .plugins
            .close_draft
            .as_ref()
            .filter(|close| close.draining)
        {
            let (reply, receive) = async_channel::bounded(1);
            self.wait_draft_close(
                Action::DrainForClose {
                    project: close.project,
                    reply,
                },
                receive,
                window,
                cx,
            );
        } else {
            self.write_close_draft(None, window, cx);
        }
    }

    pub(in crate::grid::workspace) fn write_close_draft(
        &mut self,
        export: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (reply, receive) = async_channel::bounded(1);
        let action = if let Some(path) = export {
            Action::ExportDrafts {
                path,
                project: self
                    .plugins
                    .close_draft
                    .as_ref()
                    .and_then(|close| close.project),
                reply,
            }
        } else if let Some(project) = self
            .plugins
            .close_draft
            .as_ref()
            .and_then(|close| close.project)
        {
            Action::FlushDrafts { project, reply }
        } else {
            Action::FlushForExit(reply)
        };
        self.wait_draft_close(action, receive, window, cx);
    }

    pub(in crate::grid::workspace) fn wait_draft_close(
        &mut self,
        action: Action,
        receive: async_channel::Receiver<Result<(), String>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.plugins.send(action) {
            return;
        }
        if let Some(close) = &mut self.plugins.close_draft {
            close.waiting = true;
            close.error = None;
        }
        self.plugin_refresh(window, cx);
        cx.spawn_in(window, async move |workspace, cx| {
            let result = receive
                .recv()
                .await
                .unwrap_or_else(|_| Err("Plugin draft service stopped".into()));
            let _ = cx.update(|window, cx| {
                workspace.update(cx, |workspace, cx| {
                    if let Some(close) = &mut workspace.plugins.close_draft {
                        close.waiting = false;
                        match result {
                            Ok(()) if close.draining => {
                                close.draining = false;
                                workspace.write_close_draft(None, window, cx);
                            }
                            Ok(()) => workspace.finish_draft_close(window, cx),
                            Err(error) => {
                                close.error = Some(error);
                                workspace.plugin_refresh(window, cx);
                            }
                        }
                    }
                })
            });
        })
        .detach();
    }

    pub(in crate::grid::workspace) fn finish_draft_close(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(close) = self.plugins.close_draft.take() else {
            return;
        };
        self.plugins.open = false;
        self.plugins.skip_draft_close = true;
        self.request_close(close.target, window, cx);
    }
}
