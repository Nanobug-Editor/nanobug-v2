//! Compose the workspace chrome with frames supplied by the concrete grid windows.

use super::{
    Workspace,
    layout::{
        FOOTER_BADGE_INSET_Y, FOOTER_CHIP_PAD, FOOTER_COLUMN_DIGITS, FOOTER_ENCODING,
        FOOTER_ENCODING_GAP, FOOTER_LABEL_GAP, FOOTER_LINE_DIGITS, FOOTER_PROGRESS_DIGITS,
        FOOTER_ROWS, Focus, ShellWindow,
    },
};
use crate::{
    grid::{
        render::{Frame, Rect},
        windows::WindowFrame,
    },
    ui::theme::{ACCENT, CURRENT, CURSOR_TEXT, MUTED, TEXT},
};
use gpui::{Context, px};
use nanobug::{i18n, vim::Mode};

/// Cool blue for the normal mode and cursor position badges.
const FOOTER_BLUE: u32 = 0x89adf4;
/// Darker RGBA backing for encoding and line progress.
const FOOTER_DETAILS: u32 = 0x3b4564ff;
/// Lavender distinguishes selection modes at a glance.
const FOOTER_VISUAL: u32 = 0xc4a7f4;

/// Format `progress` (0–100) and one-based `line` and `column` in fixed-width footer slots.
/// The colon anchors both coordinates. Values that exceed their field show a lower bound.
fn footer_labels(line: usize, column: usize, progress: usize) -> (String, String) {
    let line = if line < 10_000 {
        line.to_string()
    } else {
        "10k+".to_owned()
    };
    let column = if column < 1_000 {
        column.to_string()
    } else {
        "1k+".to_owned()
    };
    (
        format!("{FOOTER_ENCODING}{FOOTER_ENCODING_GAP}{progress:>FOOTER_PROGRESS_DIGITS$}%"),
        format!("{line:>FOOTER_LINE_DIGITS$}:{column:<FOOTER_COLUMN_DIGITS$}"),
    )
}

