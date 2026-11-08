//! Derive workspace and shortcut-help frames from the current documents, tree and focus.

use super::{
    Editor,
    buffer::TEXT_OVERSCAN,
    layout::{FOOTER_ROWS, Focus, Layout, ShellWindow},
    shortcuts,
};
use crate::{
    explorer::EntryIcon,
    ui::{
        grid::{Frame, Rect},
        theme::{ACCENT, BACKGROUND, CURRENT, GOLD, MUTED, SELECTION, TEXT},
    },
};
use gpui::{Context, rgba};
use nanobug::editor::Mode;

/// Visible label shared by the file tree and its focus indicator.
const EXPLORER_TITLE: &str = "Explorer";
/// Faint mint row background for the file shown in the editor.
const ACTIVE_FILE_TINT: u32 = (ACCENT << 8) | 0x24;

impl Editor {
    /// Update the workspace and shortcut-help frames through `cx`.
    pub(super) fn refresh_panels(&mut self, cx: &mut Context<Self>) {
        let (frames, tree_name_preview) = self.frames();
        for (panel, frame) in self.panels.iter().chain(self.bodies.iter()).zip(frames) {
            panel.update(cx, |panel, cx| panel.set(frame, cx));
        }
        self.tree_name_preview_rect = tree_name_preview.map(|(rect, frame)| {
            self.tree_name_preview
                .update(cx, |panel, cx| panel.set(frame, cx));
            rect
        });
        if let Some(top) = self.help_top {
            let (area, pane_rows) = self.help_layout();
            let count = pane_rows.len() + shortcuts::GLOBAL.len();
            let top = top.min(count.saturating_sub(area.h - 3));
            self.help_top = Some(top);
            let mut frame = Frame::new(area.w, area.h);
            let title = if self.focus == Focus::Manager {
                "Explorer 快捷键"
            } else {
                "编辑区快捷键"
            };
            self.window_border(&mut frame, ShellWindow::ShortcutHelp, title);
            for (row, (key, description)) in pane_rows
                .iter()
                .chain(shortcuts::GLOBAL)
                .skip(top)
                .take(area.h - 3)
                .enumerate()
            {
                frame.label(row + 1, 2..22, key, GOLD, &self.spacing);
                frame.label(row + 1, 23..area.w - 2, description, TEXT, &self.spacing);
            }
            frame.label(
                area.h - 2,
                2..area.w - 2,
                "j/k 或滚轮浏览  Esc / qq 关闭",
                MUTED,
                &self.spacing,
            );
            self.help_panel.update(cx, |panel, cx| panel.set(frame, cx));
        }
    }

    /// Draw `target`'s `title` in `frame` and reserve the close slot during border hover.
    fn window_border(&self, frame: &mut Frame, target: ShellWindow, title: &str) {
        frame.border(
            title,
            target.is_selected(self.focus, self.help_top.is_some()),
            self.hovered_border == Some(target),
            &self.spacing,
        );
    }

    /// Move the overlay by `delta` rows and update its retained panel through `cx`.
    pub(super) fn scroll_help(&mut self, delta: isize, cx: &mut Context<Self>) {
        if let Some(top) = self.help_top {
            self.help_top = Some(top.saturating_add_signed(delta));
            self.refresh_panels(cx);
            cx.notify();
        }
    }

