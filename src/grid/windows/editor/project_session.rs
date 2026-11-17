//! Save an editor pane's file labels and view positions without retaining document text.

use super::{Buffer, EditorWindow, MarkdownViewMode, markdown::is_markdown};
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
    /// UTF-8 byte within `top` when the viewport starts at a wrapped continuation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    top_byte: Option<usize>,
    /// Horizontal offset in grid columns.
    left: usize,
    /// Markdown presentation and preview scroll offset in logical pixels.
    markdown: (MarkdownViewMode, usize),
}

impl EditorWindow {
    /// Resolve a view's saved top row in document coordinates for a portable Ex script.
    pub(in crate::grid) fn ex_view_top_row(&self) -> usize {
        viewport_anchor(self.buffer(), &self.composition.text).0
    }

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
            let composition = if index == self.active {
                self.composition.text.as_str()
            } else {
                ""
            };
            let (top, top_byte) = viewport_anchor(buffer, composition);
            files.push(FileView {
                path: path.strip_prefix(root).unwrap_or(path).to_owned(),
                caret: doc.location(doc.cursor),
                top,
                top_byte,
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
            if is_markdown(&path) {
                let line = buffer.vim.doc.line(top);
                buffer.restored_wrap_byte = file
                    .top_byte
                    .map(|byte| line.floor_char_boundary(byte.min(line.len())));
            }
            buffer
                .markdown
                .scroll
                .snap(file.markdown.1.min(i32::MAX as usize), 0.);
            buffers.push(buffer);
        }
        let skipped = total - buffers.len();
        let mut editor = Self::new(Document::new(""));
        if !buffers.is_empty() {
            for buffer in &mut buffers {
                buffer.vim.set_jump_history(editor.jump_history.clone());
            }
            editor.buffers = buffers;
            editor.active = active;
        }
        (editor, skipped)
    }
}

/// Resolve the top row of `buffer` to a source line and byte, excluding any native `composition`.
fn viewport_anchor(buffer: &Buffer, composition: &str) -> (usize, Option<usize>) {
    let rows = buffer.retained_visual_rows();
    let top = buffer.scroll.target().min(rows.len().saturating_sub(1));
    let Some(row) = rows.row(top) else {
        return (0, None);
    };
    let Some(segment) = row.segment else {
        return (row.line, buffer.restored_wrap_byte);
    };
    let mut byte = segment.bytes.start;
    if let Some(preedit) = buffer.displayed_line(row.line, composition).1 {
        byte -= byte.saturating_sub(preedit.start).min(preedit.len());
    }
    let line = buffer.vim.doc.line(row.line);
    let byte = line.floor_char_boundary(byte.min(line.len()));
    (row.line, (byte > 0).then_some(byte))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::text::TextSpacing;
    use gpui::{FontId, NoopTextSystem};
    use std::sync::Arc;

    /// Persist the source position of a continuation and read sessions saved before soft wrapping.
    #[test]
    fn markdown_session_keeps_the_top_continuation_as_a_source_byte() {
        let project = tempfile::tempdir().unwrap();
        let root = project.path().canonicalize().unwrap();
        let path = root.join("wrapped.md");
        std::fs::write(&path, "lead\na中bcdefghijk\nnext").unwrap();
        let mut editor = EditorWindow::new(Document::open(&path).unwrap());
        assert!(editor.set_markdown_view_mode(MarkdownViewMode::Edit));
        let spacing = TextSpacing::new(Arc::new(NoopTextSystem::new()), FontId(0), 16., 6.4);
        editor.visual_rows(6, &spacing);
        editor.buffers[0].scroll.snap(2, 0.);

        let snapshot = editor.snapshot_for_project(&root);
        assert_eq!(snapshot.files[0].top, 1);
        assert_eq!(snapshot.files[0].top_byte, Some("a中bcd".len()));
        let mut prior_format = serde_json::to_value(&snapshot).unwrap();
        prior_format["files"][0]
            .as_object_mut()
            .unwrap()
            .remove("top_byte");
        let prior: EditorSnapshot = serde_json::from_value(prior_format).unwrap();
        assert_eq!(prior.files[0].top_byte, None);

        let (mut restored, skipped) = EditorWindow::restore_project_snapshot(snapshot, &root);
        assert_eq!(skipped, 0);
        assert_eq!(restored.buffer().scroll.target(), 1);
        assert_eq!(restored.buffer().restored_wrap_byte, Some("a中bcd".len()));
        let continuation = restored.vim().doc.starts[1] + "a中bcd".len();
        restored.buffers[0].vim.set_cursor(continuation);
        restored.track_view(
            crate::grid::render::Rect {
                w: 12,
                h: 3,
                ..Default::default()
            },
            true,
            &spacing,
            [nanobug::animation::scroll::ScrollInput::Reveal; 2],
            0.,
        );
        assert_eq!(restored.buffer().scroll.target(), 2);
        assert_eq!(restored.buffer().restored_wrap_byte, None);
    }
}
