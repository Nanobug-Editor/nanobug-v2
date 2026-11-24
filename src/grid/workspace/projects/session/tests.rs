use super::*;
use crate::grid::{
    render::Rect,
    workspace::{editor_splits::SplitAxis, projects::SplitDocument},
};
use std::fs;

/// Give the test tabs an isolated store at `directory`.
fn tabs_with_store(directory: &Path) -> WorkspaceTabs {
    let mut tabs = WorkspaceTabs::new(Vec::new());
    tabs.sessions.directory = Some(directory.to_owned());
    tabs
}

#[test]
fn quickfix_windows_are_transient_and_leave_the_source_layout_intact() -> Result<()> {
    use crate::grid::windows::editor::{Buffer, ResultList};
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?;
    let mut project = Project::new(root.clone(), None);
    let source = project.active_editor_id();
    let list = Buffer::result_list(ResultList::Quickfix, source, "#1|1:1 E| initial");
    let area = Rect {
        x: 0,
        y: 0,
        w: 80,
        h: 40,
    };
    assert!(project.open_split(
        SplitAxis::Rows,
        false,
        SplitDocument::SharedView(Box::new(list)),
        area,
        0.
    )?);
    project
        .editor
        .update_result_rows("#1|2:1 E| updated", Some(0));
    assert!(!project.editor.vim().doc.modified());
    let restored = ProjectSnapshot::capture(&project).restore(root)?;
    assert_eq!(restored.splits().ids(), vec![source]);
    assert_eq!(restored.active_editor_id(), source);
    assert!(restored.editor.buffer().list_owner().is_none());
    Ok(())
}

/// Restore a file mark in another Tabpage, using the editor where the jump starts.
#[test]
fn restores_file_marks_for_each_session() -> Result<()> {
    use nanobug::vim::Action;

    let workspace = tempfile::tempdir()?;
    let root = workspace.path().canonicalize()?;
    let store = root.join("sessions");
    let marked = root.join("README.md");
    let other = root.join("other.txt");
    fs::write(&marked, "first\n\t界x\n")?;
    fs::write(&other, "other\n")?;
    {
        let mut tabs = tabs_with_store(&store);
        tabs.replace_start_with_project(root.clone());
        let project = tabs.active_mut();
        project
            .editor
            .edit(|vim, register| vim.text("2G0lmA", register));
        project.open_page(Some(other.clone()), 0.)?;
        assert!(
            tabs.session_batch(Some(CloseTarget::Tab))
                .unwrap()
                .write()
                .is_empty()
        );
    }

    let mut tabs = tabs_with_store(&store);
    let (id, added) = tabs.add(root.clone(), None);
    assert!(added);
    let project = tabs.project_mut(id);
    let departure = project.active_editor_id();
    assert_eq!(
        project.editor.vim().doc.path.as_deref(),
        Some(other.as_path())
    );
    let Action::OpenFileMark {
        name,
        path,
        row,
        column,
        opening,
        linewise,
    } = project
        .editor
        .edit(|vim, register| vim.text("`A", register))
        .action
    else {
        anyhow::bail!("restored mark did not return a file destination");
    };
    project.jump_file_mark(
        name,
        (opening, path.as_deref()),
        (row, column),
        linewise,
        0.,
    )?;
    assert_eq!(project.active_editor_id(), departure);
    assert_eq!(
        project.editor.vim().doc.path.as_deref(),
        Some(marked.as_path())
    );
    assert_eq!(project.editor.vim().doc.cursor, "first\n\t".len());

    let unrelated = tempfile::tempdir()?;
    let mut project = Project::new(unrelated.path().to_owned(), None);
    assert_eq!(
        project
            .editor
            .edit(|vim, register| vim.text("`A", register))
            .action,
        Action::None
    );
    Ok(())
}

