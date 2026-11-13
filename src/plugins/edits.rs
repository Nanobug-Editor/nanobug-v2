//! Validate complete document edit groups before changing the editor's authoritative text.

use super::{
    package::{hash_bytes, read_limited},
    project_path,
};
use crate::document::Document;
use anyhow::{Result, ensure};
use nanobug_plugin_sdk::{Edit, ProjectText};
use std::{
    ops::Range,
    path::{Component, Path},
};

const PROJECT_TEXT_LIMIT: u64 = 120 * 1024;

/// Read a bounded UTF-8 project snapshot, with an exact disk version checked on the next read.
pub fn read_project_text(root: &Path, path: String) -> Result<ProjectText> {
    let (doc, version) = open_project_text(root, &path, None)?;
    Ok(ProjectText {
        path,
        version,
        text: doc.text,
    })
}

/// Prepare a buffer for an unopened file. The source bytes are also checked on a later Save.
pub fn prepare_project_text(root: &Path, path: &str, version: &str) -> Result<Document> {
    open_project_text(root, path, Some(version)).map(|(doc, _)| doc)
}

fn open_project_text(
    root: &Path,
    path: &str,
    expected: Option<&str>,
) -> Result<(Document, String)> {
    ensure!(
        !path.is_empty()
            && path.len() <= 2048
            && !path.contains(':')
            && !path.chars().any(char::is_control)
            && Path::new(path)
                .components()
                .all(|part| matches!(part, Component::Normal(_))),
        "Use a relative project file path"
    );
    let resolved = project_path(root, path)?;
    ensure!(resolved.is_file(), "Project file is unavailable");
    let bytes = read_limited(&resolved, PROJECT_TEXT_LIMIT)?;
    let version = hash_bytes(&bytes);
    ensure!(
        expected.is_none_or(|expected| expected == version)
            && project_path(root, path)? == resolved,
        "Project file changed while the plugin was working"
    );
    let doc = Document::from_file_bytes(resolved, bytes, expected.is_some())?;
    Ok((doc, version))
}

/// Validate ranges against one opening/revision and prepare one undoable replacement.
pub fn prepare(
    doc: &Document,
    version: Option<(u64, u64)>,
    mut edits: Vec<Edit>,
    selection: Option<Range<usize>>,
) -> Result<String> {
    ensure!(
        version == Some((doc.opening, doc.revision)),
        "The document changed while the plugin was working"
    );
    ensure!(edits.len() <= 4096, "Plugin returned too many edits");
    edits.sort_by_key(|edit| (edit.start, edit.end));
    let mut previous: Option<&Edit> = None;
    let mut length = doc.text.len();
    for edit in &edits {
        ensure!(
            edit.start <= edit.end
                && doc.text.is_char_boundary(edit.start)
                && doc.text.is_char_boundary(edit.end),
            "Plugin returned an invalid UTF-8 range"
        );
        ensure!(
            previous.is_none_or(|last| last.end <= edit.start && last.start != edit.start),
            "Plugin returned overlapping or ambiguous edits"
        );
        ensure!(
            selection
                .as_ref()
                .is_none_or(|selection| selection.start <= edit.start && edit.end <= selection.end),
            "Plugin returned a change outside the selected range"
        );
        ensure!(!edit.text.contains('\0'), "Plugin returned binary text");
        length = length
            .saturating_sub(edit.end - edit.start)
            .saturating_add(edit.text.len());
        ensure!(
            length <= 16 * 1024 * 1024,
            "Plugin result is too large for the editor"
        );
        previous = Some(edit);
    }
    let mut result = String::with_capacity(length);
    let mut start = 0;
    for edit in edits {
        result.push_str(&doc.text[start..edit.start]);
        if edit.text.contains("\r\n") {
            result.push_str(&edit.text.replace("\r\n", "\n"));
        } else {
            result.push_str(&edit.text);
        }
        start = edit.end;
    }
    result.push_str(&doc.text[start..]);
    Ok(result)
}

