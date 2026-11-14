//! UTF-8 text, grapheme-safe coordinates, undo history and atomic file writes.
mod folds;
mod history;

use crate::i18n;
use anyhow::{Context, Result, bail};
use folds::Folds;
pub(crate) use folds::{FoldChange, FoldEdge, FoldLevel};
use history::{History, Snapshot};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    ops::Range,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Display width of `text` starting at display column `column`, with four-column tab stops.
pub fn display_width(text: &str, column: usize) -> usize {
    text.graphemes(true).fold(column, |x, ch| {
        x + if ch == "\t" {
            4 - x % 4
        } else {
            ch.width().max(1)
        }
    }) - column
}

/// The only mutable owner of a file's text; drawing caches are derived from it.
pub struct Document {
    /// Identity of this in-memory opening, even when the same path is opened again.
    pub opening: u64,
    /// Absolute save path; None identifies an untitled buffer.
    pub path: Option<PathBuf>,
    /// Text with LF internal newlines and original tabs retained.
    pub text: String,
    /// Cursor at a UTF-8 grapheme boundary, measured in bytes.
    pub cursor: usize,
    /// Starting byte of every line, including a final empty line after LF.
    pub starts: Vec<usize>,
    /// Incremented for every edit or undo, used to invalidate derived text layouts.
    pub revision: u64,
    /// Retained fold definitions and their visible line projection.
    folds: Folds,
    /// Last successfully read or written content, for exact dirty detection.
    saved: String,
    /// Preserve CRLF when a CRLF file is saved.
    crlf: bool,
    /// Preserve an existing UTF-8 BOM.
    bom: bool,
    /// Only buffers created by a plugin edit plan carry an exact disk save guard.
    disk_guard: Option<([u8; 32], u64)>,
    /// Bounded chronological history and the active undo/redo branch.
    history: History,
}

impl Document {
    /// Create an untitled document containing UTF-8 `text` without touching the filesystem.
    pub fn new(text: &str) -> Self {
        static NEXT_OPENING: AtomicU64 = AtomicU64::new(1);
        let mut doc = Self {
            opening: NEXT_OPENING.fetch_add(1, Ordering::Relaxed),
            path: None,
            text: text.into(),
            cursor: 0,
            starts: Vec::new(),
            revision: 0,
            folds: Folds::default(),
            saved: text.into(),
            crlf: false,
            bom: false,
            disk_guard: None,
            history: History::default(),
        };
        doc.reindex();
        doc
    }

    /// Open `path` as UTF-8, or prepare a named empty buffer if it does not exist yet.
    pub fn open(path: &Path) -> Result<Self> {
        if !path.exists() {
            let mut doc = Self::new("");
            doc.path = Some(path.to_path_buf());
            return Ok(doc);
        }
        let metadata = fs::metadata(path)?;
        if !metadata.is_file() {
            bail!(i18n::format(
                "document.not_a_file",
                &[("path", &path.display())]
            ));
        }
        if metadata.len() > 16 * 1024 * 1024 {
            bail!(i18n::text("document.file_too_large"));
        }
        let bytes = fs::read(path)
            .with_context(|| i18n::format("document.read_failed", &[("path", &path.display())]))?;
        Self::from_file_bytes(fs::canonicalize(path)?, bytes, false)
    }

    /// Open already checked UTF-8 bytes while preserving the original on-disk encoding.
    pub(crate) fn from_file_bytes(path: PathBuf, bytes: Vec<u8>, guard_save: bool) -> Result<Self> {
        let disk_guard = guard_save.then(|| (Sha256::digest(&bytes).into(), bytes.len() as u64));
        let bom = bytes.starts_with(&[0xef, 0xbb, 0xbf]);
        let text = String::from_utf8(bytes).context(i18n::text("document.requires_utf8"))?;
        if text.contains('\0') {
            bail!(i18n::text("document.binary_file"));
        }
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        let crlf = text.contains("\r\n");
        let mut doc = Self::new(&text.replace("\r\n", "\n"));
        doc.path = Some(path);
        doc.crlf = crlf;
        doc.bom = bom;
        doc.disk_guard = disk_guard;
        Ok(doc)
    }

    /// Whether in-memory content differs from the last successful load/save.
    pub fn modified(&self) -> bool {
        self.text != self.saved
    }

    /// Build a fresh document from the last loaded or saved text, retaining its path and encoding.
    pub fn saved_version(&self) -> Self {
        let mut doc = Self::new(&self.saved);
        doc.path.clone_from(&self.path);
        doc.crlf = self.crlf;
        doc.bom = self.bom;
        doc.disk_guard = self.disk_guard;
        doc
    }

    /// Rebuild line starts after a text mutation.
    fn reindex(&mut self) {
        self.starts = std::iter::once(0)
            .chain(self.text.match_indices('\n').map(|(i, _)| i + 1))
            .collect();
        self.cursor = self.cursor.min(self.text.len());
    }

    /// Byte interval of `row`, excluding its line separator.
    pub fn line_range(&self, row: usize) -> Range<usize> {
        let start = *self.starts.get(row).unwrap_or(&self.text.len());
        start
            ..self
                .starts
                .get(row + 1)
                .map_or(self.text.len(), |n| n - 1)
                .max(start)
    }

    /// Text of `row`, excluding its separator.
    pub fn line(&self, row: usize) -> &str {
        &self.text[self.line_range(row)]
    }

