//! Close confirmation, localized input and preservation of on-disk and in-memory text.

use super::*;
use crate::grid::render::LabelStyle;
use gpui::{FontId, Keystroke, NoopTextSystem, OperatingSystem};
use nanobug::i18n::Locale;
use std::{fs, sync::Arc};

/// Type an unsaved exclamation point into the active buffer in `projects`.
fn edit_active(projects: &mut WorkspaceTabs) {
    projects.active_mut().editor.edit(|vim, register| {
        vim.text("i!", register);
        vim.key("Escape", register)
    });
}

/// Match `key` against the current locale's captions, accounting for repeats and `multiple` files.
fn press(key: &str, is_held: bool, multiple: bool) -> Option<Choice> {
    key_choice(
        &KeyDownEvent {
            keystroke: Keystroke::parse_with_os(key, OperatingSystem::Windows).unwrap(),
            is_held,
            prefer_character_input: false,
        },
        multiple,
    )
}

/// Exactly the marked letter is underlined and usable as a shortcut in either shipped locale.
#[test]
fn localized_captions_supply_both_the_underline_and_the_key() {
    let captions = match i18n::locale() {
        Locale::English => ["[Yes]", "[No]", "[Save All]", "[Discard All]", "[Cancel]"],
        Locale::SimplifiedChinese => [
            "[是(Y)]",
            "[否(N)]",
            "[全部保存(A)]",
            "[全部放弃(D)]",
            "[取消(C)]",
        ],
    };
    let keys = ["y", "n", "a", "d", "c"];
    assert_eq!(buttons(true).count(), captions.len());
    for ((choice, label), (caption, key)) in buttons(true).zip(captions.into_iter().zip(keys)) {
        let spans = label.spans();
        assert_eq!(spans.map(|(text, _)| text).concat(), caption);
        let underlined: Vec<_> = spans
            .into_iter()
            .filter_map(|(text, style)| (style == LabelStyle::Key).then_some(text))
            .collect();
        assert_eq!(underlined, [key.to_ascii_uppercase()]);
        assert_eq!(press(key, false, true), Some(choice));
        assert_eq!(press(&format!("shift-{key}"), false, true), Some(choice));
        assert_eq!(press(&format!("alt-{key}"), false, true), Some(choice));
        assert_eq!(press(&format!("ctrl-{key}"), false, true), None);
        assert_eq!(press(key, true, true), None);
    }
    assert_eq!(
        buttons(false).map(|(choice, _)| choice).collect::<Vec<_>>(),
        [Choice::Save, Choice::Discard, Choice::Cancel]
    );
    assert_eq!(press("a", false, false), None);
    assert_eq!(press("d", false, false), None);
    assert_eq!(press("escape", false, true), Some(Choice::Cancel));
    assert_eq!(press("alt-escape", false, true), None);
    assert_eq!(press("enter", false, true), None);
}

/// Clicks hit the drawn labels, and a double click cannot resolve the next file.
#[test]
fn repeated_mouse_presses_do_not_confirm_a_second_file() {
    let spacing = TextSpacing::new(Arc::new(NoopTextSystem::new()), FontId(0), 16., 9.);
    let area = Rect {
        x: 15,
        y: 10,
        w: COLUMNS,
        h: ROWS,
    };
    let row = area.y + area.h - 2;
    for multiple in [false, true] {
        for (choice, label, slot) in button_slots(&spacing, multiple) {
            assert!(slot.end <= COLUMNS - 2);
            let key = label.spans()[2].0.to_ascii_lowercase();
            let keyed = press(&key, false, multiple);
            assert_eq!(keyed, Some(choice));
            for x in slot {
                assert_eq!(
                    click_choice(area, (area.x + x, row), 1, &spacing, multiple),
                    keyed
                );
                assert_eq!(
                    click_choice(area, (area.x + x, row), 2, &spacing, multiple),
                    None
                );
            }
        }
    }
    assert_eq!(
        click_choice(area, (area.x + 1, row), 1, &spacing, true),
        None
    );
    assert_eq!(
        click_choice(area, (area.x + 2, row - 1), 1, &spacing, true),
        None
    );
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
    let mut projects = WorkspaceTabs::new(vec![first.clone()]);
    assert!(EditorClose::new(&projects, CloseTarget::Editor).is_none());
    edit_active(&mut projects);
    projects
        .active_mut()
        .editor
        .open(second.clone(), 0.)
        .unwrap();
    edit_active(&mut projects);

    let mut close = EditorClose::new(&projects, CloseTarget::Editor).unwrap();
    assert_eq!(
        close
            .pending
            .iter()
            .map(|buffer| buffer.index)
            .collect::<Vec<_>>(),
        [1, 0]
    );
    assert!(!close.resolve(Choice::Discard, None, &mut projects));
    assert!(close.resolve(Choice::Cancel, None, &mut projects));
    assert_eq!(projects.active().editor.buffers[0].vim.doc.text, "!first");
    assert_eq!(projects.active().editor.vim().doc.text, "!second\n");
    assert_eq!(fs::read_to_string(&first).unwrap(), "first");
    assert_eq!(fs::read(&second).unwrap(), original);

    let mut close = EditorClose::new(&projects, CloseTarget::Editor).unwrap();
    assert!(!close.resolve(Choice::Discard, None, &mut projects));
    assert!(close.resolve(Choice::Save, None, &mut projects));
    assert_eq!(fs::read_to_string(&first).unwrap(), "!first");
    assert_eq!(projects.active().editor.vim().doc.text, "second\n");
    assert_eq!(fs::read(&second).unwrap(), original);
    assert!(!projects.active().editor.modified());
    projects
        .active_mut()
        .editor
        .edit(|vim, register| vim.key("u", register));
    assert_eq!(projects.active().editor.vim().doc.text, "second\n");
    projects.save_buffer(0, 1, None).unwrap();
    assert_eq!(fs::read(&second).unwrap(), original);
}

