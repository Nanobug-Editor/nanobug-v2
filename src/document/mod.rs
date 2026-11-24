//! UTF-8 text, grapheme-safe coordinates, undo history and atomic file writes.
mod anchor;
mod folds;
mod history;
mod mark_modifiers;
mod recovery;
mod shared;
mod signs;

use crate::i18n;
pub use anchor::DocumentAnchor;
use anyhow::{Context, Result, bail};
use folds::Folds;
pub(crate) use folds::{FoldChange, FoldEdge, FoldLevel};
use history::{History, Snapshot};
pub(crate) use mark_modifiers::MarkEdit;
pub use mark_modifiers::{MarkModifiers, current_mark_modifiers, with_mark_modifiers};
pub use recovery::RecoveryDraft;
use sha2::{Digest, Sha256};
pub use signs::{SignGroup, SignPlacement};
use std::{
    borrow::Cow,
    fs,
    io::{Read, Write},
    ops::Range,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Display width of `text` starting at display column `column`, with four-column tab stops.
pub fn display_width(text: &str, column: usize) -> usize {
    display_width_with_tabstop(text, column, 4)
}

/// Display width of `text` from `column`, with a stop every `tabstop` columns.
pub fn display_width_with_tabstop(text: &str, column: usize, tabstop: usize) -> usize {
    let tabstop = tabstop.max(1);
    text.graphemes(true).fold(column, |x, ch| {
        x + if ch == "\t" {
            tabstop - x % tabstop
        } else {
            ch.width().max(1)
        }
    }) - column
}

/// Disk contents expected independently by each document that opens a named path.
#[derive(Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
enum DiskVersion {
    /// A named buffer for a file that did not exist at open time.
    Missing,
    /// Exact length and fingerprint of the bytes read or written at this path.
    Saved {
        /// Byte length, including any BOM and the original line endings.
        length: u64,
        /// SHA-256 of those exact bytes.
        digest: [u8; 32],
    },
}

/// One editable view of a buffer, with its own caret and folding state.
pub struct Document {
    /// Identity of this view, including when another pane shows the same buffer.
    pub opening: u64,
    /// Cursor at a UTF-8 grapheme boundary, measured in bytes.
    pub cursor: usize,
    /// The contents most recently consumed by this view.
    content: Arc<DocumentContent>,
    /// Buffer identity and most recently published contents.
    shared: Arc<shared::BufferDocument>,
    /// Tab geometry shared by all views of a buffer without changing its text revision.
    tabstop: Arc<AtomicUsize>,
    /// Retained fold definitions and their visible line projection.
    folds: Folds,
}

/// Snapshot of the shared text, undo history, and backing file of a buffer.
#[derive(Clone)]
pub struct DocumentContent {
    /// Absolute save path; None identifies an untitled buffer.
    pub path: Option<PathBuf>,
    /// Text with LF internal newlines and original tabs retained.
    pub text: String,
    /// Starting byte of every line, including a final empty line after LF.
    pub starts: Vec<usize>,
    /// Incremented after edits, undo and metadata changes that invalidate cached results.
    pub revision: u64,
    /// The consecutive interval of metadata-only revisions, independent of disk history.
    metadata_revisions: Option<(u64, u64)>,
    /// Previous byte range and replacement length for the most recent single edit.
    last_replacement: Option<(Range<usize>, usize)>,
    /// Last successfully read or written content, for exact dirty detection.
    saved: Arc<String>,
    /// A revision known to match `saved` without comparing the text again.
    saved_revision: Option<u64>,
    /// Number of successful writes, including saves that did not change the text revision.
    save_generation: u64,
    /// Preserve CRLF when a CRLF file is saved.
    crlf: bool,
    /// Preserve an existing UTF-8 BOM.
    bom: bool,
    /// Baseline for detecting writes by other independent buffers before saving.
    disk_version: Option<DiskVersion>,
    /// Most recent disk state already reported by :checktime.
    noticed_disk_version: Option<DiskVersion>,
    /// Bounded chronological history and the active undo/redo branch.
    history: History,
}

/// Allocate an ID for either an independent buffer or an additional view of one.
fn next_opening() -> u64 {
    static NEXT_OPENING: AtomicU64 = AtomicU64::new(1);
    NEXT_OPENING.fetch_add(1, Ordering::Relaxed)
}

impl Document {
    /// Consume this view and return its current text.
    pub fn into_text(self) -> String {
        Arc::unwrap_or_clone(self.content).text
    }

    /// Describe the retained undo branches for Ex inspection.
    pub fn undo_branches(&self) -> String {
        self.history.listing()
    }

    /// Create an untitled document containing UTF-8 `text` without touching the filesystem.
    pub fn new(text: &str) -> Self {
        let opening = next_opening();
        let content = Arc::new(DocumentContent {
            path: None,
            text: text.into(),
            starts: line_starts(text),
            revision: 0,
            metadata_revisions: None,
            last_replacement: None,
            saved: Arc::new(text.into()),
            saved_revision: Some(0),
            save_generation: 0,
            crlf: false,
            bom: false,
            disk_version: None,
            noticed_disk_version: None,
            history: History::default(),
        });
        Self {
            opening,
            cursor: 0,
            shared: Arc::new(shared::BufferDocument::new(opening)),
            tabstop: Arc::new(AtomicUsize::new(4)),
            content,
            folds: Folds::default(),
        }
    }

    /// Open `path` as UTF-8, or prepare a named empty buffer if it does not exist yet.
    pub fn open(path: &Path) -> Result<Self> {
        let metadata = match fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let mut doc = Self::new("");
                doc.path = Some(path.to_path_buf());
                doc.disk_version = Some(DiskVersion::Missing);
                doc.publish_content();
                return Ok(doc);
            }
            Err(error) => return Err(error.into()),
        };
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
        Self::from_file_bytes(fs::canonicalize(path)?, bytes)
    }

    /// Open already checked UTF-8 bytes while preserving the original on-disk encoding.
    pub(crate) fn from_file_bytes(path: PathBuf, bytes: Vec<u8>) -> Result<Self> {
        let version = DiskVersion::Saved {
            length: bytes.len() as u64,
            digest: Sha256::digest(&bytes).into(),
        };
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
        doc.disk_version = Some(version);
        doc.publish_content();
        Ok(doc)
    }

    /// Whether in-memory content differs from the last successful load/save.
    pub fn modified(&self) -> bool {
        self.text != *self.saved || self.content.recovery_requires_write()
    }

    /// Observe successful buffer writes independently of edits, undo and failed save attempts.
    pub fn save_generation(&self) -> u64 {
        self.save_generation
    }

    /// Mark cached language results stale in every view while preserving text, undo and save state.
    pub fn invalidate_language_dependents(&mut self) {
        self.refresh_shared();
        let before = self.revision;
        self.revision += 1;
        let from = self
            .metadata_revisions
            .filter(|(_, end)| *end == before)
            .map_or(before, |(from, _)| from);
        self.metadata_revisions = Some((from, self.revision));
        self.last_replacement = None;
        if self.saved_revision == Some(before) {
            self.saved_revision = Some(self.revision);
        }
        self.publish_content();
    }

    /// Check whether only metadata changed from `before` to the current revision.
    pub fn metadata_revised_since(&self, before: u64) -> bool {
        self.content.metadata_revised_since(before)
    }

    /// Treat editable scratch text as ephemeral without advancing its undo history.
    pub(crate) fn acknowledge_scratch_edit(&mut self) {
        self.saved = Arc::new(self.text.clone());
        self.saved_revision = Some(self.revision);
        self.history.set_recovery_file(None);
        self.publish_content();
    }

    /// Refresh a generated listing in place, preserving its buffer identity and clean state.
    pub fn refresh_listing(&mut self, text: &str) {
        if self.text != text {
            self.replace(0..self.text.len(), text, false);
            self.acknowledge_scratch_edit();
        }
    }

    /// Build a fresh document from the last loaded or saved text, retaining its path and encoding.
    pub fn saved_version(&self) -> Self {
        let mut doc = Self::new(&self.saved);
        doc.copy_file_state(self);
        doc.publish_content();
        doc
    }

    /// Create a new buffer identity at the same text and cursor, with its own undo history.
    pub fn fork(&self) -> Self {
        let mut doc = Self::new(&self.text);
        doc.copy_file_state(self);
        doc.saved.clone_from(&self.saved);
        doc.saved_revision = (doc.text == *doc.saved).then_some(doc.revision);
        doc.history.set_recovery_file(self.history.recovery_file());
        doc.cursor = self.cursor;
        doc.publish_content();
        doc
    }

    /// Inherit the filename, encoding and disk baseline from `source`.
    fn copy_file_state(&mut self, source: &Self) {
        self.path.clone_from(&source.path);
        self.crlf = source.crlf;
        self.bom = source.bom;
        self.disk_version = source.disk_version;
        self.noticed_disk_version = source.noticed_disk_version;
        self.set_tabstop(source.tabstop());
    }

    /// Assign a new filename without writing to disk or resetting text history.
    /// A newly associated path has no prior write permission if an unrelated file exists there.
    pub fn set_ex_filename(&mut self, path: Option<PathBuf>) {
        self.refresh_shared();
        if self.path != path {
            (self.crlf, self.bom) = self.content.file_format();
            self.path = path;
            self.disk_version = self.path.as_ref().map(|_| DiskVersion::Missing);
            self.history.forget_recovery_files();
            self.noticed_disk_version = None;
            self.relocate_anchors();
            self.publish_content();
        }
    }

    /// Record an external change for :checktime, preserving the original write baseline.
    pub fn check_external_change(&mut self) -> Result<Option<bool>> {
        self.refresh_shared();
        let Some(path) = &self.path else {
            return Ok(None);
        };
        let Some(expected) = self.content.expected_disk_version() else {
            return Ok(None);
        };
        let actual = disk_fingerprint(path)?;
        if actual == expected {
            if self.noticed_disk_version.take().is_some() {
                self.publish_content();
            }
            return Ok(None);
        }
        if self.noticed_disk_version == Some(actual) {
            return Ok(None);
        }
        self.noticed_disk_version = Some(actual);
        self.publish_content();
        Ok(Some(actual == DiskVersion::Missing))
    }

    /// Reread this buffer's file without replacing its Session identity or clearing undo.
    pub fn reload_file(&mut self) -> Result<()> {
        self.refresh_shared();
        let path = self
            .path
            .as_deref()
            .context(i18n::text("document.missing_filename"))?;
        let fresh = Self::open(path)?;
        let (row, column) = self.location(self.cursor);
        if self.text != fresh.text {
            self.replace(0..self.text.len(), &fresh.text, true);
        }
        self.saved = fresh.saved.clone();
        self.saved_revision = Some(self.revision);
        self.disk_version = fresh.disk_version;
        self.history.set_recovery_file(None);
        self.crlf = fresh.crlf;
        self.bom = fresh.bom;
        self.noticed_disk_version = None;
        self.cursor = self.offset(row, column);
        self.publish_content();
        Ok(())
    }

    /// Release an unloaded buffer's contents and undo while keeping its file identity.
    pub fn unload_file_contents(&mut self) {
        let mut unloaded = Self::new("");
        unloaded.set_ex_filename(self.path.clone());
        self.shared.signs.clear();
        self.replace_file_contents(unloaded);
    }

    /// Load an unloaded buffer from disk without recording an initial undo operation.
    pub fn load_file_contents(&mut self) -> Result<()> {
        let path = self
            .path
            .as_deref()
            .context(i18n::text("document.missing_filename"))?;
        self.replace_file_contents(Self::open(path)?);
        Ok(())
    }

    /// Swap contents while retaining the stable Ex number and its tracked file positions.
    fn replace_file_contents(&mut self, fresh: Self) {
        let revision = self.revision + 1;
        let save_generation = self.save_generation;
        self.content = fresh.content;
        self.save_generation = save_generation;
        self.revision = revision;
        self.saved_revision = Some(revision);
        self.folds = fresh.folds;
        self.cursor = 0;
        self.publish_content();
    }

    /// Read the effective tab width for this buffer.
    pub fn tabstop(&self) -> usize {
        self.tabstop.load(Ordering::Relaxed)
    }

    /// Change tab geometry for this buffer and all its views.
    pub fn set_tabstop(&self, columns: usize) {
        self.tabstop.store(columns.max(1), Ordering::Relaxed);
    }

    /// Use the same tab geometry as another view of this buffer.
    pub fn share_tabstop(&mut self, source: &Self) {
        self.tabstop.clone_from(&source.tabstop);
    }

    /// Retain the current geometry when moving a view to an independent buffer.
    pub fn detach_tabstop(&mut self) {
        self.tabstop = Arc::new(AtomicUsize::new(self.tabstop()));
    }

    /// Measure `text` at `column` according to the buffer's tab spacing.
    pub fn display_width(&self, text: &str, column: usize) -> usize {
        display_width_with_tabstop(text, column, self.tabstop())
    }

    /// Rebuild line starts after a text mutation.
    fn reindex(&mut self) {
        self.starts = line_starts(&self.text);
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
        (
            row,
            self.display_width(&self.text[self.starts[row]..offset], 0),
        )
    }

    /// Resolve a zero-based byte in the file's line-ending format to an internal UTF-8 boundary.
    pub(crate) fn position_in_file_bytes(&self, byte: usize) -> usize {
        let offset = if self.crlf {
            // The internal LF represents both CR and LF in a DOS-format document.
            self.starts[1..]
                .iter()
                .enumerate()
                .find_map(|(extra, &next)| {
                    (byte < next.saturating_add(extra + 1))
                        .then(|| byte.saturating_sub(extra).min(next - 1))
                })
                .unwrap_or_else(|| byte.saturating_sub(self.starts.len() - 1))
        } else {
            byte
        };
        self.text.floor_char_boundary(offset.min(self.text.len()))
    }

    /// Resolve display `column` on `row` to the preceding grapheme boundary.
    pub fn offset(&self, row: usize, column: usize) -> usize {
        let row = row.min(self.starts.len() - 1);
        let mut x = 0;
        for (byte, ch) in self.line(row).grapheme_indices(true) {
            let width = self.display_width(ch, x);
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
        self.checkpoint_content();
        self.publish_content();
    }

    /// Record the current text before an edit updates the shared contents.
    fn checkpoint_content(&mut self) {
        let cursor = self.cursor;
        let content = Arc::make_mut(&mut self.content);
        content.history.checkpoint(&content.text, cursor);
    }

    /// Replace valid byte `range` with `text`; optionally start a new undo transaction.
    pub fn replace(&mut self, range: Range<usize>, text: &str, checkpoint: bool) {
        let marks = MarkEdit::new(self, range.clone(), text.len(), None);
        self.replace_tracking_marks(range, text, checkpoint, &marks);
    }

    /// Apply a replacement with the same mark geometry used by the originating Vim view.
    pub(crate) fn replace_tracking_marks(
        &mut self,
        range: Range<usize>,
        text: &str,
        checkpoint: bool,
        marks: &MarkEdit,
    ) {
        if self.text[range.clone()] == *text {
            return;
        }
        if checkpoint {
            self.checkpoint_content();
        }
        let signs = if range == (0..self.text.len()) {
            self.sign_replacement(text)
        } else {
            self.sign_edit(range.clone(), text.len())
        };
        self.history.clear_redo();
        self.shift_folds(&range, text);
        self.text.replace_range(range.clone(), text);
        self.cursor = range.start + text.len();
        self.last_replacement = Some((range, text.len()));
        self.revision += 1;
        self.reindex();
        self.shift_anchors(marks);
        self.shift_signs(signs);
        self.publish_content();
    }

    /// Return the old byte range and new length when one tracked edit follows `revision`.
    pub fn replacement_since(&self, revision: u64) -> Option<(&Range<usize>, usize)> {
        if revision.checked_add(1) != Some(self.revision) {
            return None;
        }
        self.last_replacement
            .as_ref()
            .map(|(range, len)| (range, *len))
    }

    /// Restore a checkpoint from undo, or from redo when `redo` is true.
    pub fn undo(&mut self, redo: bool) {
        self.refresh_shared();
        if let Some(target) = self.history.branch(redo, &self.text) {
            self.restore_history(target);
        }
    }

    /// Restore a retained change `number`, returning false when that state was discarded.
    pub(crate) fn undo_number(&mut self, number: u64) -> bool {
        match self.history.numbered_target(number) {
            Some(true) => self.restore_history(number),
            Some(false) => (),
            None => return false,
        }
        true
    }

    /// Restore the `count`th chronological state in the requested direction, across branches.
    pub(crate) fn undo_chronological(&mut self, forward: bool, count: usize) {
        self.refresh_shared();
        if let Some(target) = self.history.chronological(forward, count) {
            self.restore_history(target);
        }
    }

    /// Move approximately `seconds` older or newer within the shared undo history.
    pub(crate) fn undo_by_time(&mut self, forward: bool, seconds: u64) {
        self.refresh_shared();
        if let Some(target) = self.history.time_target(forward, seconds) {
            self.restore_history(target);
        }
    }

    /// Visit the `count`th retained save point in the chosen direction.
    pub(crate) fn undo_by_write(&mut self, forward: bool, count: usize) {
        self.refresh_shared();
        if let Some(target) = self.history.write_target(forward, count) {
            self.restore_history(target);
        }
    }

    /// Reuse the last undo block for the next edit and share the choice with peer views.
    pub(crate) fn join_undo_block(&mut self) -> bool {
        self.refresh_shared();
        if !self.history.join_next() {
            return false;
        }
        self.publish_content();
        true
    }

    /// Save this buffer's retained undo history in the local archive at `path`.
    pub fn write_undo_file(&mut self, path: &Path, force: bool) -> Result<()> {
        self.refresh_shared();
        self.history.write_file(&self.text, path, force)
    }

    /// Replace the retained undo history from `path` after matching the live text.
    pub fn read_undo_file(&mut self, path: &Path) -> Result<()> {
        self.refresh_shared();
        let recovery = self.history.recovery_file();
        self.history = History::read_file(&self.text, path)?;
        self.history.set_recovery_file(recovery);
        self.publish_content();
        Ok(())
    }

    /// Restore the retained `target` while updating the text's derived geometry.
    fn restore_history(&mut self, target: u64) {
        let current = Snapshot {
            text: Arc::new(self.text.clone()),
            cursor: self.cursor,
        };
        let snapshot = self.history.visit(target, current);
        let replacement = self.anchor_replacement(&snapshot.text);
        let signs = self.sign_replacement(&snapshot.text);
        self.restore_folds(&snapshot.text);
        self.text = Arc::unwrap_or_clone(snapshot.text);
        self.cursor = snapshot.cursor;
        self.last_replacement = None;
        self.revision += 1;
        self.reindex();
        if let Some(marks) = replacement {
            self.shift_anchors(&marks);
        }
        self.shift_signs(signs);
        self.publish_content();
    }

    /// Atomically save to `destination`, or to the current filename when it is None.
    /// An unsuccessful write leaves the original file and dirty state intact.
    pub fn save(&mut self, destination: Option<&Path>) -> Result<()> {
        self.save_to(destination, false)
    }

    /// Save to a vacant `destination` and adopt its filename only after publication succeeds.
    pub fn save_new(&mut self, destination: &Path) -> Result<()> {
        self.save_to(Some(destination), true)
    }

    /// Write the current text to `destination` without changing the buffer's file association.
    /// `overwrite` must be true to replace an existing destination.
    pub fn write_copy(&self, destination: &Path, overwrite: bool) -> Result<()> {
        let content = self.shared.latest().unwrap_or_else(|| self.content.clone());
        let path = write_destination(destination, !overwrite)?;
        write_contents(&content, &path, None, !overwrite)?;
        Ok(())
    }

    /// Save to the requested file, with an atomic vacancy check when `exclusive` is set.
    fn save_to(&mut self, destination: Option<&Path>, exclusive: bool) -> Result<()> {
        self.refresh_shared();
        let requested = destination
            .or(self.path.as_deref())
            .context(i18n::text("document.missing_filename"))?;
        let path = write_destination(requested, exclusive)?;
        let expected = self.content.expected_disk_version().filter(|_| {
            !exclusive
                && (destination.is_none()
                    || self
                        .path
                        .as_ref()
                        .is_some_and(|prior| prior == &path || prior == requested))
        });
        if matches!(expected, Some(DiskVersion::Saved { .. })) && self.path.as_ref() != Some(&path)
        {
            bail!(i18n::text("document.disk_changed"));
        }
        let written = write_contents(&self.content, &path, expected, exclusive)?;
        let new_path = Some(fs::canonicalize(&path).unwrap_or(path));
        let path_changed = self.path != new_path;
        self.path = new_path;
        self.relocate_anchors();
        self.saved = Arc::new(self.text.clone());
        self.saved_revision = Some(self.revision);
        self.history.mark_saved();
        self.save_generation += 1;
        self.disk_version = Some(written);
        (self.crlf, self.bom) = self.content.file_format();
        if path_changed {
            self.history.forget_recovery_files();
        } else {
            self.history.set_recovery_file(None);
        }
        self.noticed_disk_version = None;
        self.publish_content();
        Ok(())
    }
}

