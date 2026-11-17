//! Project Markdown source lines onto width-dependent editor rows.

use super::{Buffer, EditorWindow, MarkdownViewMode, buffer::LineSource};
use crate::ui::text::{TextSpacing, UNITS_PER_COLUMN};
use gpui::FontId;
use nanobug::{
    document::Document,
    vim::{ScreenMotion, Vim},
};
use std::{cell::RefCell, ops::Range, sync::Arc};
use unicode_segmentation::UnicodeSegmentation;

/// One visible row with its source line and optional wrapped span.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::grid) struct VisualRow {
    /// Zero-based row in the file.
    pub(super) line: usize,
    /// Range in the painted line for a Markdown row, including any preedit and fold suffix.
    pub(super) segment: Option<WrapSegment>,
}

/// One grapheme-aligned fragment of a source line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct WrapSegment {
    /// Bytes in the displayed line.
    pub(super) bytes: Range<usize>,
    /// Fixed-point offset of the fragment start on the unwrapped line.
    pub(super) start_units: usize,
    /// Fixed-point offset just after the fragment on the unwrapped line.
    pub(super) end_units: usize,
    /// Logical column at the fragment start for preserving tab stops.
    pub(super) tab_column: usize,
}

/// Inputs that affect the bytes and measured width of the visible source rows.
#[derive(PartialEq, Eq)]
struct WrapKey {
    /// File contents and opening identity.
    version: (u64, u64),
    /// Rows hidden by the current folds.
    hidden: Vec<Range<usize>>,
    /// Available text columns.
    columns: usize,
    /// Width of tabs. It can change without a text revision.
    tabstop: usize,
    /// Font identity and fixed-point scale of CJK measurements.
    metrics: (FontId, u32),
    /// Temporary input drawn at the caret and its insertion byte.
    composition: Option<(usize, String)>,
    /// Active fold header can retain otherwise trimmed trailing space.
    expanded_header: Option<usize>,
    /// An insertion point at a filled end of line can need its own display row.
    end_caret: Option<usize>,
}

/// Latest width-dependent row map retained by a buffer.
pub(super) struct WrappedRows {
    /// Layout inputs from the most recent sizing pass.
    key: WrapKey,
    /// Rows in source order, shareable with buffers prepared on the startup worker.
    rows: Arc<[VisualRow]>,
}

/// Look up visible rows for a buffer, with direct fold projection when wrapping is disabled.
pub(in crate::grid) struct VisualRows<'a> {
    /// Physical rows and folding information.
    doc: &'a Document,
    /// The measured Markdown rows when this pane has a text width.
    wrapped: Option<Arc<[VisualRow]>>,
}

impl EditorWindow {
    /// Whether soft wrapping applies to the editable view of the active buffer.
    pub(super) fn wraps_source(&self) -> bool {
        let markdown = self.markdown_view_mode();
        markdown != Some(MarkdownViewMode::Preview)
            && self.window_options().wrap.unwrap_or(markdown.is_some())
    }

    /// Resolve the current source layout at `columns` with the workspace's `spacing`.
    pub(in crate::grid) fn visual_rows(
        &self,
        columns: usize,
        spacing: &TextSpacing,
    ) -> VisualRows<'_> {
        let buffer = &self.buffers[self.active];
        if self.wraps_source() && columns > 0 {
            buffer.measure_visual_rows(columns, spacing, &self.composition.text)
        } else {
            VisualRows::physical(&buffer.vim.doc)
        }
    }
}

impl Buffer {
    /// Measure rows at `columns` using `spacing`, including any active native `composition`.
    pub(super) fn measure_visual_rows(
        &self,
        columns: usize,
        spacing: &TextSpacing,
        composition: &str,
    ) -> VisualRows<'_> {
        measured_rows(
            LineSource(&self.vim),
            &self.wraps,
            columns,
            spacing,
            composition,
        )
    }

    /// Return cached rows for the preview when they still describe `columns` and this file.
    pub(in crate::grid) fn cached_visual_rows(&self, columns: usize) -> VisualRows<'_> {
        let doc = &self.vim.doc;
        let wrapped = self
            .wraps
            .borrow()
            .as_ref()
            .filter(|cache| {
                cache.key.columns == columns
                    && cache.key.version == (doc.opening, doc.revision)
                    && cache.key.hidden == doc.hidden_rows()
            })
            .map(|cache| cache.rows.clone());
        VisualRows { doc, wrapped }
    }

    /// Retain the previous geometry while refreshing the map in a new viewport.
    pub(super) fn retained_visual_rows(&self) -> VisualRows<'_> {
        VisualRows {
            doc: &self.vim.doc,
            wrapped: self.wraps.borrow().as_ref().map(|cache| cache.rows.clone()),
        }
    }
}

