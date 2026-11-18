//! Shared buffer storage, with an independent position in each view.

use super::{Document, DocumentContent, anchor::Anchors, next_opening, signs::Signs};
use std::{
    ops::{Deref, DerefMut},
    path::PathBuf,
    sync::{Arc, Mutex},
};

/// The latest contents of a buffer and its stable number across views.
pub(super) struct BufferDocument {
    /// Stable Ex number used in every view of this buffer.
    number: u64,
    /// Zero or more tracked file positions shared by the buffer's views.
    pub(super) anchors: Anchors,
    /// Placed line signs shared by each view with this buffer identity.
    pub(super) signs: Signs,
    /// Own the contents once another view can outlive the view that published them.
    latest: Mutex<Option<Arc<DocumentContent>>>,
}

impl BufferDocument {
    /// Create a buffer whose stable number is the number of its first `opening`.
    pub(super) fn new(opening: u64) -> Self {
        Self {
            number: opening,
            anchors: Anchors::default(),
            signs: Signs::default(),
            latest: Mutex::new(None),
        }
    }

    /// Read the last published contents of a shared view.
    pub(super) fn latest(&self) -> Option<Arc<DocumentContent>> {
        self.latest.lock().expect("buffer content").clone()
    }
}

impl Document {
    /// Get the stable buffer number used by Ex across that buffer's views.
    pub fn buffer_number(&self) -> u64 {
        self.shared.number
    }

    /// Add a view at this cursor, sharing text, file state and undo history.
    pub fn shared_view(&self) -> Self {
        let content = self.shared.latest().unwrap_or_else(|| self.content.clone());
        let mut view = Self {
            opening: next_opening(),
            cursor: self.cursor,
            content: self.content.clone(),
            shared: self.shared.clone(),
            tabstop: self.tabstop.clone(),
            folds: self.folds.clone(),
        };
        view.adopt_content(content);
        view.publish_content();
        view
    }

    /// Use `source`'s buffer content in a restored view, keeping its own identity and caret.
    pub fn join_buffer(&mut self, source: &Self) {
        self.shared = source.shared.clone();
        self.adopt_content(
            source
                .shared
                .latest()
                .unwrap_or_else(|| source.content.clone()),
        );
        self.publish_content();
    }

    /// Detach a view moved to another Session, preserving its text and undo history.
    pub fn detach_buffer(&mut self) {
        self.refresh_shared();
        if Arc::strong_count(&self.shared) > 1 {
            let detached = BufferDocument::new(next_opening());
            detached.signs.copy_from(&self.shared.signs);
            self.shared = Arc::new(detached);
        }
    }

    /// Discard pending edits for this buffer and publish the saved state to every view.
    pub fn discard_changes(&mut self) -> bool {
        self.refresh_shared();
        if !self.modified() {
            return false;
        }
        let saved = self.saved.clone();
        let replacement = self.anchor_replacement(&saved);
        let signs = self.sign_replacement(&saved);
        self.restore_folds(&saved);
        let content = Arc::make_mut(&mut self.content);
        content.text = (*saved).clone();
        content.history = Default::default();
        content.last_replacement = None;
        content.revision += 1;
        content.saved_revision = Some(content.revision);
        self.reindex();
        self.cursor = self.text.floor_char_boundary(self.cursor);
        if let Some(marks) = replacement {
            self.shift_anchors(&marks);
        }
        self.shift_signs(signs);
        self.publish_content();
        true
    }

    /// Store the new file path after a host filesystem move.
    pub fn relocate(&mut self, path: PathBuf) {
        self.refresh_shared();
        self.path = Some(path);
        self.relocate_anchors();
        self.publish_content();
    }

    /// Bring this view up to date and report whether the shared text changed.
    pub fn refresh_shared(&mut self) -> bool {
        let Some(latest) = self.shared.latest() else {
            return false;
        };
        self.adopt_content(latest)
    }

    /// Publish a newly edited revision or updated save state to the other views.
    pub(super) fn publish_content(&self) {
        *self.shared.latest.lock().expect("buffer content") =
            (Arc::strong_count(&self.shared) > 1).then(|| self.content.clone());
    }

    /// Update shared state while rebasing this view's cursor and folds against the new text.
    fn adopt_content(&mut self, latest: Arc<DocumentContent>) -> bool {
        if Arc::ptr_eq(&self.content, &latest) {
            return false;
        }
        let changed = self.revision != latest.revision;
        if changed && !latest.metadata_revised_since(self.revision) {
            if let Some((range, inserted)) = latest
                .last_replacement
                .as_ref()
                .filter(|_| self.revision.checked_add(1) == Some(latest.revision))
            {
                self.shift_folds(range, &latest.text[range.start..range.start + inserted]);
                if self.cursor >= range.end {
                    self.cursor = self.cursor - range.len() + inserted;
                } else if self.cursor >= range.start {
                    self.cursor = range.start;
                }
            } else {
                let (row, column) = self.location(self.cursor);
                self.restore_folds(&latest.text);
                self.content = latest;
                self.cursor = self.offset(row, column);
                return true;
            }
        }
        self.content = latest;
        self.cursor = self
            .text
            .floor_char_boundary(self.cursor.min(self.text.len()));
        changed
    }
}

impl DocumentContent {
    /// Check the newest published state before the receiving view has adopted it.
    pub(super) fn metadata_revised_since(&self, before: u64) -> bool {
        self.metadata_revisions
            .is_some_and(|(from, end)| end == self.revision && (from..end).contains(&before))
    }
}

impl Deref for Document {
    type Target = DocumentContent;

    /// Access the current view's snapshot of the buffer contents.
    fn deref(&self) -> &Self::Target {
        &self.content
    }
}

impl DerefMut for Document {
    /// Detach the current snapshot from other views before updating it.
    fn deref_mut(&mut self) -> &mut Self::Target {
        Arc::make_mut(&mut self.content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Keep unpublished-to-view edits when the window that made them closes.
    #[test]
    fn peer_can_undo_after_the_editing_view_closes() {
        let mut writer = Document::new("one\ntwo");
        let mut peer = writer.shared_view();
        peer.cursor = "one\n".len();
        writer.replace(0..3, "four", true);
        drop(writer);

        assert!(peer.refresh_shared());
        assert!(peer.modified());
        assert_eq!(peer.text, "four\ntwo");
        assert_eq!(peer.cursor, "four\n".len());
        peer.undo(false);
        assert_eq!(peer.text, "one\ntwo");
        assert!(!peer.modified());
    }
}
