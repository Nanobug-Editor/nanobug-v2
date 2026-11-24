//! Compose the workspace chrome with frames supplied by the concrete grid windows.

use super::{
    Workspace,
    editor_splits::PaneId,
    layout::{
        FOOTER_BADGE_INSET_Y, FOOTER_CHIP_PAD, FOOTER_COLUMN_DIGITS, FOOTER_ENCODING,
        FOOTER_LABEL_GAP, FOOTER_LINE_DIGITS, FOOTER_PROGRESS_DIGITS, FOOTER_ROWS, Focus,
        ShellWindow, pane_border_offsets,
    },
};
use crate::{
    grid::{
        render::{Frame, Metrics, Panel, Rect},
        windows::{
            WindowFrame,
            editor::{EditorLayers, MarkdownCache, MarkdownViewMode},
        },
    },
    ui::{
        components::grid_file_row::display_path,
        theme::{
            ACCENT, CURRENT, CURSOR_TEXT, FOOTER_BLUE, FOOTER_DETAILS, FOOTER_INSERT,
            FOOTER_VISUAL, MUTED, TEXT, ThemeComponent, WARNING_TEXT,
        },
    },
};
use gpui::{App, AppContext, Context, Entity, px};
use nanobug::{
    animation::scroll::ScrollInput, i18n, plugins::session::LanguageServerPhase, vim::Mode,
};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

/// Retained GPUI panels corresponding to the frames produced by an editor.
pub(super) type EditorPanels = EditorLayers<Entity<Panel>>;

impl EditorPanels {
    /// Allocate the three panels with the same `metrics` in `cx`.
    pub(super) fn new(metrics: Metrics, cx: &mut App) -> Self {
        Self {
            chrome: cx.new(|_| Panel::new(metrics)),
            numbers: cx.new(|_| Panel::new(metrics)),
            text: cx.new(|_| Panel::new(metrics)),
        }
    }

    /// Publish each of `frames` to its matching panel through `cx`.
    fn set_frames(&self, frames: EditorLayers<Frame>, cx: &mut App) {
        self.chrome
            .update(cx, |panel, cx| panel.set(frames.chrome, cx));
        self.numbers
            .update(cx, |panel, cx| panel.set(frames.numbers, cx));
        self.text.update(cx, |panel, cx| panel.set(frames.text, cx));
    }

    /// All retained panels that follow display-scale and appearance changes.
    pub(super) fn all(&self) -> [&Entity<Panel>; 3] {
        [&self.chrome, &self.numbers, &self.text]
    }
}

/// Panels and Markdown cache of a visible editor outside the focused pane.
pub(super) struct EditorPanePanels {
    /// Stable editor identity on this page.
    pub(super) pane: PaneId,
    /// Pane border and Bufferline, line numbers, and document text.
    pub(super) layers: EditorPanels,
    /// Parsed and shaped preview retained at this pane's own width.
    pub(super) markdown: MarkdownCache,
}

/// Format `progress` (0–100) next to one-based `line` and `column` in the position badge.
/// The colon anchors both coordinates. Values that exceed their field show a lower bound.
fn footer_position_label(line: usize, column: usize, progress: usize) -> String {
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
    format!(
        "{progress:>FOOTER_PROGRESS_DIGITS$}% {line:>FOOTER_LINE_DIGITS$}:{column:<FOOTER_COLUMN_DIGITS$}"
    )
}

