//! Open projects and their independent editing and Explorer state.

use super::{Workspace, layout::Focus};
use crate::grid::windows::{editor::EditorWindow, manager::ManagerWindow};
use gpui::{Context, Window};
use nanobug::{animation::cursor::CursorConfig, document::Document, i18n};
use std::{
    borrow::Cow,
    path::{Path, PathBuf},
};

/// State retained for one project while another project is visible.
pub(super) struct Project {
    /// Canonical root used to identify this tab and resolve its relative paths.
    pub(super) root: PathBuf,
    /// Last focused pane in this project.
    pub(super) focus: Focus,
    /// The project's open buffers, cursor positions and scroll state.
    pub(super) editor: EditorWindow,
    /// The project's tree and transfer history.
    pub(super) manager: ManagerWindow,
    /// A pane the user closed within this project.
    pub(super) hidden_pane: Option<Focus>,
    /// Preferred Explorer width in grid columns.
    pub(super) sidebar: usize,
    /// Last feedback shown in the status line.
    pub(super) status: String,
    /// Validated project-local cursor settings.
    pub(super) cursor_config: CursorConfig,
}

impl Project {
    /// Load the project at `root` with `file`, or its README when a file was omitted.
    fn new(root: PathBuf, file: Option<PathBuf>) -> Self {
        let initial = file.unwrap_or_else(|| root.join("README.md"));
        let (doc, mut status) = if initial.is_file() {
            match Document::open(&initial) {
                Ok(doc) => (doc, String::new()),
                Err(error) => (Document::new(""), format!("{error:#}")),
            }
        } else {
            (Document::new(""), String::new())
        };
        let cursor_config = CursorConfig::load(&root).unwrap_or_else(|error| {
            status = i18n::format("cursor.config_error", &[("error", &error)]);
            CursorConfig::default()
        });
        Self {
            manager: ManagerWindow::new(root.clone()),
            root,
            focus: Focus::Editor,
            editor: EditorWindow::new(doc),
            hidden_pane: None,
            sidebar: 28,
            status,
            cursor_config,
        }
    }

    /// Folder name for the tab, falling back to the path for filesystem roots.
    pub(super) fn name(&self) -> Cow<'_, str> {
        self.root
            .file_name()
            .unwrap_or(self.root.as_os_str())
            .to_string_lossy()
    }

    /// Cancel pending gestures when leaving the project, retaining document and viewport state.
    fn cancel_pending_input(&mut self) {
        self.manager.paste_timer = None;
        self.manager.count = 0;
        self.manager.click = None;
        self.editor.dragging = None;
        self.editor.horizontal_wheel = 0.;
        self.editor.composition.clear();
        self.editor.buffers[self.editor.active].vim.cancel_pending();
    }
}

/// One active project plus the remaining projects in their tab order.
pub(super) struct Projects {
    /// The project whose panes currently receive input.
    pub(super) active: Project,
    /// All other projects in display order, with the active slot omitted.
    inactive: Vec<Project>,
    /// Insertion point of `active` in `inactive` when displaying the full strip.
    pub(super) selected: usize,
}

/// Result of trying to close the active project without losing edits.
#[derive(Debug, PartialEq, Eq)]
enum ProjectClose {
    /// A neighboring project replaced the closed tab.
    Closed,
    /// The last clean project can close with the native window.
    Last,
    /// At least one of the active project's buffers needs saving.
    Unsaved,
}

impl Projects {
    /// Load nonempty startup `paths` in order, displaying the first project.
    pub(super) fn new(paths: Vec<PathBuf>) -> Self {
        let mut paths = paths.into_iter().map(project_path);
        let (root, file) = paths.next().expect("startup supplies a project path");
        let mut projects = Self {
            active: Project::new(root, file),
            inactive: Vec::new(),
            selected: 0,
        };
        for (root, file) in paths {
            projects.add(root, file);
        }
        projects
    }

    /// Number of tabs, including the active project.
    pub(super) fn len(&self) -> usize {
        self.inactive.len() + 1
    }

