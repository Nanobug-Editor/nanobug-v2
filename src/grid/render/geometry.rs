//! Grid-to-pixel geometry shared by painting and pointer input.

use super::BORDER_LABEL_INSET;
use crate::ui::{grid_geometry::Rect, text::UNITS_PER_COLUMN};
use gpui::{Bounds, Pixels, Point, point, px, size};
use std::ops::Range;

/// Split `viewport` into the four strips outside `cutout`, returning empty strips as needed.
pub(in crate::grid) fn exposed_regions(
    viewport: Bounds<Pixels>,
    cutout: Bounds<Pixels>,
) -> [Bounds<Pixels>; 4] {
    let hole = viewport.intersect(&cutout);
    if hole.is_empty() {
        return [
            viewport,
            Bounds::default(),
            Bounds::default(),
            Bounds::default(),
        ];
    }
    [
        Bounds::from_corners(viewport.origin, point(viewport.right(), hole.top())),
        Bounds::from_corners(
            point(viewport.left(), hole.top()),
            point(hole.left(), hole.bottom()),
        ),
        Bounds::from_corners(
            point(hole.right(), hole.top()),
            point(viewport.right(), hole.bottom()),
        ),
        Bounds::from_corners(
            point(viewport.left(), hole.bottom()),
            viewport.bottom_right(),
        ),
    ]
}

/// Grid columns occupied by each corner button, including its clickable padding.
const CORNER_BUTTON_COLUMNS: usize = 3;

/// Common font geometry, shared by grid drawing, pointer hit testing and the cursor.
#[derive(Clone, Copy, PartialEq)]
pub struct Metrics {
    /// Narrow-cell advance in logical pixels, snapped to a whole device pixel.
    pub cw: f32,
    /// Row height in logical pixels.
    pub lh: f32,
    /// Font size in logical pixels.
    pub font_size: f32,
    /// Grid origin inside the native client area.
    pub origin: Point<Pixels>,
}

impl Metrics {
    /// Convert character `x`, `y` coordinates to window-local pixels.
    pub fn at(self, x: f32, y: f32) -> Point<Pixels> {
        self.origin + point(px(x * self.cw), px(y * self.lh))
    }

    /// Convert horizontal `units` from text spacing to logical pixels.
    pub(super) fn spacing_px(self, units: isize) -> Pixels {
        px(units as f32 * self.cw / UNITS_PER_COLUMN as f32)
    }
    /// Convert native `point` to nonnegative character coordinates.
    pub fn cell(self, point: Point<Pixels>) -> (usize, usize) {
        (
            ((f32::from(point.x - self.origin.x) / self.cw)
                .floor()
                .max(0.)) as usize,
            ((f32::from(point.y - self.origin.y) / self.lh)
                .floor()
                .max(0.)) as usize,
        )
    }
}

impl Rect {
    /// Convert this rectangle to pixels using `metrics`.
    pub fn pixels(self, metrics: Metrics) -> Bounds<Pixels> {
        Bounds::new(
            metrics.at(self.x as f32, self.y as f32),
            size(
                px(self.w as f32 * metrics.cw),
                px(self.h as f32 * metrics.lh),
            ),
        )
    }

    /// Return local columns for this border's title, leaving room for the visible controls.
    pub(in crate::grid) fn border_title_columns(
        self,
        controls: Option<&CornerControls>,
    ) -> Range<usize> {
        let start = BORDER_LABEL_INSET + 1;
        let end = controls.map_or(self.w.saturating_sub(3), |controls| {
            (controls.area().x - self.x).saturating_sub(2)
        });
        start..end.max(start)
    }

    /// Place corner buttons at the right inset, with help to the left when `include_help` fits.
    pub(in crate::grid) fn corner_controls(self, include_help: bool) -> Option<CornerControls> {
        let width = CORNER_BUTTON_COLUMNS;
        if self.w < 2 * BORDER_LABEL_INSET + width || self.h < 2 {
            return None;
        }
        let close = Self {
            x: self.x + self.w - BORDER_LABEL_INSET - width,
            y: self.y,
            w: width,
            h: 1,
        };
        let help = (include_help && self.w >= 2 * BORDER_LABEL_INSET + 2 * width).then(|| Self {
            x: close.x - width,
            ..close
        });
        Some(CornerControls { help, close })
    }
}

/// Adjacent hit targets for the actions shown on a window's top-right border.
pub(in crate::grid) struct CornerControls {
    /// Help button for panes that have a shortcut catalog.
    pub help: Option<Rect>,
    /// Close button at the right inset.
    pub close: Rect,
}

impl CornerControls {
    /// Combined span used for border layout and hover detection.
    pub fn area(&self) -> Rect {
        self.help.map_or(self.close, |help| Rect {
            w: help.w + self.close.w,
            ..help
        })
    }
}
