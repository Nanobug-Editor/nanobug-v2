//! Bounded document history with separate branch and chronological traversal.

mod file;

use super::recovery::RecoveryFile;
use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

/// Complete editable state at a history boundary.
#[derive(Clone)]
pub(super) struct Snapshot {
    /// Text before leaving the snapshot.
    pub(super) text: Arc<String>,
    /// Byte position to restore with the text.
    pub(super) cursor: usize,
}

/// One state and the links used by ordinary undo and redo.
#[derive(Clone, Default)]
struct Entry {
    /// Older state from which this edit was made.
    parent: Option<u64>,
    /// Preferred branch when redoing from this state.
    child: Option<u64>,
    /// None denotes the live document owned by Document.
    snapshot: Option<Snapshot>,
    /// Wall-clock time at which this change was recorded, in whole seconds.
    time: u64,
    /// Whether the current text of this entry was written to disk.
    saved: bool,
    /// Original baseline of a recovered draft still present in this state.
    recovery_file: Option<RecoveryFile>,
}

/// Older states sorted by creation order, keeping abandoned redo branches available to g+ and g-.
#[derive(Clone)]
pub(super) struct History {
    /// Monotonic IDs distinguish states on different branches.
    entries: BTreeMap<u64, Entry>,
    /// ID of the live document state.
    current: u64,
    /// Unique ID to give the next checkpoint.
    next: u64,
    /// Byte count of retained snapshots.
    bytes: usize,
    /// A rewind prohibits joining a new edit to the previously visited state.
    rewound: bool,
    /// The next checkpoint can extend the current undo block.
    join: bool,
}

impl Default for History {
    fn default() -> Self {
        Self {
            entries: BTreeMap::from([(
                0,
                Entry {
                    time: now(),
                    ..Entry::default()
                },
            )]),
            current: 0,
            next: 1,
            bytes: 0,
            rewound: false,
            join: false,
        }
    }
}

impl History {
    /// Whether the requested change `number` is retained and differs from the live state.
    pub(super) fn numbered_target(&self, number: u64) -> Option<bool> {
        self.entries
            .contains_key(&number)
            .then_some(self.current != number)
    }

    /// Format retained undo leaves and indicate the state currently displayed.
    pub(super) fn listing(&self) -> String {
        use std::fmt::Write as _;
        let mut listing = String::from("number  changes  current\n");
        for &id in self.entries.keys() {
            if id == 0
                || self
                    .entries
                    .values()
                    .any(|candidate| candidate.parent == Some(id))
            {
                continue;
            }
            let mut changes = 0;
            let mut ancestor = Some(id);
            while let Some(next) = ancestor {
                ancestor = self.entries.get(&next).and_then(|node| node.parent);
                changes += usize::from(ancestor.is_some());
            }
            let current = if id == self.current { "yes" } else { "" };
            let _ = writeln!(listing, "{id:>6} {changes:>8}  {current}");
        }
        listing
    }

    /// Preserve the current `text` and `cursor` before assigning a new child state.
    pub(super) fn checkpoint(&mut self, text: &str, cursor: usize) {
        self.rewound = false;
        if std::mem::take(&mut self.join) {
            if let Some(entry) = self.entries.get_mut(&self.current) {
                entry.saved = false;
            }
            return;
        }
        let current = self
            .entries
            .get_mut(&self.current)
            .expect("live history entry");
        current.snapshot = Some(Snapshot {
            text: Arc::new(text.to_owned()),
            cursor,
        });
        current.child = Some(self.next);
        let time = now().max(current.time);
        let recovery_file = current.recovery_file;
        self.bytes += text.len();
        self.entries.insert(
            self.next,
            Entry {
                parent: Some(self.current),
                time,
                recovery_file,
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
            if snapshot.text.as_str() != text
                || self.entries[&id].recovery_file != self.recovery_file()
            {
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

    /// Find a retained change approximately `seconds` away from this state along creation order.
    pub(super) fn time_target(&self, forward: bool, seconds: u64) -> Option<u64> {
        if seconds == 0 {
            return None;
        }
        let current = self.entries[&self.current].time;
        if forward {
            let target = current.saturating_add(seconds);
            self.entries
                .range(self.current.saturating_add(1)..)
                .find(|(_, entry)| entry.time >= target)
                .or_else(|| {
                    self.entries
                        .last_key_value()
                        .filter(|(id, _)| **id != self.current)
                })
                .map(|(&id, _)| id)
        } else {
            let target = current.saturating_sub(seconds);
            self.entries
                .range(..self.current)
                .rev()
                .find(|(_, entry)| entry.time <= target)
                .or_else(|| {
                    self.entries
                        .first_key_value()
                        .filter(|(id, _)| **id != self.current)
                })
                .map(|(&id, _)| id)
        }
    }

    /// Find the `count`th written state in a direction, including retained history endpoints.
    pub(super) fn write_target(&self, forward: bool, count: usize) -> Option<u64> {
        if count == 0 {
            return None;
        }
        let lower = *self.entries.first_key_value()?.0;
        let upper = *self.entries.last_key_value()?.0;
        if forward {
            self.entries
                .range(self.current.saturating_add(1)..)
                .filter_map(|(&id, entry)| (entry.saved || id == upper).then_some(id))
                .take(count)
                .last()
        } else {
            self.entries
                .range(..self.current)
                .rev()
                .filter_map(|(&id, entry)| (entry.saved || id == lower).then_some(id))
                .take(count)
                .last()
        }
    }

    /// Mark the current change only after a successful file write.
    pub(super) fn mark_saved(&mut self) {
        self.entries
            .get_mut(&self.current)
            .expect("live history entry")
            .saved = true;
    }

    /// Return the original disk baseline of the recovered text in this history state.
    pub(super) fn recovery_file(&self) -> Option<RecoveryFile> {
        self.entries[&self.current].recovery_file
    }

    /// Associate the current history state with `recovery_file` when restoring a draft.
    pub(super) fn set_recovery_file(&mut self, recovery_file: Option<RecoveryFile>) {
        self.entries
            .get_mut(&self.current)
            .expect("live history entry")
            .recovery_file = recovery_file;
    }

    /// Drop all write baselines for the prior path when the buffer receives a new filename.
    pub(super) fn forget_recovery_files(&mut self) {
        for entry in self.entries.values_mut() {
            entry.recovery_file = None;
        }
    }

    /// Extend the current undo block on the next checkpoint, unless undo/redo preceded it.
    pub(super) fn join_next(&mut self) -> bool {
        if self.rewound {
            return false;
        }
        self.join = self.entries[&self.current].parent.is_some();
        true
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
        self.rewound = true;
        self.join = false;

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

/// Return the timestamp for a new undo block, including on systems before the Unix epoch.
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
