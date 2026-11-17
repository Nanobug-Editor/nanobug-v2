//! Check named Session buffers against disk and confirm a conflicting reload.

use super::{Action, Context, FileCommand, Window, Workspace, i18n, resolve_buffer};
use gpui::PromptLevel;
use std::path::PathBuf;

impl Workspace {
    /// Compare the disk version of all named buffers or the selected buffer in this Session.
    pub(super) fn ex_checktime(
        &mut self,
        command: FileCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let selector = if command.arguments.is_empty() {
            command.count.map(|count| count.to_string())
        } else {
            Some(command.arguments)
        };
        let project = self.projects.active_mut();
        let number = if let Some(selector) = &selector {
            let Some(number) = resolve_buffer(project, selector) else {
                self.set_status(i18n::text("vim_bracket.no_buffer").into());
                return;
            };
            Some(number)
        } else {
            None
        };
        let mut warnings = Vec::new();
        let mut prompts = Vec::new();
        let mut edits = Vec::new();
        for (pane, index) in project.session_buffers() {
            let Some(editor) = project.editor_by_id_mut(pane) else {
                continue;
            };
            let doc = &editor.buffers()[index].vim.doc;
            let Some(path) = doc
                .path
                .clone()
                .filter(|_| number.is_none_or(|number| doc.buffer_number() == number))
            else {
                continue;
            };
            let probe = (doc.buffer_number(), doc.revision, path.clone());
            edits.push(editor.edit_buffer(index, |vim, _| {
                match vim.doc.check_external_change() {
                    Ok(None) => (),
                    Ok(Some(true)) => warnings.push(i18n::format(
                        "ex.reload_deleted",
                        &[("path", &path.display())],
                    )),
                    Ok(Some(false)) if vim.autoread() && !vim.doc.modified() => {
                        if let Err(error) = vim.doc.reload_file() {
                            warnings.push(format!("{}: {error}", path.display()));
                        }
                    }
                    Ok(Some(false)) => prompts.push(probe),
                    Err(error) => warnings.push(format!("{}: {error}", path.display())),
                }
                Action::None
            }));
        }
        for edit in edits {
            self.apply_edit(edit, window, cx);
        }
        self.set_status(warnings.join("  "));
        self.sync(window, cx);
        if !prompts.is_empty() {
            self.checktime_confirm(prompts, window, cx);
        }
    }

    /// Offer reload for each changed file, revalidating the buffer revision after the prompt.
    fn checktime_confirm(
        &self,
        prompts: Vec<(u64, u64, PathBuf)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |weak, cx| {
            for (number, revision, path) in prompts {
                let description = i18n::format("ex.reload_detail", &[("path", &path.display())]);
                let Ok(answer) = cx.update(|window, cx| {
                    window.prompt(
                        PromptLevel::Warning,
                        i18n::text("ex.reload_title"),
                        Some(description.as_str()),
                        &[i18n::text("ex.reload_keep"), i18n::text("ex.reload_accept")],
                        cx,
                    )
                }) else {
                    return;
                };
                if answer.await.ok() != Some(1) {
                    continue;
                }
                let _ = cx.update(|window, cx| {
                    weak.update(cx, |workspace, cx| {
                        let Some(project) = workspace.projects.active_project_mut() else {
                            return;
                        };
                        let target =
                            project
                                .session_buffers()
                                .into_iter()
                                .find(|&(pane, index)| {
                                    let doc = &project
                                        .editor_by_id(pane)
                                        .expect("retained editor")
                                        .buffers()[index]
                                        .vim
                                        .doc;
                                    doc.buffer_number() == number
                                        && doc.revision == revision
                                        && doc.path.as_ref() == Some(&path)
                                });
                        let Some((pane, index)) = target.filter(|_| path.is_file()) else {
                            workspace.set_status(i18n::text("document.disk_changed").into());
                            return;
                        };
                        let editor = project.editor_by_id_mut(pane).expect("retained editor");
                        let mut result = Ok(());
                        let edit = editor.edit_buffer(index, |vim, _| {
                            result = vim.doc.reload_file();
                            Action::None
                        });
                        workspace.apply_edit(edit, window, cx);
                        if let Err(error) = result {
                            workspace.set_status(format!("{error:#}"));
                        }
                        workspace.sync(window, cx);
                    })
                });
            }
        })
        .detach();
    }
}
