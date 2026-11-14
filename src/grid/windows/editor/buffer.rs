//! One retained document viewport, including IME display text and measured caret positions.

use crate::ui::text::{TextSpacing, UNITS_PER_COLUMN};
use nanobug::{
    animation::scroll::GridScroll,
    document::Document,
    plugins::session::{DocumentDiagnostics, DocumentPresentation},
    vim::Vim,
};
use nanobug_plugin_sdk::{
    Diagnostic,
    language::{Highlight, Symbol},
};
use std::{
    borrow::Cow,
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

/// Workspace-wide buffer identity, retained when a buffer moves to another tab.
static NEXT_BUFFER_ID: AtomicU64 = AtomicU64::new(1);

/// Extra text columns on either side for fractional scrolling and wide glyphs.
pub(in crate::grid) const TEXT_OVERSCAN: usize = 4;

/// Virtual placeholder preceding the closing brace of a folded code block.
pub(super) const FOLD_ELLIPSIS: &str = " ... ";

/// One open file and its retained viewport, displayed through the single editing area.
pub(in crate::grid) struct Buffer {
    /// Stable identity retained when the buffer moves between tabs.
    pub(in crate::grid) id: u64,
    /// Document and modal state.
    pub(in crate::grid) vim: Vim,
    /// Continuous viewport position with a whole-row destination.
    pub(in crate::grid) scroll: GridScroll,
    /// Continuous horizontal offset in base grid columns, independent of logical text columns.
    pub(in crate::grid) columns: GridScroll,
    /// Validated syntax data from the selected language provider for this buffer.
    presentation: Option<(nanobug::plugins::session::Key, Arc<DocumentPresentation>)>,
    /// Latest diagnostic sets received for this buffer from live language providers.
    diagnostics: Vec<Arc<DocumentDiagnostics>>,
}

impl Buffer {
    /// Start editing `doc` with a fresh Vim state and viewport.
    pub(super) fn new(doc: Document) -> Self {
        Self::with_vim(Vim::new(doc))
    }

    /// Put `vim` in a fresh viewport with a unique buffer identity.
    pub(super) fn with_vim(vim: Vim) -> Self {
        Self {
            id: NEXT_BUFFER_ID.fetch_add(1, Ordering::Relaxed),
            vim,
            scroll: GridScroll::default(),
            columns: GridScroll::default(),
            presentation: None,
            diagnostics: Vec::new(),
        }
    }

    /// Replace the current provider result and synchronize these folds with the document.
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
        let same_provider = self.presentation_owner()
            == presentation
                .as_ref()
                .map(|(key, data)| (key, data.generation));
        let lines = &self.vim.doc.starts;
        let folds = presentation.as_ref().map_or_else(Vec::new, |(_, data)| {
            data.features
                .folds
                .iter()
                .map(|range| {
                    let first = lines.partition_point(|&start| start <= range.start) - 1;
                    let end = lines.partition_point(|&start| start < range.end);
                    first..end
                })
                .collect()
        });
        self.vim.doc.set_provider_folds(folds, same_provider);
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

    /// Replace the live diagnostics and report whether their snapshots changed.
    pub(in crate::grid) fn set_diagnostics(
        &mut self,
        incoming: Vec<Arc<DocumentDiagnostics>>,
    ) -> bool {
        if self.diagnostics.len() == incoming.len()
            && self
                .diagnostics
                .iter()
                .zip(&incoming)
                .all(|(old, new)| Arc::ptr_eq(old, new))
        {
            return false;
        }
        self.diagnostics = incoming;
        true
    }

    /// Show diagnostics only on the exact opening and revision they describe.
    fn diagnostics(&self) -> impl Iterator<Item = &Diagnostic> {
        let version = (self.vim.doc.opening, self.vim.doc.revision);
        self.diagnostics
            .iter()
            .filter(move |data| {
                data.version == version && self.vim.doc.path.as_ref() == Some(&data.path)
            })
            .flat_map(|data| &data.diagnostics)
    }

    /// Whether a diagnostic touches the document line, including a point at the line end.
    pub(super) fn diagnostic_on_line(&self, line: usize) -> bool {
        let range = self.vim.doc.line_range(line);
        self.diagnostics().any(|item| {
            item.start <= range.end
                && (range.start < item.end || item.start == item.end && item.start >= range.start)
        })
    }

    /// Get the first message at the caret, including diagnostics that mark a single position.
    pub(in crate::grid) fn caret_diagnostic(&self) -> Option<&str> {
        let offset = self.vim.doc.cursor;
        self.diagnostics()
            .find(|item| {
                item.start <= offset
                    && (offset < item.end || item.start == item.end && item.start == offset)
            })
            .map(|item| item.message.as_str())
    }

    /// Resolve a diagnostic offset `count` positions in the `forward` direction.
    /// With `edge`, choose the first or last available position directly.
    pub(in crate::grid) fn diagnostic_target(
        &self,
        forward: bool,
        edge: bool,
        count: usize,
    ) -> Option<usize> {
        let mut positions: Vec<_> = self
            .diagnostics()
            .map(|item| {
                self.vim
                    .doc
                    .text
                    .floor_char_boundary(item.start.min(self.vim.doc.text.len()))
            })
            .collect();
        positions.sort_unstable();
        positions.dedup();
        let len = positions.len();
        if len == 0 {
            return None;
        }
        if edge {
            return Some(positions[if forward { len - 1 } else { 0 }]);
        }
        let offset = self.vim.doc.cursor;
        let steps = count.saturating_sub(1) % len;
        let index = if forward {
            (positions.partition_point(|&position| position <= offset) + steps) % len
        } else {
            (positions.partition_point(|&position| position < offset) + len - 1 - steps) % len
        };
        Some(positions[index])
    }

    /// Toggle the innermost available fold around the cursor.
    pub(in crate::grid) fn toggle_fold(&mut self) -> bool {
        self.vim.toggle_fold()
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
            || !self.vim.mode.is_insert()
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

    /// Borrow the closing brace and adjacent terminator of a folded block, with their source offset.
    pub(super) fn folded_brace(&self, row: usize) -> Option<(usize, &str)> {
        let doc = &self.vim.doc;
        let (_, closing) = doc.folded_braces(row)?;
        let end = closing + 1 + usize::from(doc.text[closing + 1..].starts_with([',', ';']));
        Some((closing, &doc.text[closing..end]))
    }

    /// Fit the fold suffix next to its opening brace, retaining source space for editing.
    pub(super) fn fold_header<'a>(&self, row: usize, line: &'a str) -> &'a str {
        let prefix = line.trim_end();
        let doc = &self.vim.doc;
        let range = doc.line_range(row);
        if (range.start..=range.end).contains(&doc.cursor)
            && (self.vim.mode.is_insert() || doc.cursor >= range.start + prefix.len())
        {
            line
        } else {
            prefix
        }
    }

    /// Measure the rendered line and its virtual fold suffix for horizontal scrolling.
    pub(super) fn display_columns(&self, row: usize, line: &str, spacing: &TextSpacing) -> usize {
        if let Some((_, brace)) = self.folded_brace(row) {
            spacing.columns(self.fold_header(row, line)) + FOLD_ELLIPSIS.len() + brace.len()
        } else {
            spacing.columns(line)
        }
    }

    /// Measure the three visible dots in spacing units on a folded preview line.
    pub(super) fn fold_ellipsis_span(
        &self,
        row: usize,
        line: &str,
        spacing: &TextSpacing,
    ) -> Range<usize> {
        let header = self.fold_header(row, line);
        let end = spacing.cells(header).last().map_or(0, |cell| cell.end);
        end + UNITS_PER_COLUMN..end + (FOLD_ELLIPSIS.len() - 1) * UNITS_PER_COLUMN
    }

    /// Locate the caret on displayed `line` of `row` using the editor's `spacing` units.
    /// Insert carets, tabs and line endings reserve one column.
    pub(in crate::grid) fn caret_span(
        &self,
        row: usize,
        line: &str,
        spacing: &TextSpacing,
    ) -> Range<usize> {
        if self.vim.fold_cursor_on_closing_brace() {
            let start = self.fold_ellipsis_span(row, line, spacing).end + UNITS_PER_COLUMN;
            return start..start + UNITS_PER_COLUMN;
        }
        let offset = self.vim.doc.cursor - self.vim.doc.starts[row];
        let mut end = 0;
        for cell in spacing.cells(line) {
            if cell.byte + cell.text.len() > offset {
                let width = if self.vim.mode.is_insert() || cell.text == "\t" {
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