/// Measure the same rows from the live buffer or from intermediate Vim state during replay.
fn measured_rows<'a>(
    source: LineSource<'a>,
    wraps: &RefCell<Option<WrappedRows>>,
    columns: usize,
    spacing: &TextSpacing,
    composition: &str,
) -> VisualRows<'a> {
    let vim = source.0;
    let doc = &vim.doc;
    let cursor_row = doc.starts.partition_point(|&start| start <= doc.cursor) - 1;
    let insert = vim.mode.is_insert();
    let expanded_header = source.folded_brace(cursor_row).and_then(|_| {
        let line = doc.line_range(cursor_row);
        let prefix = doc.text[line.clone()].trim_end();
        (prefix.len() < line.len() && (insert || doc.cursor >= line.start + prefix.len()))
            .then_some(cursor_row)
    });
    let key = WrapKey {
        version: (doc.opening, doc.revision),
        hidden: doc.hidden_rows().to_owned(),
        columns,
        tabstop: doc.tabstop(),
        metrics: spacing.metric_key(),
        composition: (!composition.is_empty() && (insert || vim.awaits_literal_argument()))
            .then(|| (doc.cursor, composition.to_owned())),
        expanded_header,
        end_caret: (insert && doc.cursor == doc.line_range(cursor_row).end).then_some(cursor_row),
    };
    let mut cache = wraps.borrow_mut();
    if cache.as_ref().is_none_or(|cache| cache.key != key) {
        let mut rows = Vec::new();
        let mut line = 0;
        while line < doc.starts.len() {
            let painted = source.painted_line(line, composition);
            append_wrapped_line(
                &mut rows,
                line,
                &painted.text,
                columns * UNITS_PER_COLUMN,
                spacing,
                key.tabstop,
                key.end_caret == Some(line) && painted.preedit.is_none(),
            );
            line = doc.folded_end_row(line).unwrap_or(line) + 1;
        }
        *cache = Some(WrappedRows {
            key,
            rows: rows.into(),
        });
    }
    VisualRows {
        doc,
        wrapped: cache.as_ref().map(|cache| cache.rows.clone()),
    }
}

