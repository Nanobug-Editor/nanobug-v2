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

/// Restart restores the selected tab's role and never retargets Ex to another project.
#[test]
fn restart_preserves_selection_and_drops_commands_for_missing_targets() {
    use crate::app::{Restart, RestartTab};

    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().to_path_buf();
    let file = root.join("standalone.txt");
    fs::write(&file, "selected file").unwrap();
    let (tabs, command) = WorkspaceTabs::restore_restart(
        Restart {
            tabs: vec![
                RestartTab::Project(root.clone()),
                RestartTab::File(file.clone()),
            ],
            selected: 1,
            command: "write copy.txt".into(),
        },
        false,
    );
    assert_eq!(tabs.len(), 2);
    assert!(tabs.project(0).manager.is_some());
    assert!(tabs.active().manager.is_none());
    assert_eq!(tabs.active().editor.vim().doc.text, "selected file");
    assert_eq!(command.as_deref(), Some("write copy.txt"));

    fs::remove_file(&file).unwrap();
    let (tabs, command) = WorkspaceTabs::restore_restart(
        Restart {
            tabs: vec![RestartTab::Project(root), RestartTab::File(file)],
            selected: 1,
            command: "write copy.txt".into(),
        },
        false,
    );
    assert!(tabs.start().is_some());
    assert!(command.is_none());
}

/// Inactive Tabpages keep their own Explorer geometry and visible editor panes.
#[test]
fn retained_tabpage_editor_rects_use_their_own_layout() {
    let directory = tempfile::tempdir().unwrap();
    let mut project = Project::new(directory.path().into(), None);
    let mut layout = Layout::default();
    layout.cols = 100;
    layout.rows = 30;
    layout.visible_cols = 100;
    layout.visible_rows = 30;
    project.sidebar = 20;
    project.open_page(None, 0.).unwrap();
    project.stacked = true;
    project.manager_height = Some(8);
    let (_, active_region) = project.pane_regions_for_tabpage(1, layout);
    assert_eq!((active_region.w, active_region.h), (100, 20));
    assert!(
        project
            .open_split(
                SplitAxis::Columns,
                false,
                SplitDocument::Empty,
                active_region,
                0.
            )
            .unwrap()
    );

    let (_, retained_region) = project.pane_regions_for_tabpage(0, layout);
    assert_eq!((retained_region.w, retained_region.h), (79, 28));
    assert_eq!(
        project
            .editor_rects_for_tabpage(0, layout, retained_region)
            .len(),
        1
    );
    assert_eq!(
        project
            .editor_rects_for_tabpage(1, layout, active_region)
            .len(),
        2
    );

    project.toggle_editor_zoom();
    let zoomed = project.active_editor_id();
    assert!(project.select_page(0, 0.));
    project.hidden_pane = Some(Focus::Editor);
    let (_, hidden) = project.pane_regions_for_tabpage(0, layout);
    assert_eq!(hidden.w, 0);
    assert!(
        project.editor_rects_for_tabpage(1, layout, active_region) == [(zoomed, active_region)]
    );
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
    assert!(projects.close_tab());
    assert_eq!(projects.active().name(), "third");
    assert_eq!(projects.selected, 1);
    assert!(projects.project(0).editor.modified());
    assert!(projects.close_tab());
    assert_eq!(projects.active().name(), "first");
    assert_eq!(projects.selected, 0);
    assert!(projects.active().editor.modified());

    projects.active_mut().editor.open(edited, 0.).unwrap();
    assert_eq!(projects.active().editor.vim().doc.text, "!first");
    projects
        .save_buffer(0, projects.active().editor.active_index(), None)
        .unwrap();
    assert!(projects.close_tab());
    assert_eq!(projects.len(), 1);
    assert!(projects.start().is_some());
    assert!(!projects.close_tab());
}

