//! One retained document viewport, including IME display text and measured caret positions.

use crate::ui::text::{TextSpacing, UNITS_PER_COLUMN};
use nanobug::{
    animation::scroll::GridScroll,
    document::Document,
    plugins::session::DocumentPresentation,
    vim::{Mode, Vim},
};
use nanobug_plugin_sdk::language::{Highlight, Symbol};
use std::{borrow::Cow, ops::Range, sync::Arc};

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
    /// Validated syntax data from the selected language provider for this buffer.
    presentation: Option<(nanobug::plugins::session::Key, Arc<DocumentPresentation>)>,
}

impl Buffer {
    /// Start editing `doc` with a fresh Vim state and viewport.
    pub(super) fn new(doc: Document) -> Self {
        Self {
            vim: Vim::new(doc),
            scroll: GridScroll::default(),
            columns: GridScroll::default(),
            presentation: None,
        }
    }

    /// Replace the current provider result, clearing collapsed folds when the provider disappears.
    pub(in crate::grid) fn set_presentation(
        &mut self,
        presentation: Option<(nanobug::plugins::session::Key, Arc<DocumentPresentation>)>,
    ) -> bool {
        if self
            .presentation
            .as_ref()
            .zip(presentation.as_ref())
            .is_some_and(|((old_key, old), (new_key, new))| {
                old_key == new_key && Arc::ptr_eq(old, new)
            })
            || self.presentation.is_none() && presentation.is_none()
        {
            return false;
        }
        if self.presentation_owner()
            != presentation
                .as_ref()
                .map(|(key, data)| (key, data.generation))
        {
            self.vim.doc.clear_hidden_rows();
        }
        self.presentation = presentation;
        true
    }

    /// Provider that owns the retained folds while this buffer awaits fresh presentation data.
    pub(in crate::grid) fn presentation_owner(
        &self,
    ) -> Option<(&nanobug::plugins::session::Key, u64)> {
        self.presentation
            .as_ref()
            .map(|(key, data)| (key, data.generation))
    }

    /// Syntax data applies only to the exact revision of the visible buffer.
    fn presentation(&self) -> Option<&DocumentPresentation> {
        self.presentation
            .as_ref()
            .map(|(_, data)| data.as_ref())
            .filter(|data| data.version == (self.vim.doc.opening, self.vim.doc.revision))
    }

    /// Sorted syntax spans for the currently displayed revision.
    pub(super) fn highlights(&self) -> &[Highlight] {
        self.presentation()
            .map_or(&[], |data| &data.features.highlights)
    }

    /// Outline entries in source order for the currently displayed revision.
    pub(in crate::grid) fn symbols(&self) -> &[Symbol] {
        self.presentation()
            .map_or(&[], |data| &data.features.symbols)
    }

    /// Expand the fold containing the cursor after a jump to a hidden document row.
    pub(in crate::grid) fn reveal_cursor_fold(&mut self) -> bool {
        let row = self.cursor_row();
        self.vim.doc.reveal_row(row)
    }

    /// Toggle the innermost available fold around the cursor.
    pub(in crate::grid) fn toggle_fold(&mut self) -> bool {
        let row = self.cursor_row();
        if self.vim.doc.open_fold_at(row) {
            return true;
        }
        let Some(data) = self.presentation() else {
            return false;
        };
        let doc = &self.vim.doc;
        let fold = data
            .features
            .folds
            .iter()
            .filter_map(|range| {
                let first = doc.starts.partition_point(|&start| start <= range.start) - 1;
                let last = doc
                    .starts
                    .partition_point(|&start| start < range.end)
                    .saturating_sub(1);
                (first <= row && row <= last && first < last).then_some(first..last + 1)
            })
            .min_by_key(Range::len);
        let Some(fold) = fold else {
            return false;
        };
        self.vim.doc.collapse_rows(fold.start + 1..fold.end);
        if row != fold.start {
            self.vim.set_cursor(self.vim.doc.starts[fold.start]);
        }
        true
    }

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
