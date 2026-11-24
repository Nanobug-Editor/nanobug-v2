//! Read-only editor buffers used to browse Quickfix, location lists and live diagnostics.

use super::{Buffer, EditorWindow};
use nanobug::{document::Document, i18n, vim::Vim};

/// Scope of the results shown in a native list window.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(in crate::grid) enum ResultList {
    /// The current project's Quickfix list.
    Quickfix,
    /// The location list owned by this source pane.
    Location(usize),
    /// Live diagnostics from loaded project documents.
    Diagnostics,
    /// Live diagnostics for the given shared buffer number.
    BufferDiagnostics(u64),
}

impl ResultList {
    /// Localized tab title for this list.
    pub(in crate::grid) fn title(self) -> &'static str {
        i18n::text(match self {
            Self::Quickfix => "quickfix.title",
            Self::Location(_) => "quickfix.location",
            Self::Diagnostics => "quickfix.diagnostics",
            Self::BufferDiagnostics(_) => "quickfix.buffer_diagnostics",
        })
    }
}

impl Buffer {
    /// Create a read-only list pane which opens entries in its `source` editor.
    pub(in crate::grid) fn result_list(kind: ResultList, source: usize, text: &str) -> Self {
        let mut buffer = Self::with_vim(Vim::read_only(Document::new(text)));
        buffer.results = Some((kind, source));
        buffer
    }

    /// Identify the list and source pane associated with this buffer.
    pub(in crate::grid) fn list_owner(&self) -> Option<(ResultList, usize)> {
        self.results
    }
}

impl EditorWindow {
    /// Update a displayed list in place and retain its caret unless a target row was requested.
    pub(in crate::grid) fn update_result_rows(&mut self, text: &str, row: Option<usize>) {
        let vim = &mut self.buffers[self.active].vim;
        let row = row.unwrap_or_else(|| vim.doc.location(vim.doc.cursor).0);
        vim.doc.refresh_listing(text);
        let row = row.min(vim.doc.starts.len() - 1);
        vim.set_cursor(vim.doc.starts[row]);
    }
}
