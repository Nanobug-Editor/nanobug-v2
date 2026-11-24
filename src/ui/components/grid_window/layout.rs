use crate::ui::grid_geometry::Rect;

/// Window regions in the same character-cell coordinates as the supplied bounds.
#[derive(Clone, Copy, Debug)]
pub(crate) struct GridWindow {
    /// Entire window, including its one-cell border.
    bounds: Rect,
    /// Number of interior rows reserved for the header after clipping.
    header_rows: usize,
    /// Number of interior rows reserved for the footer after clipping.
    footer_rows: usize,
}

impl GridWindow {
    /// Fit the border and the requested fixed rows into `bounds`, giving the header priority.
    pub(crate) fn new(bounds: Rect, header_rows: usize, footer_rows: usize) -> Self {
        let inside_height = bounds.h.saturating_sub(2);
        let header_rows = header_rows.min(inside_height);
        Self {
            bounds,
            header_rows,
            footer_rows: footer_rows.min(inside_height - header_rows),
        }
    }

    /// Outer rectangle of the fixed window frame.
    pub(crate) fn bounds(self) -> Rect {
        self.bounds
    }

    /// The top border row available for a title or file tabs when an outline fits.
    pub(crate) fn title(self) -> Rect {
        if self.bounds.w < 2 || self.bounds.h < 2 {
            return Rect {
                w: 0,
                h: 0,
                ..self.bounds
            };
        }
        Rect {
            h: 1,
            ..self.bounds
        }
    }

    /// Fixed interior rows immediately below the title.
    pub(crate) fn header(self) -> Rect {
        Rect {
            x: self.bounds.x + self.bounds.w.min(1),
            y: self.bounds.y + self.bounds.h.min(1),
            w: self.bounds.w.saturating_sub(2),
            h: self.header_rows,
        }
    }

    /// Fixed interior rows immediately above the bottom border.
    pub(crate) fn footer(self) -> Rect {
        let header = self.header();
        Rect {
            y: header.y + (self.bounds.h.saturating_sub(2) - self.footer_rows),
            h: self.footer_rows,
            ..header
        }
    }

    /// The interior between the fixed header and footer, used for painting and input.
    pub(crate) fn body(self) -> Rect {
        let header = self.header();
        let y = header.y + header.h;
        Rect {
            y,
            h: self.footer().y - y,
            ..header
        }
    }
}
