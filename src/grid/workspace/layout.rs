//! Pane focus, cell rectangles, popup placement and close button geometry.

use super::Workspace;
use crate::grid::render::{Metrics, Rect, solid_stroke_width};
use crate::ui::components::grid_dialog::GridDialog;
use gpui::{Bounds, Pixels, Point, Window, point, px};
use std::ops::Range;

/// Extra pointer tolerance around the corner controls, in logical pixels.
const CORNER_HOVER_SLOP: f32 = 3.;
/// Matching outer insets above the project tabs and below the status bar, in logical pixels.
pub(super) const OUTER_INSET_Y: f32 = 5.;
/// Maximum caption drag area below the project tabs, in logical pixels.
#[cfg(windows)]
const CAPTION_EXTRA_Y: f32 = 4.;
/// Minimum visible gap on each side of the pane grid, in grid rows.
const MIN_PANE_OUTER_GAP_ROWS: f32 = 0.25;
/// Clearance below the content viewport at the exterior bottom edge, in grid rows.
const PANE_BOTTOM_INSET_ROWS: f32 = 0.25;
/// The workspace's shared status and command row.
pub(super) const FOOTER_ROWS: usize = 1;
/// Space above and below the status badge fills within the row, in logical pixels.
pub(super) const FOOTER_BADGE_INSET_Y: f32 = 1.5;
/// Horizontal padding inside each status badge, in columns.
pub(super) const FOOTER_CHIP_PAD: usize = 1;
/// Space between status groups, in columns.
pub(super) const FOOTER_LABEL_GAP: usize = 2;
/// Encoding displayed for the active document.
pub(super) const FOOTER_ENCODING: &str = "UTF-8";
/// Decimal columns reserved before the percent sign.
pub(super) const FOOTER_PROGRESS_DIGITS: usize = 3;
/// Decimal columns reserved for the line number.
pub(super) const FOOTER_LINE_DIGITS: usize = 4;
/// Decimal columns reserved for the column number.
pub(super) const FOOTER_COLUMN_DIGITS: usize = 3;
/// Margin on each side of the position label, including the chevron at the left.
const FOOTER_POSITION_PADDING: usize = 2;

/// Pane that owns input when shortcut help is closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
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

/// Split `available` cells between Explorer and the editor, clamping `manager` to their minima.
fn split_sizes(available: usize, manager: usize, minimum: (usize, usize)) -> (usize, usize) {
    let first = manager.clamp(
        minimum.0,
        available.saturating_sub(minimum.1).max(minimum.0),
    );
    (first, available - first)
}

