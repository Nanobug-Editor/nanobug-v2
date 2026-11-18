//! Keep the cursor history of one Vim window, including positions in other buffers.

use super::{Action, Vim};
use crate::document::{Document, MarkEdit, current_mark_modifiers};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

/// Maximum number of positions retained for one window.
const CAPACITY: usize = 100;

/// One location in an open document, with a pathname and coordinates for reopening it.
#[derive(Clone)]
pub struct JumpLocation {
    /// Identity of the in-memory document when this position was recorded.
    pub opening: u64,
    /// Filename, if this document has been saved.
    pub path: Option<PathBuf>,
    /// Zero-based line and display column used when reopening a file.
    line: (usize, usize),
    /// Byte offset in the tracked revision of the original document.
    offset: usize,
    revision: u64,
}

impl JumpLocation {
    /// Record the cursor in `doc` for later local or cross-file navigation.
    fn at(doc: &Document) -> Self {
        Self {
            opening: doc.opening,
            path: doc.path.clone(),
            line: doc.location(doc.cursor),
            offset: doc.cursor,
            revision: doc.revision,
        }
    }

    /// Resolve this position in the chosen opening or a newly reopened document.
    pub fn offset(&self, doc: &Document) -> usize {
        if (self.opening, self.revision) == (doc.opening, doc.revision) {
            self.offset
        } else {
            doc.offset(self.line.0, self.line.1)
        }
    }

    /// Compare exact coordinates in the same file or unnamed buffer.
    fn same_position(&self, other: &Self) -> bool {
        self.line == other.line
            && (self.opening == other.opening || self.path.is_some() && self.path == other.path)
    }
}

/// Shared by the buffers of a single editor window. Standalone Vim models own their own list.
#[derive(Clone, Default)]
pub struct JumpHistory(Arc<Mutex<Entries>>);

#[derive(Clone, Default)]
struct Entries {
    locations: Vec<JumpLocation>,
    /// Current location, or the slot after the last entry when not browsing the list.
    index: usize,
}

impl Entries {
    /// Move an existing position to the end, keeping the number of entries bounded.
    fn push(&mut self, location: JumpLocation) {
        self.locations
            .retain(|entry| !entry.same_position(&location));
        if self.locations.len() == CAPACITY {
            self.locations.remove(0);
        }
        self.locations.push(location);
        self.index = self.locations.len();
    }
}

impl JumpHistory {
    /// Remove destinations in a wiped file or unnamed buffer, preserving other history entries.
    pub fn forget_buffer(&self, openings: &[u64], path: Option<&Path>) {
        let mut history = self.0.lock().unwrap();
        let prior = history.index;
        let mut visited = 0;
        let mut retained = 0;
        history.locations.retain(|entry| {
            let keep = !openings.contains(&entry.opening)
                && path.is_none_or(|path| entry.path.as_deref() != Some(path));
            retained += usize::from(keep && visited < prior);
            visited += 1;
            keep
        });
        history.index = retained;
    }

    /// Start this split with an independent copy, remapping entries belonging to its forked text.
    pub fn fork(&self, from: &Document, to: &Document) -> Self {
        let mut history = self.0.lock().unwrap().clone();
        if from.path == to.path && from.text == to.text {
            for entry in &mut history.locations {
                if entry.opening == from.opening {
                    entry.offset = entry.offset(from);
                    entry.opening = to.opening;
                    entry.revision = to.revision;
                }
            }
        }
        Self(Arc::new(Mutex::new(history)))
    }

