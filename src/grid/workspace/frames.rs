//! Compose the workspace chrome with frames supplied by the concrete grid windows.

use super::{
    Workspace,
    editor_splits::PaneId,
    layout::{
        FOOTER_BADGE_INSET_Y, FOOTER_CHIP_PAD, FOOTER_COLUMN_DIGITS, FOOTER_ENCODING,
        FOOTER_ENCODING_GAP, FOOTER_LABEL_GAP, FOOTER_LINE_DIGITS, FOOTER_PROGRESS_DIGITS,
        FOOTER_ROWS, Focus, ShellWindow,
    },
};
use crate::{
    grid::{
        render::{Frame, Panel, Rect},
        windows::{
            WindowFrame,
            editor::{MarkdownCache, MarkdownViewMode},
        },
    },
    ui::theme::{
        ACCENT, CURRENT, CURSOR_TEXT, FOOTER_BLUE, FOOTER_DETAILS, FOOTER_INSERT, FOOTER_VISUAL,
        MUTED, TEXT, ThemeComponent, WARNING_TEXT, style,
    },
};
use gpui::{AppContext, Context, Entity, px};
use nanobug::{
    animation::scroll::ScrollInput, i18n, plugins::session::LanguageServerPhase, vim::Mode,
};

/// Fixed border and clipped source layers of a visible editor outside the focused pane.
pub(super) struct EditorPanePanels {
    /// Stable editor identity on this page.
    pub(super) pane: PaneId,
    /// Pane border and Bufferline, line numbers, and document text.
    pub(super) layers: [Entity<Panel>; 3],
    /// Parsed and shaped preview retained at this pane's own width.
    pub(super) markdown: MarkdownCache,
}

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

/// Select the localized footer text and foreground role for the server `phase`.
fn language_server_label(phase: LanguageServerPhase) -> (&'static str, u32) {
    let (key, color) = match phase {
        LanguageServerPhase::Starting => ("editor.language_server_starting", MUTED),
        LanguageServerPhase::Indexing => ("editor.language_server_indexing", MUTED),
        LanguageServerPhase::Ready => ("editor.language_server_ready", TEXT),
        LanguageServerPhase::Failed => ("editor.language_server_failed", WARNING_TEXT),
    };
    (i18n::text(key), color)
}

impl Workspace {
    /// Update the workspace and active popup frames through `cx`.
    pub(super) fn refresh_panels(&mut self, cx: &mut Context<Self>) {
        self.refresh_tab_bar(cx);
        self.refresh_sequence_hint(cx);
        if self.file_picker.is_some() {
            self.refresh_file_picker(cx);
            self.refresh_help_panels(cx);
            self.refresh_language_picker(cx);
            self.refresh_editor_close(cx);
            return;
        }
        if self.projects.start().is_some() {
            self.refresh_start_page(cx);
            self.refresh_remove_recent_project(cx);
            self.refresh_language_picker(cx);
            self.refresh_editor_close(cx);
            return;
        }
        let (frames, tree_name_preview) = self.frames(style(cx));
        for (panel, frame) in self.panels.iter().chain(self.bodies.iter()).zip(frames) {
            panel.update(cx, |panel, cx| panel.set(frame, cx));
        }
        self.refresh_inactive_editor_panels(cx);
        self.tree_name_preview_rect = tree_name_preview.map(|(rect, frame)| {
            self.tree_name_preview
                .update(cx, |panel, cx| panel.set(frame, cx));
            rect
        });
        self.refresh_help_panels(cx);
        self.refresh_language_picker(cx);
        self.refresh_editor_close(cx);
    }

    /// Rebuild the retained layers of the other editors visible on this Tabpage.
    fn refresh_inactive_editor_panels(&mut self, cx: &mut Context<Self>) {
        let areas = self.editor_rects();
        let project = self.projects.active();
        let active = project.active_editor_id();
        let modal_open = self.modal_open();
        self.inactive_editor_panels.retain(|panels| {
            panels.pane != active && areas.iter().any(|(pane, _)| *pane == panels.pane)
        });
        let metrics = self.metrics;
        for (pane, area) in areas.into_iter().filter(|(pane, _)| *pane != active) {
            let Some(editor) = project.editor_by_id(pane) else {
                continue;
            };
            let frames = editor.frames(
                WindowFrame {
                    area,
                    selected: false,
                    modal_open,
                    controls_hovered: false,
                    spacing: &self.spacing,
                },
                self.layout.visible_cols,
            );
            let panels = &mut self.inactive_editor_panels;
            let rendered = if let Some(index) = panels.iter().position(|entry| entry.pane == pane) {
                &panels[index]
            } else {
                panels.push(EditorPanePanels {
                    pane,
                    layers: std::array::from_fn(|_| cx.new(|_| Panel::new(metrics))),
                    markdown: MarkdownCache::default(),
                });
                &panels[panels.len() - 1]
            };
            for (panel, frame) in
                rendered
                    .layers
                    .iter()
                    .zip([frames.chrome, frames.numbers, frames.text])
            {
                panel.update(cx, |panel, cx| panel.set(frame, cx));
            }
        }
    }

