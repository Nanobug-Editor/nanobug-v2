//! Pane focus, cell rectangles, popup placement and border hit geometry.

use super::{Editor, shortcuts};
use crate::ui::grid::{Metrics, Rect};
use gpui::{Bounds, Pixels, Point, Window, point, px};
use std::ops::Range;

/// Distance from the character border's centerline to the rounded cutout, in logical pixels.
const HELP_BORDER_GAP: f32 = 12.;
/// Radius of the shortcut popup's outer corners, in logical pixels.
pub(super) const HELP_CORNER_RADIUS: f32 = 14.;
/// Extra pointer tolerance around the border cells, in logical pixels.
const BORDER_HOVER_SLOP: f32 = 3.;
/// Footer rows for status and command input or hints.
pub(super) const FOOTER_ROWS: usize = 2;

/// Pane that owns input when shortcut help is closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Focus {
    /// Arrow/Enter filesystem navigation.
    Manager,
    /// The active buffer's native Vim state machine.
    Editor,
}

impl Focus {
    /// Return the other pane in the two-pane workspace.
    pub(super) fn other(self) -> Self {
        match self {
            Self::Manager => Self::Editor,
            Self::Editor => Self::Manager,
        }
    }
}

/// One selectable window inside the editor shell.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ShellWindow {
    /// The file manager or editor pane.
    Pane(Focus),
    /// The shortcut help popup.
    ShortcutHelp,
}

impl ShellWindow {
    /// Check the sole selected window, prioritizing open help over the remembered `pane`.
    pub(super) fn is_selected(self, pane: Focus, help_open: bool) -> bool {
        let selected = if help_open {
            Self::ShortcutHelp
        } else {
            Self::Pane(pane)
        };
        self == selected
    }
}

/// Derived workspace layout, including the footer's pixel alignment.
#[derive(Clone, Copy, Default, PartialEq)]
pub(super) struct Layout {
    /// Total grid columns.
    pub(super) cols: usize,
    /// Total grid rows.
    pub(super) rows: usize,
    /// Signed offset from the grid footer row that keeps the bottom inset equal to the top inset.
    pub(super) footer_offset: Pixels,
    /// Manager viewport, excluding global chrome.
    pub(super) tree: Rect,
    /// Editing viewport, excluding global chrome.
    pub(super) editor: Rect,
}

impl Layout {
    /// Scrollable file rows, excluding the title, root path and character border.
    pub(super) fn tree_body(self) -> Rect {
        Rect {
            x: self.tree.x + 1,
            y: self.tree.y + 2,
            w: self.tree.w.saturating_sub(2),
            h: self.tree.h.saturating_sub(3),
        }
    }
    /// Body-relative branch column at `depth`, after the mark and caret gutter.
    pub(super) fn tree_indent_column(depth: usize) -> usize {
        1 + depth * 2
    }
    /// Body-relative name column at `depth`, leaving room for the branch and icon.
    pub(super) fn tree_name_column(self, depth: usize) -> usize {
        (Self::tree_indent_column(depth + 1) + 2).min(self.tree_body().w.saturating_sub(5))
    }
    /// Scrollable text rows, excluding the character border.
    pub(super) fn editor_body(self) -> Rect {
        Rect {
            x: self.editor.x + 1,
            y: self.editor.y + 1,
            w: self.editor.w.saturating_sub(2),
            h: self.editor.h.saturating_sub(2),
        }
    }
}

impl Editor {
    /// Width of the dynamic line-number gutter in character cells.
    pub(super) fn gutter(&self) -> usize {
        self.vim().doc.starts.len().to_string().len().max(4) + 3
    }

