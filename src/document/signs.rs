//! Buffer-local line decorations shared by all views of a document.

use super::Document;
use std::{cmp::Reverse, ops::Range, sync::Mutex};

/// One placed sign. IDs are unique within a buffer and group.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignPlacement {
    /// Positive sign identifier for editing, removal and navigation.
    pub id: u64,
    /// Case-sensitive group name, or the empty string for the global group.
    pub group: String,
    /// Canonical sign definition name.
    pub name: String,
    /// Zero-based buffer row.
    pub row: usize,
    /// Display priority when multiple signs occupy the same row.
    pub priority: i32,
}

/// Filter placements by Ex sign group.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum SignGroup {
    /// The unnamed global group.
    #[default]
    Global,
    /// Match one specific case-sensitive group.
    Named(String),
    /// Include all groups.
    All,
}

impl SignGroup {
    /// Return whether a placement in `group` is in this selection.
    pub fn matches(&self, group: &str) -> bool {
        match self {
            Self::Global => group.is_empty(),
            Self::Named(name) => group == name,
            Self::All => true,
        }
    }
}

/// A placement and its right-gravity position, initially at the beginning of the row.
#[derive(Clone)]
struct Placed {
    sign: SignPlacement,
    offset: usize,
}

/// Live placements in insertion order, separate from text checkpoints.
#[derive(Default)]
pub(super) struct Signs(Mutex<Vec<Placed>>);

impl Signs {
    /// Remove all signs when the buffer unloads its contents.
    pub(super) fn clear(&self) {
        self.0.lock().expect("buffer signs").clear();
    }

    /// Copy placements when an existing view becomes an independent buffer.
    pub(super) fn copy_from(&self, source: &Self) {
        let signs = source.0.lock().expect("buffer signs").clone();
        *self.0.lock().expect("buffer signs") = signs;
    }
}

/// Position changes for one text splice, using the old end row before the buffer is reindexed.
pub(super) struct SignEdit {
    range: Range<usize>,
    end_row: usize,
    inserted: usize,
}

impl Document {
    /// Whether the shared buffer has any live sign placements.
    pub fn has_signs(&self) -> bool {
        !self.shared.signs.0.lock().expect("buffer signs").is_empty()
    }

    /// Snapshot the placements ordered by row, decreasing priority and ID, then recency.
    pub fn signs(&self) -> Vec<SignPlacement> {
        let mut placements: Vec<_> = self
            .shared
            .signs
            .0
            .lock()
            .expect("buffer signs")
            .iter()
            .rev()
            .map(|entry| entry.sign.clone())
            .collect();
        placements.sort_by_key(|sign| (sign.row, Reverse(sign.priority), Reverse(sign.id)));
        placements
    }

    /// Set `sign`, replacing the same ID in its group and clamping its row at EOF.
    /// Return false for an invalid placement key.
    pub fn place_sign(&self, mut sign: SignPlacement) -> bool {
        if sign.id == 0 || sign.name.is_empty() || sign.group.starts_with('*') {
            return false;
        }
        let latest = self.shared.latest();
        let document = latest.as_deref().unwrap_or(&self.content);
        sign.row = sign.row.min(document.starts.len() - 1);
        let offset = document.starts[sign.row];
        let mut signs = self.shared.signs.0.lock().expect("buffer signs");
        if let Some(index) = signs
            .iter()
            .position(|entry| entry.sign.id == sign.id && entry.sign.group == sign.group)
        {
            signs.remove(index);
        }
        signs.push(Placed { sign, offset });
        true
    }

    /// Remove signs in `group`, narrowed to `id` when provided, and return the number removed.
    pub fn remove_signs(&self, group: &SignGroup, id: Option<u64>) -> usize {
        let mut signs = self.shared.signs.0.lock().expect("buffer signs");
        let count = signs.len();
        signs.retain(|entry| {
            !group.matches(&entry.sign.group) || id.is_some_and(|id| id != entry.sign.id)
        });
        count - signs.len()
    }

    /// Drop signs on the complete lines removed by a linewise editing operation.
    pub(crate) fn remove_sign_rows(&self, rows: Range<usize>) {
        self.shared
            .signs
            .0
            .lock()
            .expect("buffer signs")
            .retain(|entry| !rows.contains(&entry.sign.row));
    }

    /// Record the old boundary for `range` before replacing it with `inserted` bytes.
    pub(super) fn sign_edit(&self, range: Range<usize>, inserted: usize) -> Option<SignEdit> {
        self.has_signs().then(|| SignEdit {
            end_row: self.starts.partition_point(|&start| start <= range.end) - 1,
            range,
            inserted,
        })
    }

