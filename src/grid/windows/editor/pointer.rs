//! Pointer placement, fold clicks and drag selection against the editor's displayed text.

use super::EditorWindow;
use crate::{
    grid::render::{Metrics, Rect},
    ui::text::{TextSpacing, UNITS_PER_COLUMN},
};
use gpui::{Pixels, Point};
use nanobug::vim::Mode;

impl EditorWindow {
    /// Open one fold depth when `position` hits the arrow or dots in the clipped editor `area`.
    /// Use `metrics` and `spacing` to match the currently displayed scroll offsets and text.
    pub(in crate::grid) fn open_fold_at_position(
        &mut self,
        position: Point<Pixels>,
        area: Rect,
        metrics: Metrics,
        spacing: &TextSpacing,
    ) -> bool {
        let text = self.text_area(area);
        let body = self.source_body(area);
        let gutter = text.x - body.x;
        let origin = metrics.at(body.x as f32, text.y as f32);
        let x = f32::from(position.x - origin.x) / metrics.cw;
        let y = f32::from(position.y - origin.y) / metrics.lh;
        if !(0.0..(gutter + text.w) as f32).contains(&x) || !(0.0..text.h as f32).contains(&y) {
            return false;
        }
        let buffer = &mut self.buffers[self.active];
        let display_row =
            buffer.scroll.first_cell() + (y + buffer.scroll.fraction()).floor() as usize;
        let doc = &buffer.vim.doc;
        if display_row >= doc.display_rows() {
            return false;
        }
        let row = doc.document_row(display_row);
        let arrow = Self::fold_arrow_column(gutter) == Some(x.floor() as usize);
        if !arrow {
            if x < gutter as f32 || buffer.folded_brace(row).is_none() {
                return false;
            }
            let (line, _) = buffer.displayed_line(row, &self.composition.text);
            let dots = buffer.fold_ellipsis_span(row, &line, spacing);
            let visual_x = f64::from(x - gutter as f32 + buffer.columns.fraction())
                + buffer.columns.first_cell() as f64;
            let units = visual_x * UNITS_PER_COLUMN as f64;
            if !(dots.start as f64..dots.end as f64).contains(&units) {
                return false;
            }
        }
        buffer.vim.open_fold_at_header(row)
    }

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
        let y = f32::from(position.y - metrics.origin.y) / metrics.lh - body.y as f32;
        let scroll = buffer.scroll.position();
        let last_visible = (scroll + body.h as f32).ceil() as usize;
        let row = ((y + scroll).floor().max(0.) as usize).min(last_visible.saturating_sub(1));
        let row = buffer
            .vim
            .doc
            .document_row(row.min(buffer.vim.doc.display_rows() - 1));
        let x = f32::from(position.x - metrics.at(body.x as f32, 0.).x) / metrics.cw;
        let (line, preedit) = buffer.displayed_line(row, &self.composition.text);
        let line = if buffer.folded_brace(row).is_some() {
            buffer.fold_header(row, &line)
        } else {
            &line
        };
        let visual_x =
            f64::from(x.max(0.) + buffer.columns.fraction()) + buffer.columns.first_cell() as f64;
        let mut byte = spacing.byte_at(line, visual_x);
        if let Some(mark) = preedit {
            byte -= byte.saturating_sub(mark.start).min(mark.len());
        }
        buffer.vim.doc.starts[row] + byte
    }

    /// Handle a fold click or use `position` in `area` to place the caret and arm selection.
    /// `metrics` and `spacing` match the displayed fractional viewport.
    pub(in crate::grid) fn press(
        &mut self,
        position: Point<Pixels>,
        area: Rect,
        metrics: Metrics,
        spacing: &TextSpacing,
    ) {
        let (x, y) = metrics.cell(position);
        if self
            .markdown_view_mode()
            .is_some_and(|mode| mode != super::MarkdownViewMode::Edit)
        {
            let text = self.text_area(area);
            // The split's empty strip can focus the editor but has no text hit target.
            if !self.source_body(area).contains(x, y) || x >= text.x + text.w {
                self.dragging = None;
                return;
            }
        }
        self.markdown_source_takes_scroll();
        if self.open_fold_at_position(position, area, metrics, spacing) {
            self.dragging = None;
            return;
        }
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
        let mut position = position;
        if self.markdown_view_mode() == Some(super::MarkdownViewMode::Split) {
            position.x = position.x.min(self.text_area(area).pixels(metrics).right());
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
