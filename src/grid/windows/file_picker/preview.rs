//! Bounded filesystem previews and the optional language highlighter.

use super::{PickerEntry, list};
use crate::{grid::render::Frame, ui::text::TextSpacing};
use gpui::{Global, Keystroke};
use std::{
    fs::{self, File},
    io::{self, Read},
    ops::Range,
    path::Path,
    sync::Arc,
};

/// Maximum bytes read from a file, plus one byte for detecting truncation.
const MAX_BYTES: usize = 256 * 1024;
/// Maximum rows retained per text preview.
const MAX_LINES: usize = 1024;

/// Optional language service returning byte ranges and RGB colors for a text snapshot.
/// The plugin host can register it as an application global when language modules are available.
#[derive(Clone)]
pub(crate) struct PreviewHighlighter(pub Arc<Highlight>);

/// Read-only syntax callback for an absolute file path and normalized UTF-8 contents.
type Highlight = dyn Fn(&Path, &str) -> Vec<(Range<usize>, u32)> + Send + Sync;

impl Global for PreviewHighlighter {}

/// Host bridge for providers of interactive non-text previews.
#[derive(Clone)]
pub(crate) struct FilePreviewProvider(pub Arc<OpenPreview>);

/// Open a provider session for an absolute path off the UI thread.
type OpenPreview = dyn Fn(&Path) -> anyhow::Result<Option<Box<dyn PluginPreview>>> + Send + Sync;

impl Global for FilePreviewProvider {}

/// An action and its key caption as displayed by the picker.
pub(crate) struct PreviewShortcut {
    /// Localized operation name supplied by the provider.
    pub action: String,
    /// Key caption displayed next to the operation.
    pub key: String,
}

/// Read-only rendering and input supplied by the provider for one file.
pub(crate) trait PluginPreview: Send {
    /// Draw the visible content in a grid of `columns` by `rows`, using the host's text `spacing`.
    fn frame(&self, columns: usize, rows: usize, spacing: &TextSpacing) -> Frame;
    /// Return the actions currently available in this preview.
    fn shortcuts(&self) -> &[PreviewShortcut];
    /// Handle `key`, returning whether it belongs to a provider action.
    fn key(&mut self, key: &Keystroke) -> bool;
}

/// Sorted UTF-8 ranges and ink supplied by a language service.
#[derive(Default)]
pub(in crate::grid) struct SyntaxColors(Vec<(Range<usize>, u32)>);

impl SyntaxColors {
    /// Retain valid ranges in `text` from the provider's `colors`.
    pub fn new(text: &str, mut colors: Vec<(Range<usize>, u32)>) -> Self {
        colors.retain(|(range, _)| {
            range.start < range.end
                && text.is_char_boundary(range.start)
                && text.is_char_boundary(range.end)
        });
        colors.sort_by_key(|(range, _)| range.start);
        Self(colors)
    }

    /// Find the ink of the latest range covering byte `offset`.
    pub fn at(&self, offset: usize) -> Option<u32> {
        let end = self.0.partition_point(|(range, _)| range.start <= offset);
        self.0
            .get(end.checked_sub(1)?)
            .filter(|(range, _)| range.contains(&offset))
            .map(|(_, color)| *color)
    }
}

/// One read-only preview for the selected row.
#[derive(Default)]
pub(in crate::grid) enum Preview {
    /// No selected row.
    #[default]
    Empty,
    /// Waiting for a background read.
    Loading,
    /// One layer of directory entries.
    Directory(Vec<PickerEntry>),
    /// Decoded UTF-8 text, optionally colored by the language service.
    Text(TextPreview),
    /// Non-text content or a special filesystem entry.
    Unavailable,
    /// File or directory read failed.
    Failed(String),
}

/// Normalized preview text with line starts and optional byte-indexed colors.
pub(in crate::grid) struct TextPreview {
    /// Read-only snapshot shared with the syntax worker.
    pub text: Arc<str>,
    /// Whether the preview ends before the document.
    pub truncated: bool,
    /// Starts of retained lines, in bytes.
    starts: Vec<usize>,
    /// Sorted spans supplied by the language service.
    colors: SyntaxColors,
}

impl Preview {
    /// File extensions reserved for the system's associated program.
    pub fn requires_system_open(path: &Path) -> bool {
        path.extension().is_some_and(|extension| {
            ["exe", "dll", "glb"]
                .iter()
                .any(|kind| extension.eq_ignore_ascii_case(kind))
        })
    }

