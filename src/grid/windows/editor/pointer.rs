//! Pointer placement, fold clicks and drag selection against the editor's displayed text.

use super::{Buffer, EditorWindow, buffer::LineSource, wrap::VisualRow};
use crate::{
    grid::render::{Metrics, Rect},
    ui::text::{TextSpacing, UNITS_PER_COLUMN},
};
use gpui::{Pixels, Point};
use nanobug::vim::Mode;

impl Buffer {
    /// Convert `visual_x` within `visual` to a document byte using measured `spacing`.
    /// Remove the inline `composition` and fold decoration from the returned offset.
    pub(super) fn offset_in_visual_row(
        &self,
        visual: VisualRow,
        visual_x: f64,
        spacing: &TextSpacing,
        composition: &str,
    ) -> usize {
        LineSource(&self.vim).offset_in_visual_row(visual, visual_x, spacing, composition)
    }
}

impl LineSource<'_> {
    /// Project a measured screen column onto a document offset.
    pub(super) fn offset_in_visual_row(
        self,
        visual: VisualRow,
        visual_x: f64,
        spacing: &TextSpacing,
        composition: &str,
    ) -> usize {
        let row = visual.line;
        let (mut byte, preedit) = if let Some(segment) = visual.segment {
            let painted = self.painted_line(row, composition);
            let end = painted
                .fold_suffix
                .map_or(painted.text.len(), |suffix| suffix.start_byte);
            let units = (visual_x * UNITS_PER_COLUMN as f64 - segment.indent_units as f64).max(0.);
            let cells = spacing.buffer_cells(
                &painted.text[segment.bytes.clone()],
                segment.bytes.start,
                segment.tab_column,
                self.0.doc.tabstop(),
            );
            let mut byte = segment.bytes.start;
            for cell in cells {
                byte = cell.byte;
                if units < cell.end as f64 {
                    break;
                }
            }
            // The byte boundary of an intermediate segment belongs to its continuation.
            if segment.bytes.end == painted.text.len()
                && units >= (segment.end_units - segment.start_units) as f64
            {
                byte = segment.bytes.end;
            }
            (byte.min(end), painted.preedit)
        } else {
            let (line, preedit) = self.displayed_line(row, composition);
            let line = if self.folded_brace(row).is_some() {
                self.fold_header(row, &line)
            } else {
                &line
            };
            (
                spacing.byte_at(line, visual_x, self.0.doc.tabstop()),
                preedit,
            )
        };
        if let Some(mark) = preedit {
            byte -= byte.saturating_sub(mark.start).min(mark.len());
        }
        self.0.doc.starts[row] + byte
    }
}

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
        let buffer = self.buffer();
        let display_row =
            buffer.scroll.first_cell() + (y + buffer.scroll.fraction()).floor() as usize;
        let Some(visual) = self.visual_rows(text.w, spacing).row(display_row) else {
            return false;
        };
        let row = visual.line;
        let segment = visual.segment.as_ref();
        let arrow = segment.is_none_or(|segment| segment.bytes.start == 0)
            && Self::fold_arrow_column(gutter) == Some(x.floor() as usize);
        if !arrow {
            if x < gutter as f32 || buffer.folded_brace(row).is_none() {
                return false;
            }
            let (line, _) = buffer.displayed_line(row, &self.composition.text);
            let dots = buffer.fold_ellipsis_span(row, &line, spacing);
            let local_units = f64::from(x - gutter as f32) * UNITS_PER_COLUMN as f64;
            let units = if let Some(segment) = segment {
                if local_units < segment.indent_units as f64 {
                    return false;
                }
                let units = segment.start_units as f64 + local_units - segment.indent_units as f64;
                // Space after a wrapped segment cannot hit a suffix on its continuation.
                if units >= segment.end_units as f64 {
                    return false;
                }
                units
            } else {
                let visual_x = f64::from(x - gutter as f32 + buffer.columns.fraction())
                    + buffer.columns.first_cell() as f64;
                visual_x * UNITS_PER_COLUMN as f64
            };
            if !(dots.start as f64..dots.end as f64).contains(&units) {
                return false;
            }
        }
        self.buffers[self.active].vim.open_fold_at_header(row)
    }

    /// Hit-test the actually displayed fractional viewport at native `position`.
    pub(in crate::grid) fn offset(
        &self,
        position: Point<Pixels>,
        area: Rect,
        metrics: Metrics,
        spacing: &TextSpacing,
    ) -> usize {
        let buffer = self.buffer();
        let body = self.text_area(area);
        let y = f32::from(position.y - metrics.origin.y) / metrics.lh - body.y as f32;
        let scroll = buffer.scroll.position();
        let last_visible = (scroll + body.h as f32).ceil() as usize;
        let rows = self.visual_rows(body.w, spacing);
        let display_row = ((y + scroll).floor().max(0.) as usize)
            .min(last_visible.saturating_sub(1))
            .min(rows.len().saturating_sub(1));
        let Some(visual) = rows.row(display_row) else {
            return buffer.vim.doc.cursor;
        };
        let x = (f32::from(position.x - metrics.at(body.x as f32, 0.).x) / metrics.cw).max(0.);
        let visual_x = if visual.segment.is_some() {
            f64::from(x)
        } else {
            f64::from(x + buffer.columns.fraction()) + buffer.columns.first_cell() as f64
        };
        buffer.offset_in_visual_row(visual, visual_x, spacing, &self.composition.text)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::windows::editor::MarkdownViewMode;
    use gpui::{FontId, NoopTextSystem, point, px};
    use nanobug::document::Document;
    use std::sync::Arc;

    /// Open `source` as Markdown with a four-column text viewport and fixed test metrics.
    fn narrow_markdown(source: &str) -> (EditorWindow, Rect, Metrics, TextSpacing) {
        let mut doc = Document::new(source);
        doc.path = Some("README.md".into());
        let mut editor = EditorWindow::new(doc);
        assert!(editor.set_markdown_view_mode(MarkdownViewMode::Edit));
        let area = Rect {
            w: 10,
            h: 12,
            ..Rect::default()
        };
        let metrics = Metrics {
            cw: 9.,
            lh: 20.,
            font_size: 16.,
            origin: point(px(0.), px(0.)),
        };
        let spacing = TextSpacing::new(
            Arc::new(NoopTextSystem::new()),
            FontId(0),
            metrics.font_size,
            metrics.cw,
        );
        assert_eq!(editor.text_area(area).w, 4);
        (editor, area, metrics, spacing)
    }

    /// A drag across two continuation rows selects bytes within the same physical line.
    #[test]
    fn markdown_wrap_mouse_selection_uses_the_visible_segment() {
        let (mut editor, area, metrics, spacing) = narrow_markdown("abcdEFGHijkl\nlast");
        let text = editor.text_area(area);
        editor.buffers[editor.active]
            .scroll
            .follow(0.5, 1, true, 0.);
        let x = text.x as f32;
        let y = text.y as f32;
        editor.press(metrics.at(x + 1.5, y + 0.7), area, metrics, &spacing);
        assert_eq!(editor.vim().doc.cursor, 5);

        assert!(editor.drag(metrics.at(x + 2.5, y + 1.7), area, metrics, &spacing));
        assert_eq!(editor.vim().mode, Mode::Visual);
        assert_eq!(editor.vim().anchor, 5);
        assert_eq!(editor.vim().doc.cursor, 10);
        assert_eq!(editor.vim().selection_on_line(0), Some(5..11));
        assert_eq!(editor.vim().doc.text, "abcdEFGHijkl\nlast");
    }

    /// Hit testing on a wrapped preedit still returns offsets in the committed document.
    #[test]
    fn markdown_wrap_hit_testing_accounts_for_chinese_preedit() {
        let (mut editor, area, metrics, spacing) = narrow_markdown("abcdEFGH");
        {
            let vim = &mut editor.buffers[editor.active].vim;
            vim.mode = Mode::Insert;
            vim.set_cursor(4);
        }
        editor.composition_mut().mark(None, "中", None);
        let text = editor.text_area(area);
        let x = text.x as f32;
        let y = text.y as f32 + 1.;
        let (caret_x, caret_y, _, insert, visible) = editor.body_caret(area, &spacing);
        assert!(insert && visible);
        assert_eq!((caret_x, caret_y), (x, y));
        let advance = spacing.cells("中").next().unwrap().end as f32 / UNITS_PER_COLUMN as f32;
        assert_eq!(
            editor.offset(
                metrics.at(x + advance / 2., y + 0.5),
                area,
                metrics,
                &spacing
            ),
            4,
        );
        // The following English word fits on a fresh row, so it stays together.
        assert_eq!(
            editor.offset(metrics.at(x + 1.25, y + 1.5), area, metrics, &spacing,),
            5,
        );
        assert_eq!(editor.vim().doc.text, "abcdEFGH");
    }
}
