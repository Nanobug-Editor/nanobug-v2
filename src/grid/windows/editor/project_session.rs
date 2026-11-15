//! Save an editor pane's file labels and view positions without retaining document text.

use super::{Buffer, EditorWindow, MarkdownViewMode};
use nanobug::document::{Document, display_width};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The files and selection that make up one editor pane in a project Session.
#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
pub(in crate::grid) struct EditorSnapshot {
    /// Named buffers in file-label order.
    files: Vec<FileView>,
    /// Selected file index before skipping any unavailable files.
    active: usize,
}

/// Positions in the source file and the Markdown view selected for it.
#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
struct FileView {
    /// Path relative to the project when possible, otherwise the original absolute path.
    path: PathBuf,
    /// Zero-based source row and display column of the caret.
    caret: (usize, usize),
    /// Zero-based source row at the top of the viewport.
    top: usize,
    /// Horizontal offset in grid columns.
    left: usize,
    /// Markdown presentation and preview scroll offset in logical pixels.
    markdown: (MarkdownViewMode, usize),
}

impl EditorWindow {
    /// Capture named files relative to project `root` and the current viewport of each file.
    pub(in crate::grid) fn snapshot_for_project(&self, root: &Path) -> EditorSnapshot {
        let mut active = 0;
        let mut files = Vec::new();
        for (index, buffer) in self.buffers.iter().enumerate() {
            let doc = &buffer.vim.doc;
            let Some(path) = &doc.path else { continue };
            if index <= self.active {
                active = files.len();
            }
            files.push(FileView {
                path: path.strip_prefix(root).unwrap_or(path).to_owned(),
                caret: doc.location(doc.cursor),
                top: doc.document_row(
                    buffer
                        .scroll
                        .target()
                        .min(doc.display_rows().saturating_sub(1)),
                ),
                left: buffer.columns.target(),
                markdown: (buffer.markdown.mode, buffer.markdown.scroll.target()),
            });
        }
        EditorSnapshot { files, active }
    }

    /// Reopen `snapshot` files from disk relative to project `root`, counting skipped files.
    pub(in crate::grid) fn restore_project_snapshot(
        snapshot: EditorSnapshot,
        root: &Path,
    ) -> (Self, usize) {
        let total = snapshot.files.len();
        let mut active = 0;
        let mut buffers = Vec::new();
        for (index, file) in snapshot.files.into_iter().enumerate() {
            let path = root.join(file.path);
            let Ok(path) = path.canonicalize() else {
                continue;
            };
            let Ok(mut doc) = Document::open(&path) else {
                continue;
            };
            doc.cursor = doc.offset(file.caret.0, file.caret.1);
            if index <= snapshot.active {
                active = buffers.len();
            }
            let top = file.top.min(doc.starts.len().saturating_sub(1));
            let left = if file.left == 0 {
                0
            } else {
                let width = doc
                    .text
                    .split('\n')
                    .map(|line| display_width(line, 0))
                    .max()
                    .unwrap_or(0);
                file.left.min(width)
            };
            let mut buffer = Buffer::new(doc);
            buffer.scroll.snap(top, 0.);
            buffer.columns.snap(left, 0.);
            buffer.markdown.mode = file.markdown.0;
            buffer
                .markdown
                .scroll
                .snap(file.markdown.1.min(i32::MAX as usize), 0.);
            buffers.push(buffer);
        }
        let skipped = total - buffers.len();
        let mut editor = Self::new(Document::new(""));
        if !buffers.is_empty() {
            editor.buffers = buffers;
            editor.active = active;
        }
        (editor, skipped)
    }
}
