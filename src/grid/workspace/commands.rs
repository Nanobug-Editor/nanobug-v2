//! Apply editor actions to retained buffers, files and the native window.

use crate::grid::windows::editor::EditResult;

use super::{
    Workspace,
    editor_close::CloseTarget,
    layout::{Focus, ShellWindow},
};
use gpui::{Context, Window};
use nanobug::{
    i18n,
    vim::{Action, Register, Vim},
};
use std::path::PathBuf;

impl Workspace {
    /// Open `path` in the current project or in its own standalone tab.
    pub(super) fn open(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let path = path.canonicalize().unwrap_or(path);
        if !self.projects.manager_available() || self.projects.file_index(&path).is_some() {
            if let Err(error) = self.open_standalone_file(path, window, cx) {
                self.set_status(format!("{error:#}"));
            }
            return;
        }
        let project = self.projects.active_mut();
        if let Err(error) = project
            .editor
            .open(path, self.scroll_epoch.elapsed().as_secs_f64())
        {
            project.status = format!("{error:#}");
            return;
        }
        self.last_cursor_space = None;
        project.focus = Focus::Editor;
        if project.hidden_pane == Some(Focus::Editor) {
            project.hidden_pane = None;
        }
        project.status.clear();
    }

    /// Execute modal `action` in `window` through `cx`, preserving buffers on I/O errors.
    fn action(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        let active = self.projects.active().editor.active;
        let tab = self.projects.selected;
        match action {
            Action::None => {}
            Action::Open(name) => {
                let path = self.projects.active().root.join(name);
                if path.is_dir() {
                    self.open_project(&path, window, cx);
                } else {
                    self.open(path, window, cx);
                }
            }
            Action::Save(name) => {
                let path = name.map(|name| self.projects.active().root.join(name));
                match self.projects.save_buffer(tab, active, path) {
                    Ok(()) => {
                        let project = self.projects.active_mut();
                        project.status = i18n::text("workspace.saved").into();
                        if let Some(manager) = &mut project.manager {
                            let _ = manager.tree.refresh();
                        }
                    }
                    Err(error) => self.set_status(format!("{error:#}")),
                }
            }
            Action::SaveClose => match self.projects.save_buffer(tab, active, None) {
                Ok(()) => self.request_close(CloseTarget::Quit, window, cx),
                Err(error) => self.set_status(format!("{error:#}")),
            },
            Action::Close(false) => self.request_close(CloseTarget::Quit, window, cx),
            Action::Close(true) => window.remove_window(),
        }
    }

    /// Apply editor `input`, then coordinate its effects through `window` and `cx`.
    pub(super) fn run_vim(
        &mut self,
        input: impl FnOnce(&mut Vim, &mut Register) -> Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let edit = self.projects.active_mut().editor.edit(input);
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
            for manager in self
                .projects
                .iter_mut()
                .filter_map(|project| project.manager.as_mut())
            {
                manager.transfers.invalidate(&path);
            }
        }
        self.action(edit.action, window, cx);
    }

    /// Close `target` in `window` through `cx`, resolving dirty buffers in its closing scope.
    pub(super) fn close_grid_window(
        &mut self,
        target: ShellWindow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.prefix = None;
        if let Some(manager) = self
            .projects
            .active_project_mut()
            .and_then(|project| project.manager.as_mut())
        {
            manager.paste_timer = None;
            manager.count = 0;
            manager.click = None;
        }
        // Require a fresh hover before exposing the close button on an expanded pane.
        self.hovered_controls = None;
        match target {
            ShellWindow::ShortcutHelp => self.help.close(),
            ShellWindow::FilePicker => {
                if !self.picker_operation_pending() {
                    self.file_picker = None;
                    self.wheel = 0.;
                }
            }
            ShellWindow::Pane(Focus::Editor) if !self.projects.manager_available() => {
                self.request_close(CloseTarget::Project, window, cx)
            }
            ShellWindow::Pane(_) if self.projects.active().hidden_pane.is_some() => {
                self.request_close(CloseTarget::Quit, window, cx)
            }
            ShellWindow::Pane(Focus::Editor) => self.request_close(CloseTarget::Editor, window, cx),
            ShellWindow::Pane(Focus::Manager) => self.hide_pane(Focus::Manager),
        }
    }

    /// Hide `pane` after any close confirmation and focus the other pane.
    pub(super) fn hide_pane(&mut self, pane: Focus) {
        let project = self.projects.active_mut();
        if pane == Focus::Editor {
            let editor = &mut project.editor;
            editor.buffers[editor.active].vim.cancel_pending();
            editor.composition.clear();
        }
        project.hidden_pane = Some(pane);
        project.focus = pane.other();
        project.status = i18n::text(match pane {
            Focus::Manager => "workspace.manager_closed",
            Focus::Editor => "workspace.editor_closed",
        })
        .into();
    }
}
