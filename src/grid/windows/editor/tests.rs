//! Editing-window regressions for retained buffers, native input and transient composition.

use super::EditorWindow;
use gpui::Keystroke;
use nanobug::{document::Document, vim::Mode};
use std::fs;

/// Switching files must preserve unsaved buffers, and failed opens must be atomic.
#[test]
fn switching_files_retains_unsaved_text_and_failed_open_preserves_state() {
    let project = tempfile::tempdir().unwrap();
    let first = project.path().join("first.txt");
    let second = project.path().join("second.txt");
    fs::write(&first, "first").unwrap();
    fs::write(&second, "second").unwrap();
    let mut editor = EditorWindow::new(Document::open(&first).unwrap());
    editor.edit(|vim, register| vim.text("i!", register));
    editor.open(second, 1.).unwrap();
    assert_eq!(editor.vim().doc.text, "second");
    editor.open(first, 2.).unwrap();
    assert_eq!(editor.vim().doc.text, "!first");
    assert!(editor.modified());
    assert_eq!(editor.buffers.len(), 2);

    let active = editor.active;
    let invalid = project.path().join("invalid.txt");
    fs::write(&invalid, [0xff]).unwrap();
    assert!(editor.open(invalid, 3.).is_err());
    assert_eq!(editor.active, active);
    assert_eq!(editor.buffers.len(), 2);
    assert_eq!(editor.vim().doc.text, "!first");
}

/// Preedit uses UTF-16 ranges but does not mutate the document until the single committed edit.
#[test]
fn composition_preserves_surrogate_boundaries_and_commits_one_undo_unit() {
    let mut editor = EditorWindow::new(Document::new("a"));
    editor.key(&Keystroke::parse("i").unwrap(), 8).unwrap();
    editor.composition.mark(None, "中🙂", Some(1..3));
    assert_eq!(editor.vim().doc.text, "a");
    assert!(!editor.modified());
    let mut adjusted = None;
    assert_eq!(
        editor
            .composition
            .text_for_range(1..2, &mut adjusted)
            .as_deref(),
        Some("")
    );
    assert_eq!(adjusted, Some(1..1));
    assert_eq!(
        editor
            .composition
            .text_for_range(1..3, &mut adjusted)
            .as_deref(),
        Some("🙂")
    );
    assert_eq!(adjusted, Some(1..3));

    let text = editor.composition.take();
    editor.edit(|vim, register| vim.text(&text, register));
    assert_eq!(editor.vim().doc.text, "中🙂a");
    editor.key(&Keystroke::parse("escape").unwrap(), 8).unwrap();
    editor.key(&Keystroke::parse("u").unwrap(), 8).unwrap();
    assert_eq!(editor.vim().doc.text, "a");
}

/// Native mode keys and page movement are handled locally, while Insert text is reserved for IME.
#[test]
fn native_keys_route_text_and_page_movement_without_duplicate_input() {
    let mut editor = EditorWindow::new(Document::new("one\ntwo\nthree\nfour\nfive"));
    editor.key(&Keystroke::parse("i").unwrap(), 2).unwrap();
    assert_eq!(editor.vim().mode, Mode::Insert);
    assert!(editor.key(&Keystroke::parse("x").unwrap(), 2).is_none());
    assert_eq!(editor.vim().doc.text, "one\ntwo\nthree\nfour\nfive");
    editor.key(&Keystroke::parse("escape").unwrap(), 2).unwrap();
    let page = editor
        .key(&Keystroke::parse("pagedown").unwrap(), 2)
        .unwrap();
    assert!(!page.clear_status);
    assert_eq!(editor.vim().doc.location(editor.vim().doc.cursor).0, 2);
}
