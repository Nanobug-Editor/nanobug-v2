//! Apply editor actions to retained buffers, files and the native window.

use super::{
    Editor,
    buffer::Buffer,
    layout::{Focus, ShellWindow},
};
use gpui::Window;
use nanobug::{
    animation::scroll::GridScroll,
    editor::{Action, Document, Mode, Register, Vim},
};
use std::path::PathBuf;

impl Editor {
    /// Format command or search input with its prompt, before any transient IME text.
    pub(super) fn command_text(&self) -> String {
        let vim = self.vim();
        let prefix = if vim.mode == Mode::Command { ':' } else { '/' };
        format!("{prefix}{}", vim.command)
    }

    /// Open `path` in the sole editor viewport, retaining all previous unsaved buffers.
    pub(super) fn open(&mut self, path: PathBuf) {
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        if let Some(index) = self
            .buffers
            .iter()
            .position(|b| b.vim.doc.path.as_ref() == Some(&path))
        {
            self.active = index;
        } else {
            match Document::open(&path) {
                Ok(doc) => {
                    self.buffers.push(Buffer {
                        vim: Vim::new(doc),
                        scroll: GridScroll::default(),
                        columns: GridScroll::default(),
                    });
                    self.active = self.buffers.len() - 1;
                }
                Err(error) => {
                    self.status = format!("{error:#}");
                    return;
                }
            }
        }
        let now = self.scroll_epoch.elapsed().as_secs_f64();
        let buffer = &mut self.buffers[self.active];
        buffer.scroll.snap(buffer.scroll.first_cell(), now);
        buffer.columns.snap(buffer.columns.first_cell(), now);
        self.last_cursor_space = None;
        self.focus = Focus::Editor;
        if self.hidden_pane == Some(Focus::Editor) {
            self.hidden_pane = None;
        }
        self.composition.clear();
        self.status.clear();
    }

    /// Execute `action` requested by the modal model, preserving buffers on I/O errors.
    pub(super) fn action(&mut self, action: Action, window: &mut Window) {
        match action {
            Action::None => {}
            Action::Open(name) => self.open(self.project.join(name)),
            Action::Save(name) => {
                let path = name.map(|name| self.project.join(name));
                match self.save(path) {
                    Ok(()) => {
                        self.status = "已保存".into();
                        let _ = self.tree.refresh();
                    }
                    Err(error) => self.status = format!("{error:#}"),
                }
            }
            Action::SaveClose => match self.save(None) {
                Ok(()) => self.action(Action::Close(false), window),
                Err(error) => self.status = format!("{error:#}"),
            },
            Action::Close(force) => {
                if force || !self.buffers.iter().any(|buffer| buffer.vim.doc.modified()) {
                    window.remove_window();
                } else {
                    self.status = "有未保存的修改。使用 :w 保存，或 :q! 放弃修改并退出。".into();
                }
            }
        }
    }

    /// Save the active document to `path` or its existing path, then invalidate any replaced paste.
    fn save(&mut self, path: Option<PathBuf>) -> anyhow::Result<()> {
        let doc = &mut self.buffers[self.active].vim.doc;
        let previous = doc.path.clone();
        let dirty = doc.modified();
        doc.save(path.as_deref())?;
        if (dirty || doc.path != previous)
            && let Some(path) = &doc.path
        {
            self.transfers.invalidate(path);
        }
        Ok(())
    }

    /// Apply a text or key operation `input` and dispatch the resulting action in `window`.
    pub(super) fn run_vim(
        &mut self,
        input: impl FnOnce(&mut Vim, &mut Register) -> Action,
        window: &mut Window,
    ) {
        let vim = &mut self.buffers[self.active].vim;
        let before = vim.doc.revision;
        let action = input(vim, &mut self.register);
        if vim.doc.revision != before
            && let Some(path) = &vim.doc.path
        {
            self.transfers.invalidate(path);
        }
        self.action(action, window);
    }

    /// Close `target` and return input to the remaining pane, guarding the last pane's exit.
    pub(super) fn close_grid_window(&mut self, target: ShellWindow, window: &mut Window) {
        match target {
            ShellWindow::ShortcutHelp => self.help_top = None,
            ShellWindow::Pane(_) if self.hidden_pane.is_some() => {
                self.action(Action::Close(false), window);
                return;
            }
            ShellWindow::Pane(pane) => {
                if pane == Focus::Editor {
                    self.buffers[self.active].vim.cancel_pending();
                    self.composition.clear();
                    self.composition_selection = 0..0;
                }
                self.hidden_pane = Some(pane);
                self.focus = pane.other();
                self.status = match pane {
                    Focus::Manager => "已关闭 Explorer，依次按 Space、e 打开",
                    Focus::Editor => "已关闭编辑区，按 Ctrl+l 恢复",
                }
                .into();
            }
        }
        // Require a fresh hover before exposing the close button on an expanded pane.
        self.hovered_border = None;
    }
}
