//! Resolve the shared animated cursor against the same clipped viewports as settings input.

use super::{Editor, Pane, Settings, view};
use crate::{
    grid::render::{Metrics, Panel, Rect},
    ui::text::{TextSpacing, UNITS_PER_COLUMN},
};
use gpui::{Bounds, Entity, Pixels, px};
use nanobug::animation::scroll::GridScroll;

/// Each independently scrolling panel owns its own cursor effects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::grid) enum CursorRegion {
    Tree,
    Form,
    Choices,
}

/// Panel and scroll transform that own the current cursor and its nearby-text effects.
pub(in crate::grid) struct CursorViewport<'a> {
    area: Rect,
    /// The selector can be anchored to a partially scrolled form row.
    offset_y: f32,
    pub(in crate::grid) panel: &'a Entity<Panel>,
    pub(in crate::grid) scroll: &'a GridScroll,
}

impl CursorViewport<'_> {
    /// Window-space vertical position of row zero, including both scroll and anchor movement.
    pub(in crate::grid) fn origin_y(&self) -> f32 {
        self.area.y as f32 + self.offset_y - self.scroll.position()
    }

    pub(in crate::grid) fn clip(&self, metrics: Metrics) -> Bounds<Pixels> {
        let mut bounds = self.area.pixels(metrics);
        bounds.origin.y += px(self.offset_y * metrics.lh);
        bounds
    }
}

impl Settings {
    pub(in crate::grid) fn cursor_region(&self) -> CursorRegion {
        match (&self.editor, self.pane) {
            (Some(Editor::List { .. }), _) => CursorRegion::Choices,
            (_, Pane::Tree) => CursorRegion::Tree,
            (_, Pane::Form) => CursorRegion::Form,
        }
    }

    pub(in crate::grid) fn cursor_viewport(&self, area: Rect) -> CursorViewport<'_> {
        let (tree, body) = view::pane_bounds(area);
        let (area, offset_y, panel, scroll) = match self.cursor_region() {
            CursorRegion::Tree => (tree, 0., &self.panels[1], &self.tree_scroll),
            CursorRegion::Form => (body, 0., &self.panels[2], &self.form_scroll),
            CursorRegion::Choices => {
                let (rows, offset) = self
                    .selector_layout(body)
                    .map_or((Rect::default(), 0.), |layout| {
                        (layout.rows, layout.offset_y)
                    });
                (rows, offset, &self.panels[4], &self.selector_scroll)
            }
        };
        CursorViewport {
            area,
            offset_y,
            panel,
            scroll,
        }
    }

    /// Lists use their leading gutter. An active text field takes the insertion point from
    /// the same layout used to render preedit, selection and horizontally scrolled text.
    pub(in crate::grid) fn caret(&self, area: Rect, spacing: &TextSpacing) -> Option<(f32, f32)> {
        let viewport = self.cursor_viewport(area);
        let bounds = viewport.area;
        if bounds.w == 0 || bounds.h == 0 {
            return None;
        }
        let mut x = bounds.x as f32;
        let row = match self.cursor_region() {
            CursorRegion::Tree => self.visible.position(self.tree_row)?,
            CursorRegion::Form => {
                if let Some(Editor::Input { text, field }) = &self.editor {
                    let layout = field.layout(
                        text,
                        &self.composition,
                        view::input_width(bounds.w),
                        spacing,
                    );
                    x += (view::value_column(bounds.w) + 1) as f32
                        + layout.caret as f32 / UNITS_PER_COLUMN as f32;
                }
                self.body_rows().iter().position(|row| {
                    matches!(row, view::BodyRow::Field(i) | view::BodyRow::Notice(i, 0) if *i == self.row)
                })?
            }
            CursorRegion::Choices => match self.editor {
                Some(Editor::List { cursor, .. }) if cursor < self.choices().len() => cursor,
                _ => return None,
            },
        };
        let local = row as f32 - viewport.scroll.position();
        (local >= -0.001 && local + 1. <= bounds.h as f32 + 0.001)
            .then(|| (x, viewport.origin_y() + row as f32))
    }
}
