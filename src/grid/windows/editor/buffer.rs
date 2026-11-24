//! One retained document viewport, including IME display text and measured caret positions.

use super::markdown::{MarkdownHighlights, MarkdownViewport, is_markdown};
use crate::{
    grid::windows::terminal::Terminal,
    ui::text::{TextSpacing, UNITS_PER_COLUMN},
};
use nanobug::{
    animation::scroll::GridScroll,
    document::Document,
    plugins::session::{DocumentDiagnostics, DocumentPresentation},
    vim::{ExMessageOutput, ExMessageSpan, HighlightStyle, Vim},
};
use nanobug_plugin_sdk::{
    Diagnostic, DiagnosticSeverity,
    language::{Highlight, Symbol},
};
use std::{
    borrow::Cow,
    cell::{Ref, RefCell},
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

/// Workspace-wide buffer identity, retained when a buffer moves to another tab.
static NEXT_BUFFER_ID: AtomicU64 = AtomicU64::new(1);

#[cfg(test)]
mod highlight_tests {
    use super::*;
    use nanobug::plugins::session::Key;
    use nanobug_plugin_sdk::{
        Instance,
        language::{ByteRange, DocumentFeatures, SyntaxKind},
    };

    #[test]
    fn typing_keeps_untouched_colors_before_the_parser_replies() {
        let mut buffer = Buffer::new(Document::new("fn\nα\ntrue"));
        let opening = buffer.vim.doc.opening;
        buffer.set_presentation(Some((
            Key {
                namespace: "language".into(),
                instance: Instance {
                    project: 1,
                    module: "language".into(),
                },
            },
            Arc::new(DocumentPresentation {
                version: (opening, 0),
                generation: 1,
                features: DocumentFeatures {
                    highlights: vec![
                        Highlight {
                            range: ByteRange { start: 0, end: 2 },
                            kind: SyntaxKind::Keyword,
                            modifiers: Default::default(),
                        },
                        Highlight {
                            range: ByteRange { start: 6, end: 10 },
                            kind: SyntaxKind::Constant,
                            modifiers: Default::default(),
                        },
                    ],
                    ..DocumentFeatures::default()
                },
            }),
        )));
        for (start, insertion, literal_start) in [(3, "x", 7), (4, "é", 9)] {
            buffer.vim.doc.replace(start..start, insertion, true);
            buffer.retain_highlights();
            let spans = buffer.highlights();
            assert_eq!(spans[0].range, ByteRange { start: 0, end: 2 });
            assert_eq!(spans[1].range.start, literal_start);
            assert_eq!(
                &buffer.vim.doc.text[spans[1].range.start..spans[1].range.end],
                "true"
            );
        }
        buffer.vim.doc.undo(false);
        buffer.retain_highlights();
        assert!(buffer.highlights().is_empty());
    }
}

/// Extra text columns on either side for fractional scrolling and wide glyphs.
pub(in crate::grid) const TEXT_OVERSCAN: usize = 4;

/// Virtual placeholder preceding the closing brace of a folded code block.
pub(super) const FOLD_ELLIPSIS: &str = " ... ";

/// Display text for one logical line, with byte ranges for IME and synthetic fold cells.
pub(super) struct PaintedLine<'a> {
    /// Source text plus any preedit and rendered fold placeholder.
    pub(super) text: Cow<'a, str>,
    /// Byte range occupied by preedit text in `text`.
    pub(super) preedit: Option<Range<usize>>,
    /// Byte mapping for the synthesized ellipsis and closing brace.
    pub(super) fold_suffix: Option<FoldSuffix>,
}

/// Map the fold placeholder's synthetic bytes to the displayed line and source document.
pub(super) struct FoldSuffix {
    /// First byte of the synthetic ellipsis on the displayed line.
    pub(super) start_byte: usize,
    /// First displayed byte of the synthetic closing brace.
    pub(super) closing_byte: usize,
    /// Actual document byte offset of the closing brace.
    pub(super) closing_offset: usize,
}

/// Read displayed text from either a retained buffer or the Vim state during a replay.
#[derive(Clone, Copy)]
pub(super) struct LineSource<'a>(pub(super) &'a Vim);

