//! Apply version-bound replacements and undo them as one checked buffer group.

use super::EditorWindow;
use nanobug::document::Document;
use std::path::PathBuf;

/// A prepared document replacement, before any buffer has been changed.
pub(in crate::grid) struct TextChange {
    /// Existing opening/revision or a verified document to retain in this window.
    pub target: TextTarget,
    /// Complete validated replacement text.
    pub text: String,
}

/// Identity checked immediately before applying a group of edits.
pub(in crate::grid) enum TextTarget {
    /// Match both values against the retained buffer.
    Open { opening: u64, revision: u64 },
    /// A disk snapshot prepared by the caller, still subject to Document's save checks.
    New(Box<Document>),
}

/// Exact post-edit versions required to undo every participant together.
pub(in crate::grid) struct EditReceipt {
    documents: Vec<(u64, u64)>,
}

impl EditorWindow {
    /// Check every `change` before applying any replacement or retaining a new document.
    /// Return the undo receipt and paths whose filesystem history needs invalidating.
    pub(in crate::grid) fn apply_text_changes(
        &mut self,
        changes: Vec<TextChange>,
    ) -> anyhow::Result<(EditReceipt, Vec<PathBuf>)> {
        let mut targets = Vec::with_capacity(changes.len());
        for change in &changes {
            targets.push(match change.target {
                TextTarget::Open { opening, revision } => {
                    Some(self.buffer_version(opening, revision).ok_or_else(|| {
                        anyhow::anyhow!("A document changed after preview. No changes applied.")
                    })?)
                }
                TextTarget::New(_) => None,
            });
        }
        let mut documents = Vec::with_capacity(changes.len());
        let mut paths = Vec::new();
        for (index, change) in targets.into_iter().zip(changes) {
            let index = match (index, change.target) {
                (Some(index), _) => index,
                (None, TextTarget::New(doc)) => self.retain_document(*doc),
                _ => unreachable!("validated replacement target"),
            };
            let doc = &mut self.buffers[index].vim.doc;
            nanobug::plugins::edits::apply(doc, &change.text);
            documents.push((doc.opening, doc.revision));
            paths.extend(doc.path.clone());
        }
        Ok((EditReceipt { documents }, paths))
    }

    /// Undo `receipt` only while every participant still has its exact post-edit revision.
    pub(in crate::grid) fn undo_text_changes(&mut self, receipt: &EditReceipt) -> bool {
        let indices: Option<Vec<_>> = receipt
            .documents
            .iter()
            .map(|&(opening, revision)| self.buffer_version(opening, revision))
            .collect();
        let Some(indices) = indices else {
            return false;
        };
        for index in indices {
            self.buffers[index].vim.doc.undo(false);
        }
        true
    }

    /// Find a retained opening at the required revision without changing the active buffer.
    fn buffer_version(&self, opening: u64, revision: u64) -> Option<usize> {
        self.buffers.iter().position(|buffer| {
            (buffer.vim.doc.opening, buffer.vim.doc.revision) == (opening, revision)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nanobug::vim::Action;

    /// A late stale participant prevents both earlier replacements and newly retained buffers.
    #[test]
    fn stale_group_neither_edits_nor_retains_documents() {
        let mut editor = EditorWindow::new(Document::new("original"));
        let opening = editor.vim().doc.opening;
        let revision = editor.vim().doc.revision;
        let changes = vec![
            TextChange {
                target: TextTarget::New(Box::new(Document::new("disk"))),
                text: "new".into(),
            },
            TextChange {
                target: TextTarget::Open {
                    opening,
                    revision: revision + 1,
                },
                text: "stale".into(),
            },
        ];
        assert!(editor.apply_text_changes(changes).is_err());
        assert_eq!(editor.buffers().len(), 1);
        assert_eq!(editor.active_index(), 0);
        assert_eq!(editor.vim().doc.text, "original");
    }

    /// The editor rejects group undo when another operation has changed an inactive participant.
    #[test]
    fn grouped_undo_checks_all_participants_before_mutating() -> anyhow::Result<()> {
        let mut editor = EditorWindow::new(Document::new("first"));
        let doc = &editor.vim().doc;
        let changes = vec![
            TextChange {
                target: TextTarget::Open {
                    opening: doc.opening,
                    revision: doc.revision,
                },
                text: "first changed".into(),
            },
            TextChange {
                target: TextTarget::New(Box::new(Document::new("second"))),
                text: "second changed".into(),
            },
        ];
        let (receipt, _) = editor.apply_text_changes(changes)?;
        assert_eq!(editor.active_index(), 0);
        editor.edit_buffer(1, |vim, _| {
            vim.doc.replace(0..0, "user ", true);
            Action::None
        });
        assert!(!editor.undo_text_changes(&receipt));
        assert_eq!(editor.buffers()[0].vim.doc.text, "first changed");
        assert_eq!(editor.buffers()[1].vim.doc.text, "user second changed");
        Ok(())
    }
}
