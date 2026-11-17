//! Global A–Z marks for a single project Session.

use crate::document::{Document, DocumentAnchor};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

/// One file mark saved with the project's layout and editor references.
#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct FileMarkSnapshot {
    /// Uppercase letter selected with `m`.
    name: char,
    /// Named file containing the mark, possibly outside the project directory.
    path: PathBuf,
    /// Zero-based logical row and byte column.
    location: (usize, usize),
}

/// The file mark bank shared by all Tabpages and splits of one project.
#[derive(Clone, Default)]
pub struct FileMarks(Arc<Mutex<BTreeMap<char, Mark>>>);

/// A mark stays attached to the shared text while its file has a retained buffer.
enum Mark {
    /// Position updated by a shared buffer, retaining its last location after close.
    Live(DocumentAnchor),
    /// Location restored from disk before the buffer is available.
    Saved(FileMarkSnapshot),
}

/// A resolved file mark with a path fallback if its original opener is gone.
pub(super) struct FileMark {
    /// Current file backing the target buffer.
    pub(super) path: Option<PathBuf>,
    /// Retained buffer identity, or zero for a restored mark.
    pub(super) opening: u64,
    /// Stable buffer number and byte offset for an open buffer.
    live: Option<(u64, usize)>,
    /// Logical row and UTF-8 byte column.
    pub(super) location: (usize, usize),
}

impl Mark {
    /// Resolve the latest coordinates to use in a jump or a session snapshot.
    fn position(&self) -> FileMark {
        match self {
            Self::Live(anchor) => {
                let position = anchor.position();
                FileMark {
                    path: position.path,
                    opening: position.number,
                    live: Some((position.number, position.offset)),
                    location: position.location,
                }
            }
            Self::Saved(saved) => FileMark {
                path: Some(saved.path.clone()),
                opening: 0,
                live: None,
                location: saved.location,
            },
        }
    }
}

impl FileMarks {
    /// Clear marks that belonged to a wiped buffer or its backing file.
    pub fn forget_buffer(&self, number: u64, path: Option<&Path>) {
        self.0.lock().expect("file marks").retain(|_, mark| {
            let position = mark.position();
            position.opening != number
                && path.is_none_or(|path| position.path.as_deref() != Some(path))
        });
    }

    /// Take stable, path-based snapshots of this Session's file marks for persistence.
    pub fn snapshot(&self) -> Vec<FileMarkSnapshot> {
        self.list()
            .into_iter()
            .filter_map(|(name, mark)| {
                Some(FileMarkSnapshot {
                    name,
                    path: mark.path?,
                    location: mark.location,
                })
            })
            .collect()
    }

    /// Recreate this Session's marks from saved `positions` before reopening the files.
    pub fn restore(positions: Vec<FileMarkSnapshot>) -> Self {
        Self(Arc::new(Mutex::new(
            positions
                .into_iter()
                .filter(|mark| mark.name.is_ascii_uppercase() && !mark.path.as_os_str().is_empty())
                .map(|mark| (mark.name, Mark::Saved(mark)))
                .collect(),
        )))
    }

    /// Attach matching saved coordinates to `doc` when its buffer is opened.
    pub(super) fn bind(&self, doc: &Document) {
        let Some(path) = doc.path.as_ref() else {
            return;
        };
        for mark in self.0.lock().expect("file marks").values_mut() {
            let position = mark.position();
            if position.path.as_ref() == Some(path)
                && position
                    .live
                    .is_none_or(|(number, _)| number != doc.buffer_number())
            {
                *mark = Mark::Live(doc.anchor(file_offset(doc, position.location)));
            }
        }
    }

    /// Store `name` at the current caret in `doc`.
    pub(super) fn set(&self, name: char, doc: &Document) {
        self.0
            .lock()
            .expect("file marks")
            .insert(name, Mark::Live(doc.anchor(doc.cursor)));
    }