    /// Walk by `count` without committing until `resolve` accepts a destination.
    /// `Err(())` asks the native host to resolve an entry, while `Ok(None)` skips an unavailable one.
    pub fn visit<T>(
        &self,
        doc: &Document,
        forward: bool,
        count: usize,
        mut resolve: impl FnMut(&JumpLocation) -> Result<Option<T>, ()>,
    ) -> Result<Option<T>, ()> {
        let mut history = self.0.lock().unwrap();
        let mut tail = None;
        let mut from = history.index;
        if !forward
            && from == history.locations.len()
            && from != 0
            && !current_mark_modifiers().keep_jumps
        {
            // Save the caret at the first backwards step so forward navigation can return here.
            let mut with_tail = history.clone();
            with_tail.push(JumpLocation::at(doc));
            from = with_tail.locations.len() - 1;
            tail = Some(with_tail);
        }
        let locations = &tail.as_ref().unwrap_or(&history).locations;
        let next = if forward {
            from.checked_add(count)
        } else {
            from.checked_sub(count)
        };
        let Some(mut index) = next else {
            return Ok(None);
        };
        while let Some(location) = locations.get(index) {
            if let Some(destination) = resolve(location)? {
                if let Some(with_tail) = tail {
                    *history = with_tail;
                }
                history.index = index;
                return Ok(Some(destination));
            }
            if forward {
                index += 1;
            } else if index > 0 {
                index -= 1;
            } else {
                break;
            }
        }
        Ok(None)
    }

    /// Rebase retained positions after a replacement, applying the command's mark rules.
    pub(super) fn shift(&self, doc: &Document, edit: &MarkEdit) {
        for entry in &mut self.0.lock().unwrap().locations {
            if entry.opening != doc.opening || entry.revision.checked_add(1) != Some(doc.revision) {
                continue;
            }
            entry.offset = edit.retained_position(doc, entry.offset);
            entry.line = doc.location(entry.offset);
            entry.revision = doc.revision;
        }
    }

    /// Track a saved or renamed document so its positions still work after closing the buffer.
    pub fn update_path(&self, opening: u64, path: Option<&Path>) {
        for entry in &mut self.0.lock().unwrap().locations {
            if entry.opening == opening {
                entry.path = path.map(Path::to_path_buf);
            }
        }
    }

    /// Empty this window's history without affecting other windows.
    pub(super) fn clear(&self) {
        *self.0.lock().unwrap() = Entries::default();
    }

    /// Render the jump distances, positions and filenames for the read-only history view.
    pub(super) fn list(&self, doc: &Document) -> String {
        use std::fmt::Write as _;
        let history = self.0.lock().unwrap();
        let mut list = String::from(crate::i18n::text("vim_g.jump_list_header"));
        for (index, entry) in history.locations.iter().enumerate() {
            let marker = if index == history.index { '>' } else { ' ' };
            let (row, column) = entry.line;
            let label = if doc.opening == entry.opening {
                doc.line(row.min(doc.starts.len() - 1)).trim()
            } else if let Some(path) = &entry.path {
                path.to_str().unwrap_or_default()
            } else {
                crate::i18n::text("editor.untitled")
            };
            let _ = write!(
                list,
                "\n{marker}{:>4} {:>4} {:>4} {label}",
                index.abs_diff(history.index),
                row + 1,
                column
            );
        }
        if history.index == history.locations.len() {
            list.push_str("\n>");
        }
        list
    }
}

impl Vim {
    /// Use the jumplist owned by the editor that presents this buffer.
    pub fn set_jump_history(&mut self, history: JumpHistory) {
        self.jump_history = history;
    }

    /// Record the active cursor before a confirmed jump, and update the previous-context marks.
    pub fn record_jump(&mut self) {
        if current_mark_modifiers().keep_jumps {
            return;
        }
        self.marks.insert('\'', self.doc.cursor);
        self.marks.insert('`', self.doc.cursor);
        self.jump_history
            .0
            .lock()
            .unwrap()
            .push(JumpLocation::at(&self.doc));
    }

    /// Move locally or request the host to restore a position from another buffer.
    pub(super) fn jump_history(&mut self, forward: bool, count: usize) -> Action {
        match self
            .jump_history
            .visit(&self.doc, forward, count, |location| {
                if location.opening == self.doc.opening {
                    Ok(Some(location.offset(&self.doc)))
                } else {
                    Err(())
                }
            }) {
            Ok(offset) => {
                if let Some(offset) = offset {
                    self.set_cursor(offset);
                }
                Action::None
            }
            Err(()) => Action::JumpHistory { forward, count },
        }
    }

    /// Restore a validated jump position without adding a second entry for the landing.
    pub fn place_jump(&mut self, offset: usize) {
        let previous = self.doc.cursor;
        self.set_cursor(offset);
        self.settle_fold_cursor(previous, self.doc.revision);
    }
}