/// Restore nested splits and duplicate file views while rereading text from disk.
#[test]
fn restores_project_pages_panes_and_file_views_without_drafts() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let root = workspace.path().canonicalize()?;
    let store = root.join("sessions");
    let readme = root.join("README.md");
    let source = root.join("source.txt");
    fs::write(&readme, "first\nsecond\nthird\nfourth\n")?;
    fs::write(&source, "source\n")?;

    let mut tabs = tabs_with_store(&store);
    tabs.replace_start_with_project(root.clone());
    let project = tabs.active_mut();
    project.editor.open(source.clone(), 0.)?;
    project.editor.open(readme.clone(), 0.)?;
    let first = project.active_editor_id();
    let buffer = project.editor.buffer_mut();
    buffer.vim.doc.cursor = buffer.vim.doc.offset(1, 2);
    buffer.scroll.snap(1, 0.);
    buffer.columns.snap(2, 0.);
    let area = Rect {
        x: 0,
        y: 0,
        w: 120,
        h: 40,
    };
    assert!(project.open_split(
        SplitAxis::Columns,
        true,
        SplitDocument::File(readme.clone(), None),
        area,
        0.,
    )?);
    let second = project.active_editor_id();
    let buffer = project.editor.buffer_mut();
    buffer.vim.doc.cursor = buffer.vim.doc.offset(2, 3);
    buffer.scroll.snap(2, 0.);
    assert!(
        project
            .editor
            .set_markdown_view_mode(crate::grid::windows::editor::MarkdownViewMode::Edit)
    );
    assert!(project.open_split(
        SplitAxis::Rows,
        true,
        SplitDocument::File(source.clone(), None),
        area,
        0.,
    )?);
    project.sidebar = 33;
    project.open_page(Some(source.clone()), 0.)?;
    assert_eq!(project.active_page(), 1);
    let snapshot = ProjectSnapshot::capture(project);
    project
        .editor
        .edit(|vim, register| vim.text("iPRIVATE DRAFT", register));
    assert!(
        tabs.session_batch(Some(CloseTarget::Tab))
            .unwrap()
            .write()
            .is_empty()
    );
    let bytes = fs::read_to_string(session_path(&store, &root))?;
    assert!(!bytes.contains("PRIVATE DRAFT"));

    let mut reopened = tabs_with_store(&store);
    let (_, added) = reopened.add(root.clone(), None);
    assert!(added);
    let recovered = reopened.project_mut(1);
    assert_eq!(recovered.editor.vim().doc.text, "source\n");
    assert_eq!(recovered.active_page(), 1);
    assert!(ProjectSnapshot::capture(recovered).pages == snapshot.pages);
    assert!(recovered.select_page(0, 0.));
    assert_eq!(recovered.sidebar, 33);
    assert_eq!(recovered.split_count(), 3);
    let shared_number = recovered
        .editor_by_id(first)
        .unwrap()
        .vim()
        .doc
        .buffer_number();
    for (id, row) in [(first, 1), (second, 2)] {
        let pane = recovered.editor_by_id(id).unwrap();
        let buffer = pane.buffer();
        assert_eq!(buffer.vim.doc.path.as_deref(), Some(readme.as_path()));
        assert_eq!(buffer.vim.doc.buffer_number(), shared_number);
        assert_eq!(buffer.vim.doc.location(buffer.vim.doc.cursor).0, row);
        assert_eq!(buffer.scroll.target(), row);
    }
    assert_eq!(recovered.editor_by_id(first).unwrap().buffers().len(), 2);
    assert_eq!(
        recovered
            .editor_by_id(first)
            .unwrap()
            .buffer()
            .columns
            .target(),
        2
    );
    assert_eq!(
        recovered.editor_by_id(second).unwrap().markdown_view_mode(),
        Some(crate::grid::windows::editor::MarkdownViewMode::Edit)
    );
    Ok(())
}