    /// Resolve `name` in this Session without holding the mark bank during a jump.
    pub(super) fn get(&self, name: char) -> Option<FileMark> {
        self.0
            .lock()
            .expect("file marks")
            .get(&name)
            .map(Mark::position)
    }

    /// Delete `name` from the Session's uppercase mark bank.
    pub(super) fn remove(&self, name: char) {
        self.0.lock().expect("file marks").remove(&name);
    }

    /// Return marks in alphabetical order for Ex listing and Session persistence.
    pub(super) fn list(&self) -> Vec<(char, FileMark)> {
        self.0
            .lock()
            .expect("file marks")
            .iter()
            .map(|(&name, mark)| (name, mark.position()))
            .collect()
    }
}

impl FileMark {
    /// Convert the mark to an offset if `doc` displays the marked file.
    pub(super) fn offset_in(&self, doc: &Document) -> Option<usize> {
        if let Some((number, offset)) = self.live
            && number == doc.buffer_number()
        {
            return Some(doc.text.floor_char_boundary(offset.min(doc.text.len())));
        }
        self.path
            .as_ref()
            .filter(|&path| doc.path.as_ref() == Some(path))
            .map(|_| file_offset(doc, self.location))
    }
}

/// Resolve a persisted `(row, column)` of `doc` at a valid UTF-8 byte boundary.
pub(super) fn file_offset(doc: &Document, (row, column): (usize, usize)) -> usize {
    let range = doc.line_range(row.min(doc.starts.len() - 1));
    doc.text
        .floor_char_boundary(range.start + column.min(range.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vim::{Action, Register, Vim};

    /// Verify tracked marks through Vim input, cross-view edits and a saved Session round trip.
    #[test]
    fn uppercase_marks_follow_shared_edits_and_restore_by_file() -> anyhow::Result<()> {
        let project = tempfile::tempdir()?;
        let path = project.path().join("marked.txt");
        std::fs::write(&path, "head\n\t界x\n")?;
        let mut source = Vim::new(Document::open(&path)?);
        let mut peer = Vim::new(source.doc.shared_view());
        let mut source_register = Register::default();
        let mut peer_register = Register::default();
        peer_register.set_file_marks(source_register.file_marks.clone());
        source.doc.cursor = 9;
        source.text("mA", &mut source_register);
        peer.text("ggOnew", &mut peer_register);
        peer.key("Escape", &mut peer_register);
        peer.text("`A", &mut peer_register);
        assert_eq!(peer.doc.cursor, 13);
        peer.text("u`A", &mut peer_register);
        assert_eq!(peer.doc.cursor, 9);

        let snapshot = serde_json::to_string(&peer_register.file_marks.snapshot())?;
        let mut reopened = Register::default();
        reopened.set_file_marks(FileMarks::restore(serde_json::from_str(&snapshot)?));
        let mut other_file = Vim::new(Document::new("other"));
        assert!(matches!(
            other_file.text("`A", &mut reopened),
            Action::OpenFileMark { path: Some(target), row: 1, column: 4, .. }
                if target == path.canonicalize()?
        ));
        assert_eq!(
            other_file.text("`A", &mut Register::default()),
            Action::None
        );
        Ok(())
    }

    /// An unnamed mark is reachable inside the Session and becomes portable when the file is named.
    #[test]
    fn uppercase_unnamed_mark_follows_buffer_until_first_save() -> anyhow::Result<()> {
        let mut source = Vim::new(Document::new("first"));
        let mut register = Register::default();
        let source_number = source.doc.buffer_number();
        source.text("lmA", &mut register);
        let mut other = Vim::new(Document::new(""));
        assert!(matches!(
            other.text("`A", &mut register),
            Action::OpenFileMark { path: None, opening, row: 0, column: 1, .. }
                if opening == source_number
        ));
        assert!(register.file_marks.snapshot().is_empty());
        let project = tempfile::tempdir()?;
        source.doc.save(Some(&project.path().join("first.txt")))?;
        assert_eq!(register.file_marks.snapshot().len(), 1);
        Ok(())
    }
}
