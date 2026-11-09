//! Close confirmation, localized input and preservation of on-disk and in-memory text.

use super::*;
use gpui::Keystroke;
use nanobug::i18n::Locale;
use std::fs;

/// Type an unsaved exclamation point into the active buffer in `projects`.
fn edit_active(projects: &mut Projects) {
    projects.active.editor.edit(|vim, register| {
        vim.text("i!", register);
        vim.key("Escape", register)
    });
}

/// Match a `key` and its repeat flag against the current locale's captions.
fn press(key: &'static str, is_held: bool) -> Option<Choice> {
    key_choice(
        &KeyDownEvent {
            keystroke: Keystroke::parse(key).unwrap(),
            is_held,
            prefer_character_input: false,
        },
        buttons(),
    )
}

/// Exactly the marked letter is underlined and usable as a shortcut in either shipped locale.
#[test]
fn localized_captions_supply_both_the_underline_and_the_key() {
    let captions = match i18n::locale() {
        Locale::English => ["[Yes]", "[No]", "[Cancel]"],
        Locale::SimplifiedChinese => ["[是(Y)]", "[否(N)]", "[取消(C)]"],
    };
    let keys = [
        ("y", "alt-y", "ctrl-y"),
        ("n", "alt-n", "ctrl-n"),
        ("c", "alt-c", "ctrl-c"),
    ];
    for ((choice, label), (caption, (key, alt, control))) in
        buttons().into_iter().zip(captions.into_iter().zip(keys))
    {
        let spans = label.spans();
        assert_eq!(spans.map(|(text, _)| text).concat(), caption);
        let underlined: Vec<_> = spans
            .into_iter()
            .filter_map(|(text, style)| (style == LabelStyle::Key).then_some(text))
            .collect();
        assert_eq!(underlined, [key.to_ascii_uppercase()]);
        assert_eq!(press(key, false), Some(choice));
        assert_eq!(press(alt, false), Some(choice));
        assert_eq!(press(control, false), None);
        assert_eq!(press(key, true), None);
    }
    assert_eq!(press("escape", false), Some(Choice::Cancel));
    assert_eq!(press("alt-escape", false), None);
    assert_eq!(press("enter", false), None);
}

/// Clean buffers close directly. Cancel preserves staged discards, including in inactive buffers.
#[test]
fn closing_multiple_buffers_saves_and_discards_only_the_chosen_files() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first.txt");
    let second = dir.path().join("second.txt");
    fs::write(&first, "first").unwrap();
    let original = b"\xef\xbb\xbfsecond\r\n";
    fs::write(&second, original).unwrap();
    let mut projects = Projects::new(vec![first.clone()]);
    assert!(EditorClose::new(&projects.active.editor).is_none());
    edit_active(&mut projects);
    projects.active.editor.open(second.clone(), 0.).unwrap();
    edit_active(&mut projects);

    let mut close = EditorClose::new(&projects.active.editor).unwrap();
    assert_eq!(close.pending, [1, 0]);
    assert!(!close.resolve(Choice::Discard, None, &mut projects));
    assert!(close.resolve(Choice::Cancel, None, &mut projects));
    assert_eq!(projects.active.editor.buffers[0].vim.doc.text, "!first");
    assert_eq!(projects.active.editor.vim().doc.text, "!second\n");
    assert_eq!(fs::read_to_string(&first).unwrap(), "first");
    assert_eq!(fs::read(&second).unwrap(), original);

    let mut close = EditorClose::new(&projects.active.editor).unwrap();
    assert!(!close.resolve(Choice::Discard, None, &mut projects));
    assert!(close.resolve(Choice::Save, None, &mut projects));
    assert_eq!(fs::read_to_string(&first).unwrap(), "!first");
    assert_eq!(projects.active.editor.vim().doc.text, "second\n");
    assert_eq!(fs::read(&second).unwrap(), original);
    assert!(!projects.active.editor.modified());
    projects
        .active
        .editor
        .edit(|vim, register| vim.key("u", register));
    assert_eq!(projects.active.editor.vim().doc.text, "second\n");
    projects.save_buffer(1, None).unwrap();
    assert_eq!(fs::read(&second).unwrap(), original);
}

/// A failed save keeps the same buffer pending and can be retried after the error is fixed.
#[test]
fn save_failure_keeps_the_draft_and_original_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("read-only.txt");
    fs::write(&path, "original").unwrap();
    let mut projects = Projects::new(vec![path.clone()]);
    edit_active(&mut projects);

    let permissions = fs::metadata(&path).unwrap().permissions();
    let mut read_only = permissions.clone();
    read_only.set_readonly(true);
    fs::set_permissions(&path, read_only).unwrap();
    let mut close = EditorClose::new(&projects.active.editor).unwrap();
    let completed = close.resolve(Choice::Save, None, &mut projects);
    fs::set_permissions(&path, permissions).unwrap();
    assert!(!completed);
    assert!(close.error.is_some());
    assert_eq!(close.pending, [0]);
    assert_eq!(fs::read_to_string(&path).unwrap(), "original");
    assert_eq!(projects.active.editor.vim().doc.text, "!original");
    assert!(projects.active.editor.modified());

    assert!(close.resolve(Choice::Save, None, &mut projects));
    assert_eq!(fs::read_to_string(&path).unwrap(), "!original");
    assert!(!projects.active.editor.modified());
}

/// Saving an unnamed buffer uses the chosen filename and protects a file already open in the editor.
#[test]
fn untitled_close_can_save_as_without_overwriting_an_open_buffer() {
    let dir = tempfile::tempdir().unwrap();
    let existing = dir.path().join("existing.txt");
    let new = dir.path().join("new.txt");
    fs::write(&existing, "existing").unwrap();
    let mut projects = Projects::new(vec![dir.path().into()]);
    edit_active(&mut projects);
    projects.active.editor.open(existing.clone(), 0.).unwrap();

    let mut close = EditorClose::new(&projects.active.editor).unwrap();
    assert!(!close.resolve(Choice::Save, Some(existing.clone()), &mut projects));
    assert_eq!(fs::read_to_string(&existing).unwrap(), "existing");
    assert!(projects.active.editor.buffers[0].vim.doc.path.is_none());
    assert!(close.resolve(Choice::Save, Some(new.clone()), &mut projects));
    assert_eq!(fs::read_to_string(&new).unwrap(), "!");
    assert!(!projects.active.editor.modified());
}