    /// Track a snapshot restore or whole-buffer rewrite between its unchanged edges.
    pub(super) fn sign_replacement(&self, text: &str) -> Option<SignEdit> {
        if !self.has_signs() {
            return None;
        }
        let (range, inserted) = super::anchor::changed_span(&self.text, text)?;
        self.sign_edit(range, inserted)
    }

    /// Rebase positions after reindexing and discard any signs whose line was removed.
    pub(super) fn shift_signs(&self, edit: Option<SignEdit>) {
        let Some(SignEdit {
            range,
            end_row,
            inserted,
        }) = edit
        else {
            return;
        };
        self.shared
            .signs
            .0
            .lock()
            .expect("buffer signs")
            .retain_mut(|entry| {
                if range.contains(&entry.offset) && entry.sign.row < end_row {
                    return false;
                }
                let offset = if entry.offset >= range.end {
                    entry.offset - range.len() + inserted
                } else if entry.offset >= range.start {
                    range.start + inserted
                } else {
                    entry.offset
                };
                entry.offset = offset.min(self.text.len());
                entry.sign.row = self.starts.partition_point(|&start| start <= entry.offset) - 1;
                true
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Place `id` at `row` in a specific `group` with an independent display `priority`.
    fn sign(id: u64, group: &str, row: usize, priority: i32) -> SignPlacement {
        SignPlacement {
            id,
            group: group.into(),
            name: "Test".into(),
            row,
            priority,
        }
    }

    /// Keep independent copies after a detach and order ties by ID before insertion order.
    #[test]
    fn sign_placements_share_between_views_and_are_group_scoped() {
        let doc = Document::new("one\ntwo\nthree");
        let mut peer = doc.shared_view();
        for sign in [
            sign(1, "", 1, 12),
            sign(2, "", 1, 12),
            sign(2, "remote", 1, 12),
        ] {
            assert!(doc.place_sign(sign));
        }
        assert_eq!(
            peer.signs()
                .iter()
                .map(|mark| (mark.id, mark.group.as_str()))
                .collect::<Vec<_>>(),
            [(2, "remote"), (2, ""), (1, "")]
        );
        assert!(peer.place_sign(sign(1, "", 99, 15)));
        assert_eq!(doc.signs().last().unwrap().row, 2);
        assert_eq!(
            doc.remove_signs(&SignGroup::Named("remote".into()), Some(2)),
            1
        );
        assert_eq!(peer.signs().len(), 2);
        peer.detach_buffer();
        assert_eq!(peer.remove_signs(&SignGroup::All, None), 2);
        assert!(!peer.has_signs());
        assert!(doc.has_signs());
    }

    /// Follow inserted lines, clear deleted lines and leave deleted signs absent after undo.
    #[test]
    fn sign_rows_follow_edits_and_do_not_return_with_undo() {
        let mut doc = Document::new("a\nb\nc");
        let mut peer = doc.shared_view();
        assert!(doc.place_sign(sign(1, "", 1, 10)));
        assert!(doc.place_sign(sign(2, "", 2, 10)));
        doc.replace(2..2, "line one\nline two\n", true);
        assert_eq!(
            peer.signs().iter().map(|mark| mark.row).collect::<Vec<_>>(),
            [3, 4]
        );
        let row = doc.starts[3];
        doc.replace(row..doc.starts[4], "", true);
        assert_eq!(peer.signs(), [sign(2, "", 3, 10)]);
        peer.undo(false);
        assert_eq!(peer.signs(), [sign(2, "", 4, 10)]);
        peer.undo(false);
        assert_eq!(peer.signs(), [sign(2, "", 2, 10)]);
    }

    /// Preserve signs on moved lines and delete a sign with the last line without a final LF.
    #[test]
    fn sign_rows_preserve_move_identity_and_delete_at_eof() {
        use crate::vim::{Register, Vim};

        let doc = Document::new("one\ntwo\nthree");
        let peer = doc.shared_view();
        let mut vim = Vim::new(doc);
        let mut register = Register::default();
        assert!(vim.doc.place_sign(sign(1, "", 1, 10)));
        assert!(vim.doc.place_sign(sign(2, "", 2, 10)));
        vim.execute_ex_line("2move 3", &mut register);
        assert_eq!(vim.doc.text, "one\nthree\ntwo");
        assert_eq!(peer.signs(), [sign(2, "", 1, 10), sign(1, "", 2, 10)]);
        vim.execute_ex_line("3delete", &mut register);
        assert_eq!(vim.doc.text, "one\nthree");
        assert_eq!(peer.signs(), [sign(2, "", 1, 10)]);
        vim.doc.undo(false);
        assert_eq!(vim.doc.text, "one\nthree\ntwo");
        assert_eq!(peer.signs(), [sign(2, "", 1, 10)]);
    }
}