/// Apply the already checked replacement as one undo step, keeping the cursor near its old position.
pub fn apply(doc: &mut Document, result: &str) {
    if doc.text == result {
        return;
    }
    let (row, column) = doc.location(doc.cursor);
    doc.replace(0..doc.text.len(), result, true);
    doc.cursor = doc.offset(row.min(doc.starts.len() - 1), column);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_group_is_version_checked_and_undoable() {
        let mut doc = Document::new("你好👋\n\told");
        let version = Some((doc.opening, doc.revision));
        let invalid = vec![Edit {
            start: 1,
            end: 3,
            text: "a".into(),
        }];
        assert!(prepare(&doc, version, invalid, None).is_err());
        let edits = vec![
            Edit {
                start: 0,
                end: 3,
                text: "再".into(),
            },
            Edit {
                start: 12,
                end: 15,
                text: "new\r\n".into(),
            },
        ];
        let result = prepare(&doc, version, edits, None).unwrap();
        apply(&mut doc, &result);
        assert_eq!(doc.text, "再好👋\n\tnew\n");
        assert!(prepare(&doc, version, vec![], None).is_err());
        doc.undo(false);
        assert_eq!(doc.text, "你好👋\n\told");
    }

    #[test]
    fn project_text_checks_disk_version_and_preserves_file_encoding() -> Result<()> {
        let folder = tempfile::tempdir()?;
        let root = folder.path();
        let path = root.join("sample.txt");
        std::fs::write(&path, "\u{feff}你好👋\r\n")?;
        let snapshot = read_project_text(root, "sample.txt".into())?;
        assert_eq!(snapshot.text, "你好👋\n");
        let mut doc = prepare_project_text(root, &snapshot.path, &snapshot.version)?;
        apply(&mut doc, "再见👋\n");
        std::fs::write(&path, "\u{feff}原文👋\r\n")?;
        assert!(prepare_project_text(root, &snapshot.path, &snapshot.version).is_err());
        assert!(doc.save(None).is_err());
        assert!(doc.modified());
        assert_eq!(std::fs::read_to_string(&path)?, "\u{feff}原文👋\r\n");
        doc.save(Some(&root.join("copy.txt")))?;
        assert_eq!(
            std::fs::read_to_string(root.join("copy.txt"))?,
            "\u{feff}再见👋\r\n"
        );
        Ok(())
    }

    #[test]
    fn formatting_uses_normalized_utf8_offsets_and_rejects_reopened_files() -> Result<()> {
        let project = tempfile::tempdir()?;
        let path = project.path().join("words.txt");
        let original = "头👩‍💻\te\u{301}\r\n第二行\r\n";
        std::fs::write(&path, original)?;
        let mut doc = Document::open(&path)?;
        let version = Some((doc.opening, doc.revision));
        let start = doc.text.find('e').unwrap();
        let end = doc.text.find('\n').unwrap();
        let selection = Some(start..end);
        let change = Edit {
            start,
            end,
            text: "É".into(),
        };
        let unrelated = Edit {
            start: 0,
            end: "头".len(),
            text: "新".into(),
        };
        assert!(
            prepare(
                &doc,
                version,
                vec![change.clone(), unrelated],
                selection.clone()
            )
            .is_err()
        );
        assert!(!doc.modified());
        let updated = prepare(&doc, version, vec![change.clone()], selection)?;
        apply(&mut doc, &updated);
        assert_eq!(doc.text, "头👩‍💻\tÉ\n第二行\n");
        doc.undo(false);
        assert!(!doc.modified());
        doc.undo(true);
        doc.save(None)?;
        assert_eq!(std::fs::read_to_string(&path)?, "头👩‍💻\tÉ\r\n第二行\r\n");

        let reopened = Document::open(&path)?;
        assert_ne!(reopened.opening, doc.opening);
        assert!(prepare(&reopened, version, vec![change], None).is_err());
        Ok(())
    }
}
