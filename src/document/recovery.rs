//! Capture and restore buffer drafts while retaining save conflict protection.

use super::{DiskVersion, Document, DocumentContent, disk_fingerprint, file_encoding};
use crate::i18n;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

/// Text and file state sufficient to recover a draft without its original on-disk bytes.
#[derive(Clone, Deserialize, Serialize)]
pub struct RecoveryDraft {
    /// Absolute backing filename, or no filename for an untitled draft.
    pub path: Option<PathBuf>,
    /// Draft text in the editor's internal LF representation.
    pub text: String,
    /// Cursor position supplied by the captured view, in bytes.
    #[serde(default)]
    cursor: Option<usize>,
    /// Disk state of the file the recovered draft was originally editing.
    baseline: Option<DiskVersion>,
    /// Whether recovering and saving should produce CRLF line endings.
    crlf: bool,
    /// Whether recovering and saving should preserve the source UTF-8 BOM.
    bom: bool,
}

/// Write precondition and encoding retained separately from the live buffer's loaded file state.
#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) struct RecoveryFile {
    /// Disk state required for a subsequent save to the original path.
    baseline: DiskVersion,
    /// Use the draft's CRLF setting for future writes.
    crlf: bool,
    /// Use the draft's BOM setting for future writes.
    bom: bool,
}

impl RecoveryDraft {
    /// Check whether the file still matches the original recovery baseline.
    pub fn matches_disk(&self) -> Result<bool> {
        match (&self.path, self.baseline) {
            (Some(path), Some(baseline)) => Ok(disk_fingerprint(path)? == baseline),
            (None, None) => Ok(true),
            _ => Ok(false),
        }
    }

    /// Check whether the exact draft bytes are already present at the original path.
    pub fn written_to_disk(&self) -> Result<bool> {
        let Some(path) = &self.path else {
            return Ok(false);
        };
        let (bom, text) = file_encoding(&self.text, self.crlf, self.bom);
        let mut digest = Sha256::new();
        digest.update(bom);
        digest.update(text.as_bytes());
        let draft = DiskVersion::Saved {
            length: (bom.len() + text.len()) as u64,
            digest: digest.finalize().into(),
        };
        Ok(disk_fingerprint(path)? == draft)
    }
}

impl DocumentContent {
    /// Return the expected disk version of the active draft or the normally loaded file.
    pub(super) fn expected_disk_version(&self) -> Option<DiskVersion> {
        self.history
            .recovery_file()
            .map(|file| file.baseline)
            .or(self.disk_version)
    }

    /// Return the CRLF and BOM flags that should be used when writing this text.
    pub(super) fn file_format(&self) -> (bool, bool) {
        self.history
            .recovery_file()
            .map_or((self.crlf, self.bom), |file| (file.crlf, file.bom))
    }

    /// Whether recovery must be resolved even when the text matches the loaded file.
    pub(super) fn recovery_requires_write(&self) -> bool {
        self.history.recovery_file().is_some()
    }
}

impl Document {
    /// Read the latest shared revision, filename and dirty state for background collection.
    pub fn recovery_state(&self) -> (u64, Option<PathBuf>, bool) {
        let content = self.shared.latest().unwrap_or_else(|| self.content.clone());
        let dirty = content.recovery_requires_write()
            || (content.saved_revision != Some(content.revision) && content.text != *content.saved);
        (content.revision, content.path.clone(), dirty)
    }

    /// Capture the newest shared text and the disk version it expects to replace.
    pub fn recovery_draft(&self) -> RecoveryDraft {
        let content = self.shared.latest().unwrap_or_else(|| self.content.clone());
        let (crlf, bom) = content.file_format();
        RecoveryDraft {
            path: content.path.clone(),
            text: content.text.clone(),
            cursor: (content.revision == self.revision).then_some(self.cursor),
            baseline: content.expected_disk_version(),
            crlf,
            bom,
        }
    }