impl Workspace {
    /// Update the workspace and active popup frames through `cx`.
    pub(super) fn refresh_panels(&mut self, cx: &mut Context<Self>) {
        self.refresh_tab_bar(cx);
        if self.file_picker.is_some() {
            self.refresh_file_picker(cx);
            self.refresh_help_panels(cx);
            self.refresh_language_picker(cx);
            self.refresh_editor_close(cx);
            return;
        }
        if self.projects.start().is_some() {
            self.refresh_start_page(cx);
            self.refresh_language_picker(cx);
            self.refresh_editor_close(cx);
            return;
        }
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
            controls_hovered: self.hovered_controls == Some(ShellWindow::ShortcutHelp),
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

    /// Draw the footer with inward-pointing status badges and dim the mode when `modal_open`.
    fn footer_frame(&self, modal_open: bool) -> Frame {
        let active = &self.projects.active();
        let vim = active.editor.vim();
        let cols = self.layout.cols;
        let mut footer = Frame::new(cols, FOOTER_ROWS);
        footer.inset_row_tints(px(FOOTER_BADGE_INSET_Y));
        let doc = &vim.doc;
        let (row, column) = doc.location(doc.cursor);
        let line = row + 1;
        let progress = line * 100 / doc.starts.len().max(1);
        let (details, position) = footer_labels(line, column + 1, progress);
        let slots = self.footer_slots();
        let blue = (FOOTER_BLUE << 8) | 0xff;
        footer.tint(
            0,
            slots.details_tip + 1,
            slots.position_tip + 1,
            FOOTER_DETAILS,
        );
        footer.tint(0, slots.position_tip + 1, cols, blue);
        footer.chevron(0, slots.details_tip, true, FOOTER_DETAILS);
        footer.chevron(0, slots.position_tip, true, blue);
        footer.label(0, slots.details, &details, TEXT, &self.spacing);
        footer.right_label(0, slots.position.end, &position, CURSOR_TEXT, &self.spacing);

        if active.hidden_pane != Some(Focus::Editor)
            && matches!(vim.mode, Mode::Command | Mode::Search)
        {
            let mut command = active.editor.command_text();
            command.push_str(&active.editor.composition.text);
            footer.label(0, slots.left, &command, ACCENT, &self.spacing);
            return footer;
        }

        let (mode, mode_color) = if active.focus == Focus::Manager {
            (i18n::text("manager.title"), FOOTER_BLUE)
        } else {
            let color = match vim.mode {
                Mode::Insert => ACCENT,
                Mode::Visual | Mode::VisualLine => FOOTER_VISUAL,
                _ => FOOTER_BLUE,
            };
            (vim.mode.label(), color)
        };
        let mode_limit = slots.left.end.saturating_sub(FOOTER_CHIP_PAD + 1);
        let mode_end = (slots.left.start + self.spacing.columns(mode)).min(mode_limit);
        let tip = mode_end + FOOTER_CHIP_PAD;
        let (background, foreground) = if modal_open {
            (CURRENT, MUTED)
        } else {
            ((mode_color << 8) | 0xff, CURSOR_TEXT)
        };
        footer.tint(0, 0, tip, background);
        footer.chevron(0, tip, false, background);
        footer.label(
            0,
            slots.left.start..mode_end,
            mode,
            foreground,
            &self.spacing,
        );

        let message = if active.status.is_empty() {
            &vim.message
        } else {
            &active.status
        };
        let message_start = (tip + 1 + FOOTER_LABEL_GAP).min(slots.left.end);
        footer.label(
            0,
            message_start..slots.left.end,
            message,
            TEXT,
            &self.spacing,
        );
        footer
    }

    /// Compose the active project's panes with the shared status and command row.
    fn frames(&self) -> ([Frame; 6], Option<(Rect, Frame)>) {
        let layout = self.layout;
        let vim = self.projects.active().editor.vim();
        let help_open = self.help.is_open();
        let modal_open = self.modal_open();
        let footer = self.footer_frame(modal_open);
        let context = |pane, area| WindowFrame {
            area,
            selected: !self.small_modal_open()
                && ShellWindow::Pane(pane).is_selected(self.projects.active().focus, help_open),
            modal_open,
            controls_hovered: self.hovered_controls == Some(ShellWindow::Pane(pane)),
            spacing: &self.spacing,
        };
        let (manager_chrome, manager_body, manager_preview) =
            self.projects.active().manager.as_ref().map_or_else(
                || (Frame::new(0, 0), Frame::new(0, 0), None),
                |manager| {
                    let frames = manager.frames(
                        context(Focus::Manager, layout.tree),
                        layout.cols,
                        vim.doc.path.as_deref(),
                    );
                    (frames.chrome, frames.body, frames.preview)
                },
            );
        let editor = self.projects.active().editor.frames(
            context(Focus::Editor, layout.editor),
            &self.projects.active().root,
        );
        (
            [
                manager_chrome,
                editor.chrome,
                footer,
                manager_body,
                editor.numbers,
                editor.text,
            ],
            manager_preview,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::footer_labels;

    /// Crossing digit boundaries keeps both labels and their punctuation in the same slots.
    #[test]
    fn footer_labels_keep_fixed_columns() {
        for (line, column, progress, expected_details, expected_position) in [
            (9, 9, 9, "UTF-8    9%", "   9:9  "),
            (10, 10, 10, "UTF-8   10%", "  10:10 "),
            (9_999, 999, 99, "UTF-8   99%", "9999:999"),
            (10_000, 999, 100, "UTF-8  100%", "10k+:999"),
            (9_999, 1_000, 100, "UTF-8  100%", "9999:1k+"),
        ] {
            let (details, position) = footer_labels(line, column, progress);
            assert_eq!(details, expected_details);
            assert_eq!(position, expected_position);
        }
    }
}