    /// Iterate over projects in their visible ordering.
    pub(super) fn ordered(&self) -> impl Iterator<Item = &Project> {
        let (before, after) = self.inactive.split_at(self.selected);
        before
            .iter()
            .chain(std::iter::once(&self.active))
            .chain(after)
    }

    /// Visit the active project followed by inactive projects for shared filesystem effects.
    pub(super) fn iter_mut(&mut self) -> impl Iterator<Item = &mut Project> {
        std::iter::once(&mut self.active).chain(&mut self.inactive)
    }

    /// Reuse the tab at `root` or append it with its optional initial `file`.
    pub(super) fn add(&mut self, root: PathBuf, file: Option<PathBuf>) -> usize {
        if let Some(index) = self.ordered().position(|project| project.root == root) {
            return index;
        }
        let index = self.len();
        self.inactive.push(Project::new(root, file));
        index
    }

    /// Switch to `index` while leaving the other projects in the same order.
    fn select(&mut self, index: usize) -> bool {
        if index == self.selected || index >= self.len() {
            return false;
        }
        self.active.cancel_pending_input();
        let next = self
            .inactive
            .remove(index - usize::from(index > self.selected));
        let previous = std::mem::replace(&mut self.active, next);
        self.inactive
            .insert(self.selected - usize::from(index < self.selected), previous);
        self.selected = index;
        true
    }

    /// Move the active tab to `index` without changing the focused project.
    pub(super) fn move_selected(&mut self, index: usize) {
        self.selected = index.min(self.len() - 1);
    }

    /// Drop the clean active project and select its right neighbor, or its left neighbor at the end.
    fn close_active(&mut self) -> ProjectClose {
        if self.active.editor.modified() {
            return ProjectClose::Unsaved;
        }
        if self.inactive.is_empty() {
            return ProjectClose::Last;
        }
        self.selected = self.selected.min(self.inactive.len() - 1);
        self.active = self.inactive.remove(self.selected);
        ProjectClose::Closed
    }

    /// Explain which project is blocking a safe exit, including unnamed buffers.
    pub(super) fn exit_message(&self) -> Option<String> {
        if self.active.editor.modified() {
            Some(i18n::text("workspace.unsaved_exit").into())
        } else {
            self.inactive_exit_message()
        }
    }

    /// Explain which other project would block closing the last visible pane.
    pub(super) fn inactive_exit_message(&self) -> Option<String> {
        self.inactive
            .iter()
            .find(|project| project.editor.modified())
            .map(|project| {
                i18n::format(
                    "workspace.unsaved_other_project",
                    &[("project", &project.name())],
                )
            })
    }

    /// Save active-project buffer `index` to `path` or its filename and invalidate affected transfers.
    pub(super) fn save_buffer(
        &mut self,
        index: usize,
        path: Option<PathBuf>,
    ) -> anyhow::Result<()> {
        if let Some(path) = self.active.editor.save(index, path)? {
            for project in self.iter_mut() {
                project.manager.transfers.invalidate(&path);
            }
        }
        Ok(())
    }
}

/// Canonicalize a startup `path` and split a file argument into its root and initial file.
fn project_path(path: PathBuf) -> (PathBuf, Option<PathBuf>) {
    let path = std::fs::canonicalize(&path).unwrap_or(path);
    if path.is_file() {
        (
            path.parent()
                .expect("absolute file has a parent")
                .to_owned(),
            Some(path),
        )
    } else {
        (path, None)
    }
}

impl Workspace {
    /// Activate tab `index` in `window` through `cx`; the caller refreshes the pane layout.
    pub(super) fn select_project(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.projects.select(index) {
            return false;
        }
        self.reset_project_view(window, cx);
        true
    }

