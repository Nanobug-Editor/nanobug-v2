//! Compose the workspace chrome with frames supplied by the concrete grid windows.

use super::{
    Workspace,
    layout::{FOOTER_ROWS, Focus, ShellWindow},
};
use crate::{
    grid::{
        render::{Frame, Rect},
        windows::WindowFrame,
    },
    ui::theme::{ACCENT, GOLD, MUTED, TEXT},
};
use gpui::Context;
use nanobug::{i18n, vim::Mode};

impl Workspace {
    /// Update the workspace and active popup frames through `cx`.
    pub(super) fn refresh_panels(&mut self, cx: &mut Context<Self>) {
        self.refresh_tab_bar(cx);
        let (frames, tree_name_preview) = self.frames();
        for (panel, frame) in self.panels.iter().chain(self.bodies.iter()).zip(frames) {
            panel.update(cx, |panel, cx| panel.set(frame, cx));
        }
        self.tree_name_preview_rect = tree_name_preview.map(|(rect, frame)| {
            self.tree_name_preview
                .update(cx, |panel, cx| panel.set(frame, cx));
            rect
        });
        self.refresh_help_panels(cx);
        self.refresh_language_picker(cx);
        self.refresh_editor_close(cx);
    }

    /// Update the popup's retained border, list, dim title and active search through `cx`.
    pub(super) fn refresh_help_panels(&self, cx: &mut Context<Self>) {
        let context = WindowFrame {
            area: self.help_layout(),
            selected: true,
            modal_open: true,
            close_hovered: self.hovered_close == Some(ShellWindow::ShortcutHelp),
            spacing: &self.spacing,
        };
        if let Some(frames) = self.help.frames(context) {
            self.help
                .panel
                .update(cx, |panel, cx| panel.set(frames.chrome, cx));
            self.help
                .body
                .update(cx, |panel, cx| panel.set(frames.body, cx));
            self.help
                .dim_title
                .update(cx, |panel, cx| panel.set(frames.dim_title, cx));
            if let Some(search) = frames.search {
                self.help
                    .search
                    .update(cx, |panel, cx| panel.set(search, cx));
            }
        }
    }

    /// Format the right-hand status shared by footer rendering and the command caret.
    pub(super) fn footer_position(&self) -> String {
        let doc = &self.projects.active.editor.vim().doc;
        let (row, column) = doc.location(doc.cursor);
        format!(
            "{}  UTF-8 {}:{}",
            self.cursor_effect.label(),
            row + 1,
            column + 1
        )
    }

    /// Compose the active project's panes with the shared status and command row.
    fn frames(&self) -> ([Frame; 6], Option<(Rect, Frame)>) {
        let layout = self.layout;
        let vim = self.projects.active.editor.vim();
        let help_open = self.help.is_open();
        let modal_open = self.modal_open();
        let mut footer = Frame::new(layout.cols, FOOTER_ROWS);
        let position = self.footer_position();
        let position_x = footer.right_label(0, layout.cols - 1, &position, GOLD, &self.spacing);
        let command = if self.projects.active.hidden_pane != Some(Focus::Editor)
            && matches!(vim.mode, Mode::Command | Mode::Search)
        {
            let mut command = self.projects.active.editor.command_text();
            command.push_str(&self.projects.active.editor.composition.text);
            Some(command)
        } else if !self.projects.active.status.is_empty() {
            Some(self.projects.active.status.clone())
        } else if !vim.message.is_empty() {
            Some(vim.message.clone())
        } else {
            None
        };
        if let Some(command) = command {
            footer.label(
                0,
                self.command_slot(&position),
                &command,
                ACCENT,
                &self.spacing,
            );
        } else {
            let mode = if self.projects.active.focus == Focus::Manager {
                i18n::text("manager.title")
            } else {
                vim.mode.label()
            };
            let mode_end = 1 + self.spacing.columns(mode).max(8);
            footer.label(
                0,
                1..mode_end,
                mode,
                if modal_open { MUTED } else { ACCENT },
                &self.spacing,
            );
            footer.label(
                0,
                mode_end + 1..position_x.saturating_sub(1),
                &self
                    .projects
                    .active
                    .editor
                    .title(&self.projects.active.root),
                TEXT,
                &self.spacing,
            );
        }

        let context = |pane, area| WindowFrame {
            area,
            selected: !self.small_modal_open()
                && ShellWindow::Pane(pane).is_selected(self.projects.active.focus, help_open),
            modal_open,
            close_hovered: self.hovered_close == Some(ShellWindow::Pane(pane)),
            spacing: &self.spacing,
        };
        let manager = self.projects.active.manager.frames(
            context(Focus::Manager, layout.tree),
            layout.cols,
            vim.doc.path.as_deref(),
        );
        let editor = self.projects.active.editor.frames(
            context(Focus::Editor, layout.editor),
            &self.projects.active.root,
        );
        (
            [
                manager.chrome,
                editor.chrome,
                footer,
                manager.body,
                editor.numbers,
                editor.text,
            ],
            manager.preview,
        )
    }
}
