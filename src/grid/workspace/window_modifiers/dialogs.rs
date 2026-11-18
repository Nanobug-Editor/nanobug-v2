//! Offer existing editor and native file dialogs for Ex command modifiers.

use super::Workspace;
use crate::grid::{
    windows::editor::EditResult,
    workspace::{editor_close::CloseTarget, ex_files::edit_arguments},
};
use gpui::{Context, PromptLevel, Window};
use nanobug::{
    i18n,
    vim::{Action, FileCommand, FileOperation, WindowModifiers},
};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::SystemTime,
};

/// Identity and revision of an editor that is waiting for a dialog answer.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Origin {
    session: u64,
    pane: usize,
    opening: u64,
    revision: u64,
}

impl Origin {
    /// Capture the active editor view independently of its file path.
    fn current(workspace: &Workspace) -> Option<Self> {
        let project = workspace.projects.active_project()?;
        let doc = &project.editor.vim().doc;
        Some(Self {
            session: project.plugin_scope,
            pane: project.active_editor_id(),
            opening: doc.opening,
            revision: doc.revision,
        })
    }
}

/// Capture the destination state presented to the user before requesting confirmation.
fn file_stamp(path: &Path) -> Option<(PathBuf, u64, Option<SystemTime>, bool)> {
    let meta = path.metadata().ok()?;
    Some((
        path.canonicalize().ok()?,
        meta.len(),
        meta.modified().ok(),
        meta.permissions().readonly(),
    ))
}

impl Workspace {
    /// Execute the command in the active modifier context, including confirmation when required.
    pub(super) fn run_window_modified_command(
        &mut self,
        command: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.projects.active_project_mut() else {
            return;
        };
        let edit = project
            .editor
            .edit(|vim, register| vim.execute_ex_line(command, register));
        self.dispatch_window_modified_edit(edit, window, cx);
    }

    /// Confirm writes and application exits through the same native controls as other actions.
    fn dispatch_window_modified_edit(
        &mut self,
        mut edit: EditResult,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_ex_window_modifiers.confirm {
            if matches!(edit.action, Action::QuitAll(false)) {
                edit.action = Action::None;
                self.apply_edit(edit, window, cx);
                self.request_close(CloseTarget::Quit, window, cx);
                self.sync(window, cx);
                return;
            }
            if let Some(path) = self.confirmation_destination(&edit.action) {
                let action = std::mem::replace(&mut edit.action, Action::None);
                self.apply_edit(edit, window, cx);
                self.confirm_ex_write(path, action, window, cx);
                return;
            }
        }
        self.apply_edit(edit, window, cx);
    }

    /// Return the destination when normal Ex writing would fail for a confirmable reason.
    fn confirmation_destination(&self, action: &Action) -> Option<PathBuf> {
        let (name, rename) = match action {
            Action::WriteFile {
                name, force: false, ..
            } => (name.as_deref(), false),
            Action::SaveAs { path, force: false } => (Some(path.as_str()), true),
            Action::SaveClose => (None, false),
            _ => return None,
        };
        let vim = self.projects.active().editor.vim();
        let path = match name {
            Some(name) => self.file_operand(name)?,
            None => vim.doc.path.clone()?,
        };
        let same = vim.doc.path.as_ref() == Some(&path);
        let existing_copy =
            !same && path.is_file() && (rename || !self.input_mappings.options().writeany);
        (existing_copy || same && vim.file_read_only()).then_some(path)
    }