/// Resolve a motion in measured rows, preserving the target column across vertical moves.
pub(super) fn motion_resolver<'a>(
    wraps: &'a RefCell<Option<WrappedRows>>,
    columns: usize,
    spacing: &'a TextSpacing,
    composition: &'a str,
) -> impl FnMut(&Vim, ScreenMotion, usize, Option<usize>) -> Option<(usize, usize)> + 'a {
    move |vim, motion, count, column| {
        let source = LineSource(vim);
        let doc = &vim.doc;
        let rows = measured_rows(source, wraps, columns, spacing, composition);
        let line = doc.starts.partition_point(|&start| start <= doc.cursor) - 1;
        let painted = source.painted_line(line, composition);
        let mut byte = doc.cursor - doc.starts[line];
        if let Some(suffix) = painted.fold_suffix {
            byte = if vim.fold_cursor_on_closing_brace() {
                suffix.closing_byte
            } else {
                byte.min(suffix.start_byte)
            };
        }
        let current = rows.row_for_byte(line, byte);
        let segment = rows.row(current)?.segment?;
        let visible_row = |index| {
            rows.row(index).filter(|row| {
                // In a narrow pane, the fold decoration can wrap onto its own rows.
                source.folded_brace(row.line).is_none()
                    || source
                        .painted_line(row.line, composition)
                        .fold_suffix
                        .zip(row.segment.as_ref())
                        .is_none_or(|(suffix, segment)| segment.bytes.start < suffix.start_byte)
            })
        };
        if !matches!(motion, ScreenMotion::Up | ScreenMotion::Down) {
            let target = if matches!(motion, ScreenMotion::End | ScreenMotion::LastNonblank) {
                (current..rows.len())
                    .filter_map(visible_row)
                    .take(count)
                    .last()?
            } else {
                rows.row(current)?
            };
            let line = doc.line_range(target.line);
            let end = doc.previous(line.end).max(line.start);
            let segment = target.segment.as_ref()?;
            let start = segment.bytes.start;
            let offset = if motion == ScreenMotion::Middle {
                source.offset_in_visual_row(target, columns as f64 / 2., spacing, composition)
            } else {
                let painted = source.painted_line(target.line, composition);
                let nonblank = |&(_, grapheme): &(usize, &str)| !matches!(grapheme, " " | "\t");
                let byte = match motion {
                    ScreenMotion::FirstNonblank => painted.text[start..]
                        .grapheme_indices(true)
                        .find(nonblank)
                        .map_or(end.saturating_sub(line.start), |(byte, _)| start + byte),
                    ScreenMotion::End => painted.text[segment.bytes.clone()]
                        .grapheme_indices(true)
                        .next_back()
                        .map_or(start, |(byte, _)| start + byte),
                    ScreenMotion::LastNonblank => painted.text[..segment.bytes.end]
                        .grapheme_indices(true)
                        .rev()
                        .find(nonblank)
                        .map_or(0, |(byte, _)| byte),
                    _ => start,
                };
                line.start.saturating_add(byte)
            };
            return Some((offset.min(end), 0));
        }
        let column = column.unwrap_or_else(|| {
            spacing
                .buffer_cells(
                    &painted.text[segment.bytes.clone()],
                    segment.bytes.start,
                    segment.tab_column,
                    doc.tabstop(),
                )
                .find(|cell| cell.byte >= byte)
                .map_or(segment.end_units - segment.start_units, |cell| cell.start)
        });
        let down = motion == ScreenMotion::Down;
        let available = if down {
            rows.len() - current - 1
        } else {
            current
        };
        let target = (1..=available)
            .filter_map(|step| visible_row(if down { current + step } else { current - step }))
            .take(count)
            .last();
        let Some(target) = target else {
            return Some((doc.cursor, column));
        };
        let line = doc.line_range(target.line);
        let end = if line.is_empty() {
            line.end
        } else {
            doc.previous(line.end)
        };
        let offset = source.offset_in_visual_row(
            target,
            column as f64 / UNITS_PER_COLUMN as f64,
            spacing,
            composition,
        );
        Some((offset.min(end), column))
    }
}

impl<'a> VisualRows<'a> {
    /// Use the fold-aware physical rows of `doc` without additional wrapping.
    pub(super) fn physical(doc: &'a Document) -> Self {
        Self { doc, wrapped: None }
    }

    /// Whether Markdown wrapping determines this view's rows and columns.
    pub(super) fn is_wrapped(&self) -> bool {
        self.wrapped.is_some()
    }

    /// Number of displayed rows including soft continuations.
    pub(in crate::grid) fn len(&self) -> usize {
        self.wrapped
            .as_ref()
            .map_or_else(|| self.doc.display_rows(), |rows| rows.len())
    }

    /// Source line and fragment at visual index `index`, if it exists.
    pub(super) fn row(&self, index: usize) -> Option<VisualRow> {
        match &self.wrapped {
            Some(rows) => rows.get(index).cloned(),
            None => (index < self.len()).then(|| VisualRow {
                line: self.doc.document_row(index),
                segment: None,
            }),
        }
    }

    /// First visual row of physical `line` or of the header hiding it.
    pub(in crate::grid) fn first_row(&self, line: usize) -> usize {
        let Some(rows) = &self.wrapped else {
            return self.doc.display_row(line);
        };
        if line >= self.doc.starts.len() {
            return rows.len();
        }
        let index = rows.partition_point(|row| row.line < line);
        if index > 0 && rows.get(index).is_none_or(|row| row.line != line) {
            let header = rows[index - 1].line;
            rows.partition_point(|row| row.line < header)
        } else {
            index
        }
    }

