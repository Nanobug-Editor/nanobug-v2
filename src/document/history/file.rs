//! A versioned Nanobug undo archive tied to the buffer's current text.

use super::{Entry, History, Snapshot};
use crate::document::Document;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{borrow::Cow, collections::BTreeMap, io::ErrorKind, path::Path, sync::Arc};

/// Signature and format version for a local undo archive.
const FORMAT: &str = "nanobug-undo-1";

/// Serialized history and the digest of the document state it belongs to.
#[derive(Deserialize, Serialize)]
struct Archive<'a> {
    format: String,
    digest: [u8; 32],
    current: u64,
    next: u64,
    entries: BTreeMap<u64, ArchivedEntry<'a>>,
}

/// Saved undo edges and the prior text of every retained noncurrent entry.
#[derive(Deserialize, Serialize)]
struct ArchivedEntry<'a> {
    parent: Option<u64>,
    child: Option<u64>,
    snapshot: Option<(Cow<'a, str>, usize)>,
    time: u64,
    saved: bool,
}

impl History {
    /// Atomically archive this `text` and its history to `path`, allowing overwrite with `force`.
    pub(in crate::document) fn write_file(
        &self,
        text: &str,
        path: &Path,
        force: bool,
    ) -> Result<()> {
        if self.entries.len() < 2 {
            return Ok(());
        }
        let archive = Archive {
            format: FORMAT.into(),
            digest: Sha256::digest(text.as_bytes()).into(),
            current: self.current,
            next: self.next,
            entries: self
                .entries
                .iter()
                .map(|(&id, entry)| {
                    (
                        id,
                        ArchivedEntry {
                            parent: entry.parent,
                            child: entry.child,
                            time: entry.time,
                            saved: entry.saved,
                            snapshot: entry.snapshot.as_ref().map(|snapshot| {
                                (Cow::Borrowed(snapshot.text.as_str()), snapshot.cursor)
                            }),
                        },
                    )
                })
                .collect(),
        };
        let encoded = serde_json::to_string(&archive)?;
        if force {
            return Document::new(&encoded).write_copy(path, true);
        }
        match std::fs::symlink_metadata(path) {
            Ok(_) => {
                let mut existing = Document::open(path)?;
                let previous: Archive<'_> = serde_json::from_str(&existing.text)?;
                ensure!(
                    previous.format == FORMAT,
                    "Existing file is not a Nanobug undo archive"
                );
                existing.replace(0..existing.text.len(), &encoded, false);
                existing.save(None)
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {
                Document::new(&encoded).save_new(path)
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Read `path` for the current `text`, checking its graph before use.
    pub(in crate::document) fn read_file(text: &str, path: &Path) -> Result<Self> {
        let file = Document::open(path)?;
        let archive: Archive<'_> = serde_json::from_str(&file.text)?;
        let digest: [u8; 32] = Sha256::digest(text.as_bytes()).into();
        ensure!(
            archive.format == FORMAT,
            "Unrecognized Nanobug undo archive"
        );
        ensure!(
            archive.digest == digest,
            "Undo archive does not match the current buffer"
        );
        ensure!(
            archive.entries.contains_key(&archive.current) && archive.entries.len() <= 129,
            "Invalid undo archive size or current state"
        );
        let mut entries = BTreeMap::new();
        let mut bytes = 0_usize;
        for (id, entry) in archive.entries {
            ensure!(
                id < archive.next
                    && entry.parent.is_none_or(|parent| parent < id)
                    && entry
                        .child
                        .is_none_or(|child| child > id && child < archive.next),
                "Invalid undo archive links"
            );
            ensure!(
                entry.snapshot.is_none() == (id == archive.current),
                "Invalid undo archive snapshot"
            );
            let snapshot = if let Some((contents, cursor)) = entry.snapshot {
                ensure!(
                    contents.is_char_boundary(cursor),
                    "Invalid undo archive cursor"
                );
                bytes = bytes.saturating_add(contents.len());
                Some(Snapshot {
                    text: Arc::new(contents.into_owned()),
                    cursor,
                })
            } else {
                None
            };
            entries.insert(
                id,
                Entry {
                    parent: entry.parent,
                    child: entry.child,
                    snapshot,
                    time: entry.time,
                    saved: entry.saved,
                },
            );
        }
        Ok(Self {
            entries,
            current: archive.current,
            next: archive.next,
            bytes,
            rewound: false,
            join: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write and reopen an undo archive, rejecting a different buffer without losing its text.
    #[test]
    fn undo_archive_reopens_history_only_for_matching_text() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let undo = temp.path().join("buffer.undo");
        let mut source = Document::new("first\n");
        source.replace(0..5, "second", true);
        source.write_undo_file(&undo, false)?;
        let mut unrelated = Document::new("other\n");
        assert!(unrelated.read_undo_file(&undo).is_err());
        assert_eq!(unrelated.text, "other\n");
        let mut restored = Document::new("second\n");
        restored.read_undo_file(&undo)?;
        restored.undo(false);
        assert_eq!(restored.text, "first\n");
        restored.undo(true);
        assert_eq!(restored.text, "second\n");
        Ok(())
    }
}