    /// Restore `draft` into its associated buffer as one edit without writing the file.
    /// The caller checks whether replacing any pending edits is allowed.
    pub fn recover_draft(&mut self, draft: &RecoveryDraft) -> Result<()> {
        self.refresh_shared();
        let same_file = self.path == draft.path
            || self
                .path
                .as_ref()
                .zip(draft.path.as_ref())
                .is_some_and(|(path, original)| {
                    original
                        .canonicalize()
                        .is_ok_and(|canonical| canonical == *path)
                });
        ensure!(
            same_file && draft.path.is_some() == draft.baseline.is_some(),
            i18n::text("document.disk_changed")
        );

        let recovery_file = draft
            .baseline
            .filter(|baseline| {
                Some(*baseline) != self.disk_version
                    || (draft.crlf, draft.bom) != (self.crlf, self.bom)
            })
            .map(|baseline| RecoveryFile {
                baseline,
                crlf: draft.crlf,
                bom: draft.bom,
            });
        self.noticed_disk_version = None;
        if self.text != draft.text || self.history.recovery_file() != recovery_file {
            self.checkpoint_content();
        }
        self.history.set_recovery_file(recovery_file);
        if self.text == draft.text {
            self.publish_content();
        } else {
            self.replace(0..self.text.len(), &draft.text, false);
        }
        if let Some(cursor) = draft.cursor {
            self.cursor = self.text.floor_char_boundary(cursor.min(self.text.len()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Capture an unpublished-to-peer edit and recover it as one shared undo step.
    #[test]
    fn restored_shared_draft_is_undoable_and_keeps_disk_unchanged() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let file = directory.path().join("draft.txt");
        fs::write(&file, "saved\n")?;
        let mut writer = Document::open(&file)?;
        let peer = writer.shared_view();
        writer.replace(0..5, "edited", true);
        let (revision, _, dirty) = peer.recovery_state();
        assert_eq!(revision, writer.revision);
        assert!(dirty);
        let serialized = serde_json::to_vec(&peer.recovery_draft())?;
        let draft: RecoveryDraft = serde_json::from_slice(&serialized)?;
        assert!(draft.matches_disk()?);
        assert!(!draft.written_to_disk()?);

        let mut reopened = Document::open(&file)?;
        let mut split = reopened.shared_view();
        reopened.recover_draft(&draft)?;
        assert_eq!(reopened.text, "edited\n");
        assert!(reopened.modified());
        assert_eq!(fs::read_to_string(&file)?, "saved\n");
        assert!(split.refresh_shared());
        assert_eq!(split.text, "edited\n");
        split.undo(false);
        assert_eq!(split.text, "saved\n");
        assert!(!split.modified());
        Ok(())
    }

    /// Keep a vanished baseline distinct from an empty file created by another editor.
    #[test]
    fn conflicting_empty_draft_keeps_dirty_state_until_discarded() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let file = directory.path().join("empty.txt");
        let original = Document::open(&file)?;
        let draft = original.recovery_draft();
        fs::write(&file, "")?;
        assert!(!draft.matches_disk()?);
        assert!(draft.written_to_disk()?);

        let mut reopened = Document::open(&file)?;
        reopened.recover_draft(&draft)?;
        assert!(reopened.modified());
        assert!(reopened.recovery_state().2);
        reopened.undo(false);
        assert!(!reopened.modified());
        reopened.undo(true);
        assert!(reopened.modified());
        assert!(reopened.discard_changes());
        assert!(!reopened.modified());
        Ok(())
    }

    /// A conflicting external write survives recovery, and copying preserves draft encoding.
    #[test]
    fn conflicting_draft_preserves_encoding_and_original_file() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let file = directory.path().join("draft.txt");
        let copy = directory.path().join("copy.txt");
        fs::write(&file, b"\xef\xbb\xbfbase\r\n")?;
        let mut original = Document::open(&file)?;
        original.replace(0..4, "edit", true);
        let draft = original.recovery_draft();
        fs::write(&file, "edit\n")?;
        assert!(!draft.matches_disk()?);
        assert!(!draft.written_to_disk()?);

        let mut reopened = Document::open(&file)?;
        reopened.recover_draft(&draft)?;
        assert!(reopened.modified());
        assert!(reopened.save(None).is_err());
        assert_eq!(fs::read(&file)?, b"edit\n");
        reopened.undo(false);
        assert!(!reopened.modified());
        reopened.undo(true);
        assert!(reopened.modified());
        assert!(reopened.save(None).is_err());
        reopened.save_new(&copy)?;
        assert!(!reopened.modified());
        assert_eq!(fs::read(&copy)?, b"\xef\xbb\xbfedit\r\n");
        Ok(())
    }
}