/// Resolve existing aliases for ordinary writes and keep the exact entry for exclusive writes.
fn write_destination(path: &Path, exclusive: bool) -> std::io::Result<PathBuf> {
    if !exclusive && path.exists() {
        fs::canonicalize(path)
    } else {
        Ok(path.to_owned())
    }
}

/// Write `content` beside `path` and publish it atomically after applying the requested guards.
fn write_contents(
    content: &DocumentContent,
    path: &Path,
    expected: Option<DiskVersion>,
    exclusive: bool,
) -> Result<DiskVersion> {
    static NEXT_STAGE: AtomicU64 = AtomicU64::new(0);
    let parent = path
        .parent()
        .context(i18n::text("document.missing_save_directory"))?;
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let serial = NEXT_STAGE.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".nanobug-{}-{nonce}-{serial}.tmp",
        std::process::id()
    ));
    let error_context = || i18n::format("document.save_failed", &[("path", &path.display())]);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .with_context(error_context)?;
    let result = (|| -> Result<DiskVersion> {
        let (crlf, bom) = content.file_format();
        let (bom, text) = file_encoding(&content.text, crlf, bom);
        file.write_all(bom)?;
        file.write_all(text.as_bytes())?;
        let mut digest = Sha256::new();
        digest.update(bom);
        digest.update(text.as_bytes());
        let written = DiskVersion::Saved {
            length: (bom.len() + text.len()) as u64,
            digest: digest.finalize().into(),
        };
        file.sync_all()?;
        drop(file);
        if exclusive {
            crate::filesystem::move_exclusive(&temporary, path)?;
        } else {
            if let Ok(metadata) = fs::metadata(path) {
                if metadata.permissions().readonly() {
                    bail!(i18n::format(
                        "document.read_only",
                        &[("path", &path.display())]
                    ));
                }
                fs::set_permissions(&temporary, metadata.permissions())?;
            }
            if let Some(expected) = expected {
                check_disk_version(path, expected, written)?;
            }
            fs::rename(&temporary, path)?;
        }
        Ok(written)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.with_context(error_context)
}

/// Encode internal LF text with the requested line endings and UTF-8 BOM.
fn file_encoding(text: &str, crlf: bool, bom: bool) -> (&'static [u8], Cow<'_, str>) {
    let prefix: &[u8] = if bom { &[0xef, 0xbb, 0xbf] } else { &[] };
    let text = if crlf {
        Cow::Owned(text.replace('\n', "\r\n"))
    } else {
        Cow::Borrowed(text)
    };
    (prefix, text)
}

/// Return the byte offset of every logical line, including a final empty line.
fn line_starts(text: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(text.match_indices('\n').map(|(at, _)| at + 1))
        .collect()
}

/// Require `path` to match the original `expected` bytes or the identical `written` bytes.
fn check_disk_version(path: &Path, expected: DiskVersion, written: DiskVersion) -> Result<()> {
    let disk_changed = || i18n::text("document.disk_changed");
    let size = |version| match version {
        DiskVersion::Missing => 0,
        DiskVersion::Saved { length, .. } => length,
    };
    let actual = match fs::File::open(path) {
        Ok(file) => {
            let mut bytes = Vec::new();
            file.take(size(expected).max(size(written)) + 1)
                .read_to_end(&mut bytes)?;
            DiskVersion::Saved {
                length: bytes.len() as u64,
                digest: Sha256::digest(&bytes).into(),
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => DiskVersion::Missing,
        Err(error) => return Err(error).context(disk_changed()),
    };
    if actual != expected && actual != written {
        bail!(disk_changed());
    }
    Ok(())
}

/// Hash the current contents of `path`, keeping memory bounded by a read block.
fn disk_fingerprint(path: &Path) -> Result<DiskVersion> {
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(DiskVersion::Missing);
        }
        Err(error) => return Err(error.into()),
    };
    let mut digest = Sha256::new();
    let mut length = 0;
    let mut block = [0; 16 * 1024];
    loop {
        let read = file.read(&mut block)?;
        if read == 0 {
            break;
        }
        digest.update(&block[..read]);
        length += read as u64;
    }
    Ok(DiskVersion::Saved {
        length,
        digest: digest.finalize().into(),
    })
}

#[cfg(test)]
mod write_tests {
    use super::*;

    /// A rejected Save As keeps the source file, association, and pending edits.
    #[test]
    fn exclusive_save_as_adopts_only_a_vacant_destination() -> Result<()> {
        let folder = tempfile::tempdir()?;
        let source = folder.path().join("source.txt");
        let destination = folder.path().join("destination.txt");
        fs::write(&source, "saved\n")?;
        fs::write(&destination, "existing\n")?;
        let mut doc = Document::open(&source)?;
        doc.replace(0..5, "edited", true);

        assert!(doc.save_new(&destination).is_err());
        assert_eq!(doc.path, Some(source.canonicalize()?));
        assert!(doc.modified());
        assert_eq!(fs::read_to_string(&destination)?, "existing\n");

        fs::remove_file(&destination)?;
        doc.save_new(&destination)?;
        assert_eq!(doc.path, Some(destination.canonicalize()?));
        assert!(!doc.modified());
        assert_eq!(fs::read_to_string(&destination)?, "edited\n");
        assert_eq!(fs::read_to_string(&source)?, "saved\n");
        Ok(())
    }

    /// One exclusive write-copy wins a race without clearing the original drafts.
    #[test]
    fn exclusive_copies_publish_at_most_one_complete_file() -> Result<()> {
        use std::{sync::Barrier, thread};
        let folder = tempfile::tempdir()?;
        let destination = folder.path().join("copy.txt");
        let ready = Barrier::new(2);
        let outcomes = thread::scope(|scope| {
            let workers = ["first", "second"].map(|text| {
                let destination = &destination;
                let ready = &ready;
                scope.spawn(move || {
                    let mut doc = Document::new("");
                    doc.replace(0..0, text, true);
                    ready.wait();
                    let written = doc.write_copy(destination, false).is_ok();
                    assert!(doc.modified());
                    assert!(doc.path.is_none());
                    (text, written)
                })
            });
            workers.map(|worker| worker.join().expect("copy writer"))
        });
        let winners: Vec<_> = outcomes
            .into_iter()
            .filter_map(|(text, written)| written.then_some(text))
            .collect();
        assert_eq!(winners.len(), 1);
        assert_eq!(fs::read_to_string(&destination)?, winners[0]);
        assert_eq!(folder.path().read_dir()?.count(), 1);
        Ok(())
    }
}