    /// Close the active tab, applying its neighbor's view through `window` and `cx`.
    pub(super) fn close_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.projects.close_active() {
            ProjectClose::Closed => self.reset_project_view(window, cx),
            ProjectClose::Last => self.quit(false, window),
            ProjectClose::Unsaved => {
                self.projects.active.status = i18n::text("workspace.unsaved_close_project").into();
            }
        }
    }

    /// Reset transient input and apply the active project's cursor settings through `window` and `cx`.
    fn reset_project_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.help.close();
        self.help.release_backdrop(window);
        self.prefix = None;
        self.hovered_close = None;
        self.resizing = false;
        self.wheel = 0.;
        self.last_cursor_space = None;
        let config = self.projects.active.cursor_config.clone();
        self.cursor
            .update(cx, |cursor, cx| cursor.configure(config, window, cx));
    }

    /// Open `path` as a project in `window`, selecting its existing tab if already open.
    pub(super) fn open_project(
        &mut self,
        path: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match std::fs::canonicalize(path) {
            Ok(root) => {
                let index = self.projects.add(root, None);
                self.select_project(index, window, cx);
            }
            Err(error) => {
                self.projects.active.status = i18n::format(
                    "workspace.open_project_failed",
                    &[("path", &path.display()), ("error", &error)],
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Open three distinct projects rooted under the test `directory`.
    fn projects_in(directory: &Path) -> Projects {
        let paths = ["first", "second", "third"]
            .into_iter()
            .map(|name| {
                let path = directory.join(name);
                fs::create_dir(&path).unwrap();
                fs::write(path.join("README.md"), name).unwrap();
                path
            })
            .collect();
        Projects::new(paths)
    }

    /// Reorder and revisit a project without losing its dirty buffer or pane focus.
    #[test]
    fn switching_and_reordering_retain_the_project() {
        let directory = tempfile::tempdir().unwrap();
        let mut projects = projects_in(directory.path());
        let first = fs::canonicalize(directory.path().join("first")).unwrap();
        assert_eq!(projects.add(first, None), 0);
        assert_eq!(projects.len(), 3);
        projects
            .active
            .editor
            .edit(|vim, register| vim.text("i!", register));
        projects.active.focus = Focus::Manager;

        assert!(projects.select(1));
        assert_eq!(projects.active.editor.vim().doc.text, "second");
        assert!(projects.exit_message().unwrap().contains("first"));
        projects.move_selected(2);
        assert_eq!(
            projects
                .ordered()
                .map(|project| project.name().into_owned())
                .collect::<Vec<_>>(),
            ["first", "third", "second"]
        );
        assert!(projects.select(0));
        assert_eq!(projects.active.focus, Focus::Manager);
        assert_eq!(projects.active.editor.vim().doc.text, "!first");
        assert!(projects.select(2));
        assert_eq!(projects.active.name(), "second");
    }

    /// Closing selects a neighboring tab and protects dirty buffers even when they are hidden.
    #[test]
    fn closing_keeps_unsaved_projects_and_selects_the_neighbor() {
        let directory = tempfile::tempdir().unwrap();
        let mut projects = projects_in(directory.path());
        let edited = projects.active.root.join("README.md");
        let clean = projects.active.root.join("clean.txt");
        fs::write(&clean, "clean").unwrap();
        projects
            .active
            .editor
            .edit(|vim, register| vim.text("i!", register));
        projects.active.editor.open(clean, 0.).unwrap();
        assert_eq!(projects.close_active(), ProjectClose::Unsaved);
        assert_eq!(projects.len(), 3);

        assert!(projects.select(1));
        assert_eq!(projects.close_active(), ProjectClose::Closed);
        assert_eq!(projects.active.name(), "third");
        assert_eq!(projects.selected, 1);
        assert!(projects.exit_message().unwrap().contains("first"));
        assert_eq!(projects.close_active(), ProjectClose::Closed);
        assert_eq!(projects.active.name(), "first");
        assert_eq!(projects.selected, 0);
        assert_eq!(projects.close_active(), ProjectClose::Unsaved);

        projects.active.editor.open(edited, 0.).unwrap();
        assert_eq!(projects.active.editor.vim().doc.text, "!first");
        projects
            .save_buffer(projects.active.editor.active, None)
            .unwrap();
        assert_eq!(projects.close_active(), ProjectClose::Last);
        assert_eq!(projects.len(), 1);
        assert_eq!(projects.exit_message(), None);
    }
}
