use gpui::{Bounds, Pixels, Point, Size, point, px};
use std::ops::Range;

/// Space around an anchor row. Both ranges use the same units, such as cells or logical pixels.
#[derive(Clone, Debug)]
pub(crate) struct GridPopover {
    /// Visible vertical interval of the owning surface.
    viewport: Range<f32>,
    /// Vertical interval of the visible part of the anchor.
    anchor: Range<f32>,
}

/// Whole grid rows available to a scrollable list, with its fractional position on screen.
pub(crate) struct PopoverRows {
    /// First row and fractional offset in the same coordinates as the anchor.
    pub(crate) top: f32,
    /// Height of the frame in rows.
    pub(crate) height: usize,
    /// Number of border rows at each end, zero or one.
    pub(crate) inset: usize,
}

impl GridPopover {
    /// Use `anchor` in `viewport`, including a partially scrolled row and rejecting invisible rows.
    pub(crate) fn new(viewport: Range<f32>, anchor: Range<f32>) -> Option<Self> {
        let anchor = anchor.start.max(viewport.start)..anchor.end.min(viewport.end);
        (anchor.start < anchor.end).then_some(Self { viewport, anchor })
    }

    /// Anchor to a fully visible `caret` row of `line_height` pixels within the editor `body`.
    pub(crate) fn at_caret(
        body: Bounds<Pixels>,
        caret: Point<Pixels>,
        line_height: Pixels,
    ) -> Option<Self> {
        if !body.contains(&caret) || body.bottom() < caret.y + line_height {
            return None;
        }
        Self::new(
            f32::from(body.top())..f32::from(body.bottom()),
            f32::from(caret.y)..f32::from(caret.y + line_height),
        )
    }

    /// Maximum content extent on either side of the anchor.
    pub(crate) fn available_height(&self) -> f32 {
        (self.anchor.start - self.viewport.start).max(self.viewport.end - self.anchor.end)
    }

    /// Place content of `height` below when it fits, otherwise above, constrained to the viewport.
    fn top(&self, height: f32) -> f32 {
        let below = self.viewport.end - self.anchor.end;
        let above = self.anchor.start - self.viewport.start;
        let top = if height <= below || below >= above {
            self.anchor.end
        } else {
            self.anchor.start - height
        };
        top.clamp(
            self.viewport.start,
            (self.viewport.end - height).max(self.viewport.start),
        )
    }

    /// Place a list of `count` choices and optional border in whole rows on one side of its anchor.
    /// A one-row viewport still exposes a selectable choice over its anchor.
    pub(crate) fn rows(&self, count: usize, allow_border: bool) -> PopoverRows {
        let above = (self.anchor.start - self.viewport.start).floor() as usize;
        let below = (self.viewport.end - self.anchor.end).floor() as usize;
        let up = below < count.saturating_add(2) && above > below;
        let room = if up { above } else { below };
        let inset = usize::from(room >= 3 && allow_border);
        let height = count.saturating_add(2 * inset).min(room.max(1));
        let top = if room == 0 {
            self.anchor
                .start
                .min((self.viewport.end - height as f32).max(self.viewport.start))
        } else if up {
            self.anchor.start - height as f32
        } else {
            self.anchor.end
        };
        PopoverRows { top, height, inset }
    }

    /// Position pixel `size` at `anchor_x` within `body`, sharing bounds with paint and hit testing.
    pub(crate) fn pixel_bounds(
        &self,
        body: Bounds<Pixels>,
        anchor_x: Pixels,
        size: Size<Pixels>,
    ) -> Bounds<Pixels> {
        let size = gpui::size(
            size.width.min(body.size.width),
            size.height.min(body.size.height),
        );
        let left = anchor_x.min(body.right() - size.width).max(body.left());
        Bounds::new(point(left, px(self.top(size.height.into()))), size)
    }
}
