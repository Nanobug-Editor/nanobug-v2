//! Pane focus, cell rectangles, popup placement and close button geometry.

use super::Workspace;
use crate::grid::render::{Metrics, Rect};
use gpui::{Bounds, Pixels, Point, Window, point, px};
use std::ops::Range;

/// Extra pointer tolerance around the close button, in logical pixels.
const CLOSE_HOVER_SLOP: f32 = 3.;
/// The workspace's shared status and command row.
pub(super) const FOOTER_ROWS: usize = 1;

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

impl Workspace {
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
        let sidebar = match self.projects.active.hidden_pane {
            Some(Focus::Manager) => 0,
            Some(Focus::Editor) => cols,
            None => self
                .projects
                .active
                .sidebar
                .clamp(16, cols.saturating_sub(30).max(16)),
        };
        let editor_x = sidebar + usize::from(self.projects.active.hidden_pane.is_none());
        let pane_height = rows - 1 - FOOTER_ROWS;
        Layout {
            cols,
            rows,
            footer_offset: px((grid_height - rows as f32 * self.metrics.lh).max(-border_slack)),
            tree: Rect {
                x: 0,
                y: 1,
                w: sidebar,
                h: pane_height,
            },
            editor: Rect {
                x: editor_x,
                y: 1,
                w: cols - editor_x,
                h: pane_height,
            },
        }
    }

    /// Available columns for commands and messages, separated from the right-aligned `position`.
    pub(super) fn command_slot(&self, position: &str) -> Range<usize> {
        let reserved = self.spacing.columns(position) + 2;
        1..self.layout.cols.saturating_sub(reserved).max(1)
    }

    /// Locate the open popup within the current workspace grid.
    pub(super) fn help_layout(&self) -> Rect {
        self.help.layout(self.layout.cols, self.layout.rows)
    }

    /// Place a `width` by `height` grid popup above the lower-right status line.
    pub(super) fn small_popup_area(&self, width: usize, height: usize) -> Rect {
        Rect {
            x: self.layout.cols.saturating_sub(width + 1),
            y: self.layout.rows.saturating_sub(height + FOOTER_ROWS + 1),
            w: width,
            h: height,
        }
    }

    /// Return `target`'s grid rectangle, with zero width for a hidden pane.
    pub(super) fn window_area(&self, target: ShellWindow) -> Rect {
        match target {
            ShellWindow::Pane(Focus::Manager) => self.layout.tree,
            ShellWindow::Pane(Focus::Editor) => self.layout.editor,
            ShellWindow::ShortcutHelp => self.help_layout(),
        }
    }
}

/// Test `position` near `area`'s close label, using `metrics` to convert cells to pixels.
pub(super) fn near_close_button(area: Rect, metrics: Metrics, position: Point<Pixels>) -> bool {
    area.close_button().is_some_and(|button| {
        let bounds = button.pixels(metrics);
        let slop = point(px(CLOSE_HOVER_SLOP), px(CLOSE_HOVER_SLOP));
        Bounds::from_corners(bounds.origin - slop, bounds.bottom_right() + slop).contains(&position)
    })
}

#[cfg(test)]
mod tests;