/// One open file and its retained viewport, displayed through the single editing area.
pub(in crate::grid) struct Buffer {
    /// Stable identity retained when the buffer moves between tabs.
    pub(in crate::grid) id: u64,
    /// Document and modal state.
    pub(in crate::grid) vim: Vim,
    /// Terminal screen and process when this buffer is backed by a PTY.
    pub(super) terminal: Option<Terminal>,
    /// List shown by this read-only buffer and the pane used for jumps.
    pub(super) results: Option<(super::ResultList, usize)>,
    /// Continuous viewport position with a whole-row destination.
    pub(in crate::grid) scroll: GridScroll,
    /// Last bound target and the requested row before clamping at a document boundary.
    pub(super) bound_scroll: Option<(usize, isize)>,
    /// Continuous horizontal offset in base grid columns, independent of logical text columns.
    pub(in crate::grid) columns: GridScroll,
    /// Markdown mode and scroll position materialized from the current Tabpage.
    pub(super) markdown: MarkdownViewport,
    /// Most recently measured Markdown source rows and their layout inputs.
    pub(super) wraps: RefCell<Option<super::wrap::WrappedRows>>,
    /// Source byte of the saved viewport's top fragment, awaiting native layout on restore.
    pub(super) restored_wrap_byte: Option<usize>,
    /// Validated syntax data from the selected language provider for this buffer.
    presentation: Option<(nanobug::plugins::session::Key, Arc<DocumentPresentation>)>,
    /// Unchanged syntax spans remapped to the current revision while the parser catches up.
    retained_highlights: Option<(u64, Vec<Highlight>)>,
    /// Native grammar and language-server spans, independently bound to a text snapshot.
    pub(super) syntax_layers: [Option<Arc<nanobug::plugins::session::DocumentSyntax>>; 2],
    /// Native Markdown syntax cached independently of optional language providers.
    pub(super) markdown_highlights: MarkdownHighlights,
    /// Styles captured with the text of an immutable Ex message listing.
    message_spans: Vec<ExMessageSpan>,
    /// Latest diagnostic sets received for this buffer from live language providers.
    diagnostics: Vec<Arc<DocumentDiagnostics>>,
    /// Shared buffer number at delivery, so reopening a file cannot inherit stale ranges.
    diagnostic_buffer: Option<u64>,
    /// Changes whenever a provider replaces the diagnostics, including within one text revision.
    diagnostic_epoch: u64,
    /// SQL dialect override retained only for this open buffer.
    sql_dialect: Option<String>,
}

impl Buffer {
    /// Create an immutable view of an Ex listing, keeping captured message colors.
    pub(super) fn message_output(output: ExMessageOutput) -> Self {
        let mut buffer = Self::with_vim(Vim::read_only(Document::new(&output.text)));
        buffer.message_spans = output.spans;
        buffer
    }

    /// Read the retained presentation at this byte in a message listing.
    pub(super) fn message_highlight(&self, offset: usize) -> Option<HighlightStyle> {
        ExMessageOutput::style_at(&self.message_spans, offset)
    }
    /// Create a listed placeholder for a file that has not been loaded yet.
    pub(in crate::grid) fn unloaded(doc: Document) -> Self {
        Self::new(doc)
    }

    /// Create the Session's terminal buffer with a stable Ex number.
    pub(in crate::grid) fn terminal_view(terminal: Terminal) -> Self {
        let mut view = Self::with_vim(Vim::read_only(Document::new("")));
        view.terminal = Some(terminal);
        view
    }

    /// Access this buffer's local terminal input state and shared process.
    pub(in crate::grid) fn terminal(&self) -> Option<&Terminal> {
        self.terminal.as_ref()
    }

    /// Create a scratch Options view that applies its commands in `pane`.
    pub(in crate::grid) fn options_sheet(doc: Document, pane: usize) -> Self {
        let mut view = Self::new(doc);
        view.vim.mark_ex_options_editor(pane);
        view
    }

    /// Whether this buffer represents editable text that other panes can display.
    pub(in crate::grid) fn supports_shared_view(&self) -> bool {
        self.terminal.is_none() && !self.vim.is_read_only()
    }

