use crate::ui::grid_geometry::Rect;

/// Geometry of a modal task within its available workspace grid.
pub(crate) struct GridDialog {
    /// Usable workspace region, including its origin in a split or below tabs.
    viewport: Rect,
}

impl GridDialog {
    /// Open a dialog inside `viewport`. The caller owns its task and active input fields.
    pub(crate) fn new(viewport: Rect) -> Self {
        Self { viewport }
    }

    /// Center the requested `width` and `height`, clipping them to the available grid.
    pub(crate) fn centered(&self, width: usize, height: usize) -> Rect {
        let row = self.viewport.y + self.viewport.h.saturating_sub(height) / 2;
        self.centered_on_row(row, width, height)
    }

    /// Center horizontally at workspace `row`, fitting `width` and `height` inside the dialog region.
    pub(crate) fn centered_on_row(&self, row: usize, width: usize, height: usize) -> Rect {
        self.viewport.centered_on_row(row, width, height)
    }

    /// Place a `width` by `height` dialog at the lower-right of the available grid.
    pub(crate) fn bottom_right(&self, width: usize, height: usize) -> Rect {
        self.viewport.bottom_right(width, height)
    }
}