/// Keep the most recent snapshot, skip missing file labels and honor an explicit file request.
#[test]
fn skips_missing_files_and_rejects_an_older_write_after_close() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let root = workspace.path().canonicalize()?;
    let store = root.join("sessions");
    let readme = root.join("README.md");
    let missing = root.join("missing.txt");
    let requested = root.join("requested.txt");
    fs::write(&readme, "first\nlast\n")?;
    fs::write(&missing, "removed\n")?;
    fs::write(&requested, "explicitly opened\n")?;
    let mut tabs = tabs_with_store(&store);
    tabs.replace_start_with_project(root.clone());
    let delayed = tabs.session_batch(None).unwrap();
    let project = tabs.active_mut();
    let cursor = project.editor.vim().doc.offset(1, 3);
    project.editor.buffer_mut().vim.doc.cursor = cursor;
    project.editor.open(missing.clone(), 0.)?;
    project.open_page(Some(readme), 0.)?;
    assert!(
        tabs.session_batch(Some(CloseTarget::Quit))
            .unwrap()
            .write()
            .is_empty()
    );
    assert!(delayed.write().is_empty());
    assert_eq!(fs::read_dir(&store)?.count(), 1);
    fs::remove_file(missing)?;

    let mut recovered = tabs.sessions.open(root, Some(requested.clone()));
    assert_eq!(recovered.page_count(), 2);
    assert_eq!(
        recovered.editor.vim().doc.path.as_deref(),
        Some(requested.as_path())
    );
    assert_eq!(
        recovered.status,
        i18n::format("project_session.skipped_files", &[("count", &1)])
    );
    assert!(recovered.select_page(0, 0.));
    assert_eq!(recovered.editor.buffers().len(), 1);
    assert_eq!(
        recovered
            .editor
            .vim()
            .doc
            .location(recovered.editor.vim().doc.cursor),
        (1, 3)
    );
    Ok(())
}

/// Reopen the Explorer by path after rows shift, and accept snapshots made before window state was saved.
#[test]
fn restores_explorer_navigation_and_reads_older_sessions() -> Result<()> {
    use nanobug::animation::scroll::ScrollInput;

    let workspace = tempfile::tempdir()?;
    let store = tempfile::tempdir()?;
    let root = workspace.path().canonicalize()?;
    let selected = root.join("src/selected.txt");
    let deleted = root.join("src/deleted.txt");
    fs::create_dir(root.join("src"))?;
    fs::write(&selected, "selected")?;
    fs::write(&deleted, "deleted")?;

    let mut tabs = tabs_with_store(store.path());
    tabs.replace_start_with_project(root.clone());
    let manager = tabs.active_mut().manager.as_mut().unwrap();
    assert!(manager.reveal_path(&deleted)?);
    manager.key("s", None);
    assert!(manager.select_path(&selected));
    manager.key("s", None);
    let row = manager.tree().selected;
    manager.track_view(ScrollInput::Position(row), 1, 0.);
    assert!(tabs.session_batch(None).unwrap().write().is_empty());

    fs::remove_file(deleted)?;
    for folder in ["aaa", "bbb"] {
        fs::create_dir(root.join(folder))?;
    }
    let reopened = tabs.sessions.open(root.clone(), None);
    let manager = reopened.manager.as_ref().unwrap();
    assert!(manager.tree().expanded.contains(&root.join("src")));
    assert_eq!(manager.tree().marked, [selected.clone()].into());
    assert_eq!(
        manager.tree().entries[manager.tree().selected].path,
        selected
    );
    assert_eq!(manager.scroll().target(), manager.tree().selected);
    assert_ne!(manager.tree().selected, row);

    let path = session_path(store.path(), &root);
    let mut original: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
    let original = original.as_object_mut().unwrap();
    original.remove("explorer");
    original.remove("plugin_windows");
    fs::write(path, serde_json::to_vec(original)?)?;
    let reopened = tabs.sessions.open(root, None);
    assert!(reopened.status.is_empty());
    assert!(reopened.manager.unwrap().tree().expanded.is_empty());
    Ok(())
}
