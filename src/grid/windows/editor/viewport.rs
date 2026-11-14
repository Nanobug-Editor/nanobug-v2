//! Text viewport geometry shared by the editor pane and read-only file previews.

use super::{EditorWindow, TEXT_OVERSCAN};
use crate::{
    grid::render::{GridLayer, Metrics, Panel, Rect},
    ui::text::{TextSpacing, UNITS_PER_COLUMN},
};
use gpui::{Entity, px};
use nanobug::{animation::scroll::ScrollInput, vim::Mode};

impl EditorWindow {
    /// Track the row and column `inputs` inside `area` at animation time `now`.
    /// Return whether cells changed and the row and column displacement for the animated cursor.
    pub(in crate::grid) fn track_view(
        &mut self,
        area: Rect,
        focused: bool,
        spacing: &TextSpacing,
        inputs: [ScrollInput; 2],
        now: f64,
    ) -> (bool, f32, f32) {
        let [rows, columns] = inputs;
        let text = self.text_area(area);
        let composition = if focused {
            self.composition.text.as_str()
        } else {
            ""
        };
        let buffer = &mut self.buffers[self.active];
        let before = (buffer.scroll.first_cell(), buffer.columns.first_cell());
        let row = (!matches!(buffer.vim.mode, Mode::Command | Mode::Search))
            .then(|| buffer.vim.doc.display_row(buffer.cursor_row()));
        let (next, mut row_delta) =
            buffer
                .scroll
                .track_rows(rows, row, text.h, buffer.vim.doc.display_rows(), now);
        let moved = next != row;
        let next = next.map(|row| buffer.vim.doc.document_row(row));
        if let Some(row) = next.filter(|_| moved) {
            let before = buffer.vim.doc.display_rows();
            buffer.vim.move_to_row(row);
            let after = buffer.vim.doc.display_rows();
            if before != after {
                let caret = buffer.vim.doc.display_row(buffer.cursor_row());
                // Retarget the viewport after the wheel moved out of a search-open fold.
                row_delta = buffer
                    .scroll
                    .track_rows(ScrollInput::Reveal, Some(caret), text.h, after, now)
                    .1;
            }
        }
        let line = next.map(|row| buffer.displayed_line(row, composition).0);
        let caret = next
            .zip(line.as_deref())
            .filter(|_| text.w > 0)
            .map(|(row, line)| {
                let span = buffer.caret_span(row, line, spacing);
                span.start / UNITS_PER_COLUMN..span.end.div_ceil(UNITS_PER_COLUMN)
            });
        let extent = match columns {
            ScrollInput::Wheel(_) => {
                // Keep long visible lines pannable when the caret is on a short line.
                let first = buffer.scroll.first_cell();
                let last = first + text.h + usize::from(buffer.scroll.fraction() > 0.);
                (first..last.min(buffer.vim.doc.display_rows()))
                    .map(|row| buffer.vim.doc.document_row(row))
                    .map(|row| {
                        buffer.display_columns(
                            row,
                            &buffer.displayed_line(row, composition).0,
                            spacing,
                        ) + 1
                    })
                    .max()
            }
            ScrollInput::Reveal | ScrollInput::PositionWithTail(_) => next
                .zip(line.as_deref())
                .map(|(row, line)| buffer.display_columns(row, line, spacing) + 1),
            _ => None,
        };
        let column_delta = buffer
            .columns
            .track_columns(columns, caret, text.w, extent, now);
        let after = (buffer.scroll.first_cell(), buffer.columns.first_cell());
        (moved || before != after, row_delta, column_delta)
    }

    /// Place the line-number and text panels in `area`, retaining fractional scroll offsets.
    pub(in crate::grid) fn content_layers(
        &self,
        area: Rect,
        metrics: Metrics,
        numbers: &Entity<Panel>,
        text: &Entity<Panel>,
    ) -> [GridLayer; 2] {
        let buffer = &self.buffers[self.active];
        let body = self.text_area(area);
        let gutter = Self::body(area);
        let gutter = Rect {
            w: body.x - gutter.x,
            h: body.h,
            ..gutter
        };
        let fraction = buffer.scroll.fraction();
        let mut text = GridLayer::scrolled(text, body, fraction, metrics);
        if body.w > 0 {
            text.bounds.origin.x -= px(buffer.text_displacement() * metrics.cw);
            text.bounds.size.width +=
                px((buffer.left_padding() + TEXT_OVERSCAN) as f32 * metrics.cw);
        }
        [
            GridLayer::scrolled(numbers, gutter, fraction, metrics),
            text,
        ]
    }

    /// Resolve the document caret inside `area` using measured text `spacing`.
    /// Return grid position, cell width, insert state and viewport visibility.
    pub(in crate::grid) fn body_caret(
        &self,
        area: Rect,
        spacing: &TextSpacing,
    ) -> (f32, f32, f32, bool, bool) {
        let buffer = &self.buffers[self.active];
        let body = self.text_area(area);
        let row = buffer.cursor_row();
        let (line, _) = buffer.displayed_line(row, &self.composition.text);
        let span = buffer.caret_span(row, &line, spacing);
        let width = (span.len() as f32 / UNITS_PER_COLUMN as f32).min(body.w.max(1) as f32);
        let retained_left = (buffer.columns.first_cell() * UNITS_PER_COLUMN) as isize;
        let x = (span.start as isize - retained_left) as f32 / UNITS_PER_COLUMN as f32
            - buffer.columns.fraction();
        let y = body.y as f32 + buffer.vim.doc.display_row(row) as f32 - buffer.scroll.position();
        (
            body.x as f32 + x,
            y,
            width,
            buffer.vim.mode.is_insert(),
            y >= body.y as f32
                && y + 1. <= (body.y + body.h) as f32 + 0.001
                && body.w > 0
                && x >= -0.001
                && x + width <= body.w as f32 + 0.001,
        )
    }
}
