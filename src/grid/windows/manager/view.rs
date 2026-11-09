//! Derive the file-manager border, visible tree rows and overflowing-name preview.

use super::{EntryIcon, ManagerWindow};
use crate::{
    grid::{
        render::{Frame, Rect},
        windows::WindowFrame,
    },
    ui::theme::{ACCENT, BACKGROUND, GOLD, SELECTION, TEXT},
};
use gpui::rgba;
use nanobug::i18n;
use std::path::Path;

/// Faint mint row background for the file shown in the editor window.
const ACTIVE_FILE_TINT: u32 = (ACCENT << 8) | 0x24;

/// File-manager frames and the optional cross-pane filename preview.
pub(in crate::grid) struct ManagerFrames {
    /// Border, title and project-root row.
    pub chrome: Frame,
    /// Scrollable file rows.
    pub body: Frame,
    /// Selected filename painted above adjacent content when it overflows.
    pub preview: Option<(Rect, Frame)>,
}

impl ManagerWindow {
    /// Return the file rows inside `area`, excluding the title, root path and character border.
    pub(in crate::grid) fn body(area: Rect) -> Rect {
        Rect {
            x: area.x + 1,
            y: area.y + 2,
            w: area.w.saturating_sub(2),
            h: area.h.saturating_sub(3),
        }
    }

    /// Body-relative branch column at `depth`, after the mark and caret gutter.
    pub(in crate::grid) fn indent_column(depth: usize) -> usize {
        1 + depth * 2
    }

    /// Return the name column at `depth` within `area`, leaving room for the branch and icon.
    pub(in crate::grid) fn name_column(area: Rect, depth: usize) -> usize {
        (Self::indent_column(depth + 1) + 2).min(Self::body(area).w.saturating_sub(5))
    }

    /// Build frames for `context` within `cols`, highlighting the editor's `active_file`.
    pub(in crate::grid) fn frames(
        &self,
        context: WindowFrame<'_>,
        cols: usize,
        active_file: Option<&Path>,
    ) -> ManagerFrames {
        let project_name = self
            .tree
            .root
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        let modal_open = context.modal_open;
        let selection_tint = context.selection_tint();
        let mut tree = Frame::new(context.area.w, context.area.h);
        context.border(&mut tree, i18n::text("manager.title"));
        let root_icon_column = Self::indent_column(0) + 1;
        let root_name_column = root_icon_column + 2;
        tree.icon(
            root_icon_column,
            1,
            EntryIcon::root(&self.tree.root).image(),
        );
        tree.label(
            1,
            root_name_column..context.area.w.saturating_sub(2),
            &format!("{project_name}/"),
            GOLD,
            context.spacing,
        );
        let tree_rect = Self::body(context.area);
        let first = self.scroll.first_cell();
        let mut tree_body = Frame::viewport(tree_rect.w, tree_rect.h + 1, first);
        let mut tree_name_preview = None;
        let tree_focused = context.selected;
        for (i, entry) in self
            .tree
            .entries
            .iter()
            .enumerate()
            .skip(first)
            .take(tree_rect.h + 1)
        {
            let y = i - first;
            let x = Self::name_column(context.area, entry.depth);
            let icon_column = x.saturating_sub(2);
            let selected = i == self.tree.selected;
            if selected {
                tree_body.tint(y, 0, tree_rect.w, selection_tint);
            } else if !modal_open && !entry.directory && active_file == Some(entry.path.as_path()) {
                tree_body.tint(y, 0, tree_rect.w, ACTIVE_FILE_TINT);
            }
            if self.tree.marked.contains(&entry.path) {
                tree_body.text(0, y, "●", GOLD, 1);
            }
            for depth in 0..=entry.depth {
                let column = Self::indent_column(depth);
                if column >= icon_column {
                    break;
                }
                let continues = entry.branch_continues(depth);
                let branch_before = (depth == entry.depth).then_some(icon_column);
                if continues || branch_before.is_some() {
                    tree_body.tree_guide(column, y, continues, branch_before);
                }
            }
            let color = if selected && !modal_open {
                ACCENT
            } else {
                TEXT
            };
            let icon = entry.icon(self.tree.expanded.contains(&entry.path));
            tree_body.icon(icon_column, y, icon);
            let name = entry.path.file_name().unwrap_or_default().to_string_lossy();
            let slot = x..tree_rect.w.saturating_sub(1);
            let fitted = context.spacing.ellipsize(&name, slot.len());
            let left = tree_rect.x + x;
            let available = cols.saturating_sub(left + 1);
            if selected
                && tree_focused
                && fitted.as_ref() != name.as_ref()
                && available > slot.len()
            {
                let expanded = context.spacing.ellipsize(&name, available);
                let width = (context.spacing.columns(&expanded) + 1).min(available);
                let mut frame = Frame::new(width, 1);
                // The extension masks editor text while the label starts on the tree row.
                let extension_bg = rgba(BACKGROUND | 0xff).blend(rgba(SELECTION));
                frame.tint(0, tree_rect.w - x, width, u32::from(extension_bg));
                frame.label(0, 0..width, &expanded, color, context.spacing);
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
            tree_body.label(y, slot, &fitted, color, context.spacing);
        }
        ManagerFrames {
            chrome: tree,
            body: tree_body,
            preview: tree_name_preview,
        }
    }
}
