//! Cross-window regressions using actual filesystem transfers and editor-owned buffers.

use super::{
    editor::EditorWindow,
    manager::{ManagerCommand, ManagerWindow},
};
use nanobug::document::Document;
use std::{fs, path::Path};

/// Select a manager entry by `path`, avoiding assumptions about filesystem enumeration order.
fn select(manager: &mut ManagerWindow, path: &Path) {
    manager.tree.selected = manager
        .tree
        .entries
        .iter()
        .position(|entry| entry.path == path)
        .unwrap();
}

/// A cut retargets the open buffer; a subsequent text edit invalidates undo for that transfer.
#[test]
fn manager_move_retargets_editor_and_edit_invalidates_transfer_undo() {
    let project = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(project.path()).unwrap();
    let source = root.join("source.txt");
    let destination = root.join("destination");
    fs::write(&source, "saved").unwrap();
    fs::create_dir(&destination).unwrap();
    let mut editor = EditorWindow::new(Document::open(&source).unwrap());
    let mut manager = ManagerWindow::new(root.clone());
    select(&mut manager, &source);
    manager.key("s", None).unwrap();
    manager.key("x", None).unwrap();
    select(&mut manager, &destination);
    let (changes, _) = manager.transfer("v", &root, &editor.dirty_paths());
    assert!(!changes.is_empty());
    assert!(editor.apply_path_changes(&changes));
    let moved = destination.join("source.txt");
    assert_eq!(editor.vim().doc.path.as_deref(), Some(moved.as_path()));
    assert!(!source.exists());
    assert_eq!(fs::read_to_string(&moved).unwrap(), "saved");

    let edit = editor.edit(|vim, register| vim.text("i!", register));
    assert_eq!(edit.changed_path.as_deref(), Some(moved.as_path()));
    manager
        .transfers
        .invalidate(edit.changed_path.as_deref().unwrap());
    let (changes, _) = manager.transfer("z", &root, &editor.dirty_paths());
    assert!(changes.is_empty());
    assert!(moved.exists());
    assert!(!source.exists());
    assert_eq!(editor.vim().doc.text, "!saved");
}

/// The manager receives dirty paths from the editing window and refuses a stale on-disk copy.
#[test]
fn unsaved_editor_buffers_guard_manager_transfers() {
    let project = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(project.path()).unwrap();
    let source = root.join("source.txt");
    let destination = root.join("destination");
    fs::write(&source, "saved").unwrap();
    fs::create_dir(&destination).unwrap();
    let mut editor = EditorWindow::new(Document::open(&source).unwrap());
    let mut manager = ManagerWindow::new(root.clone());
    select(&mut manager, &source);
    manager.key("s", None).unwrap();
    manager.key("c", None).unwrap();
    editor.edit(|vim, register| vim.text("i!", register));
    select(&mut manager, &destination);
    let (changes, _) = manager.transfer("v", &root, &editor.dirty_paths());
    assert!(changes.is_empty());
    assert!(!destination.join("source.txt").exists());
    assert_eq!(editor.vim().doc.text, "!saved");
    assert_eq!(fs::read_to_string(&source).unwrap(), "saved");
}

/// Manager activation returns a file-open request instead of acquiring editing state itself.
#[test]
fn manager_navigation_reports_the_selected_file_to_the_workspace() {
    let project = tempfile::tempdir().unwrap();
    for name in ["a.txt", "b.txt", "c.txt"] {
        fs::write(project.path().join(name), name).unwrap();
    }
    let mut manager = ManagerWindow::new(project.path().to_path_buf());
    manager.key("2", None).unwrap();
    manager.key("j", None).unwrap();
    let (_, command) = manager.key("enter", None).unwrap();
    let ManagerCommand::Open(path) = command else {
        panic!("expected an editor file-open request");
    };
    assert_eq!(path.file_name().unwrap(), "c.txt");
    manager.key("g", Some('g')).unwrap();
    assert_eq!(manager.tree.selected, 0);
}
