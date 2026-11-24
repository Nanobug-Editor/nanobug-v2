//! Character-cell coordinates shared by UI components and the grid renderer.

/// Half-open rectangle in character cells.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Rect {
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
    /// Fit `area` to this viewport by clipping its size and shifting its origin inside.
    pub(super) fn fit(self, area: Rect) -> Rect {
        let w = area.w.min(self.w);
        let h = area.h.min(self.h);
        Rect {
            x: area.x.clamp(self.x, self.x + (self.w - w)),
            y: area.y.clamp(self.y, self.y + (self.h - h)),
            w,
            h,
        }
    }

    /// Anchor a rectangle at the lower-right corner in workspace grid coordinates.
    pub(super) fn bottom_right(self, w: usize, h: usize) -> Rect {
        self.fit(Rect {
            x: self.x + self.w.saturating_sub(w),
            y: self.y + self.h.saturating_sub(h),
            w,
            h,
        })
    }

    /// Center a rectangle horizontally, keeping its top as close as possible to workspace `row`.
    pub(super) fn centered_on_row(self, row: usize, w: usize, h: usize) -> Rect {
        self.fit(Rect {
            x: self.x + self.w.saturating_sub(w) / 2,
            y: row,
            w,
            h,
        })
    }

    /// Whether the cell at `(x, y)` lies inside this rectangle.
    pub(crate) fn contains(self, x: usize, y: usize) -> bool {
        x >= self.x && y >= self.y && x - self.x < self.w && y - self.y < self.h
    }
}
