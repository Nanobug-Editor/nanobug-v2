//! UTF-8 text, grapheme-safe coordinates, undo history and atomic file writes.
use crate::i18n;
use anyhow::{Context, Result, bail};
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

/// One undo checkpoint, recorded once per insert session or normal-mode change.
#[derive(Clone)]
struct Snapshot {
    /// Complete text before the change.
    text: String,
    /// Logical cursor byte offset before the change.
    cursor: usize,
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
    /// Document-row ranges currently hidden by the editor, excluding each fold's visible header.
    hidden_rows: Vec<Range<usize>>,
    /// Last successfully read or written content, for exact dirty detection.
    saved: String,
    /// Preserve CRLF when a CRLF file is saved.
    crlf: bool,
    /// Preserve an existing UTF-8 BOM.
    bom: bool,
    /// Only buffers created by a plugin edit plan carry an exact disk save guard.
    disk_guard: Option<([u8; 32], u64)>,
    /// Undo checkpoints, bounded by count and approximate retained text size.
    undo: Vec<Snapshot>,
    /// Checkpoints available to redo.
    redo: Vec<Snapshot>,
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
            hidden_rows: Vec::new(),
            saved: text.into(),
            crlf: false,
            bom: false,
            disk_guard: None,
            undo: Vec::new(),
            redo: Vec::new(),
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

    /// Close a fold with half-open hidden rows, preserving disjoint folds and their headers.
    pub fn collapse_rows(&mut self, rows: Range<usize>) {
        if rows.start == 0 || rows.start >= rows.end || rows.end > self.starts.len() {
            return;
        }
        self.hidden_rows
            .retain(|fold| fold.end <= rows.start || rows.end <= fold.start);
        let index = self
            .hidden_rows
            .partition_point(|fold| fold.start < rows.start);
        self.hidden_rows.insert(index, rows);
    }

    /// Clear all folds, for example when a language provider is disabled.
    pub fn clear_hidden_rows(&mut self) {
        self.hidden_rows.clear();
    }

    /// Expand the collapsed fold whose header is document row `row`.
    pub fn open_fold_at(&mut self, row: usize) -> bool {
        let before = self.hidden_rows.len();
        self.hidden_rows.retain(|range| range.start != row + 1);
        self.hidden_rows.len() != before
    }

    /// Expand any collapsed fold hiding document `row`.
    pub fn reveal_row(&mut self, row: usize) -> bool {
        let before = self.hidden_rows.len();
        self.hidden_rows.retain(|range| !range.contains(&row));
        self.hidden_rows.len() != before
    }

    /// Convert document row `row` to a display row, mapping hidden lines to their fold header.
    pub fn display_row(&self, row: usize) -> usize {
        let mut hidden = 0;
        for range in &self.hidden_rows {
            if row < range.start {
                break;
            }
            if row < range.end {
                return range.start - 1 - hidden;
            }
            hidden += range.len();
        }
        row - hidden
    }

    /// Convert a display `row` back to its visible document row.
    pub fn document_row(&self, mut row: usize) -> usize {
        for range in &self.hidden_rows {
            if row < range.start {
                break;
            }
            row = row.saturating_add(range.len());
        }
        row
    }

    /// Number of visible document rows after folds have been applied.
    pub fn display_rows(&self) -> usize {
        self.starts.len() - self.hidden_rows.iter().map(Range::len).sum::<usize>()
    }

    /// Whether a collapsed fold starts just below document `row`.
    pub fn folded_at(&self, row: usize) -> bool {
        self.hidden_rows.iter().any(|range| range.start == row + 1)
    }

    /// Keep folds on unaffected rows when changing the byte `range` to `text`.
    fn shift_folds(&mut self, range: &Range<usize>, text: &str) {
        if self.hidden_rows.is_empty() {
            return;
        }
        let first = self.starts.partition_point(|&start| start <= range.start) - 1;
        let last = self.starts.partition_point(|&start| start <= range.end) - 1;
        let delta =
            text.bytes().filter(|&byte| byte == b'\n').count() as isize - (last - first) as isize;
        self.hidden_rows.retain_mut(|hidden| {
            if hidden.end <= first {
                return true;
            }
            if hidden.start.saturating_sub(1) <= last {
                return false;
            }
            hidden.start = hidden.start.saturating_add_signed(delta);
            hidden.end = hidden.end.saturating_add_signed(delta);
            true
        });
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

    /// Save a pre-change snapshot; redo is cleared only when an actual edit follows.
    pub fn checkpoint(&mut self) {
        self.undo.push(Snapshot {
            text: self.text.clone(),
            cursor: self.cursor,
        });
        let mut bytes: usize = self.undo.iter().map(|s| s.text.len()).sum();
        while self.undo.len() > 1 && (self.undo.len() > 128 || bytes > 16 * 1024 * 1024) {
            bytes -= self.undo.remove(0).text.len();
        }
    }

    /// Replace valid byte `range` with `text`; optionally start a new undo transaction.
    pub fn replace(&mut self, range: Range<usize>, text: &str, checkpoint: bool) {
        if self.text[range.clone()] == *text {
            return;
        }
        if checkpoint {
            self.checkpoint();
        }
        self.redo.clear();
        self.shift_folds(&range, text);
        self.text.replace_range(range.clone(), text);
        self.cursor = range.start + text.len();
        self.revision += 1;
        self.reindex();
    }

    /// Restore a checkpoint from undo, or from redo when `redo` is true.
    pub fn undo(&mut self, redo: bool) {
        let stack = if redo { &mut self.redo } else { &mut self.undo };
        let mut restored = stack.pop();
        while restored.as_ref().is_some_and(|s| s.text == self.text) {
            restored = stack.pop();
        }
        let Some(snapshot) = restored else { return };
        let current = Snapshot {
            text: std::mem::take(&mut self.text),
            cursor: self.cursor,
        };
        if redo {
            self.undo.push(current);
        } else {
            self.redo.push(current);
        }
        self.text = snapshot.text;
        self.hidden_rows.clear();
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
