//! Pointer placement and drag selection against the editor's displayed text.

use super::EditorWindow;
use crate::{
    grid::render::{Metrics, Rect},
    ui::text::TextSpacing,
};
use gpui::{Pixels, Point};
use nanobug::vim::Mode;

impl EditorWindow {
    /// Hit-test the actually displayed fractional viewport at native `position`.
    pub(in crate::grid) fn offset(
        &self,
        position: Point<Pixels>,
        area: Rect,
        metrics: Metrics,
        spacing: &TextSpacing,
    ) -> usize {
        let buffer = &self.buffers[self.active];
        let body = self.text_area(area);
        let row = (f32::from(position.y - metrics.origin.y) / metrics.lh - body.y as f32
            + buffer.scroll.position())
        .floor()
        .max(0.) as usize;
        let row = row.min(buffer.vim.doc.starts.len() - 1);
        let x = f32::from(position.x - metrics.at(body.x as f32, 0.).x) / metrics.cw;
        let (line, preedit) = buffer.displayed_line(row, &self.composition.text);
        let visual_x =
            f64::from(x.max(0.) + buffer.columns.fraction()) + buffer.columns.first_cell() as f64;
        let mut byte = spacing.byte_at(&line, visual_x);
        if let Some(mark) = preedit {
            byte -= byte.saturating_sub(mark.start).min(mark.len());
        }
        buffer.vim.doc.starts[row] + byte
    }

    /// Place the caret from `position` in `area` using `metrics` and `spacing`, then arm selection.
    pub(in crate::grid) fn press(
        &mut self,
        position: Point<Pixels>,
        area: Rect,
        metrics: Metrics,
        spacing: &TextSpacing,
    ) {
        let offset = self.offset(position, area, metrics, spacing);
        self.buffers[self.active].vim.set_cursor(offset);
        self.dragging = Some((self.vim().doc.cursor, position));
    }

    /// Extend selection at `position` inside `area`; return false while movement is still click jitter.
    /// `metrics` and `spacing` match the displayed fractional viewport.
    pub(in crate::grid) fn drag(
        &mut self,
        position: Point<Pixels>,
        area: Rect,
        metrics: Metrics,
        spacing: &TextSpacing,
    ) -> bool {
        let Some((anchor, origin)) = self.dragging else {
            return false;
        };
        if f32::from(position.x - origin.x)
            .abs()
            .max(f32::from(position.y - origin.y).abs())
            < 4.
        {
            return false;
        }
        let at = self.offset(position, area, metrics, spacing);
        let vim = &mut self.buffers[self.active].vim;
        if at != anchor {
            vim.mode = Mode::Visual;
            vim.anchor = anchor;
            vim.set_cursor(at);
        }
        true
    }
}