/// A failed save keeps the same buffer pending and can be retried after the error is fixed.
#[test]
fn save_failure_keeps_the_draft_and_original_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("read-only.txt");
    fs::write(&path, "original").unwrap();
    let mut projects = WorkspaceTabs::new(vec![path.clone()]);
    edit_active(&mut projects);

    let permissions = fs::metadata(&path).unwrap().permissions();
    let mut read_only = permissions.clone();
    read_only.set_readonly(true);
    fs::set_permissions(&path, read_only).unwrap();
    let mut close = EditorClose::new(&projects, CloseTarget::Editor).unwrap();
    let completed = close.resolve(Choice::Save, None, &mut projects);
    fs::set_permissions(&path, permissions).unwrap();
    assert!(!completed);
    assert!(close.error.is_some());
    assert_eq!(close.pending, [PendingBuffer { tab: 0, index: 0 }]);
    assert_eq!(fs::read_to_string(&path).unwrap(), "original");
    assert_eq!(projects.active().editor.vim().doc.text, "!original");
    assert!(projects.active().editor.modified());

    assert!(close.resolve(Choice::Save, None, &mut projects));
    assert_eq!(fs::read_to_string(&path).unwrap(), "!original");
    assert!(!projects.active().editor.modified());
}

/// Saving an unnamed buffer uses the chosen filename and protects a file already open in the editor.
#[test]
fn untitled_close_can_save_as_without_overwriting_an_open_buffer() {
    let dir = tempfile::tempdir().unwrap();
    let existing = dir.path().join("existing.txt");
    let new = dir.path().join("new.txt");
    fs::write(&existing, "existing").unwrap();
    let mut projects = WorkspaceTabs::new(vec![dir.path().into()]);
    edit_active(&mut projects);
    projects
        .active_mut()
        .editor
        .open(existing.clone(), 0.)
        .unwrap();

    let mut close = EditorClose::new(&projects, CloseTarget::Editor).unwrap();
    assert!(!close.resolve(Choice::Save, Some(existing.clone()), &mut projects));
    assert_eq!(fs::read_to_string(&existing).unwrap(), "existing");
    assert!(projects.active().editor.buffers[0].vim.doc.path.is_none());
    assert!(close.resolve(Choice::Save, Some(new.clone()), &mut projects));
    assert_eq!(fs::read_to_string(&new).unwrap(), "!");
    assert!(!projects.active().editor.modified());
}

/// Quit checks inactive projects even from a start page, and Cancel keeps staged discards.
#[test]
fn quitting_from_start_prompts_for_inactive_projects_and_defers_discards() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first");
    let second = dir.path().join("second");
    for root in [&first, &second] {
        fs::create_dir(root).unwrap();
        fs::write(root.join("README.md"), "draft").unwrap();
    }
    let mut tabs = WorkspaceTabs::new(vec![first.clone(), second.clone()]);
    edit_active(&mut tabs);
    assert!(tabs.select(1));
    edit_active(&mut tabs);
    let project_close = EditorClose::new(&tabs, CloseTarget::Project).unwrap();
    assert_eq!(project_close.pending, [PendingBuffer { tab: 1, index: 0 }]);
    let start = tabs.add_start();
    assert!(tabs.select(start));
    assert!(EditorClose::new(&tabs, CloseTarget::Project).is_none());

    let mut close = EditorClose::new(&tabs, CloseTarget::Quit).unwrap();
    assert_eq!(
        close
            .pending
            .iter()
            .map(|buffer| buffer.tab)
            .collect::<Vec<_>>(),
        [0, 1]
    );
    assert!(!close.resolve(Choice::Discard, None, &mut tabs));
    assert!(close.resolve(Choice::Cancel, None, &mut tabs));
    assert!(
        tabs.ordered()
            .all(|project| project.editor.vim().doc.text == "!draft")
    );
    assert_eq!(tabs.selected, start);

    let mut close = EditorClose::new(&tabs, CloseTarget::Quit).unwrap();
    assert!(!close.resolve(Choice::Discard, None, &mut tabs));
    assert!(close.resolve(Choice::Save, None, &mut tabs));
    assert_eq!(
        fs::read_to_string(first.join("README.md")).unwrap(),
        "draft"
    );
    assert_eq!(
        fs::read_to_string(second.join("README.md")).unwrap(),
        "!draft"
    );
    assert!(tabs.ordered().all(|project| !project.editor.modified()));
}

