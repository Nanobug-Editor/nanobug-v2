//! Tracked file positions remain current across edits in any buffer view.

use super::Document;
use std::{
    ops::Range,
    path::PathBuf,
    sync::{Arc, Mutex, Weak},
};

/// A persistent file position updated by the buffer while any editable view is alive.
#[derive(Clone)]
pub struct DocumentAnchor(Arc<Mutex<Position>>);

/// An anchored byte offset and its last known disk location.
#[derive(Clone)]
pub struct Position {
    /// Stable buffer number used to recognize other views of the same contents.
    pub number: u64,
    /// Most recent named path for reopening the buffer after close.
    pub path: Option<PathBuf>,
    /// Byte offset in the most recent live text.
    pub offset: usize,
    /// Logical row and UTF-8 byte column for reopening from disk.
    pub location: (usize, usize),
}

impl DocumentAnchor {
    /// Read the latest position without retaining either the buffer or an editor's view.
    pub fn position(&self) -> Position {
        self.0.lock().expect("file anchor").clone()
    }
}

/// Weak references avoid retaining replaced marks or other buffers.
#[derive(Default)]
pub(super) struct Anchors(Mutex<Vec<Weak<Mutex<Position>>>>);

impl Anchors {
    /// Update each retained position through `update`, pruning any freed spots.
    fn update(&self, mut update: impl FnMut(&mut Position)) {
        self.0.lock().expect("file anchors").retain(|entry| {
            let Some(entry) = entry.upgrade() else {
                return false;
            };
            update(&mut entry.lock().expect("file anchor"));
            true
        });
    }

    /// Whether any tracked position still has an owner.
    fn occupied(&self) -> bool {
        self.0
            .lock()
            .expect("file anchors")
            .iter()
            .any(|entry| entry.strong_count() > 0)
    }
}

impl Document {
    /// Track `offset` in this document, including later edits from other views.
    pub fn anchor(&self, offset: usize) -> DocumentAnchor {
        let offset = self.text.floor_char_boundary(offset.min(self.text.len()));
        let entry = Arc::new(Mutex::new(Position {
            number: self.buffer_number(),
            path: self.path.clone(),
            offset,
            location: self.anchor_location(offset),
        }));
        let mut anchors = self.shared.anchors.0.lock().expect("file anchors");
        anchors.retain(|previous| previous.strong_count() > 0);
        anchors.push(Arc::downgrade(&entry));
        DocumentAnchor(entry)
    }

    /// Move anchors after replacing the old byte `range` with `inserted` bytes.
    pub(super) fn shift_anchors(&self, range: &Range<usize>, inserted: usize) {
        self.shared.anchors.update(|position| {
            let offset = if position.offset >= range.end {
                position.offset - range.len() + inserted
            } else if position.offset >= range.start {
                range.start
            } else {
                position.offset
            };
            position.offset = self.text.floor_char_boundary(offset.min(self.text.len()));
            position.location = self.anchor_location(position.offset);
        });
    }

    /// Resolve an anchored `offset` independently of the view's tab geometry.
    fn anchor_location(&self, offset: usize) -> (usize, usize) {
        let row = self.starts.partition_point(|&start| start <= offset) - 1;
        (row, offset - self.starts[row])
    }

    /// Update reopen paths after a successful write or filesystem move.
    pub(super) fn relocate_anchors(&self) {
        self.shared
            .anchors
            .update(|position| position.path.clone_from(&self.path));
    }

    /// Treat a history restore to `text` as a replacement between its unchanged edges.
    pub(super) fn anchor_replacement(&self, text: &str) -> Option<(Range<usize>, usize)> {
        if !self.shared.anchors.occupied() || self.text == text {
            return None;
        }
        let prefix = self
            .text
            .bytes()
            .zip(text.bytes())
            .take_while(|(a, b)| a == b)
            .count();
        let prefix = self.text.floor_char_boundary(prefix);
        let suffix = self.text[prefix..]
            .bytes()
            .rev()
            .zip(text[prefix..].bytes().rev())
            .take_while(|(a, b)| a == b)
            .count();
        let end = self.text.ceil_char_boundary(self.text.len() - suffix);
        let suffix = self.text.len() - end;
        Some((prefix..end, text.len() - suffix - prefix))
    }
}