    /// Open a fresh caret and viewport onto this buffer's shared document.
    pub(in crate::grid) fn shared_view(&self) -> Self {
        let mut view = Self::new(self.vim.doc.shared_view());
        view.vim.share_buffer_options(&self.vim);
        view
    }

    /// Create an independent preview while preserving editability and any PTY backing.
    pub(in crate::grid) fn preview_view(&self) -> Self {
        if !self.vim.is_read_only() {
            return self.shared_view();
        }
        let doc = self.vim.doc.shared_view();
        let mut view = Self::with_vim(if self.vim.is_help() {
            Vim::help(doc)
        } else {
            Vim::read_only(doc)
        });
        view.terminal = self.terminal.as_ref().map(Terminal::shared_view);
        if view.terminal.is_some() {
            view.vim.share_buffer_options(&self.vim);
        }
        view
    }

    /// Share a text buffer or PTY in a new pane with its own local input state.
    pub(in crate::grid) fn split_view(&self) -> Option<Self> {
        (self.supports_shared_view() || self.terminal.is_some()).then(|| self.preview_view())
    }

    /// Incorporate changes from another view, invalidating derived presentation for old text.
    pub(super) fn refresh_shared_document(&mut self) {
        if self.vim.refresh_shared_document() {
            self.set_presentation(None);
        }
    }

    /// Start editing `doc` with a fresh Vim state and viewport.
    pub(super) fn new(doc: Document) -> Self {
        Self::with_vim(Vim::new(doc))
    }

    /// Put `vim` in a fresh viewport with a unique buffer identity.
    pub(super) fn with_vim(vim: Vim) -> Self {
        Self {
            id: NEXT_BUFFER_ID.fetch_add(1, Ordering::Relaxed),
            vim,
            terminal: None,
            results: None,
            scroll: GridScroll::default(),
            bound_scroll: None,
            columns: GridScroll::default(),
            markdown: MarkdownViewport::default(),
            wraps: RefCell::new(None),
            restored_wrap_byte: None,
            presentation: None,
            retained_highlights: None,
            markdown_highlights: MarkdownHighlights::default(),
            syntax_layers: Default::default(),
            message_spans: Vec::new(),
            diagnostics: Vec::new(),
            diagnostic_buffer: None,
            diagnostic_epoch: 0,
            sql_dialect: None,
        }
    }

    /// Return the explicit SQL dialect for this buffer, if one was chosen.
    pub(in crate::grid) fn sql_dialect(&self) -> Option<&str> {
        self.sql_dialect.as_deref()
    }

    /// Replace the buffer's temporary SQL dialect and discard the prior provider's presentation.
    pub(super) fn set_sql_dialect(&mut self, dialect: Option<String>) {
        if self.sql_dialect != dialect {
            self.sql_dialect = dialect;
            self.set_presentation(None);
        }
    }

