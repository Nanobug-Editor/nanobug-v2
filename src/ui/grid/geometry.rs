//! Grid-to-pixel geometry and the border close glyph shared by painting and pointer input.

use super::{BORDER_LABEL_INSET, CLOSE_LABEL_WIDTH};
use crate::ui::{
    text::UNITS_PER_COLUMN,
    theme::{GOLD, GRID_TEXT_FONT},
};
use gpui::{
    Bounds, FontId, GlyphId, IntoElement, Pixels, PlatformTextSystem, Point, Styled, TextRun,
    Window, canvas, point, px, rgb, rgba, size,
};

/// Common font geometry, shared by grid drawing, pointer hit testing and the cursor.
#[derive(Clone, Copy)]
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

/// Half-open panel rectangle in character cells.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct Rect {
    /// First column.
    pub x: usize,
    /// First row.
    pub y: usize,
    /// Width in cells.
    pub w: usize,
    /// Height in cells.
    pub h: usize,
}

impl Rect {
    /// Whether cell `(x, y)` lies inside this rectangle.
    pub fn contains(self, x: usize, y: usize) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }
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

    /// Return the padded close label's rectangle, mirrored from the title's left inset.
    pub fn close_button(self) -> Option<Self> {
        if self.w <= 2 * (BORDER_LABEL_INSET + CLOSE_LABEL_WIDTH) || self.h < 2 {
            return None;
        }
        Some(Self {
            x: self.x + self.w - BORDER_LABEL_INSET - CLOSE_LABEL_WIDTH,
            y: self.y,
            w: CLOSE_LABEL_WIDTH,
            h: 1,
        })
    }
}

/// The grid font's multiplication glyph fitted to the ink height of a capital X.
#[derive(Clone, Copy)]
pub struct CloseGlyph {
    /// Text face that provides the multiplication glyph.
    font_id: FontId,
    /// Glyph index in the bundled font.
    glyph_id: GlyphId,
    /// Em size needed to match the capital ink height.
    font_size: Pixels,
    /// Offset from the label's horizontal center and top edge to the glyph's baseline.
    offset: Point<Pixels>,
}

impl CloseGlyph {
    /// Measure the bundled `font_id` through `glyphs` and align it to the normal text in `window`.
    /// `metrics` supplies the body font size and grid row height.
    pub fn new(
        metrics: Metrics,
        font_id: FontId,
        glyphs: &dyn PlatformTextSystem,
        window: &Window,
    ) -> Self {
        let glyph_id = glyphs.glyph_for_char(font_id, '×').expect("grid font ×");
        let capital_id = glyphs.glyph_for_char(font_id, 'X').expect("grid font X");
        let cross = glyphs
            .typographic_bounds(font_id, glyph_id)
            .expect("grid font × bounds");
        let capital = glyphs
            .typographic_bounds(font_id, capital_id)
            .expect("grid font X bounds");
        let text_system = window.text_system();
        let units_per_em = text_system.units_per_em(font_id) as f32;
        let body_scale = metrics.font_size / units_per_em;
        let cross_scale = body_scale * capital.size.height / cross.size.height;
        let font_size = px(cross_scale * units_per_em);
        let capital_line = text_system.shape_line(
            "X".into(),
            px(metrics.font_size),
            &[TextRun {
                len: 1,
                font: GRID_TEXT_FONT.clone(),
                ..Default::default()
            }],
            Some(px(metrics.cw)),
        );
        let baseline = (px(metrics.lh) + capital_line.ascent - capital_line.descent) / 2.;
        let offset = point(
            px(-cross.center().x * cross_scale),
            baseline + px(cross.origin.y * cross_scale - capital.origin.y * body_scale),
        );
        Self {
            font_id,
            glyph_id,
            font_size,
            offset,
        }
    }

    /// Paint the glyph and its text shadow in the center of the padded button.
    pub fn element(self) -> impl IntoElement {
        canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                let origin = point(bounds.center().x, bounds.origin.y) + self.offset;
                let _ = window.paint_glyph(
                    origin + point(px(0.), px(1.)),
                    self.font_id,
                    self.glyph_id,
                    self.font_size,
                    rgba(0x00000070).into(),
                );
                let _ = window.paint_glyph(
                    origin,
                    self.font_id,
                    self.glyph_id,
                    self.font_size,
                    rgb(GOLD).into(),
                );
            },
        )
        .size_full()
    }
}