    /// Resolve this window's current dimensions to the two panel rectangles.
    pub(super) fn derive_layout(&self, window: &Window) -> Layout {
        let size = window.viewport_size();
        let cols = ((f32::from(size.width) - 2. * f32::from(self.metrics.origin.x))
            / self.metrics.cw)
            .floor()
            .max(60.) as usize;
        let grid_height = f32::from(size.height - self.metrics.origin.y * 2.);
        // The bottom rule sits at mid-row, leaving room to share part of that row with the footer.
        let border_slack = self.metrics.lh / 3.;
        let rows = ((grid_height + border_slack) / self.metrics.lh)
            .floor()
            .max(16.) as usize;
        let sidebar = match self.hidden_pane {
            Some(Focus::Manager) => 0,
            Some(Focus::Editor) => cols,
            None => self.sidebar.clamp(16, cols.saturating_sub(30).max(16)),
        };
        let editor_x = sidebar + usize::from(self.hidden_pane.is_none());
        Layout {
            cols,
            rows,
            footer_offset: px((grid_height - rows as f32 * self.metrics.lh).max(-border_slack)),
            tree: Rect {
                x: 0,
                y: 1,
                w: sidebar,
                h: rows - 3,
            },
            editor: Rect {
                x: editor_x,
                y: 1,
                w: cols - editor_x,
                h: rows - 3,
            },
        }
    }

    /// Text viewport excluding the line numbers, which scroll vertically only.
    pub(super) fn editor_text(&self) -> Rect {
        let body = self.layout.editor_body();
        let gutter = (self.gutter() - 1).min(body.w);
        Rect {
            x: body.x + gutter,
            w: body.w - gutter,
            ..body
        }
    }

    /// Available columns for the footer's command, search or status message.
    pub(super) fn command_slot(&self) -> Range<usize> {
        1..self.layout.cols.saturating_sub(1)
    }

    /// Return the centered help bounds and the focused pane's rows before shared shortcuts.
    pub(super) fn help_layout(&self) -> (Rect, &'static [(&'static str, &'static str)]) {
        let pane_rows = if self.focus == Focus::Manager {
            shortcuts::MANAGER
        } else {
            shortcuts::EDITOR
        };
        let w = (self.layout.cols - 4).min(74);
        let h = (self.layout.rows - 4).min(pane_rows.len() + shortcuts::GLOBAL.len() + 3);
        (
            Rect {
                x: (self.layout.cols - w) / 2,
                y: (self.layout.rows - h) / 2,
                w,
                h,
            },
            pane_rows,
        )
    }

    /// Return `target`'s grid rectangle, with zero width for a hidden pane.
    pub(super) fn window_area(&self, target: ShellWindow) -> Rect {
        match target {
            ShellWindow::Pane(Focus::Manager) => self.layout.tree,
            ShellWindow::Pane(Focus::Editor) => self.layout.editor,
            ShellWindow::ShortcutHelp => self.help_layout().0,
        }
    }
}

/// Test `position` against `area`'s border cells and their pixel tolerance using `metrics`.
pub(super) fn near_border(area: Rect, metrics: Metrics, position: Point<Pixels>) -> bool {
    if area.close_button().is_none() {
        return false;
    }
    let bounds = area.pixels(metrics);
    let slop = point(px(BORDER_HOVER_SLOP), px(BORDER_HOVER_SLOP));
    let inset = point(px(metrics.cw), px(metrics.lh)) + slop;
    let outer = Bounds::from_corners(bounds.origin - slop, bounds.bottom_right() + slop);
    let inner = Bounds::from_corners(bounds.origin + inset, bounds.bottom_right() - inset);
    outer.contains(&position) && !inner.contains(&position)
}

/// Return the backdrop cutout bounds with an even gap around the character border inside `grid`.
/// `metrics` locates the centerline in the first and last rows and columns.
pub(super) fn help_outline(grid: Bounds<Pixels>, metrics: Metrics) -> Bounds<Pixels> {
    let padding = point(
        px(HELP_BORDER_GAP - metrics.cw * 0.5),
        px(HELP_BORDER_GAP - metrics.lh * 0.5),
    );
    Bounds::from_corners(grid.origin - padding, grid.bottom_right() + padding)
}

#[cfg(test)]
mod tests;
