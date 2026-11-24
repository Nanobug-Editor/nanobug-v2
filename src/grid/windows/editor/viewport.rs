//! Text viewport geometry shared by the editor pane and read-only file previews.

use super::{Buffer, EditorWindow, MarkdownViewMode, TEXT_OVERSCAN, wrap::VisualRows};
use crate::{
    grid::render::{GridLayer, Metrics, Panel, Rect},
    ui::text::{TextSpacing, UNITS_PER_COLUMN},
};
use gpui::{Entity, px};
use nanobug::{animation::scroll::ScrollInput, vim::Mode};
use std::ops::Range;

impl EditorWindow {
    /// Return the current source viewport position when it participates in scroll binding.
    pub(in crate::grid) fn scrollbind_target(&self) -> Option<usize> {
        (self.window_options().scrollbind
            && self.terminal().is_none()
            && self.markdown_view_mode() != Some(MarkdownViewMode::Preview))
        .then(|| self.buffer().scroll.target())
    }

    /// On focus, bring peers back from a bound position this shorter viewport cannot reach.
    pub(in crate::grid) fn scrollbind_focus_correction(&mut self) -> isize {
        let Some(actual) = self.scrollbind_target() else {
            return 0;
        };
        self.buffers[self.active]
            .bound_scroll
            .take()
            .filter(|(target, _)| *target == actual)
            .map_or(0, |(_, requested)| {
                (actual as isize).saturating_sub(requested)
            })
    }

    /// Apply a peer's target without changing focus, retaining the unclamped relative offset.
    pub(in crate::grid) fn scrollbind_rows(
        &mut self,
        area: Rect,
        spacing: &TextSpacing,
        shift: isize,
        align: Option<usize>,
        now: f64,
    ) {
        let Some(actual) = self.scrollbind_target() else {
            return;
        };
        let remembered = self
            .buffer()
            .bound_scroll
            .filter(|(target, _)| *target == actual)
            .map_or(actual as isize, |(_, requested)| requested);
        let requested = align.map_or_else(|| remembered.saturating_add(shift), |row| row as isize);
        let delta = requested.max(0).saturating_sub(actual as isize);
        self.track_view(
            area,
            false,
            spacing,
            [ScrollInput::Wheel(delta), ScrollInput::Advance],
            now,
        );
        let buffer = &mut self.buffers[self.active];
        buffer.bound_scroll = Some((buffer.scroll.target(), requested));
    }

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
        if !self.window_options().scrollbind {
            self.buffers[self.active].bound_scroll = None;
        }
        let text = self.text_area(area);
        if let Some(terminal) = self.terminal_mut() {
            return terminal.track_view(text, rows, now);
        }
        let markdown = self.markdown_view_mode();
        if markdown == Some(MarkdownViewMode::Preview) {
            return (false, 0., 0.);
        }
        if text.w > 0 && self.wraps_source() {
            return self.track_wrapped_view(text, area, spacing, rows, now);
        }
        let source_request = match rows {
            ScrollInput::Wheel(delta) if self.source_follows_markdown_preview(area) => {
                let top = self.buffer().scroll.target();
                let maximum = self.vim().doc.display_rows().saturating_sub(text.h.max(1));
                top.saturating_add_signed(delta).min(maximum) != top
            }
            ScrollInput::Wheel(_) | ScrollInput::Position(_) | ScrollInput::PositionWithTail(_) => {
                true
            }
            _ => false,
        };
        if source_request
            || self
                .buffer()
                .markdown
                .preview_caret
                .is_some_and(|caret| caret != self.vim().doc.cursor)
        {
            self.markdown_source_takes_scroll();
        }
        let preview_leads = self.source_follows_markdown_preview(area);
        let composition = if focused {
            self.composition.text.as_str()
        } else {
            ""
        };
        let margin = self.vim().scrolloff(&self.register);
        let buffer = &mut self.buffers[self.active];
        if buffer.wraps.borrow().is_some() {
            let rows = buffer.retained_visual_rows();
            let line = rows
                .row(buffer.scroll.target().min(rows.len().saturating_sub(1)))
                .map_or(0, |row| row.line);
            buffer.scroll.snap(buffer.vim.doc.display_row(line), now);
            buffer.wraps.get_mut().take();
        }
        let before = (buffer.scroll.first_cell(), buffer.columns.first_cell());
        let row = (!matches!(buffer.vim.mode, Mode::Command | Mode::Search))
            .then(|| buffer.vim.doc.display_row(buffer.cursor_row()));
        let (next, mut row_delta) = if preview_leads {
            (row, 0.)
        } else {
            buffer.scroll.track_rows_with_margin(
                rows,
                row,
                text.h,
                buffer.vim.doc.display_rows(),
                now,
                margin,
            )
        };
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
                    .track_rows_with_margin(
                        ScrollInput::Reveal,
                        Some(caret),
                        text.h,
                        after,
                        now,
                        margin,
                    )
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