    /// Derive the workspace frames and any selected filename preview.
    fn frames(&self) -> ([Frame; 7], Option<(Rect, Frame)>) {
        let layout = self.layout;
        let buffer = &self.buffers[self.active];
        let vim = &buffer.vim;
        let active_file = vim.doc.path.as_deref();
        let help_open = self.help_top.is_some();
        let help_selected = ShellWindow::ShortcutHelp.is_selected(self.focus, help_open);
        let active = ShellWindow::Pane(Focus::Editor).is_selected(self.focus, help_open);
        let selection_tint = if help_selected { CURRENT } else { SELECTION };
        let project_name = self
            .project
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        let filename = active_file
            .map(|p| {
                p.strip_prefix(&self.project)
                    .unwrap_or(p)
                    .to_string_lossy()
                    .into_owned()
            })
            .unwrap_or("[Untitled]".into());
        let modified = if vim.doc.modified() { " [+]" } else { "" };
        let (row, column) = vim.doc.location(vim.doc.cursor);
        let mut header = Frame::new(layout.cols, 1);
        header.label(0, 1..9, "NANOBUG", ACCENT, &self.spacing);
        header.label(
            0,
            12..layout.cols.saturating_sub(2),
            &project_name,
            MUTED,
            &self.spacing,
        );
        let mut footer = Frame::new(layout.cols, FOOTER_ROWS);
        footer.label(
            0,
            1..9,
            if self.focus == Focus::Manager {
                EXPLORER_TITLE
            } else {
                vim.mode.label()
            },
            if help_selected { MUTED } else { ACCENT },
            &self.spacing,
        );
        let position = format!(
            "{}  UTF-8 {}:{}",
            self.cursor_effect.label(),
            row + 1,
            column + 1
        );
        let position_x = footer.right_label(0, layout.cols - 1, &position, GOLD, &self.spacing);
        footer.label(
            0,
            10..position_x.saturating_sub(1),
            &format!("{filename}{modified}"),
            TEXT,
            &self.spacing,
        );
        let command = if self.hidden_pane != Some(Focus::Editor)
            && matches!(vim.mode, Mode::Command | Mode::Search)
        {
            let mut command = self.command_text();
            command.push_str(&self.composition);
            command
        } else if !self.status.is_empty() {
            self.status.clone()
        } else if !vim.message.is_empty() {
            vim.message.clone()
        } else {
            "Ctrl+h/l 切窗  Space e Explorer  i 编辑  Esc 普通模式  :w 保存  :q 退出  F5 刷新  F6 动效  F7 配置".into()
        };
        footer.label(1, self.command_slot(), &command, ACCENT, &self.spacing);
        let mut tree = Frame::new(layout.tree.w, layout.tree.h);
        self.window_border(&mut tree, ShellWindow::Pane(Focus::Manager), EXPLORER_TITLE);
        let root_icon_column = Layout::tree_indent_column(0) + 1;
        let root_name_column = root_icon_column + 2;
        tree.icon(root_icon_column, 1, EntryIcon::root(&self.tree.root));
        tree.label(
            1,
            root_name_column..layout.tree.w.saturating_sub(2),
            &format!("{project_name}/"),
            GOLD,
            &self.spacing,
        );
        let tree_rect = layout.tree_body();
        let first = self.tree_scroll.first_cell();
        let mut tree_body = Frame::viewport(tree_rect.w, tree_rect.h + 1, first);
        let mut tree_name_preview = None;
        let tree_focused = ShellWindow::Pane(Focus::Manager).is_selected(self.focus, help_open);
        for (i, entry) in self
            .tree
            .entries
            .iter()
            .enumerate()
            .skip(first)
            .take(tree_rect.h + 1)
        {
            let y = i - first;
            let x = layout.tree_name_column(entry.depth);
            let icon_column = x.saturating_sub(2);
            let selected = i == self.tree.selected;
            if selected {
                tree_body.tint(y, 0, tree_rect.w, selection_tint);
            } else if !help_selected
                && !entry.directory
                && active_file == Some(entry.path.as_path())
            {
                tree_body.tint(y, 0, tree_rect.w, ACTIVE_FILE_TINT);
            }
            if self.tree.marked.contains(&entry.path) {
                tree_body.text(0, y, "●", GOLD, 1);
            }
            for depth in 0..=entry.depth {
                let column = Layout::tree_indent_column(depth);
                if column >= icon_column {
                    break;
                }
                let continues = entry.branch_continues(depth);
                let branch_before = (depth == entry.depth).then_some(icon_column);
                if continues || branch_before.is_some() {
                    tree_body.tree_guide(column, y, continues, branch_before);
                }
            }
            let color = if selected && !help_selected {
                ACCENT
            } else {
                TEXT
            };
            let icon = entry.icon(self.tree.expanded.contains(&entry.path));
            tree_body.icon(icon_column, y, icon);
            let name = entry.path.file_name().unwrap_or_default().to_string_lossy();
            let slot = x..tree_rect.w.saturating_sub(1);
            let fitted = self.spacing.ellipsize(&name, slot.len());
            let left = tree_rect.x + x;
            let available = layout.cols.saturating_sub(left + 1);
            if selected
                && tree_focused
                && fitted.as_ref() != name.as_ref()
                && available > slot.len()
            {
                let expanded = self.spacing.ellipsize(&name, available);
                let width = (self.spacing.columns(&expanded) + 1).min(available);
                let mut frame = Frame::new(width, 1);
                // The extension masks editor text while the label starts on the tree row.
                let extension_bg = rgba(BACKGROUND | 0xff).blend(rgba(SELECTION));
                frame.tint(0, tree_rect.w - x, width, u32::from(extension_bg));
                frame.label(0, 0..width, &expanded, color, &self.spacing);
                tree_name_preview = Some((
                    Rect {
                        x: left,
                        y: tree_rect.y + y,
                        w: width,
                        h: 1,
                    },
                    frame,
                ));
                continue;
            }
            tree_body.label(y, slot, &fitted, color, &self.spacing);
        }
        let mut edit = Frame::new(layout.editor.w, layout.editor.h);
        self.window_border(
            &mut edit,
            ShellWindow::Pane(Focus::Editor),
            &format!("{filename}{modified}"),
        );
        let body = self.editor_text();
        let gutter = body.x - layout.editor_body().x;
        let first = buffer.scroll.first_cell();
        let mut numbers = Frame::viewport(gutter, body.h + 1, first);
        let left = buffer.columns.first_cell() - buffer.left_padding();
        let width = body.w + buffer.left_padding() + TEXT_OVERSCAN;
        let right = left + width;
        let mut text = Frame::viewport(width, body.h + 1, first);
        let selection = vim.selection();
        let composition = if active {
            self.composition.as_str()
        } else {
            ""
        };
        for visible in 0..=body.h {
            let line = first + visible;
            if line >= vim.doc.starts.len() {
                numbers.text(2, visible, "~", MUTED, 1);
                continue;
            }
            let current = row == line && active;
            if current {
                numbers.tint(visible, 0, gutter, CURRENT);
            }
            numbers.text(
                0,
                visible,
                &format!("{:>width$} │", line + 1, width = gutter.saturating_sub(2)),
                if current { GOLD } else { MUTED },
                gutter,
            );
            let (displayed, preedit) = buffer.displayed_line(line, composition);
            let tint = if current { CURRENT } else { 0 };
            text.viewport_line(
                visible,
                self.spacing.cells(&displayed),
                left..right,
                tint,
                |byte| {
                    if preedit.as_ref().is_some_and(|range| range.contains(&byte)) {
                        (GOLD, 0xc9a3f251)
                    } else if selection
                        .as_ref()
                        .is_some_and(|range| range.contains(&(vim.doc.starts[line] + byte)))
                    {
                        (TEXT, selection_tint)
                    } else {
                        (TEXT, tint)
                    }
                },
            );
        }
        (
            [header, tree, edit, footer, tree_body, numbers, text],
            tree_name_preview,
        )
    }
}