    /// Source offsets of the braces represented on the visible folded header at `row`.
    pub fn folded_braces(&self, row: usize) -> Option<(usize, usize)> {
        let end_row = self.folded_end_row(row)?;
        let header = self.line(row).trim_end().strip_suffix('{')?;
        let closing_line = self.line(end_row);
        let closing = closing_line.trim_start();
        closing.strip_prefix('}')?;
        Some((
            self.starts[row] + header.len(),
            self.starts[end_row] + closing_line.len() - closing.len(),
        ))
    }

    /// Resolve grapheme-aligned byte `offset` to a line number and a display column.
    pub fn location(&self, offset: usize) -> (usize, usize) {
        let offset = offset.min(self.text.len());
        let row = self
            .starts
            .partition_point(|n| *n <= offset)
            .saturating_sub(1);
        (row, display_width(&self.text[self.starts[row]..offset], 0))
    }

    /// Resolve display `column` on `row` to the preceding grapheme boundary.
    pub fn offset(&self, row: usize, column: usize) -> usize {
        let row = row.min(self.starts.len() - 1);
        let mut x = 0;
        for (byte, ch) in self.line(row).grapheme_indices(true) {
            let width = display_width(ch, x);
            if x + width > column {
                return self.starts[row] + byte;
            }
            x += width;
        }
        self.line_range(row).end
    }

    /// Previous grapheme boundary before `offset`, including line breaks.
    pub fn previous(&self, offset: usize) -> usize {
        self.text[..offset]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(i, _)| i)
    }

    /// Next grapheme boundary after `offset`, including line breaks.
    pub fn next(&self, offset: usize) -> usize {
        offset
            + self.text[offset..]
                .graphemes(true)
                .next()
                .map_or(0, str::len)
    }

    /// Save a pre-change snapshot and open a new branch for the following edit.
    pub fn checkpoint(&mut self) {
        self.history.checkpoint(&self.text, self.cursor);
    }

    /// Replace valid byte `range` with `text`; optionally start a new undo transaction.
    pub fn replace(&mut self, range: Range<usize>, text: &str, checkpoint: bool) {
        if self.text[range.clone()] == *text {
            return;
        }
        if checkpoint {
            self.checkpoint();
        }
        self.history.clear_redo();
        self.shift_folds(&range, text);
        self.text.replace_range(range.clone(), text);
        self.cursor = range.start + text.len();
        self.revision += 1;
        self.reindex();
    }

    /// Restore a checkpoint from undo, or from redo when `redo` is true.
    pub fn undo(&mut self, redo: bool) {
        if let Some(target) = self.history.branch(redo, &self.text) {
            self.restore_history(target);
        }
    }

    /// Restore the `count`th chronological state in the requested direction, across branches.
    pub(crate) fn undo_chronological(&mut self, forward: bool, count: usize) {
        if let Some(target) = self.history.chronological(forward, count) {
            self.restore_history(target);
        }
    }

    /// Restore the retained `target` while updating the text's derived geometry.
    fn restore_history(&mut self, target: u64) {
        let current = Snapshot {
            text: self.text.clone(),
            cursor: self.cursor,
        };
        let snapshot = self.history.visit(target, current);
        self.restore_folds(&snapshot.text);
        self.text = snapshot.text;
        self.cursor = snapshot.cursor;
        self.revision += 1;
        self.reindex();
    }

    /// Atomically save to `destination`, or to the current filename when it is None.
    /// An unsuccessful write leaves the original file and dirty state intact.
    pub fn save(&mut self, destination: Option<&Path>) -> Result<()> {
        let requested = destination
            .or(self.path.as_deref())
            .context(i18n::text("document.missing_filename"))?;
        let path = if requested.exists() {
            fs::canonicalize(requested)?
        } else {
            requested.to_path_buf()
        };
        let guard = self.disk_guard.filter(|_| {
            destination.is_none()
                || self
                    .path
                    .as_ref()
                    .is_some_and(|prior| prior == &path || prior == requested)
        });
        if guard.is_some() && self.path.as_ref() != Some(&path) {
            bail!("The file location changed on disk. Reload it or save a copy.");
        }
        let parent = path
            .parent()
            .context(i18n::text("document.missing_save_directory"))?;
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let temporary = parent.join(format!(".nanobug-{}-{nonce}.tmp", std::process::id()));
        let result = (|| -> Result<()> {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            if self.bom {
                file.write_all(&[0xef, 0xbb, 0xbf])?;
            }
            let text = if self.crlf {
                self.text.replace('\n', "\r\n")
            } else {
                self.text.clone()
            };
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
            drop(file);
            if let Ok(metadata) = fs::metadata(&path) {
                if metadata.permissions().readonly() {
                    bail!(i18n::format(
                        "document.read_only",
                        &[("path", &path.display())]
                    ));
                }
                fs::set_permissions(&temporary, metadata.permissions())?;
            }
            if let Some((digest, length)) = guard {
                let file = fs::File::open(&path)
                    .context("The file changed on disk. Reload it or save a copy.")?;
                let mut current = Vec::new();
                file.take(length + 1).read_to_end(&mut current)?;
                let actual: [u8; 32] = Sha256::digest(&current).into();
                if current.len() as u64 != length || actual != digest {
                    bail!("The file changed on disk. Reload it or save a copy.");
                }
            }
            fs::rename(&temporary, &path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
            .with_context(|| i18n::format("document.save_failed", &[("path", &path.display())]))?;
        self.path = Some(path);
        self.saved = self.text.clone();
        self.disk_guard = None;
        Ok(())
    }
}
