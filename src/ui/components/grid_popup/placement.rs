use crate::ui::grid_geometry::Rect;

/// Available grid cells for a temporary menu, input prompt or key-sequence hint.
#[derive(Clone, Copy)]
pub(crate) struct GridPopup {
    /// Visible region in workspace coordinates, excluding reserved rows.
    viewport: Rect,
}

impl GridPopup {
    /// Use `viewport` as the placement and clipping boundary.
    pub(crate) fn new(viewport: Rect) -> Self {
        Self { viewport }
    }

    /// Fit the requested `area` to the viewport, clipping its size and shifting it inside.
    pub(crate) fn at(self, area: Rect) -> Rect {
        self.viewport.fit(area)
    }

    /// Align a `width` by `height` popup to the viewport's lower-right corner.
    pub(crate) fn bottom_right(self, width: usize, height: usize) -> Rect {
        self.viewport.bottom_right(width, height)
    }

    /// Center a `width` by `height` popup horizontally at workspace `row`.
    pub(crate) fn centered_on_row(self, row: usize, width: usize, height: usize) -> Rect {
        self.viewport.centered_on_row(row, width, height)
    }
}
