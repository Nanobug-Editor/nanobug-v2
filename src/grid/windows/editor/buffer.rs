//! One retained document viewport, including IME display text and measured caret positions.

use crate::ui::text::{TextSpacing, UNITS_PER_COLUMN};
use nanobug::{
    animation::scroll::GridScroll,
    vim::{Mode, Vim},
};
use std::{borrow::Cow, ops::Range};

/// Extra text columns on either side for fractional scrolling and wide glyphs.
pub(in crate::grid) const TEXT_OVERSCAN: usize = 4;

/// One open file and its retained viewport, displayed through the single editing area.
pub(in crate::grid) struct Buffer {
    /// Document and modal state.
    pub(in crate::grid) vim: Vim,
    /// Continuous viewport position with a whole-row destination.
    pub(in crate::grid) scroll: GridScroll,
    /// Continuous horizontal offset in base grid columns, independent of logical text columns.
    pub(in crate::grid) columns: GridScroll,
}

impl Buffer {
    /// Return the active byte cursor's document row without scanning graphemes.
    pub(in crate::grid) fn cursor_row(&self) -> usize {
        self.vim
            .doc
            .starts
            .partition_point(|&start| start <= self.vim.doc.cursor)
            - 1
    }

    /// Return document `row` with any active insert `composition`, plus its temporary byte range.
    pub(in crate::grid) fn displayed_line(
        &self,
        row: usize,
        composition: &str,
    ) -> (Cow<'_, str>, Option<Range<usize>>) {
        let doc = &self.vim.doc;
        let range = doc.line_range(row);
        let line = &doc.text[range.clone()];
        if composition.is_empty()
            || self.vim.mode != Mode::Insert
            || !(range.start..=range.end).contains(&doc.cursor)
        {
            return (Cow::Borrowed(line), None);
        }
        let offset = doc.cursor - range.start;
        let mut displayed = String::with_capacity(line.len() + composition.len());
        displayed.push_str(&line[..offset]);
        displayed.push_str(composition);
        displayed.push_str(&line[offset..]);
        (
            Cow::Owned(displayed),
            Some(offset..offset + composition.len()),
        )
    }

    /// Locate the caret on displayed `line` of `row` using the editor's `spacing` units.
    /// Insert carets, tabs and line endings reserve one column.
    pub(in crate::grid) fn caret_span(
        &self,
        row: usize,
        line: &str,
        spacing: &TextSpacing,
    ) -> Range<usize> {
        let offset = self.vim.doc.cursor - self.vim.doc.starts[row];
        let mut end = 0;
        for cell in spacing.cells(line) {
            if cell.byte + cell.text.len() > offset {
                let width = if self.vim.mode == Mode::Insert || cell.text == "\t" {
                    UNITS_PER_COLUMN
                } else {
                    cell.end - cell.start
                };
                return cell.start..cell.start + width;
            }
            end = cell.end;
        }
        end..end + UNITS_PER_COLUMN
    }

    /// Number of retained columns before the first visible text column.
    pub(in crate::grid) fn left_padding(&self) -> usize {
        self.columns.first_cell().min(TEXT_OVERSCAN)
    }

    /// Text panel's horizontal displacement from its clip, in base grid columns.
    pub(in crate::grid) fn text_displacement(&self) -> f32 {
        self.left_padding() as f32 + self.columns.fraction()
    }
}