    /// Advance unfocused editor viewports without retargeting the shared caret.
    pub(super) fn advance_inactive_editor_scrolling(&mut self) -> bool {
        let areas = self.editor_rects();
        let Some(project) = self.projects.active_project_mut() else {
            return false;
        };
        let active = project.active_editor_id();
        let now = self.scroll_epoch.elapsed().as_secs_f64();
        let mut changed = false;
        for (pane, area) in areas.into_iter().filter(|(pane, _)| *pane != active) {
            let Some(editor) = project.editor_by_id_mut(pane) else {
                continue;
            };
            changed |= editor
                .track_view(area, false, &self.spacing, [ScrollInput::Advance; 2], now)
                .0;
            if let Some(panels) = self
                .inactive_editor_panels
                .iter()
                .find(|entry| entry.pane == pane)
            {
                changed |= editor
                    .advance_markdown_preview(&panels.markdown, area, now)
                    .0;
            }
        }
        changed
    }

    /// Whether any other visible editor or its preview needs another scroll frame.
    pub(super) fn inactive_editor_scroll_active(&self) -> bool {
        let Some(project) = self.projects.active_project() else {
            return false;
        };
        let active = project.active_editor_id();
        self.editor_rects().into_iter().any(|(pane, area)| {
            pane != active
                && project.editor_by_id(pane).is_some_and(|editor| {
                    let buffer = editor.buffer();
                    editor.markdown_scroll_active(area)
                        || editor.markdown_view_mode() != Some(MarkdownViewMode::Preview)
                            && (buffer.scroll.is_active() || buffer.columns.is_active())
                })
        })
    }

    /// Update only the retained editor footer when plugin state changes through `cx`.
    pub(super) fn refresh_footer(&self, cx: &mut Context<Self>) {
        if self.projects.active_project().is_some() && self.file_picker.is_none() {
            let frame = self.footer_frame(self.modal_open());
            self.panels[2].update(cx, |panel, cx| panel.set(frame, cx));
        }
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
        footer.set_theme_component(ThemeComponent::Footer);
        footer.inset_row_tints(px(FOOTER_BADGE_INSET_Y));
        let doc = &vim.doc;
        let (row, column) = doc.location(doc.cursor);
        let line = row + 1;
        let progress = line.min(vim.line_count()) * 100 / vim.line_count();
        let (details, position) = footer_labels(line, column + 1, progress);
        let slots = self.footer_slots();
        let blue = FOOTER_BLUE;
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
            footer.label(
                0,
                slots.left,
                &active.editor.command_text(),
                ACCENT,
                &self.spacing,
            );
            return footer;
        }

        let (mode, mode_color) = if active.focus == Focus::Manager {
            (i18n::text("manager.title"), FOOTER_BLUE)
        } else if active.editor.markdown_view_mode() == Some(MarkdownViewMode::Preview) {
            (i18n::text("editor.markdown_preview"), FOOTER_BLUE)
        } else {
            let color = match vim.mode {
                Mode::Insert | Mode::Replace => FOOTER_INSERT,
                Mode::Visual | Mode::VisualLine | Mode::VisualBlock => FOOTER_VISUAL,
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
            (mode_color, CURSOR_TEXT)
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

        let (message, color) = if !active.status.is_empty() {
            (active.status.as_str(), TEXT)
        } else if !vim.message.is_empty() {
            (vim.message.as_str(), TEXT)
        } else if active.focus == Focus::Editor
            && let Some(message) = active.editor.buffer().caret_diagnostic()
        {
            // The footer is a single grid row even when the language server sends notes.
            (message.lines().next().unwrap_or_default(), WARNING_TEXT)
        } else {
            ("", TEXT)
        };
        let message_start = (tip + 1 + FOOTER_LABEL_GAP).min(slots.left.end);
        let mut message_end = slots.left.end;
        if let Some((server, phase)) = self.active_language_server_status() {
            let (state, color) = language_server_label(phase);
            let message = message.lines().next().unwrap_or_default();
            let available = message_end - message_start;
            // Keep space for operation feedback and diagnostics before choosing a status label.
            let feedback = self.spacing.columns(message).min(available / 2);
            let gap = if feedback > 0 { FOOTER_LABEL_GAP } else { 0 };
            for name in [server, "LSP"] {
                let label = format!("{name} · {state}");
                if self.spacing.columns(&label) + feedback + gap <= available {
                    let left = footer.right_label(0, message_end, &label, color, &self.spacing);
                    message_end = left.saturating_sub(gap).max(message_start);
                    break;
                }
            }
        }
        footer.label(0, message_start..message_end, message, color, &self.spacing);
        footer
    }

    /// Compose the active project's panes and use `appearance` for the Explorer preview background.
    fn frames(&self, appearance: crate::ui::theme::Style) -> ([Frame; 6], Option<(Rect, Frame)>) {
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
                        appearance,
                    );
                    (frames.chrome, frames.body, frames.preview)
                },
            );
        let editor = self.projects.active().editor.frames(
            context(Focus::Editor, self.active_editor_area()),
            layout.visible_cols,
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
