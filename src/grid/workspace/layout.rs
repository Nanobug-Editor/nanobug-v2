//! Pane focus, cell rectangles, popup placement and close button geometry.

use super::Workspace;
use crate::grid::render::{Metrics, Rect};
use gpui::{Bounds, Pixels, Point, Window, point, px};
use std::ops::Range;

/// Extra pointer tolerance around the corner controls, in logical pixels.
const CORNER_HOVER_SLOP: f32 = 3.;
/// Matching outer insets above the project tabs and below the status bar, in logical pixels.
pub(super) const OUTER_INSET_Y: f32 = 5.;
/// Extra separation between the project tabs and the panes, in logical pixels.
pub(super) const TAB_GAP_Y: f32 = 4.;
/// The workspace's shared status and command row.
pub(super) const FOOTER_ROWS: usize = 1;
/// Minimum clearance from the panes' bottom rule to the visible status badges, in logical pixels.
const FOOTER_GAP_Y: f32 = 4.;
/// Space above and below the status badge fills within the row, in logical pixels.
pub(super) const FOOTER_BADGE_INSET_Y: f32 = 3.;
/// Horizontal padding inside each status badge, in columns.
pub(super) const FOOTER_CHIP_PAD: usize = 1;
/// Space between status groups, in columns.
pub(super) const FOOTER_LABEL_GAP: usize = 2;
/// Encoding displayed for the active document.
pub(super) const FOOTER_ENCODING: &str = "UTF-8";
/// Space between the encoding and the percentage label.
pub(super) const FOOTER_ENCODING_GAP: &str = "  ";
/// Decimal columns reserved before the percent sign.
pub(super) const FOOTER_PROGRESS_DIGITS: usize = 3;
/// Decimal columns reserved for the line number.
pub(super) const FOOTER_LINE_DIGITS: usize = 4;
/// Decimal columns reserved for the column number.
pub(super) const FOOTER_COLUMN_DIGITS: usize = 3;
/// Total width of the position badge, including its left tip and padding.
const FOOTER_POSITION_WIDTH: usize = 12;

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
    /// The three-column file picker.
    FilePicker,
    /// The shortcut help popup.
    ShortcutHelp,
}

impl ShellWindow {
    /// Whether this window has a shortcut catalog beside its close control.
    pub(super) fn has_help(self) -> bool {
        matches!(self, Self::Pane(_) | Self::FilePicker)
    }

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
    /// Signed offset from the grid footer row that aligns the visible badges with the top inset.
    pub(super) footer_offset: Pixels,
    /// Manager viewport, excluding global chrome.
    pub(super) tree: Rect,
    /// Editing viewport, excluding global chrome.
    pub(super) editor: Rect,
}

/// Nonoverlapping text slots in the footer, shared with command caret measurement.
pub(super) struct FooterSlots {
    /// Mode, operation feedback, or command input.
    pub(super) left: Range<usize>,
    /// Encoding and line progress, inset within their shared badge.
    pub(super) details: Range<usize>,
    /// Column occupied by the leading tip of the details badge.
    pub(super) details_tip: usize,
    /// Line and column label, inset within its badge.
    pub(super) position: Range<usize>,
    /// Column occupied by the position tip on top of the details badge.
    pub(super) position_tip: usize,
}

/// Fit rows into `height` using `line_height`, returning the footer's offset from its grid row.
fn footer_geometry(height: Pixels, line_height: f32) -> (usize, Pixels) {
    let grid_height = f32::from(height) - OUTER_INSET_Y * 2. + FOOTER_BADGE_INSET_Y;
    // The rule sits halfway up its final row, and the badges begin below the footer row's top.
    let border_slack = line_height * 0.5 - TAB_GAP_Y - FOOTER_GAP_Y + FOOTER_BADGE_INSET_Y;
    let rows = ((grid_height + border_slack) / line_height)
        .floor()
        .max(16.) as usize;
    let offset = (grid_height - rows as f32 * line_height).max(-border_slack) - TAB_GAP_Y;
    (rows, px(offset))
}

impl Workspace {
    /// Bottom edge of the Windows caption strip, including the gap below the project tabs.
    #[cfg(windows)]
    pub(super) fn native_caption_height(&self) -> Pixels {
        self.metrics.at(0., 1.).y
    }

    /// Resolve this window's current dimensions to the two panel rectangles.
    pub(super) fn derive_layout(&self, window: &Window) -> Layout {
        let size = window.viewport_size();
        let cols = ((f32::from(size.width) - 2. * f32::from(self.metrics.origin.x))
            / self.metrics.cw)
            .floor()
            .max(60.) as usize;
        let (rows, footer_offset) = footer_geometry(size.height, self.metrics.lh);
        let Some(project) = self.projects.active_project() else {
            return Layout {
                cols,
                rows,
                footer_offset,
                ..Layout::default()
            };
        };
        let (sidebar, editor_x) = match (&project.manager, project.hidden_pane) {
            (None, _) | (_, Some(Focus::Manager)) => (0, 0),
            (_, Some(Focus::Editor)) => (cols, cols),
            (Some(_), None) => {
                let sidebar = project.sidebar.clamp(16, cols.saturating_sub(30).max(16));
                (sidebar, sidebar + 1)
            }
        };
        let pane_height = rows - 1 - FOOTER_ROWS;
        Layout {
            cols,
            rows,
            footer_offset,
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

    /// Reserve fixed-width status badges and their tips, leaving the left side for input.
    pub(super) fn footer_slots(&self) -> FooterSlots {
        let position_width = FOOTER_LINE_DIGITS + 1 + FOOTER_COLUMN_DIGITS;
        let position_padding = (FOOTER_POSITION_WIDTH - position_width) / 2;
        let details_width =
            FOOTER_ENCODING.len() + FOOTER_ENCODING_GAP.len() + FOOTER_PROGRESS_DIGITS + 1;
        let position_end = self.layout.cols.saturating_sub(position_padding);
        let position_start = position_end.saturating_sub(position_width);
        let position_tip = self.layout.cols.saturating_sub(FOOTER_POSITION_WIDTH);
        let details_end = position_tip.saturating_sub(FOOTER_CHIP_PAD);
        let details_start = details_end.saturating_sub(details_width);
        let details_tip = details_start.saturating_sub(FOOTER_CHIP_PAD + 1);
        let left_end = details_tip
            .saturating_sub(FOOTER_LABEL_GAP)
            .max(FOOTER_CHIP_PAD);
        FooterSlots {
            left: FOOTER_CHIP_PAD..left_end,
            details: details_start..details_end,
            details_tip,
            position: position_start..position_end,
            position_tip,
        }
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
            ShellWindow::FilePicker => self.file_picker_layout().area,
            ShellWindow::ShortcutHelp => self.help_layout(),
        }
    }
}

/// Test `position` near `area`'s controls, adding the help target when `include_help` is true.
/// `metrics` converts the grid slots to window-local pixels.
pub(super) fn near_corner_controls(
    area: Rect,
    include_help: bool,
    metrics: Metrics,
    position: Point<Pixels>,
) -> bool {
    area.corner_controls(include_help).is_some_and(|controls| {
        let bounds = controls.area().pixels(metrics);
        let slop = point(px(CORNER_HOVER_SLOP), px(CORNER_HOVER_SLOP));
        Bounds::from_corners(bounds.origin - slop, bounds.bottom_right() + slop).contains(&position)
    })
}

#[cfg(test)]
mod tests;
