//! Close confirmation, localized input and preservation of on-disk and in-memory text.

use super::*;
use crate::grid::render::LabelStyle;
use crate::grid::workspace::projects::SplitDocument;
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

/// Closing one label asks about its own draft and preserves another page's draft.
#[test]
fn file_labels_close_only_their_own_page_draft() {
    let directory = tempfile::tempdir().unwrap();
    let mut projects = WorkspaceTabs::new(vec![directory.path().to_path_buf()]);
    edit_active(&mut projects);
    let original = projects.active().editor.buffer().id;
    projects.active_mut().open_page(None, 0.).unwrap();
    edit_active(&mut projects);
    let buffer = projects.active().editor.buffer().id;
    assert_ne!(buffer, original);
    let mut close = EditorClose::new(&projects, CloseTarget::Buffer(buffer)).unwrap();
    assert_eq!(close.pending.len(), 1);
    assert!(close.resolve(Choice::Discard, None, &mut projects));
    assert!(projects.active_mut().editor.remove_buffer_tab(buffer, 0.));
    assert!(projects.active().editor.buffer_index(buffer).is_none());
    assert!(projects.active_mut().select_page(0, 0.));
    assert_eq!(projects.active().editor.buffer().id, original);
    assert_eq!(projects.active().editor.vim().doc.text, "!");
}

/// A range close prompts only for the selected sides and leaves the reference draft intact.
#[test]
fn range_close_queues_only_its_own_labels() {
    let directory = tempfile::tempdir().unwrap();
    let mut projects = WorkspaceTabs::new(vec![directory.path().to_path_buf()]);
    edit_active(&mut projects);
    let editor = &mut projects.active_mut().editor;
    let middle = editor.retain_document(Document::new("middle"));
    let reference = editor.buffers()[middle].id;
    let right = editor.retain_document(Document::new("right"));
    let right_label = editor.buffers()[right].id;
    assert!(editor.select_buffer(right_label, 0.));
    edit_active(&mut projects);
    assert!(projects.active_mut().editor.select_buffer(reference, 0.));
    edit_active(&mut projects);

    for (range, expected) in [
        (BufferRange::Left, vec![0]),
        (BufferRange::Right, vec![right]),
        (BufferRange::Others, vec![0, right]),
    ] {
        let target = CloseTarget::BufferRange(reference, range);
        let mut close = EditorClose::new(&projects, target).unwrap();
        assert_eq!(
            close
                .pending
                .iter()
                .map(|buffer| buffer.index())
                .collect::<Vec<_>>(),
            expected
        );
        assert!(close.resolve(Choice::Cancel, None, &mut projects));
        assert!(
            projects
                .active()
                .editor
                .buffers()
                .iter()
                .all(|buffer| buffer.vim.doc.modified())
        );
    }
}

/// A counted close resolves the addressed split without changing its sibling's draft.
#[test]
fn split_close_uses_the_addressed_pane() {
    use super::super::editor_splits::SplitAxis;

    let directory = tempfile::tempdir().unwrap();
    let mut projects = WorkspaceTabs::new(vec![directory.path().to_path_buf()]);
    let original = projects.active().active_editor_id();
    edit_active(&mut projects);
    let area = Rect {
        x: 0,
        y: 0,
        w: 100,
        h: 30,
    };
    assert!(
        projects
            .active_mut()
            .open_split(SplitAxis::Columns, true, SplitDocument::Empty, area, 0.)
            .unwrap()
    );
    let sibling = projects.active().active_editor_id();
    edit_active(&mut projects);

    let mut close = EditorClose::new(&projects, CloseTarget::Split(original)).unwrap();
    assert_eq!(close.pending, [PendingBuffer::pane(0, original, 0)]);
    assert!(close.resolve(Choice::Discard, None, &mut projects));
    let project = projects.active();
    assert!(!project.editor_by_id(original).unwrap().modified());
    assert_eq!(project.active_editor_id(), sibling);
    assert_eq!(project.editor.vim().doc.text, "!");
}

