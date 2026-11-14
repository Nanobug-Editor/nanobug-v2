//! Bounded document history with separate branch and chronological traversal.

use std::collections::BTreeMap;

/// Complete editable state at a history boundary.
pub(super) struct Snapshot {
    /// Text before leaving the snapshot.
    pub(super) text: String,
    /// Byte position to restore with the text.
    pub(super) cursor: usize,
}

/// One state and the links used by ordinary undo and redo.
#[derive(Default)]
struct Entry {
    /// Older state from which this edit was made.
    parent: Option<u64>,
    /// Preferred branch when redoing from this state.
    child: Option<u64>,
    /// None denotes the live document owned by Document.
    snapshot: Option<Snapshot>,
}

/// Older states sorted by creation order, keeping abandoned redo branches available to g+ and g-.
pub(super) struct History {
    /// Monotonic IDs distinguish states on different branches.
    entries: BTreeMap<u64, Entry>,
    /// ID of the live document state.
    current: u64,
    /// Unique ID to give the next checkpoint.
    next: u64,
    /// Byte count of retained snapshots.
    bytes: usize,
}

impl Default for History {
    fn default() -> Self {
        Self {
            entries: BTreeMap::from([(0, Entry::default())]),
            current: 0,
            next: 1,
            bytes: 0,
        }
    }
}

impl History {
    /// Preserve the current `text` and `cursor` before assigning a new child state.
    pub(super) fn checkpoint(&mut self, text: &str, cursor: usize) {
        let current = self
            .entries
            .get_mut(&self.current)
            .expect("live history entry");
        current.snapshot = Some(Snapshot {
            text: text.to_owned(),
            cursor,
        });
        current.child = Some(self.next);
        self.bytes += text.len();
        self.entries.insert(
            self.next,
            Entry {
                parent: Some(self.current),
                ..Entry::default()
            },
        );
        self.current = self.next;
        self.next += 1;
        self.trim();
    }

    /// Clear preferred redo after a mutation changes the current text.
    pub(super) fn clear_redo(&mut self) {
        if let Some(entry) = self.entries.get_mut(&self.current) {
            entry.child = None;
        }
    }

    /// Find the next distinct snapshot in the `redo` direction.
    pub(super) fn branch(&self, redo: bool, text: &str) -> Option<u64> {
        let mut id = self.current;
        while let Some(entry) = self.entries.get(&id) {
            id = if redo { entry.child? } else { entry.parent? };
            let snapshot = self.entries.get(&id)?.snapshot.as_ref()?;
            if snapshot.text != text {
                return Some(id);
            }
        }
        None
    }

    /// Find the `count`th newer or older state by creation order across branches.
    pub(super) fn chronological(&self, forward: bool, count: usize) -> Option<u64> {
        if forward {
            self.entries
                .range(self.current.saturating_add(1)..)
                .take(count)
                .last()
        } else {
            self.entries.range(..self.current).rev().take(count).last()
        }
        .map(|(&id, _)| id)
    }

    /// Save the departing `snapshot` and return the state at `target`.
    pub(super) fn visit(&mut self, target: u64, snapshot: Snapshot) -> Snapshot {
        let entry = self
            .entries
            .get_mut(&self.current)
            .expect("live history entry");
        self.bytes += snapshot.text.len();
        entry.snapshot = Some(snapshot);
        let next = self
            .entries
            .get_mut(&target)
            .expect("retained history entry");
        let restored = next.snapshot.take().expect("inactive history snapshot");
        self.bytes -= restored.text.len();
        self.current = target;

        // Choosing a branch also makes subsequent ordinary redo follow it.
        let mut id = target;
        while let Some(parent) = self.entries.get(&id).and_then(|entry| entry.parent) {
            let Some(entry) = self.entries.get_mut(&parent) else {
                break;
            };
            entry.child = Some(id);
            id = parent;
        }
        self.trim();
        restored
    }

    /// Keep the same snapshot count and approximate byte budget as ordinary undo.
    fn trim(&mut self) {
        while self.entries.len() > 2 && (self.entries.len() > 129 || self.bytes > 16 * 1024 * 1024)
        {
            let oldest = self.entries.keys().copied().find(|&id| id != self.current);
            let Some(oldest) = oldest else { break };
            let evicted = self.entries.remove(&oldest).expect("oldest history entry");
            self.bytes -= evicted.snapshot.map_or(0, |snapshot| snapshot.text.len());
        }
    }
}
