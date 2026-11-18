//! Route language-related Ex commands through the existing editor and plugin Session.

use super::Workspace;
use crate::grid::workspace::ex_commands::ExContinuation;
use gpui::{Context, Window};
use nanobug::{
    i18n,
    plugins::session::{LanguageServerOperation, LanguageServerTarget},
    vim::{Action, FiletypeCommand, LspCommand, Mode},
};

impl Workspace {
    /// Apply Vim filetype detection and publish any changed provider route to the host.
    pub(in crate::grid::workspace) fn run_filetype_command(&mut self, command: FiletypeCommand) {
        if let Some(project) = self.projects.active_project_mut() {
            project.editor.edit(|vim, register| {
                vim.apply_filetype_command(command, register);
                Action::None
            });
            if command == FiletypeCommand::Detect {
                self.sync_plugin_projects();
            }
        }
    }

    /// Give the Ex prompt the current Session's available language server and filetype identifiers.
    pub(in crate::grid::workspace) fn refresh_ex_language_completion(&mut self) {
        let Some(project) = self.projects.active_project_mut() else {
            return;
        };
        if project.editor.vim().mode == Mode::Command {
            let servers = self
                .plugins
                .view
                .language_server_targets(project.plugin_scope)
                .collect();
            let filetypes = self
                .plugins
                .view
                .installed
                .iter()
                .filter(|record| record.enabled)
                .flat_map(|record| &record.release.manifest.modules)
                .flat_map(|module| &module.document_filetypes)
                .cloned()
                .collect();
            project.editor.set_language_completion(servers, filetypes);
        }
    }

    /// Resolve one Ex operation against its originating Session and resume its optional script.
    pub(in crate::grid::workspace) fn control_ex_language_server(
        &mut self,
        command: LspCommand,
        name: Option<String>,
        resume: Option<ExContinuation>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.projects.active_project() else {
            return;
        };
        let doc = &project.editor.vim().doc;
        let origin = (
            project.plugin_scope,
            project.active_editor_id(),
            doc.opening,
        );
        let target = LanguageServerTarget {
            project: project.plugin_scope,
            document: doc.path.clone().map(|path| (doc.opening, path)),
            name,
        };
        let (operation, success) = match command {
            LspCommand::Enable => (LanguageServerOperation::Enable, "ex.lsp_enabled"),
            LspCommand::Disable => (LanguageServerOperation::Disable, "ex.lsp_disabled"),
            LspCommand::Stop => (LanguageServerOperation::Stop, "ex.lsp_stopped"),
            LspCommand::Restart => (LanguageServerOperation::Restart, "ex.lsp_restarted"),
        };
        let silence = self.input_mappings.ex_silence();
        self.sync_plugin_projects();
        let result = self
            .plugins
            .session
            .as_ref()
            .ok_or_else(|| i18n::text("ex.lsp_unavailable").to_string())
            .and_then(|session| {
                session
                    .control_language_server(operation, target)
                    .map_err(|error| error.to_string())
            });
        let receiver = match result {
            Ok(receiver) => receiver,
            Err(error) => {
                if silence == Some(true) {
                    self.input_mappings.remember_ex_error(&error);
                }
                let error = (silence != Some(true)).then_some(error);
                if let Some(resume) = resume {
                    resume(self, window, cx, error);
                } else if let Some(error) = error {
                    self.set_status(error);
                }
                return;
            }
        };
        cx.spawn_in(window, async move |view, cx| {
            let result = receiver
                .recv()
                .await
                .unwrap_or_else(|_| Err(i18n::text("ex.lsp_unavailable").to_string()));
            let _ = cx.update(|window, cx| {
                view.update(cx, |workspace, cx| {
                    if !workspace.projects.active_project().is_some_and(|project| {
                        (
                            project.plugin_scope,
                            project.active_editor_id(),
                            project.editor.vim().doc.opening,
                        ) == origin
                    }) {
                        return;
                    }
                    match result {
                        Ok(count) => {
                            if let Some(resume) = resume {
                                resume(workspace, window, cx, None);
                            } else if silence.is_none() {
                                workspace.set_status(i18n::format(success, &[("count", &count)]));
                            }
                        }
                        Err(error) => {
                            let error = i18n::format("ex.lsp_failed", &[("error", &error)]);
                            if silence == Some(true) {
                                workspace.input_mappings.remember_ex_error(&error);
                            }
                            let error = (silence != Some(true)).then_some(error);
                            if let Some(resume) = resume {
                                resume(workspace, window, cx, error);
                            } else if let Some(error) = error {
                                workspace.set_status(error);
                            }
                        }
                    }
                    workspace.sync(window, cx);
                })
            });
        })
        .detach();
    }
}