/// Display a single-line path relative to the project, using an absolute path across volumes.
fn footer_file_path(root: &Path, file: &Path) -> String {
    let single_line = |path: &Path| path.to_string_lossy().replace(['\t', '\r', '\n'], " ");
    if let Ok(relative) = file.strip_prefix(root) {
        return single_line(relative);
    }
    let root = display_path(root);
    let file = display_path(file);
    let path = Path::new(file.as_ref());
    let mut relative = PathBuf::new();
    for ancestor in Path::new(root.as_ref()).ancestors() {
        if let Ok(tail) = path.strip_prefix(ancestor) {
            relative.push(tail);
            return single_line(&relative);
        }
        relative.push("..");
    }
    single_line(path)
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
    /// Resize each displayed terminal once for the widest and tallest view across its Session.
    pub(super) fn resize_visible_terminals(&self) {
        for project in self.projects.ordered() {
            if !project
                .editors()
                .any(|(_, editor)| editor.terminal().is_some())
            {
                continue;
            }
            let mut sizes = HashMap::new();
            for page in 0..project.page_count() {
                let (_, region) = project.pane_regions_for_tabpage(page, self.layout);
                for (pane, area) in project.editor_rects_for_tabpage(page, self.layout, region) {
                    let Some(editor) = project.editor_by_id(pane) else {
                        continue;
                    };
                    let Some(terminal) = editor.terminal() else {
                        continue;
                    };
                    let body = self.layout.visible_region(editor.text_area(area));
                    if body.w == 0 || body.h == 0 {
                        continue;
                    }
                    let number = editor.vim().doc.buffer_number();
                    let (_, columns, rows) = sizes.entry(number).or_insert((terminal, 0, 0));
                    // The largest dimensions may come from different panes or inactive Tabpages.
                    *columns = (*columns).max(body.w);
                    *rows = (*rows).max(body.h);
                }
            }
            for (terminal, columns, rows) in sizes.into_values() {
                terminal.resize(columns, rows);
            }
        }
    }

    /// Update the workspace and active popup frames through `cx`.
    pub(super) fn refresh_panels(&mut self, cx: &mut Context<Self>) {
        self.refresh_settings(cx);
        self.refresh_vim_menu(cx);
        if let Some((history, area)) = self.command_window.as_ref().zip(self.command_window_area())
        {
            history.refresh(area, &self.spacing, cx);
        }
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
        let tree_name_preview = self.refresh_pane_frames(cx);
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
        let pane_areas: Vec<_> = std::iter::once(self.layout.tree)
            .chain(areas.iter().map(|&(_, area)| area))
            .collect();
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
            let mut frames = editor.frames(
                WindowFrame {
                    area,
                    selected: false,
                    modal_open,
                    controls_hovered: false,
                    spacing: &self.spacing,
                },
                self.layout.visible_cols,
                &self.input_mappings,
            );
            let (top, bottom) = pane_border_offsets(area, &pane_areas, metrics);
            frames.chrome.offset_pane_borders(top, bottom);
            let panels = &mut self.inactive_editor_panels;
            let rendered = if let Some(index) = panels.iter().position(|entry| entry.pane == pane) {
                &panels[index]
            } else {
                panels.push(EditorPanePanels {
                    pane,
                    layers: EditorPanels::new(metrics, cx),
                    markdown: MarkdownCache::default(),
                });
                &panels[panels.len() - 1]
            };
            rendered.layers.set_frames(frames, cx);
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
                    editor
                        .terminal()
                        .is_some_and(|terminal| terminal.scroll().is_active())
                        || editor.markdown_scroll_active(area)
                        || editor.markdown_view_mode() != Some(MarkdownViewMode::Preview)
                            && (buffer.scroll.is_active() || buffer.columns.is_active())
                })
        })
    }

    /// Update only the retained editor footer when plugin state changes through `cx`.
    pub(super) fn refresh_footer(&self, cx: &mut Context<Self>) {
        if self.projects.active_project().is_some() && self.file_picker.is_none() {
            let frame = self.footer_frame(self.modal_open());
            self.footer_panel
                .update(cx, |panel, cx| panel.set(frame, cx));
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
        let active = self.projects.active();
        let vim = active.editor.vim();
        let cols = self.layout.visible_cols;
        let mut footer = Frame::new(cols, FOOTER_ROWS);
        footer.set_theme_component(ThemeComponent::Footer);
        footer.inset_row_tints(px(FOOTER_BADGE_INSET_Y));
        let doc = &vim.doc;
        let (row, column) = doc.location(doc.cursor);
        let line = row + 1;
        let progress = line.min(vim.line_count()) * 100 / vim.line_count();
        let position = footer_position_label(line, column + 1, progress);
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
        footer.label(0, slots.details, FOOTER_ENCODING, TEXT, &self.spacing);
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
        } else if let Some(terminal) = active.editor.terminal() {
            (
                terminal.mode_label(),
                if terminal.accepts_text() {
                    FOOTER_INSERT
                } else {
                    FOOTER_BLUE
                },
            )
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
        let mode_with_keymap = (active.focus == Focus::Editor
            && active.editor.terminal().is_none())
        .then(|| vim.active_keymap_label())
        .flatten()
        .map(|keymap| format!("{mode} [{keymap}]"));
        let mode = mode_with_keymap.as_deref().unwrap_or(mode);
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

        let verbose = self.input_mappings.last_ex_diagnostic();
        let feedback = [
            active.status.as_str(),
            vim.message.as_str(),
            verbose.as_str(),
        ]
        .into_iter()
        .find(|message| !message.is_empty());
        let path = doc
            .path
            .as_deref()
            .filter(|_| feedback.is_none())
            .map(|file| footer_file_path(&active.root, file));
        let message = feedback.or(path.as_deref()).unwrap_or_default();
        let message_start = (tip + 1 + FOOTER_LABEL_GAP).min(slots.left.end);
        let mut message_end = slots.left.end;
        if let Some((server, phase)) = self.active_language_server_status() {
            let (state, color) = language_server_label(phase);
            let message = message.lines().next().unwrap_or_default();
            let available = message_end - message_start;
            // Reserve part of the row for the path or command feedback when the server is present.
            let reserved = self.spacing.columns(message).min(available / 2);
            let gap = if reserved > 0 { FOOTER_LABEL_GAP } else { 0 };
            for name in [server, "LSP"] {
                let label = format!("{name} · {state}");
                if self.spacing.columns(&label) + reserved + gap <= available {
                    let left = footer.right_label(0, message_end, &label, color, &self.spacing);
                    message_end = left.saturating_sub(gap).max(message_start);
                    break;
                }
            }
        }
        let slot = message_start..message_end;
        if let Some(path) = &path {
            let fitted = self.spacing.ellipsize_start(path, slot.len());
            footer.label(0, slot, &fitted, TEXT, &self.spacing);
        } else if active.status.is_empty()
            && !vim.message.is_empty()
            && !vim.ex_message_spans().is_empty()
        {
            footer.highlighted_label(0, slot, message, TEXT, &self.spacing, |offset| {
                nanobug::vim::ExMessageOutput::style_at(vim.ex_message_spans(), offset)
            });
        } else {
            footer.label(0, slot, message, TEXT, &self.spacing);
        }
        footer
    }

    /// Refresh the active project's named pane layers through `cx`, returning its optional filename preview.
    fn refresh_pane_frames(&self, cx: &mut Context<Self>) -> Option<(Rect, Frame)> {
        let layout = self.layout;
        let pane_areas = self.workspace_pane_areas();
        let vim = self.projects.active().editor.vim();
        let help_open = self.help.is_open();
        let modal_open = self.modal_open();
        self.refresh_footer(cx);
        let context = |pane, area| WindowFrame {
            area,
            selected: !self.small_modal_open()
                && ShellWindow::Pane(pane).is_selected(self.projects.active().focus, help_open),
            modal_open,
            controls_hovered: false,
            spacing: &self.spacing,
        };
        let (manager_chrome, manager_body, manager_preview) =
            self.projects.active().manager.as_ref().map_or_else(
                || (Frame::new(0, 0), Frame::new(0, 0), None),
                |manager| {
                    let mut frames = manager.frames(
                        context(Focus::Manager, layout.tree),
                        layout.cols,
                        vim.doc.path.as_deref(),
                    );
                    let (top, bottom) = pane_border_offsets(layout.tree, &pane_areas, self.metrics);
                    frames.chrome.offset_pane_borders(top, bottom);
                    (frames.chrome, frames.body, frames.preview)
                },
            );
        let editor_area = self.active_editor_area();
        let mut editor = self.projects.active().editor.frames(
            context(Focus::Editor, editor_area),
            layout.visible_cols,
            &self.input_mappings,
        );
        let (top, bottom) = pane_border_offsets(editor_area, &pane_areas, self.metrics);
        editor.chrome.offset_pane_borders(top, bottom);
        self.manager_chrome
            .update(cx, |panel, cx| panel.set(manager_chrome, cx));
        self.manager_body
            .update(cx, |panel, cx| panel.set(manager_body, cx));
        self.editor_panels.set_frames(editor, cx);
        manager_preview
    }
}