    /// Scroll the Markdown `text` viewport in visual rows within `area` using `spacing`.
    /// Apply `input` at `now`, then return retained-cell changes and animated displacement.
    fn track_wrapped_view(
        &mut self,
        text: Rect,
        area: Rect,
        spacing: &TextSpacing,
        input: ScrollInput,
        now: f64,
    ) -> (bool, f32, f32) {
        let preview_leads = self.source_follows_markdown_preview(area);
        let margin = self.vim().scrolloff(&self.register);
        let buffer = &mut self.buffers[self.active];
        let composition = self.composition.text.as_str();
        let restored_byte = buffer.restored_wrap_byte.take();
        let (reflow, anchor, row, caret, total) = {
            let old = buffer.retained_visual_rows();
            let map = buffer.measure_visual_rows(text.w, spacing, composition);
            let reflow = !old.same_geometry(&map);
            let anchor = old
                .row(buffer.scroll.target().min(old.len().saturating_sub(1)))
                .map_or(0, |top| {
                    let byte = restored_byte
                        .or_else(|| top.segment.map(|segment| segment.bytes.start))
                        .unwrap_or(0);
                    map.row_for_byte(top.line, byte)
                })
                .min(map.len().saturating_sub(1));
            let (row, caret) = buffer.visual_caret(&map, spacing, composition);
            (reflow, anchor, row, caret, map.len())
        };
        if reflow || restored_byte.is_some() {
            if anchor != buffer.scroll.target() {
                buffer.scroll.snap(anchor, now);
            }
            buffer.markdown.source_sync_pending |= preview_leads;
        }
        let before = buffer.scroll.first_cell();
        let column_before = buffer.columns.position();
        if column_before != 0. || buffer.columns.target() != 0 {
            buffer.columns.snap(0, now);
        }
        let moves_source = match input {
            ScrollInput::Wheel(delta) => {
                let top = buffer.scroll.target();
                top.saturating_add_signed(delta)
                    .min(total.saturating_sub(text.h.max(1)))
                    != top
            }
            ScrollInput::Position(_) | ScrollInput::PositionWithTail(_) => true,
            _ => false,
        };
        let changed_caret = buffer
            .markdown
            .preview_caret
            .is_some_and(|caret| caret != buffer.vim.doc.cursor);
        if moves_source || changed_caret {
            buffer.markdown.preview_caret = None;
            buffer.markdown.preview_scroll_origin = None;
            buffer.markdown.source_sync_pending = false;
        }
        if preview_leads && !moves_source && !changed_caret {
            return (reflow, 0., -column_before);
        }
        let tracked = (!matches!(buffer.vim.mode, Mode::Command | Mode::Search)).then_some(row);
        let (next, mut row_delta) = buffer
            .scroll
            .track_rows_with_margin(input, tracked, text.h, total, now, margin);
        if let Some(next) = next.filter(|&next| next != row) {
            let map = buffer.measure_visual_rows(text.w, spacing, composition);
            if let Some(row) = map.row(next) {
                let x = caret.start as f64 / UNITS_PER_COLUMN as f64;
                let offset = buffer.offset_in_visual_row(row, x, spacing, composition);
                let visible_lines = buffer.vim.doc.display_rows();
                buffer.vim.set_cursor(offset);
                if buffer.vim.doc.display_rows() != visible_lines {
                    let map = buffer.measure_visual_rows(text.w, spacing, composition);
                    let caret = buffer.visual_caret(&map, spacing, composition).0;
                    let next_total = map.len();
                    row_delta += buffer
                        .scroll
                        .track_rows_with_margin(
                            ScrollInput::Reveal,
                            Some(caret),
                            text.h,
                            next_total,
                            now,
                            margin,
                        )
                        .1;
                }
                return (true, row_delta, -column_before);
            }
        }
        (
            reflow || before != buffer.scroll.first_cell(),
            row_delta,
            -column_before,
        )
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
        let source = self.source_body(area);
        let gutter = Rect {
            w: body.x - source.x,
            h: body.h,
            ..source
        };
        if let Some(terminal) = &buffer.terminal {
            return [
                GridLayer::scrolled(numbers, gutter, 0., metrics),
                GridLayer::scrolled(text, body, terminal.scroll().fraction(), metrics),
            ];
        }
        let fraction = buffer.scroll.fraction();
        let mut text = GridLayer::scrolled(text, body, fraction, metrics);
        if body.w > 0 {
            // Leave half the divider-side gap clear beside the tint, including the rule itself.
            if let Some(clip) = &mut text.clip {
                let divider_padding = source.x + source.w - (body.x + body.w);
                clip.size.width += px(divider_padding as f32 * metrics.cw * 0.5);
            }
            let wrapped = self
                .markdown_view_mode()
                .is_some_and(|mode| mode != MarkdownViewMode::Preview);
            text.bounds.size.width += px(TEXT_OVERSCAN as f32 * metrics.cw);
            if !wrapped {
                text.bounds.origin.x -= px(buffer.text_displacement() * metrics.cw);
                text.bounds.size.width += px(buffer.left_padding() as f32 * metrics.cw);
            }
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
        if let Some(terminal) = &buffer.terminal {
            return terminal.caret(body);
        }
        let rows = self.visual_rows(body.w, spacing);
        let (row, span) = buffer.visual_caret(&rows, spacing, &self.composition.text);
        let left = if rows.is_wrapped() {
            0.
        } else {
            buffer.columns.position()
        };
        let x = span.start as f32 / UNITS_PER_COLUMN as f32 - left;
        let mut width = (span.len() as f32 / UNITS_PER_COLUMN as f32).min(body.w.max(1) as f32);
        if rows.is_wrapped() && buffer.vim.mode.is_insert() {
            width = width.min((body.w as f32 - x).max(0.));
        }
        let y = body.y as f32 + row as f32 - buffer.scroll.position();
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

impl Buffer {
    /// Resolve the caret's visual row and horizontal span, including any list padding.
    /// Use the measured `rows` and `spacing` for the line with the current `composition`.
    pub(super) fn visual_caret(
        &self,
        rows: &VisualRows<'_>,
        spacing: &TextSpacing,
        composition: &str,
    ) -> (usize, Range<usize>) {
        let line = self.cursor_row();
        let (displayed, _) = self.displayed_line(line, composition);
        let span = self.caret_span(line, &displayed, spacing);
        let mut byte = self.vim.doc.cursor - self.vim.doc.starts[line];
        if rows.is_wrapped()
            && self.vim.fold_cursor_on_closing_brace()
            && let Some(suffix) = self.painted_line(line, composition).fold_suffix
        {
            byte = suffix.closing_byte;
        }
        let row = rows.row_for_byte(line, byte);
        let Some(segment) = rows.row(row).and_then(|row| row.segment) else {
            return (row, span);
        };
        let position =
            |units: usize| units.saturating_sub(segment.start_units) + segment.indent_units;
        (row, position(span.start)..position(span.end))
    }
}