    /// Prompt before forcing a write, rechecking the originating document and destination first.
    fn confirm_ex_write(
        &mut self,
        path: PathBuf,
        action: Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let origin = Origin::current(self);
        let stamp = file_stamp(&path);
        let detail = i18n::format("ex.confirm_write_detail", &[("path", &path.display())]);
        let answer = window.prompt(
            PromptLevel::Warning,
            i18n::text("ex.confirm_write_title"),
            Some(&detail),
            &[
                i18n::text("ex.confirm_cancel"),
                i18n::text("ex.confirm_write_accept"),
            ],
            cx,
        );
        cx.spawn_in(window, async move |view, cx| {
            if answer.await.ok() != Some(1) {
                return;
            }
            let _ = cx.update(|window, cx| {
                let _ = view.update(cx, |workspace, cx| {
                    if Origin::current(workspace) != origin || file_stamp(&path) != stamp {
                        workspace.set_status(i18n::text("document.disk_changed").into());
                        workspace.sync(window, cx);
                        return;
                    }
                    let name = escaped_path(&path);
                    let forced = match action {
                        Action::WriteFile { close, .. } => Action::WriteFile {
                            name: Some(name),
                            force: true,
                            close,
                        },
                        Action::SaveAs { .. } => Action::SaveAs {
                            path: name,
                            force: true,
                        },
                        Action::SaveClose => Action::WriteFile {
                            name: Some(name),
                            force: true,
                            close: true,
                        },
                        _ => return,
                    };
                    workspace.action(forced, window, cx);
                    workspace.sync(window, cx);
                });
            });
        })
        .detach();
    }

    /// Select a pathname using the existing grid chooser or the native Save As prompt.
    pub(super) fn browse_ex_command(
        &mut self,
        command: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let (head, arguments) = command
            .split_once(char::is_whitespace)
            .unwrap_or((command, ""));
        if arguments.trim().is_empty() && matches!(head, "se" | "set") {
            self.run_window_modified_command("options", window, cx);
            return true;
        }
        if arguments.trim().is_empty() && !head.is_empty() && "oldfiles".starts_with(head) {
            self.browse_old_files(window, cx);
            return true;
        }
        let body =
            head.trim_start_matches(|ch: char| ch.is_ascii_digit() || ",.$%+-;".contains(ch));
        let word = body.trim_end_matches('!');
        let Some((canonical, save)) = browse_command(word) else {
            return false;
        };
        let Some((after, path)) = edit_arguments(arguments) else {
            self.set_status(i18n::format(
                "editor.invalid_ex_argument",
                &[("argument", &arguments)],
            ));
            return true;
        };
        let initial = if path.is_empty() {
            None
        } else {
            self.file_operand(path)
        };
        if !path.is_empty() && initial.is_none() {
            self.set_status(i18n::format(
                "editor.invalid_ex_argument",
                &[("argument", &path)],
            ));
            return true;
        }
        let origin = Origin::current(self);
        let mut modifiers = self.active_ex_window_modifiers.clone();
        modifiers.browse = false;
        let head = format!(
            "{}{}{}",
            &head[..head.len() - body.len()],
            canonical,
            &body[word.len()..]
        );
        let after = after.map(str::to_owned);
        let finish = move |workspace: &mut Workspace,
                           selection: Option<PathBuf>,
                           window: &mut Window,
                           cx: &mut Context<Workspace>| {
            let Some(path) = selection.filter(|_| Origin::current(workspace) == origin) else {
                return;
            };
            workspace.apply_browsed_path(&head, &path, after.as_deref(), modifiers, window, cx);
        };
        if save {
            let vim = self.projects.active().editor.vim();
            let directory = initial
                .as_deref()
                .and_then(|path| {
                    if path.is_dir() {
                        Some(path)
                    } else {
                        path.parent()
                    }
                })
                .or_else(|| vim.doc.path.as_deref().and_then(Path::parent))
                .unwrap_or_else(|| self.ex_current_dir());
            let name = initial
                .as_deref()
                .filter(|path| !path.is_dir())
                .or(vim.doc.path.as_deref())
                .and_then(Path::file_name)
                .and_then(|name| name.to_str());
            let picker = cx.prompt_for_new_path(directory, name);
            cx.spawn_in(window, async move |view, cx| {
                let result = picker.await;
                let _ = cx.update(|window, cx| {
                    let _ = view.update(cx, |workspace, cx| match result {
                        Ok(Ok(path)) => finish(workspace, path, window, cx),
                        Ok(Err(error)) => workspace.set_status(format!("{error:#}")),
                        Err(_) => {}
                    });
                });
            })
            .detach();
        } else {
            let initial = initial.or_else(|| Some(self.ex_current_dir().to_owned()));
            self.choose_file(initial, None, &[], finish, window, cx);
        }
        true
    }