/// Save All resumes after Save As and writes the files remaining after the untitled buffer.
#[test]
fn save_all_resumes_after_naming_an_untitled_buffer() {
    let dir = tempfile::tempdir().unwrap();
    let [first, second, named] =
        ["first.txt", "second.txt", "named.txt"].map(|name| dir.path().join(name));
    let mut tabs = WorkspaceTabs::new(vec![dir.path().into()]);
    edit_active(&mut tabs);
    for path in [&first, &second] {
        fs::write(path, "draft").unwrap();
        tabs.active_mut().editor.open(path.clone(), 0.).unwrap();
        edit_active(&mut tabs);
    }

    let mut close = EditorClose::new(&tabs, CloseTarget::Project).unwrap();
    assert!(!close.resolve(Choice::SaveAll, None, &mut tabs));
    assert!(close.picking_path);
    assert_eq!(close.pending[0], PendingBuffer { tab: 0, index: 0 });
    assert_eq!(fs::read_to_string(&second).unwrap(), "!draft");
    assert_eq!(fs::read_to_string(&first).unwrap(), "draft");

    close.picking_path = false;
    assert!(close.resolve(Choice::SaveAll, Some(named.clone()), &mut tabs));
    assert_eq!(fs::read_to_string(&named).unwrap(), "!");
    assert_eq!(fs::read_to_string(&first).unwrap(), "!draft");
    assert!(!tabs.active().editor.modified());
}

/// Failed bulk saves leave subsequent drafts pending and eligible for Discard All.
#[test]
fn save_all_stops_on_error_and_discard_all_keeps_the_remaining_files_intact() {
    let dir = tempfile::tempdir().unwrap();
    let [first, failing, last] =
        ["first.txt", "read-only.txt", "last.txt"].map(|name| dir.path().join(name));
    for path in [&first, &failing, &last] {
        fs::write(path, "draft").unwrap();
    }
    let mut tabs = WorkspaceTabs::new(vec![failing.clone()]);
    edit_active(&mut tabs);
    for path in [&last, &first] {
        tabs.active_mut().editor.open(path.clone(), 0.).unwrap();
        edit_active(&mut tabs);
    }
    let permissions = fs::metadata(&failing).unwrap().permissions();
    let mut read_only = permissions.clone();
    read_only.set_readonly(true);
    fs::set_permissions(&failing, read_only).unwrap();

    let mut close = EditorClose::new(&tabs, CloseTarget::Quit).unwrap();
    let completed = close.resolve(Choice::SaveAll, None, &mut tabs);
    fs::set_permissions(&failing, permissions).unwrap();
    assert!(!completed);
    assert!(close.error.is_some());
    assert_eq!(close.pending.len(), 2);
    assert_eq!(close.pending[0].index, 0);
    assert_eq!(fs::read_to_string(&first).unwrap(), "!draft");
    assert!(close.resolve(Choice::DiscardAll, None, &mut tabs));
    for path in [&failing, &last] {
        assert_eq!(fs::read_to_string(path).unwrap(), "draft");
    }
    assert!(!tabs.active().editor.modified());
}

/// A file open in overlapping projects cannot silently lose a draft in a bulk save.
#[test]
fn save_all_requires_individual_decisions_for_duplicate_drafts() {
    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("nested");
    fs::create_dir(&nested).unwrap();
    let path = nested.join("README.md");
    fs::write(&path, "draft").unwrap();
    let mut tabs = WorkspaceTabs::new(vec![dir.path().into(), nested]);
    tabs.active_mut().editor.open(path.clone(), 0.).unwrap();
    edit_active(&mut tabs);
    assert!(tabs.select(1));
    edit_active(&mut tabs);
    edit_active(&mut tabs);

    let mut close = EditorClose::new(&tabs, CloseTarget::Quit).unwrap();
    assert!(!close.resolve(Choice::SaveAll, None, &mut tabs));
    assert_eq!(
        close.error.as_deref(),
        Some(i18n::text("editor_close.duplicate_draft"))
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), "draft");
    assert!(tabs.ordered().all(|project| project.editor.modified()));
}