/// Closing a start tab returns to the most recent tab and preserves its draft.
#[test]
fn start_page_close_returns_to_the_most_recent_tab() {
    let directory = tempfile::tempdir().unwrap();
    let mut tabs = projects_in(directory.path());
    assert!(tabs.select(1));
    tabs.active_mut()
        .editor
        .edit(|vim, register| vim.text("i!", register));
    let start = tabs.add_start();
    assert!(tabs.select(start));
    let other_start = tabs.add_start();
    assert!(tabs.select(other_start));
    tabs.move_selected(0);
    assert!(tabs.close_tab());
    assert!(tabs.start().is_some());
    assert!(tabs.close_tab());
    assert_eq!(tabs.active().name(), "second");
    assert_eq!(tabs.active().editor.vim().doc.text, "!second");
    assert!(tabs.active().editor.modified());
}

/// Closing the final project leaves a new start page selected.
#[test]
fn closing_last_project_leaves_one_start_page() {
    let directory = tempfile::tempdir().unwrap();
    let mut tabs = WorkspaceTabs::new(vec![directory.path().to_path_buf()]);
    assert!(tabs.close_tab());
    assert_eq!(tabs.len(), 1);
    assert_eq!(tabs.selected, 0);
    assert!(tabs.start().is_some());
    assert_eq!(tabs.ordered().count(), 0);
}

/// Visit a file already edited on another Tabpage without changing its pane or rereading disk.
#[test]
fn session_file_jump_keeps_the_source_pane_and_reuses_undo() -> anyhow::Result<()> {
    let folder = tempfile::tempdir()?;
    let root = folder.path().canonicalize()?;
    let source = root.join("README.md");
    let destination = root.join("destination.txt");
    fs::write(&source, "source\n")?;
    fs::write(&destination, "disk\n")?;
    let mut project = Project::new(root, None);
    let source_pane = project.active_editor_id();
    project.open_page(Some(destination.clone()), 0.)?;
    let destination_pane = project.active_editor_id();
    project
        .editor
        .edit(|vim, register| vim.text("ifresh ", register));
    let destination_cursor = project.editor.vim().doc.cursor;
    let destination_number = project.editor.vim().doc.buffer_number();

    assert!(project.select_page(0, 0.));
    assert!(project.jump_file(&destination, true, 0., |_| Some(0))?);
    assert_eq!(project.active_editor_id(), source_pane);
    assert_eq!(project.editor.vim().doc.text, "fresh disk\n");
    assert_eq!(project.editor.vim().doc.buffer_number(), destination_number);
    assert_eq!(project.session_buffers().len(), 2);
    assert_eq!(project.editor.buffers().len(), 2);
    assert_eq!(
        project
            .editor_by_id(destination_pane)
            .unwrap()
            .buffers()
            .len(),
        1
    );
    assert_eq!(
        project
            .editor_by_id(destination_pane)
            .unwrap()
            .vim()
            .doc
            .cursor,
        destination_cursor
    );

    project.editor.edit(|vim, register| vim.text("u", register));
    project.refresh_shared_documents();
    let peer = &project.editor_by_id(destination_pane).unwrap().vim().doc;
    assert_eq!(peer.text, "disk\n");
    assert!(!peer.modified());
    assert!(project.visit_editor_jump(false, 1, 0.));
    assert_eq!(
        project.editor.vim().doc.path.as_deref(),
        Some(source.as_path())
    );
    assert!(project.remove_session_buffer(destination_number, 0.));
    assert_eq!(project.page_count(), 2);
    assert!(
        project
            .editor_by_id(destination_pane)
            .unwrap()
            .vim()
            .doc
            .path
            .is_none()
    );
    Ok(())
}

/// Start a new Tabpage with a blank editor and Explorer even if Explorer was hidden.
#[test]
fn tabnew_without_path_starts_blank() -> anyhow::Result<()> {
    let folder = tempfile::tempdir()?;
    let root = folder.path().canonicalize()?;
    fs::write(root.join("README.md"), "keep this file\n")?;
    let mut project = Project::new(root, None);
    project.hidden_pane = Some(Focus::Manager);
    project.open_page(None, 0.)?;
    assert_eq!(project.split_count(), 1);
    assert!(project.manager.is_some());
    assert_eq!(project.hidden_pane, None);
    assert!(project.editor.vim().doc.path.is_none());
    assert!(project.editor.vim().doc.text.is_empty());
    assert!(project.select_page(0, 0.));
    assert_eq!(project.hidden_pane, Some(Focus::Manager));
    assert_eq!(project.editor.vim().doc.text, "keep this file\n");
    Ok(())
}
