//! Apply editor actions to retained buffers, files and the native window.

use crate::grid::windows::editor::EditResult;

use super::{
    Workspace,
    layout::{Focus, ShellWindow},
};
use gpui::{Context, Window};
use nanobug::{
    i18n,
    vim::{Action, Register, Vim},
};
use std::path::PathBuf;

impl Workspace {
    /// Open `path` and restore editor focus only after its window accepts the file.
    pub(super) fn open(&mut self, path: PathBuf) {
        if let Err(error) = self
            .projects
            .active
            .editor
            .open(path, self.scroll_epoch.elapsed().as_secs_f64())
        {
            self.projects.active.status = format!("{error:#}");
            return;
        }
        self.last_cursor_space = None;
        self.projects.active.focus = Focus::Editor;
        if self.projects.active.hidden_pane == Some(Focus::Editor) {
            self.projects.active.hidden_pane = None;
        }
        self.projects.active.status.clear();
    }

    /// Execute modal `action` in `window` through `cx`, preserving buffers on I/O errors.
    fn action(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        let active = self.projects.active.editor.active;
        match action {
            Action::None => {}
            Action::Open(name) => {
                let path = self.projects.active.root.join(name);
                if path.is_dir() {
                    self.open_project(&path, window, cx);
                } else {
                    self.open(path);
                }
            }
            Action::Save(name) => {
                let path = name.map(|name| self.projects.active.root.join(name));
                match self.projects.save_buffer(active, path) {
                    Ok(()) => {
                        self.projects.active.status = i18n::text("workspace.saved").into();
                        let _ = self.projects.active.manager.tree.refresh();
                    }
                    Err(error) => self.projects.active.status = format!("{error:#}"),
                }
            }
            Action::SaveClose => match self.projects.save_buffer(active, None) {
                Ok(()) => self.quit(false, window),
                Err(error) => self.projects.active.status = format!("{error:#}"),
            },
            Action::Close(force) => self.quit(force, window),
        }
    }

    /// Close the native `window`, checking all projects unless `force` was requested.
    pub(super) fn quit(&mut self, force: bool, window: &mut Window) {
        if !force && let Some(message) = self.projects.exit_message() {
            self.projects.active.status = message;
        } else {
            window.remove_window();
        }
    }

    /// Apply editor `input`, then coordinate its effects through `window` and `cx`.
    pub(super) fn run_vim(
        &mut self,
        input: impl FnOnce(&mut Vim, &mut Register) -> Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let edit = self.projects.active.editor.edit(input);
        self.apply_edit(edit, window, cx);
    }

    /// Reconcile `edit` with transfer history, then dispatch through `window` and `cx`.
    pub(super) fn apply_edit(
        &mut self,
        edit: EditResult,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(path) = edit.changed_path {
            for project in self.projects.iter_mut() {
                project.manager.transfers.invalidate(&path);
            }
        }
        self.action(edit.action, window, cx);
    }

    /// Request closure of `target` in `window`, asking about dirty buffers before hiding the editor.
    pub(super) fn close_grid_window(&mut self, target: ShellWindow, window: &mut Window) {
        self.prefix = None;
        self.projects.active.manager.paste_timer = None;
        self.projects.active.manager.count = 0;
        self.projects.active.manager.click = None;
        // Require a fresh hover before exposing the close button on an expanded pane.
        self.hovered_close = None;
        if target != ShellWindow::Pane(Focus::Editor) || !self.begin_editor_close() {
            self.finish_grid_close(target, window);
        }
    }

    /// Close a cleared `target` in `window`, guarding the last pane's exit across projects.
    pub(super) fn finish_grid_close(&mut self, target: ShellWindow, window: &mut Window) {
        match target {
            ShellWindow::ShortcutHelp => self.help.close(),
            ShellWindow::Pane(_) if self.projects.active.hidden_pane.is_some() => {
                self.quit(false, window)
            }
            ShellWindow::Pane(pane) => {
                if pane == Focus::Editor {
                    self.projects.active.editor.buffers[self.projects.active.editor.active]
                        .vim
                        .cancel_pending();
                    self.projects.active.editor.composition.clear();
                }
                self.projects.active.hidden_pane = Some(pane);
                self.projects.active.focus = pane.other();
                self.projects.active.status = i18n::text(match pane {
                    Focus::Manager => "workspace.manager_closed",
                    Focus::Editor => "workspace.editor_closed",
                })
                .into();
            }
        }
    }
}
