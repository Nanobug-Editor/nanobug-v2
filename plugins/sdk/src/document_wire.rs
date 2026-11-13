//! Versioned document refreshes on the private pipe. WASM still receives a complete Input.

use crate::{DocumentSnapshot, Edit};
use serde::{Deserialize, Serialize};

pub const MAX_DOCUMENT_BYTES: usize = 120 * 1024;

/// One contiguous replacement between two versions of the same opening.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentDelta {
    pub opening: u64,
    pub base_revision: u64,
    pub revision: u64,
    pub edit: Edit,
}

impl DocumentDelta {
    /// Keep the common prefix and suffix only where both texts have UTF-8 boundaries.
    pub fn between(base: &DocumentSnapshot, target: &DocumentSnapshot) -> Option<Self> {
        if base.opening != target.opening
            || base.path != target.path
            || base.revision >= target.revision
            || target.text.len() > MAX_DOCUMENT_BYTES
        {
            return None;
        }
        let before = base.text.as_bytes();
        let after = target.text.as_bytes();
        let mut prefix = before.iter().zip(after).take_while(|(a, b)| a == b).count();
        while !base.text.is_char_boundary(prefix) || !target.text.is_char_boundary(prefix) {
            prefix -= 1;
        }
        let mut suffix = before[prefix..]
            .iter()
            .rev()
            .zip(after[prefix..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        while !base.text.is_char_boundary(before.len() - suffix)
            || !target.text.is_char_boundary(after.len() - suffix)
        {
            suffix -= 1;
        }
        // The omitted bytes must outweigh the metadata for a small replacement.
        if prefix + suffix <= 192 {
            return None;
        }
        Some(Self {
            opening: base.opening,
            base_revision: base.revision,
            revision: target.revision,
            edit: Edit {
                start: prefix,
                end: before.len() - suffix,
                text: target.text[prefix..after.len() - suffix].to_owned(),
            },
        })
    }

    /// Reject an unknown version, invalid boundary, or result above the snapshot budget.
    pub fn apply(self, base: &DocumentSnapshot) -> Option<DocumentSnapshot> {
        let Edit { start, end, text } = self.edit;
        if base.opening != self.opening
            || base.revision != self.base_revision
            || self.revision <= self.base_revision
            || start > end
            || !base.text.is_char_boundary(start)
            || !base.text.is_char_boundary(end)
            || (base.text.len() - (end - start)).checked_add(text.len())? > MAX_DOCUMENT_BYTES
        {
            return None;
        }
        let mut document = base.clone();
        document.text.replace_range(start..end, &text);
        document.revision = self.revision;
        Some(document)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_matches_stop_at_unicode_boundaries() {
        let unchanged = "\t".repeat(200);
        let before = DocumentSnapshot {
            path: None,
            opening: 1,
            revision: 0,
            text: format!("{unchanged}😃中{unchanged}"),
        };
        let after = DocumentSnapshot {
            text: format!("{unchanged}😄丬{unchanged}"),
            revision: 2,
            ..before.clone()
        };
        let delta = DocumentDelta::between(&before, &after).unwrap();
        assert_eq!(delta.edit.start, unchanged.len());
        assert_eq!(delta.edit.end, unchanged.len() + "😃中".len());
        assert_eq!(delta.apply(&before), Some(after));
    }
}
