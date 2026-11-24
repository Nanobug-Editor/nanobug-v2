//! Derive the file-manager border, visible tree rows and overflowing-name preview.

use super::ManagerWindow;
use crate::{
    grid::{
        render::{Frame, Rect},
        windows::WindowFrame,
    },
    ui::{
        components::{
            grid_file_row::{FileRowState, GridFileRow},
            grid_window::GridWindow,
        },
        file_icons::EntryIcon,
        theme::{GOLD, ThemeComponent},
    },
};
use nanobug::i18n;
use std::path::Path;

/// File-manager frames and the optional cross-pane filename preview.
pub(in crate::grid) struct ManagerFrames {
    /// Border, title and project-root row.
    pub chrome: Frame,
    /// Scrollable file rows.
    pub body: Frame,
    /// Selected filename painted above adjacent content when it overflows.
    pub preview: Option<(Rect, Frame)>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::text::TextSpacing;
    use gpui::{FontId, NoopTextSystem};
    use std::sync::Arc;

    /// The extension uses the same selection role as the row and leaves existing row tint alone.
    #[test]
    fn overflowing_name_uses_the_same_tint_as_the_selected_tree_row() -> anyhow::Result<()> {
        let project = tempfile::tempdir()?;
        std::fs::write(project.path().join("Markdown-Preview-Notices.txt"), "")?;
        let manager = ManagerWindow::new(project.path().into());
        let spacing = TextSpacing::new(Arc::new(NoopTextSystem::new()), FontId(0), 16., 9.);
        let context = WindowFrame {
            area: Rect {
                x: 3,
                y: 1,
                w: 18,
                h: 8,
            },
            selected: true,
            modal_open: false,
            controls_hovered: false,
            spacing: &spacing,
        };
        let frames = manager.frames(context, 80, None);
        let (area, preview) = frames
            .preview
            .expect("long name should extend over the pane");
        let tree = ManagerWindow::body(context.area);
        let edge = tree.x + tree.w - area.x;
        assert!(area.w > edge && edge > 0);
        assert_eq!(preview.cell_paint_at(0, edge - 1).background, 0);
        assert_eq!(
            preview.cell_paint_at(0, edge).background,
            frames.body.cell_paint_at(0, tree.w - 1).background,
        );
        Ok(())
    }
}

impl ManagerWindow {
    /// Lay out Explorer in `area`, reserving one fixed interior row for the project root.
    pub(in crate::grid) fn grid_window(area: Rect) -> GridWindow {
        GridWindow::new(area, 1, 0)
    }

    /// Return the file rows inside `area`, excluding the title, root path and pane border.
    pub(in crate::grid) fn body(area: Rect) -> Rect {
        Self::grid_window(area).body()
    }

    /// Build the pane in `context`, highlighting `active_file` and expanding selected names within `cols`.
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
        let window = Self::grid_window(context.area);
        let tree_rect = window.body();
        let mut tree = Frame::grid_window(
            window,
            ThemeComponent::Explorer,
            context.selected,
            |frame, slot, ink| {
                Some(frame.label(0, slot, i18n::text("manager.title"), ink, context.spacing))
            },
        );
        let header = window.header();
        if header.h > 0 && header.w > 0 {
            let row = header.y - window.bounds().y;
            let root_icon_column = 2;
            let root_name_column = root_icon_column + 2;
            tree.icon(
                root_icon_column,
                row,
                EntryIcon::root(&self.tree.root).image(),
            );
            tree.label(
                row,
                root_name_column..context.area.w.saturating_sub(2),
                &format!("{project_name}/"),
                GOLD,
                context.spacing,
            );
        }
        let first = self.scroll.first_cell();
        let mut tree_body = Frame::viewport(tree_rect.w, tree_rect.h + 1, first);
        tree_body.set_theme_component(ThemeComponent::ExplorerTree);
        let mut tree_name_preview = None;
        let tree_focused = context.selected;
        for (y, row) in self.tree.visible.visible_rows(first, tree_rect.h) {
            let entry = &self.tree.entries[row.id()];
            let x = (row.control_column() + 2).min(tree_rect.w.saturating_sub(5));
            let selected = row.id() == self.tree.selected;
            let mut file = GridFileRow::new(
                &entry.path,
                entry.directory,
                self.tree.expanded.contains(&entry.path),
                0..tree_rect.w,
            );
            file.name_columns = x..tree_rect.w.saturating_sub(1);
            file.state = if selected {
                FileRowState::selection(!modal_open)
            } else if !modal_open && !entry.directory && active_file == Some(entry.path.as_path()) {
                FileRowState::ActiveFile
            } else {
                FileRowState::Normal
            };
            file.mark = self.tree.marked.contains(&entry.path).then_some(0);
            tree_body.tree_row_guides(y, &row, file.icon_column());
            let left = tree_rect.x + x;
            let available = cols.saturating_sub(left + 1);
            let overflow = (selected && tree_focused).then_some(available);
            if let Some(expanded) = tree_body.file_row(y, &file, context.spacing, overflow) {
                let width = (context.spacing.columns(&expanded) + 1).min(available);
                let mut frame = Frame::new(width, 1);
                frame.set_theme_component(ThemeComponent::ExplorerTree);
                // The tree row supplies the background under the beginning of the label.
                let extension_start = tree_rect.w - x;
                frame.exclude_theme_background(Rect {
                    w: extension_start,
                    h: 1,
                    ..Rect::default()
                });
                if let Some(tint) = file.state.tint() {
                    frame.tint(0, extension_start, width, tint);
                }
                frame.label(0, 0..width, &expanded, file.ink(), context.spacing);
                tree_name_preview = Some((
                    Rect {
                        x: left,
                        y: tree_rect.y + y,
                        w: width,
                        h: 1,
                    },
                    frame,
                ));
            }
        }
        ManagerFrames {
            chrome: tree,
            body: tree_body,
            preview: tree_name_preview,
        }
    }
}