    /// Read `path` as bounded text or, when `directory`, list children matching `extensions`.
    pub fn read(path: &Path, directory: bool, extensions: &[&str]) -> Self {
        if directory {
            return match list(path, extensions) {
                Ok(entries) => Self::Directory(entries),
                Err(error) => Self::Failed(format!("{error:#}")),
            };
        }
        match read_text(path) {
            Ok(Some(text)) => Self::Text(text),
            Ok(None) => Self::Unavailable,
            Err(error) => Self::Failed(error.to_string()),
        }
    }

    /// Number of rows available for scrolling in the right pane.
    pub fn rows(&self) -> usize {
        match self {
            Self::Directory(entries) => entries.len(),
            Self::Text(text) => text.starts.len(),
            _ => 0,
        }
    }
}

impl TextPreview {
    /// Return the text of zero-based `row` and its byte offset in the snapshot.
    pub fn line(&self, row: usize) -> Option<(usize, &str)> {
        let start = *self.starts.get(row)?;
        let end = self.starts.get(row + 1).copied().unwrap_or(self.text.len());
        Some((start, self.text[start..end].trim_end_matches('\n')))
    }

    /// Accept syntax `colors` that lie on UTF-8 boundaries inside this snapshot.
    pub fn set_colors(&mut self, colors: Vec<(Range<usize>, u32)>) {
        self.colors = SyntaxColors::new(&self.text, colors);
    }

    /// Return the syntax color covering byte `offset`, if supplied by a provider.
    pub fn color(&self, offset: usize) -> Option<u32> {
        self.colors.at(offset)
    }
}

/// Decode a bounded regular file at `path`, returning None for unsupported content.
fn read_text(path: &Path) -> io::Result<Option<TextPreview>> {
    if !fs::metadata(path)?.is_file() || Preview::requires_system_open(path) {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    File::open(path)?
        .take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    let mut truncated = bytes.len() > MAX_BYTES;
    bytes.truncate(MAX_BYTES);
    if bytes.contains(&0) {
        return Ok(None);
    }
    let decoded = match std::str::from_utf8(&bytes) {
        Ok(text) => text,
        Err(error) if truncated && error.error_len().is_none() => {
            std::str::from_utf8(&bytes[..error.valid_up_to()]).expect("validated UTF-8 prefix")
        }
        Err(_) => return Ok(None),
    };
    let mut text = decoded.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    let mut starts = vec![0];
    for (newline, _) in text.match_indices('\n') {
        if starts.len() == MAX_LINES {
            text.truncate(newline);
            truncated = true;
            break;
        }
        starts.push(newline + 1);
    }
    Ok(Some(TextPreview {
        text: text.into(),
        truncated,
        starts,
        colors: SyntaxColors::default(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A UTF-8 sequence split by the byte cap stays previewable.
    #[test]
    fn preview_keeps_a_valid_utf8_prefix_and_classifies_binary_files() {
        let directory = tempfile::tempdir().unwrap();
        let text = directory.path().join("long.txt");
        let mut bytes = vec![b'a'; MAX_BYTES - 1];
        bytes.extend_from_slice("中".as_bytes());
        fs::write(&text, bytes).unwrap();
        let Preview::Text(text) = Preview::read(&text, false, &[]) else {
            panic!("expected text preview")
        };
        assert!(text.truncated);
        assert_eq!(text.text.len(), MAX_BYTES - 1);
        let binary = directory.path().join("model.GLB");
        fs::write(&binary, b"glTF\0\0").unwrap();
        assert!(matches!(
            Preview::read(&binary, false, &[]),
            Preview::Unavailable
        ));
    }

    /// The highlighter receives normalized text and its spans must respect UTF-8 boundaries.
    #[test]
    fn preview_applies_only_valid_syntax_spans() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("example.rs");
        fs::write(&path, "\u{feff}let 中\r\n").unwrap();
        let Preview::Text(mut text) = Preview::read(&path, false, &[]) else {
            panic!("expected text preview")
        };
        assert_eq!(text.line(0), Some((0, "let 中")));
        text.set_colors(vec![(0..3, 0xff), (5..7, 0x99), (7..100, 0x22)]);
        assert_eq!(text.color(0), Some(0xff));
        assert_eq!(text.color(4), None);
    }
}