#[cfg(test)]
mod tests {
    use super::{footer_file_path, footer_position_label};
    use std::path::Path;

    #[test]
    fn footer_paths_are_relative_to_the_project() {
        let root = Path::new("workspace").join("project");
        let file = Path::new("src").join("main.rs");
        assert_eq!(
            footer_file_path(&root, &root.join(&file)),
            file.to_string_lossy()
        );
        let sibling = Path::new("workspace").join("library").join("lib.rs");
        assert_eq!(
            footer_file_path(&root, &sibling),
            Path::new("..")
                .join("library")
                .join("lib.rs")
                .to_string_lossy()
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_footer_handles_verbatim_paths_and_other_drives() {
        let root = Path::new(r"\\?\C:\project");
        assert_eq!(
            footer_file_path(root, Path::new(r"C:\project\src\main.rs")),
            r"src\main.rs"
        );
        assert_eq!(
            footer_file_path(root, Path::new(r"\\?\D:\lib.rs")),
            r"D:\lib.rs"
        );
    }

    /// Crossing digit boundaries keeps the percent sign and colon in the same slots.
    #[test]
    fn footer_position_label_keeps_fixed_columns() {
        for (line, column, progress, expected) in [
            (9, 9, 9, "  9%    9:9  "),
            (10, 10, 10, " 10%   10:10 "),
            (9_999, 999, 99, " 99% 9999:999"),
            (10_000, 999, 100, "100% 10k+:999"),
            (9_999, 1_000, 100, "100% 9999:1k+"),
        ] {
            assert_eq!(footer_position_label(line, column, progress), expected);
        }
    }
}