    /// Replace the current provider result and synchronize these folds with the document.
    pub(in crate::grid) fn set_presentation(
        &mut self,
        presentation: Option<(nanobug::plugins::session::Key, Arc<DocumentPresentation>)>,
    ) -> bool {
        self.retained_highlights = None;
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

    /// Remap only unchanged syntax while waiting for an updated result from the same provider.
    pub(super) fn retain_highlights(&mut self) {
        let doc = &self.vim.doc;
        let Some((_, data)) = self.presentation.as_ref() else {
            return;
        };
        if data.version.0 != doc.opening {
            self.retained_highlights = None;
            return;
        }
        let revision = self
            .retained_highlights
            .as_ref()
            .map_or(data.version.1, |(revision, _)| *revision);
        if revision == doc.revision {
            return;
        }
        let Some((old, inserted)) = doc.replacement_since(revision) else {
            self.retained_highlights = None;
            return;
        };
        let (_, mut spans) = self
            .retained_highlights
            .take()
            .unwrap_or_else(|| (revision, data.features.highlights.clone()));
        let mut split_suffix = None;
        spans.retain_mut(|span| {
            let range = &mut span.range;
            if range.end <= old.start {
                return true;
            }
            if range.start >= old.end {
                range.start = range.start - old.end + old.start + inserted;
                range.end = range.end - old.end + old.start + inserted;
                return true;
            }
            if range.start < old.start {
                if range.end > old.end {
                    let mut suffix = *span;
                    suffix.range.start = old.start + inserted;
                    suffix.range.end = suffix.range.end - old.end + old.start + inserted;
                    split_suffix = Some(suffix);
                }
                span.range.end = old.start;
                return true;
            }
            if range.end > old.end {
                range.start = old.start + inserted;
                range.end = range.end - old.end + old.start + inserted;
                return true;
            }
            false
        });
        if let Some(suffix) = split_suffix {
            let index = spans.partition_point(|span| span.range.start < suffix.range.start);
            spans.insert(index, suffix);
        }
        self.retained_highlights = Some((doc.revision, spans));
    }

    /// Sorted current syntax spans, including revalidated ranges pending a fresh parse.
    pub(super) fn highlights(&self) -> &[Highlight] {
        if let Some(data) = self.presentation() {
            &data.features.highlights
        } else {
            self.retained_highlights
                .as_ref()
                .filter(|(revision, _)| *revision == self.vim.doc.revision)
                .map_or(&[], |(_, spans)| spans)
        }
    }

    /// Return built-in syntax ranges for the current Markdown revision.
    pub(super) fn markdown_highlights(&self) -> Option<Ref<'_, [Highlight]>> {
        let doc = &self.vim.doc;
        doc.path.as_deref().filter(|path| is_markdown(path))?;
        Some(self.markdown_highlights.get(doc))
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
        let number = (!incoming.is_empty()).then(|| self.vim.doc.buffer_number());
        if self.diagnostic_buffer == number
            && self.diagnostics.len() == incoming.len()
            && self
                .diagnostics
                .iter()
                .zip(&incoming)
                .all(|(old, new)| Arc::ptr_eq(old, new))
        {
            return false;
        }
        self.diagnostics = incoming;
        self.diagnostic_buffer = number;
        self.diagnostic_epoch += 1;
        true
    }

    /// Identify the data used by a diagnostic popup while the caret stays still.
    pub(in crate::grid) fn diagnostic_epoch(&self) -> u64 {
        self.diagnostic_epoch
    }

    /// Share a provider's ranges across views of the same buffer, path and revision.
    pub(super) fn diagnostics(&self) -> impl Iterator<Item = &Diagnostic> {
        let doc = &self.vim.doc;
        self.diagnostics
            .iter()
            .filter(move |data| {
                self.diagnostic_buffer == Some(doc.buffer_number())
                    && data.version.1 == doc.revision
                    && doc.path == data.path
            })
            .flat_map(|data| &data.diagnostics)
    }

    /// Return all diagnostics touching displayed `line`, including points at the end of the file.
    pub(in crate::grid) fn line_diagnostics(
        &self,
        line: usize,
    ) -> impl Iterator<Item = &Diagnostic> {
        let mut range = self.vim.doc.line_range(line);
        if line + 1 == self.vim.line_count() {
            range.end = self.vim.doc.text.len();
        }
        self.diagnostics().filter(move |item| {
            item.start <= range.end
                && (range.start < item.end || item.start == item.end && item.start >= range.start)
        })
    }