    /// Choose an existing file from portable numbered-mark history without listing directories.
    fn browse_old_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut seen = HashSet::new();
        let marks = self
            .numbered_marks
            .iter()
            .filter(|mark| mark.path.is_file() && seen.insert(&mark.path))
            .cloned()
            .collect::<Vec<_>>();
        if marks.is_empty() {
            self.set_status(i18n::text("ex.browse_no_oldfiles").into());
            return;
        }
        let choices = marks
            .iter()
            .map(|mark| mark.path.to_string_lossy().replace('&', "&&"))
            .collect::<Vec<_>>();
        let labels = std::iter::once(i18n::text("ex.confirm_cancel"))
            .chain(choices.iter().map(String::as_str))
            .collect::<Vec<_>>();
        let origin = Origin::current(self);
        let answer = window.prompt(
            PromptLevel::Info,
            i18n::text("ex.browse_oldfiles"),
            None,
            &labels,
            cx,
        );
        cx.spawn_in(window, async move |view, cx| {
            let Some(index) = answer.await.ok().and_then(|index| index.checked_sub(1)) else {
                return;
            };
            let Some(mark) = marks.get(index) else {
                return;
            };
            let _ = cx.update(|window, cx| {
                let _ = view.update(cx, |workspace, cx| {
                    if Origin::current(workspace) != origin {
                        return;
                    }
                    if let Err(error) = workspace.jump_to_file(&mark.path, true, |doc| {
                        Some(doc.offset(mark.row, mark.column))
                    }) {
                        workspace.set_status(format!("{error:#}"));
                    }
                    workspace.sync(window, cx);
                });
            });
        })
        .detach();
    }

    /// Bind a selected filename to a validated command's typed path parameter.
    fn apply_browsed_path(
        &mut self,
        head: &str,
        path: &Path,
        after: Option<&str>,
        modifiers: WindowModifiers,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let argument = escaped_path(path);
        let probe = format!("{head} nanobug-file-operand");
        self.with_ex_window_modifiers(modifiers, |workspace| {
            let edit = workspace
                .projects
                .active_mut()
                .editor
                .edit(|vim, register| {
                    let mut action = vim.execute_ex_line(&probe, register);
                    match &mut action {
                        Action::Files(command) => command.arguments = argument,
                        Action::Open(_) => {
                            action = Action::Files(FileCommand {
                                operation: FileOperation::Edit,
                                count: None,
                                force: head.ends_with('!'),
                                arguments: argument,
                            })
                        }
                        Action::ReadFile { path, .. }
                        | Action::PreviewOpen(path)
                        | Action::SaveAs { path, .. } => *path = argument,
                        Action::WriteFile { name, .. }
                        | Action::Split { file: name, .. }
                        | Action::TabNew(name)
                        | Action::Source { file: name, .. } => *name = Some(argument),
                        Action::Close(_) => {} // :xit may close an unchanged buffer without writing.
                        _ => return Action::None,
                    }
                    action
                });
            workspace.dispatch_window_modified_edit(edit, window, cx);
        });
        if self
            .projects
            .active_project()
            .is_some_and(|project| project.status.is_empty())
        {
            self.after_file_load(after, window, cx);
        }
        self.sync(window, cx);
    }
}

/// Select the Save As or existing-file chooser for supported single-path Ex commands.
fn browse_command(word: &str) -> Option<(&'static str, bool)> {
    const SAVE: &[(&str, usize)] = &[
        ("write", 1),
        ("wq", 2),
        ("xit", 1),
        ("exit", 3),
        ("update", 2),
        ("saveas", 3),
    ];
    const OPEN: &[(&str, usize)] = &[
        ("edit", 1),
        ("ex", 2),
        ("view", 3),
        ("sview", 2),
        ("split", 2),
        ("vsplit", 2),
        ("read", 1),
        ("pedit", 3),
        ("source", 2),
        ("tabedit", 4),
        ("tabnew", 6),
    ];
    SAVE.iter()
        .map(|&(name, length)| (name, length, true))
        .chain(OPEN.iter().map(|&(name, length)| (name, length, false)))
        .find_map(|(name, length, save)| {
            (word.len() >= length && name.starts_with(word)).then_some((name, save))
        })
}

/// Preserve embedded spaces in the selected pathname without reparsing an Ex script.
fn escaped_path(path: &Path) -> String {
    path.to_string_lossy().replace(' ', "\\ ")
}