/// Middle-click resolves the clicked title's draft in its owning split.
#[test]
fn editor_title_close_scopes_the_draft_to_its_split() {
    use super::super::editor_splits::SplitAxis;

    let directory = tempfile::tempdir().unwrap();
    let mut projects = WorkspaceTabs::new(vec![directory.path().to_path_buf()]);
    edit_active(&mut projects);
    let project = projects.active_mut();
    let pane = project.active_editor_id();
    let first = project.editor.buffer().id;
    project.editor.retain_document(Document::new("second"));
    let area = Rect {
        x: 0,
        y: 0,
        w: 100,
        h: 30,
    };
    assert!(
        project
            .open_split(SplitAxis::Columns, true, SplitDocument::Empty, area, 0.)
            .unwrap()
    );
    let focused = project.active_editor_id();
    assert_ne!(focused, pane);

    let target = editor_title_close_target(&projects, pane, first).unwrap();
    assert_eq!(target, CloseTarget::EditorTitle(pane, first));
    let mut close = EditorClose::new(&projects, target).unwrap();
    assert_eq!(close.pending, [PendingBuffer::pane(0, pane, 0)]);
    assert!(close.resolve(Choice::Discard, None, &mut projects));
    let project = projects.active_mut();
    assert!(
        project
            .editor_by_id_mut(pane)
            .unwrap()
            .remove_buffer_tab(first, 0.)
    );
    assert_eq!(project.active_editor_id(), focused);
}

/// Close the last titles in a split and a sole editor while keeping Explorer focused.
#[cfg(windows)]
#[test]
#[ignore = "requires the Windows DirectWrite and DirectX renderer"]
fn native_last_editor_title_stays_closed_after_reopening_the_editor() -> anyhow::Result<()> {
    use super::super::{editor_splits::SplitAxis, native_test};
    use anyhow::ensure;
    use nanobug::plugins::session::Options;

    let temp = tempfile::tempdir()?;
    let root = temp.path().join("project");
    fs::create_dir(&root)?;
    fs::write(root.join("README.md"), "closed file")?;
    let next = root.join("next.txt");
    fs::write(&next, "next file")?;
    let options = Options::take(
        &mut vec![
            "--plugin-profile".into(),
            temp.path().join("profile").into_os_string(),
            "--no-plugins".into(),
        ],
        temp.path(),
    )?;
    native_test::run_async(
        root,
        options,
        |_| {},
        async move |handle, app| {
            app.update(|app| {
                handle.update(app, |workspace, window, cx| -> anyhow::Result<()> {
                    let project = workspace.projects.active_mut();
                    let pane = project.active_editor_id();
                    let title = project.editor.buffer().id;
                    ensure!(project.open_split(
                        SplitAxis::Columns,
                        true,
                        SplitDocument::Current,
                        Rect {
                            x: 0,
                            y: 0,
                            w: 100,
                            h: 30
                        },
                        0.,
                    )?);
                    let sibling = project.active_editor_id();
                    let sibling_title = project.editor.buffer().id;
                    project.focus_pane(Focus::Manager);

                    workspace.close_editor_title(sibling, sibling_title, window, cx);
                    let project = workspace.projects.active();
                    ensure!(project.split_count() == 1);
                    ensure!(project.active_editor_id() == pane);
                    ensure!(project.focus == Focus::Manager);

                    workspace.close_editor_title(pane, title, window, cx);
                    let project = workspace.projects.active();
                    ensure!(project.hidden_pane == Some(Focus::Editor));
                    ensure!(project.focus == Focus::Manager);

                    workspace.open(next, window, cx);
                    let project = workspace.projects.active();
                    ensure!(project.hidden_pane.is_none());
                    ensure!(project.editor.buffer_index(title).is_none());
                    ensure!(project.editor.buffers().len() == 1);
                    ensure!(project.editor.vim().doc.text == "next file");
                    Ok(())
                })
            })??;
            Ok(())
        },
    )
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
            .map(|buffer| buffer.index())
            .collect::<Vec<_>>(),
        [1, 0]
    );
    assert!(!close.resolve(Choice::Discard, None, &mut projects));
    assert!(close.resolve(Choice::Cancel, None, &mut projects));
    assert_eq!(projects.active().editor.buffers()[0].vim.doc.text, "!first");
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
    assert_eq!(close.pending, [PendingBuffer::pane(0, 0, 0)]);
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
    assert!(projects.active().editor.buffers()[0].vim.doc.path.is_none());
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
    let project_close = EditorClose::new(&tabs, CloseTarget::Tab).unwrap();
    assert_eq!(project_close.pending, [PendingBuffer::pane(1, 0, 0)]);
    let start = tabs.add_start();
    assert!(tabs.select(start));
    assert!(EditorClose::new(&tabs, CloseTarget::Tab).is_none());

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

    let mut close = EditorClose::new(&tabs, CloseTarget::Tab).unwrap();
    assert!(!close.resolve(Choice::SaveAll, None, &mut tabs));
    assert!(close.picking_path);
    assert_eq!(close.pending[0], PendingBuffer::pane(0, 0, 0));
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
    assert_eq!(close.pending[0].index(), 0);
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