    /// Locate displayed `byte` in physical `line`, including an end-of-line caret.
    pub(super) fn row_for_byte(&self, line: usize, byte: usize) -> usize {
        let first = self.first_row(line);
        let Some(rows) = &self.wrapped else {
            return first;
        };
        let count = rows[first..].partition_point(|row| {
            row.line == line
                && row
                    .segment
                    .as_ref()
                    .is_some_and(|segment| segment.bytes.start <= byte)
        });
        first + count.saturating_sub(1)
    }

    /// Translate fractional visual `position` into a physical row and its progress through that line.
    pub(super) fn document_position(&self, position: f32) -> f32 {
        let index = position.floor().max(0.) as usize;
        let Some(row) = self.row(index) else {
            return self.doc.starts.len() as f32;
        };
        if let Some(rows) = &self.wrapped {
            let first = self.first_row(row.line);
            let next = rows.partition_point(|item| item.line <= row.line);
            let fraction = (position - first as f32) / (next - first) as f32;
            let following = rows
                .get(next)
                .map_or(self.doc.starts.len(), |item| item.line);
            row.line as f32 + fraction * (following - row.line) as f32
        } else {
            let following = self.doc.document_row(index + 1);
            row.line as f32 + position.fract() * (following - row.line) as f32
        }
    }

    /// Translate fractional physical `position` to a visual row, holding steady across hidden rows.
    pub(super) fn visual_position(&self, position: f32) -> f32 {
        let line = position.floor().max(0.) as usize;
        let first = self.first_row(line);
        if let Some(rows) = &self.wrapped
            && let Some(header) = rows.get(first)
        {
            let next = rows.partition_point(|row| row.line <= header.line);
            let following = rows.get(next).map_or(self.doc.starts.len(), |row| row.line);
            if next - first > 1 && following > header.line + 1 {
                let fraction = (position - header.line as f32) / (following - header.line) as f32;
                return first as f32 + fraction * (next - first) as f32;
            }
        }
        let next = self.first_row(line + 1);
        first as f32 + position.fract() * next.saturating_sub(first) as f32
    }

    /// Whether remeasuring this view changed any visual row or source byte boundary.
    pub(super) fn same_geometry(&self, other: &Self) -> bool {
        match (&self.wrapped, &other.wrapped) {
            (Some(lhs), Some(rhs)) => Arc::ptr_eq(lhs, rhs) || lhs == rhs,
            (None, None) => true,
            _ => false,
        }
    }
}

/// A possible wrap point in the full line, preserving its tab-stop position.
#[derive(Clone, Copy, Default)]
struct WrapBoundary {
    /// Display byte at the start of a grapheme or after the final grapheme.
    byte: usize,
    /// Measured position in spacing units.
    units: usize,
    /// Logical column before the boundary.
    column: usize,
}

/// Append grapheme-aligned rows of `text` for file `line` up to `width` spacing units.
/// Use `spacing` for mixed-width text and retain a cell for a full-width insertion `end_caret`.
fn append_wrapped_line(
    rows: &mut Vec<VisualRow>,
    line: usize,
    text: &str,
    width: usize,
    spacing: &TextSpacing,
    tabstop: usize,
    end_caret: bool,
) {
    let mut start = WrapBoundary::default();
    let mut end = start;
    let mut word_break = start;
    let mut push = |start: WrapBoundary, end: WrapBoundary| {
        rows.push(VisualRow {
            line,
            segment: Some(WrapSegment {
                bytes: start.byte..end.byte,
                start_units: start.units,
                end_units: end.units,
                tab_column: start.column,
            }),
        });
    };
    for cell in spacing.buffer_cells(text, 0, 0, tabstop) {
        if cell.end - start.units > width && end.byte > start.byte {
            if word_break.byte > start.byte {
                push(start, word_break);
                start = word_break;
            }
            if end.byte > start.byte && cell.end - start.units > width {
                push(start, end);
                start = end;
            }
        }
        end = WrapBoundary {
            byte: cell.byte + cell.text.len(),
            units: cell.end,
            column: end.column + cell.columns,
        };
        if cell.text == " " || cell.text == "\t" {
            word_break = end;
        }
    }
    push(start, end);
    if end_caret && end.units - start.units >= width {
        push(end, end);
    }
}
