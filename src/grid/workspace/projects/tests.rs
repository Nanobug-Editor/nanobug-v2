//! Project state, tab ordering and activation history regressions.

use super::*;

/// Open three distinct projects rooted under the test `directory`.
fn projects_in(directory: &Path) -> WorkspaceTabs {
    let paths = ["first", "second", "third"]
        .into_iter()
        .map(|name| {
            let path = directory.join(name);
            fs::create_dir(&path).unwrap();
            fs::write(path.join("README.md"), name).unwrap();
            path
        })
        .collect();
    WorkspaceTabs::new(paths)
}

/// Reorder and revisit a project without losing its dirty buffer or pane focus.
#[test]
fn switching_and_reordering_retain_the_project() {
    let directory = tempfile::tempdir().unwrap();
    let mut projects = projects_in(directory.path());
    let first = fs::canonicalize(directory.path().join("first")).unwrap();
    assert_eq!(projects.add(first, None), (0, false));
    assert_eq!(projects.len(), 3);
    let project = projects.active_mut();
    project
        .editor
        .edit(|vim, register| vim.text("i!", register));
    project.focus = Focus::Manager;

    assert!(projects.select(1));
    assert_eq!(projects.active().editor.vim().doc.text, "second");
    assert!(projects.project(0).editor.modified());
    projects.move_selected(2);
    assert_eq!(
        projects
            .ordered()
            .map(|project| project.name().into_owned())
            .collect::<Vec<_>>(),
        ["first", "third", "second"]
    );
    assert!(projects.select(0));
    assert_eq!(projects.active().focus, Focus::Manager);
    assert_eq!(projects.active().editor.vim().doc.text, "!first");
    assert!(projects.select(2));
    assert_eq!(projects.active().name(), "second");
}

/// Opening a directory replaces the selected start page in place.
#[test]
fn opening_project_replaces_start_tab() {
    let directory = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(directory.path()).unwrap();
    let mut tabs = WorkspaceTabs::new(Vec::new());
    let revision = tabs.revision;

    tabs.replace_start_with_project(root.clone());

    assert_eq!(tabs.len(), 1);
    assert_eq!(tabs.selected, 0);
    assert_eq!(tabs.active().root, root);
    assert!(tabs.revision > revision);
}

/// Selecting a file in the picker transfers its retained buffer to an editor-only tab.
#[test]
fn standalone_file_reuses_its_tab_and_keeps_unsaved_edits() {
    let directory = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(directory.path()).unwrap();
    let path = root.join("README.md");
    fs::write(&path, "hello").unwrap();
    let mut tabs = WorkspaceTabs::new(vec![root.clone()]);
    tabs.active_mut()
        .editor
        .edit(|vim, register| vim.text("i!", register));

    let file = tabs.add_file(&path).unwrap();
    assert_eq!(tabs.add(root.clone(), None), (0, false));
    assert_eq!(tabs.add_file(&path).unwrap(), file);
    assert!(tabs.manager_available());
    assert!(tabs.select(file));
    assert!(!tabs.manager_available());
    assert_eq!(tabs.active().name(), "README.md");
    assert_eq!(tabs.active().root, root);
    assert_eq!(tabs.active().editor.vim().doc.text, "!hello");
    assert!(tabs.active().editor.modified());
    assert!(!tabs.project(0).editor.modified());
}

/// A file in the same folder does not claim the directory's project tab.
#[test]
fn directory_can_open_as_a_project_after_opening_a_file() {
    let directory = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(directory.path()).unwrap();
    let path = root.join("notes.txt");
    fs::write(&path, "text").unwrap();
    let mut tabs = WorkspaceTabs::new(Vec::new());
    let file = tabs.add_file(&path).unwrap();

    tabs.replace_start_with_project(root);
    assert!(tabs.manager_available());
    assert_eq!(tabs.file_index(&path), Some(file));
}

/// Reopening a loaded project moves its unsaved buffers to the current slot from either side.
#[test]
fn opening_existing_project_moves_its_retained_state() {
    let directory = tempfile::tempdir().unwrap();
    let mut tabs = projects_in(directory.path());
    let root = tabs.active().root.clone();
    let project = tabs.active_mut();
    project
        .editor
        .edit(|vim, register| vim.text("i!", register));
    project.focus = Focus::Manager;

    for (start_slot, expected_index, expected_order) in [
        (2, 1, ["second", "first", "third"]),
        (0, 0, ["first", "second", "third"]),
    ] {
        let start = tabs.add_start();
        assert!(tabs.select(start));
        tabs.move_selected(start_slot);
        tabs.replace_start_with_project(root.clone());
        assert_eq!(tabs.len(), 3);
        assert_eq!(tabs.selected, expected_index);
        assert_eq!(
            tabs.ordered()
                .map(|project| project.name().into_owned())
                .collect::<Vec<_>>(),
            expected_order
        );
        assert_eq!(tabs.active().focus, Focus::Manager);
        assert_eq!(tabs.active().editor.vim().doc.text, "!first");
        assert!(tabs.active().editor.modified());
    }
    assert_eq!(fs::read_to_string(root.join("README.md")).unwrap(), "first");
}

/// Closing clean projects preserves other buffers and falls back to a start page at the end.
#[test]
fn closing_selects_the_neighbor_then_a_start_page() {
    let directory = tempfile::tempdir().unwrap();
    let mut projects = projects_in(directory.path());
    let project = projects.active_mut();
    let edited = project.root.join("README.md");
    let clean = project.root.join("clean.txt");
    fs::write(&clean, "clean").unwrap();
    project
        .editor
        .edit(|vim, register| vim.text("i!", register));
    project.editor.open(clean, 0.).unwrap();
    assert!(project.editor.modified());

    assert!(projects.select(1));
    projects.close_project();
    assert_eq!(projects.active().name(), "third");
    assert_eq!(projects.selected, 1);
    assert!(projects.project(0).editor.modified());
    projects.close_project();
    assert_eq!(projects.active().name(), "first");
    assert_eq!(projects.selected, 0);
    assert!(projects.active().editor.modified());

    projects.active_mut().editor.open(edited, 0.).unwrap();
    assert_eq!(projects.active().editor.vim().doc.text, "!first");
    projects
        .save_buffer(0, projects.active().editor.active, None)
        .unwrap();
    projects.close_project();
    assert_eq!(projects.len(), 1);
    assert!(projects.start().is_some());
    assert!(!projects.close_start());
}

/// Start tabs close in activation order even after the strip was reordered.
#[test]
fn start_page_close_returns_to_the_most_recent_tab() {
    let directory = tempfile::tempdir().unwrap();
    let mut tabs = projects_in(directory.path());
    assert!(tabs.select(1));
    let start = tabs.add_start();
    assert!(tabs.select(start));
    let other_start = tabs.add_start();
    assert!(tabs.select(other_start));
    tabs.move_selected(0);
    assert!(tabs.close_start());
    assert!(tabs.start().is_some());
    assert!(tabs.close_start());
    assert_eq!(tabs.active().name(), "second");
}