/// Position Explorer and the editor inside `area`. `stacked` chooses the split axis,
/// `editor_first` chooses the order, and the preferred sizes apply to Explorer on each axis.
pub(super) fn split_panes(
    area: Rect,
    stacked: bool,
    editor_first: bool,
    sidebar: usize,
    manager_height: Option<usize>,
) -> (Rect, Rect) {
    let (manager, editor) = if stacked {
        let available = area.h;
        let minimum = (available / 2).min(4);
        let (manager, editor) = split_sizes(
            available,
            manager_height.unwrap_or(available / 2),
            (minimum, minimum),
        );
        (Rect { h: manager, ..area }, Rect { h: editor, ..area })
    } else {
        let (manager, editor) = split_sizes(area.w - 1, sidebar, (16, 29));
        (Rect { w: manager, ..area }, Rect { w: editor, ..area })
    };
    let (first, mut second) = if editor_first {
        (editor, manager)
    } else {
        (manager, editor)
    };
    if stacked {
        second.y += first.h;
    } else {
        second.x += first.w + 1;
    }
    if editor_first {
        (second, first)
    } else {
        (first, second)
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

/// Derived workspace layout, including the vertical alignment of the panes and footer.
#[derive(Clone, Copy, Default, PartialEq)]
pub(super) struct Layout {
    /// Total grid columns.
    pub(super) cols: usize,
    /// Total grid rows.
    pub(super) rows: usize,
    /// Whole columns that fit the native client area, before the pane minimum.
    pub(super) visible_cols: usize,
    /// Whole rows that fit below the native client area's grid origin.
    pub(super) visible_rows: usize,
    /// Window-local y coordinate of grid row zero, with the panes centered below it.
    grid_origin_y: Pixels,
    /// Window-local y coordinate of the footer text row.
    footer_top: Pixels,
    /// Manager viewport, excluding global chrome.
    pub(super) tree: Rect,
    /// Editing viewport, excluding global chrome.
    pub(super) editor: Rect,
}

impl Layout {
    /// Full pane allocation between the workspace header and footer.
    pub(super) fn pane_area(self) -> Rect {
        Rect {
            x: 0,
            y: 1,
            w: self.cols,
            h: self.rows.saturating_sub(1 + FOOTER_ROWS),
        }
    }

    /// Clip a rectangle to whole cells within the native viewport.
    pub(super) fn visible_region(self, area: Rect) -> Rect {
        Rect {
            w: area.w.min(self.visible_cols.saturating_sub(area.x)),
            h: area.h.min(self.visible_rows.saturating_sub(area.y)),
            ..area
        }
    }
}

/// Nonoverlapping text slots in the footer, shared with command caret measurement.
pub(super) struct FooterSlots {
    /// Mode and file path, operation feedback, or command input.
    pub(super) left: Range<usize>,
    /// Encoding label, inset within its badge.
    pub(super) details: Range<usize>,
    /// Column occupied by the leading tip of the details badge.
    pub(super) details_tip: usize,
    /// Progress and line:column label, inset within their shared badge.
    pub(super) position: Range<usize>,
    /// Column occupied by the position tip on top of the details badge.
    pub(super) position_tip: usize,
}

/// Distance from the first pane row's start to the top of its centered title icon.
fn pane_title_inset(line_height: f32, font_size: f32) -> f32 {
    (line_height - font_size.min(line_height)) * 0.5
}

/// Reserve both outer gaps, then fit whole pane rows of `line_height` and share the remaining `height`.
/// `font_size` determines the title height and bottom stroke width.
/// Return the total row count and the window-local origins of the grid and footer.
fn vertical_geometry(height: Pixels, line_height: f32, font_size: f32) -> (usize, Pixels, Pixels) {
    let tab_bottom = OUTER_INSET_Y + line_height;
    let footer_top = f32::from(height) - OUTER_INSET_Y - line_height + FOOTER_BADGE_INSET_Y;
    let badge_top = footer_top + FOOTER_BADGE_INSET_Y;
    let title_inset = pane_title_inset(line_height, font_size);
    let bottom_inset = line_height * (1. - PANE_BOTTOM_INSET_ROWS)
        - f32::from(solid_stroke_width(font_size)) * 0.5;
    let min_gap = line_height * MIN_PANE_OUTER_GAP_ROWS;
    let available = badge_top - tab_bottom + title_inset + bottom_inset - 2. * min_gap;
    // Reserve room for the picker contents even during transient, undersized viewports.
    let pane_rows = ((available / line_height).floor() as usize).max(6);
    let gap = min_gap + (available - pane_rows as f32 * line_height).max(0.) * 0.5;
    let grid_origin_y = tab_bottom + gap - title_inset - line_height;
    (
        1 + pane_rows + FOOTER_ROWS,
        px(grid_origin_y),
        px(footer_top),
    )
}

/// Distances from a stacked seam to the top and bottom path centerlines.
/// The title edge gets half a stroke less, and the lower edge gets the same amount more.
fn stacked_path_insets(metrics: Metrics) -> (Pixels, Pixels) {
    let column = px(metrics.cw);
    let half_stroke = solid_stroke_width(metrics.font_size) * 0.5;
    (column - half_stroke, column + half_stroke)
}

/// Return signed vertical shifts of `area`'s paths at stacked seams and the exterior bottom.
pub(super) fn pane_border_offsets(
    area: Rect,
    panes: &[Rect],
    metrics: Metrics,
) -> (Pixels, Pixels) {
    if area.w == 0 || area.h == 0 {
        return (px(0.), px(0.));
    }
    let mut offsets = (px(0.), px(metrics.lh * (PANE_BOTTOM_INSET_ROWS - 0.5)));
    let (top, bottom) = stacked_path_insets(metrics);
    let half_row = px(metrics.lh * 0.5);
    for neighbor in panes.iter().filter(|neighbor| {
        neighbor.w > 0
            && neighbor.h > 0
            && neighbor.x < area.x + area.w
            && area.x < neighbor.x + neighbor.w
    }) {
        if neighbor.y + neighbor.h == area.y {
            offsets.0 = top - half_row;
        }
        if area.y + area.h == neighbor.y {
            offsets.1 = half_row - bottom;
        }
    }
    offsets
}

/// Convert a logical stacked divider `seam` to the blank area between its two painted paths.
pub(super) fn stacked_divider_bounds(seam: Rect, metrics: Metrics) -> Bounds<Pixels> {
    let bounds = seam.pixels(metrics);
    let (top, bottom) = stacked_path_insets(metrics);
    let half_stroke = solid_stroke_width(metrics.font_size) * 0.5;
    Bounds::from_corners(
        point(bounds.left(), bounds.top() - bottom + half_stroke),
        point(bounds.right(), bounds.top() + top - half_stroke),
    )
}

impl Workspace {
    /// The currently visible docked windows used to find shared exterior edges.
    pub(super) fn workspace_pane_areas(&self) -> Vec<Rect> {
        std::iter::once(self.layout.tree)
            .chain(self.editor_rects().into_iter().map(|(_, area)| area))
            .collect()
    }

    /// Bottom edge of the Windows caption strip, stopping at the first visible pane title.
    #[cfg(windows)]
    pub(super) fn native_caption_height(&self) -> Pixels {
        let tab_bottom = self.tab_metrics().at(0., 1.).y;
        let limit = tab_bottom + px(CAPTION_EXTRA_Y);
        if self.projects.active_project().is_none() {
            return limit;
        }
        let title_top = self.metrics.at(0., 1.).y
            + px(pane_title_inset(self.metrics.lh, self.metrics.font_size));
        title_top.max(tab_bottom).min(limit)
    }

    /// Apply `window`'s layout and its shared pane origin for painting and input.
    pub(super) fn update_layout(&mut self, window: &Window) {
        self.layout = self.derive_layout(window);
        self.metrics.origin.y = self.layout.grid_origin_y;
    }

    /// Resolve `window` dimensions and the active project's split to the pane rectangles.
    pub(super) fn derive_layout(&self, window: &Window) -> Layout {
        let size = window.viewport_size();
        let visible_cols = ((f32::from(size.width) - 2. * f32::from(self.metrics.origin.x))
            / self.metrics.cw)
            .floor() as usize;
        let (rows, grid_origin_y, footer_top) =
            vertical_geometry(size.height, self.metrics.lh, self.metrics.font_size);
        let visible_rows =
            (f32::from(size.height - grid_origin_y) / self.metrics.lh).floor() as usize;
        let cols = visible_cols.max(60);
        let mut layout = Layout {
            cols,
            rows,
            visible_cols,
            visible_rows,
            grid_origin_y,
            footer_top,
            ..Layout::default()
        };
        if let Some(project) = self.projects.active_project() {
            (layout.tree, layout.editor) =
                project.pane_regions_for_tabpage(project.active_page(), layout);
        }
        layout
    }

    /// Reserve fixed-width status badges and their tips, leaving the left side for input.
    pub(super) fn footer_slots(&self) -> FooterSlots {
        // Reserve one column each for the percent sign, separating space, and colon.
        let position_width = FOOTER_PROGRESS_DIGITS + FOOTER_LINE_DIGITS + FOOTER_COLUMN_DIGITS + 3;
        let position_end = self
            .layout
            .visible_cols
            .saturating_sub(FOOTER_POSITION_PADDING);
        let position_start = position_end.saturating_sub(position_width);
        let position_tip = position_start.saturating_sub(FOOTER_POSITION_PADDING);
        let details_end = position_tip.saturating_sub(FOOTER_CHIP_PAD);
        let details_start = details_end.saturating_sub(FOOTER_ENCODING.len());
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

    /// Place a `width` by `height` dialog above the lower-right status line.
    pub(super) fn corner_dialog(&self, width: usize, height: usize) -> Rect {
        GridDialog::new(Rect {
            w: self.layout.cols.saturating_sub(1),
            h: self.layout.rows.saturating_sub(FOOTER_ROWS + 1),
            ..Rect::default()
        })
        .bottom_right(width, height)
    }

    /// Anchor the footer row to the window bottom, independently of the pane grid.
    pub(super) fn footer_bounds(&self) -> Bounds<Pixels> {
        let mut bounds = Rect {
            x: 0,
            y: 0,
            w: self.layout.visible_cols,
            h: FOOTER_ROWS,
        }
        .pixels(self.metrics);
        bounds.origin.y = self.layout.footer_top;
        bounds
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