    /// Resolve a diagnostic offset `count` positions in the `forward` direction.
    /// With `edge`, choose the first or last position directly. `severity` filters before counting.
    pub(in crate::grid) fn diagnostic_target(
        &self,
        forward: bool,
        edge: bool,
        count: usize,
        severity: Option<DiagnosticSeverity>,
    ) -> Option<usize> {
        use unicode_segmentation::UnicodeSegmentation;
        let doc = &self.vim.doc;
        let mut positions: Vec<_> = self
            .diagnostics()
            .filter(|item| severity.is_none_or(|level| level == item.severity))
            .map(|item| {
                // Count attainable Normal-mode positions, including diagnostics inside a grapheme
                // or at an insertion point just beyond the last character.
                let row = doc
                    .starts
                    .partition_point(|&start| start <= item.start)
                    .saturating_sub(1)
                    .min(self.vim.line_count() - 1);
                let start = doc.starts[row];
                doc.line(row)
                    .grapheme_indices(true)
                    .map(|(byte, _)| start + byte)
                    .take_while(|&byte| byte <= item.start)
                    .last()
                    .unwrap_or(start)
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

    /// Return document `row` with inline `composition` for editing or a literal argument.
    /// The optional byte range marks the temporary text in the returned line.
    pub(in crate::grid) fn displayed_line(
        &self,
        row: usize,
        composition: &str,
    ) -> (Cow<'_, str>, Option<Range<usize>>) {
        LineSource(&self.vim).displayed_line(row, composition)
    }

    /// Assemble the displayed logical `row` with current IME `composition` and any virtual fold suffix.
    pub(super) fn painted_line(&self, row: usize, composition: &str) -> PaintedLine<'_> {
        LineSource(&self.vim).painted_line(row, composition)
    }

    /// Borrow the closing brace and adjacent terminator of a folded block, with their source offset.
    pub(super) fn folded_brace(&self, row: usize) -> Option<(usize, &str)> {
        LineSource(&self.vim).folded_brace(row)
    }

    /// Fit the fold suffix next to its opening brace, retaining source space for editing.
    pub(super) fn fold_header<'a>(&self, row: usize, line: &'a str) -> &'a str {
        LineSource(&self.vim).fold_header(row, line)
    }

    /// Measure the rendered line and its virtual fold suffix for horizontal scrolling.
    pub(super) fn display_columns(&self, row: usize, line: &str, spacing: &TextSpacing) -> usize {
        let columns = |line| {
            spacing
                .buffer_cells(line, 0, 0, self.vim.doc.tabstop())
                .last()
                .map_or(0, |cell| cell.end.div_ceil(UNITS_PER_COLUMN))
        };
        if let Some((_, brace)) = self.folded_brace(row) {
            columns(self.fold_header(row, line)) + FOLD_ELLIPSIS.len() + brace.len()
        } else {
            columns(line)
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
        let end = spacing
            .buffer_cells(header, 0, 0, self.vim.doc.tabstop())
            .last()
            .map_or(0, |cell| cell.end);
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
        for cell in spacing.buffer_cells(line, 0, 0, self.vim.doc.tabstop()) {
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

impl<'a> LineSource<'a> {
    /// Return the source line with pending IME text inserted at the caret.
    pub(super) fn displayed_line(
        self,
        row: usize,
        composition: &str,
    ) -> (Cow<'a, str>, Option<Range<usize>>) {
        let doc = &self.0.doc;
        let range = doc.line_range(row);
        let line = &doc.text[range.clone()];
        if composition.is_empty()
            || !(self.0.mode.is_insert() || self.0.awaits_literal_argument())
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

    /// Append the virtual ellipsis and closing brace of a collapsed source line.
    pub(super) fn painted_line(self, row: usize, composition: &str) -> PaintedLine<'a> {
        let (mut text, preedit) = self.displayed_line(row, composition);
        let fold_suffix = self.folded_brace(row).map(|(closing_offset, brace)| {
            let start_byte = self.fold_header(row, &text).len();
            let text = text.to_mut();
            text.truncate(start_byte);
            text.push_str(FOLD_ELLIPSIS);
            text.push_str(brace);
            FoldSuffix {
                start_byte,
                closing_byte: start_byte + FOLD_ELLIPSIS.len(),
                closing_offset,
            }
        });
        PaintedLine {
            text,
            preedit,
            fold_suffix,
        }
    }

    /// Find the real source offset and text of a folded line's closing brace.
    pub(super) fn folded_brace(self, row: usize) -> Option<(usize, &'a str)> {
        let doc = &self.0.doc;
        let (_, closing) = doc.folded_braces(row)?;
        let end = closing + 1 + usize::from(doc.text[closing + 1..].starts_with([',', ';']));
        Some((closing, &doc.text[closing..end]))
    }

    /// Trim trailing space from a fold header unless the caret can edit that space.
    pub(super) fn fold_header<'b>(self, row: usize, line: &'b str) -> &'b str {
        let prefix = line.trim_end();
        let doc = &self.0.doc;
        let range = doc.line_range(row);
        if (range.start..=range.end).contains(&doc.cursor)
            && (self.0.mode.is_insert() || doc.cursor >= range.start + prefix.len())
        {
            line
        } else {
            prefix
        }
    }
}
