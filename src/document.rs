//! UTF-8 text, grapheme-safe coordinates, undo history and atomic file writes.
use anyhow::{Context, Result, bail};
use std::{
    fs,
    io::Write,
    ops::Range,
    path::{Path, PathBuf},
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
    /// Last successfully read or written content, for exact dirty detection.
    saved: String,
    /// Preserve CRLF when a CRLF file is saved.
    crlf: bool,
    /// Preserve an existing UTF-8 BOM.
    bom: bool,
    /// Undo checkpoints, bounded by count and approximate retained text size.
    undo: Vec<Snapshot>,
    /// Checkpoints available to redo.
    redo: Vec<Snapshot>,
}

impl Document {
    /// Create an untitled document containing UTF-8 `text` without touching the filesystem.
    pub fn new(text: &str) -> Self {
        let mut doc = Self {
            path: None,
            text: text.into(),
            cursor: 0,
            starts: Vec::new(),
            revision: 0,
            saved: text.into(),
            crlf: false,
            bom: false,
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
            bail!("不是文件：{}", path.display());
        }
        if metadata.len() > 16 * 1024 * 1024 {
            bail!("基础框架暂支持 16 MiB 以内的文本文件");
        }
        let bytes = fs::read(path).with_context(|| format!("无法读取 {}", path.display()))?;
        let bom = bytes.starts_with(&[0xef, 0xbb, 0xbf]);
        let text = String::from_utf8(bytes).context("当前仅支持 UTF-8 文本文件")?;
        if text.contains('\0') {
            bail!("当前文件包含二进制内容");
        }
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        let crlf = text.contains("\r\n");
        let mut doc = Self::new(&text.replace("\r\n", "\n"));
        doc.path = Some(fs::canonicalize(path)?);
        doc.crlf = crlf;
        doc.bom = bom;
        Ok(doc)
    }

    /// Whether in-memory content differs from the last successful load/save.
    pub fn modified(&self) -> bool {
        self.text != self.saved
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
        self.cursor = snapshot.cursor;
        self.revision += 1;
        self.reindex();
    }

    /// Atomically save to `destination`, or to the current filename when it is None.
    /// An unsuccessful write leaves the original file and dirty state intact.
    pub fn save(&mut self, destination: Option<&Path>) -> Result<()> {
        let path = destination
            .or(self.path.as_deref())
            .context("没有文件名，请使用 :w 文件名")?
            .to_path_buf();
        let path = if path.exists() {
            fs::canonicalize(&path)?
        } else {
            path
        };
        let parent = path.parent().context("无法确定保存目录")?;
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
                    bail!("文件为只读：{}", path.display());
                }
                fs::set_permissions(&temporary, metadata.permissions())?;
            }
            fs::rename(&temporary, &path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result.with_context(|| format!("保存失败：{}", path.display()))?;
        self.path = Some(path);
        self.saved = self.text.clone();
        Ok(())
    }
}
