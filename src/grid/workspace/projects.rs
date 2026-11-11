//! Project state and the ordered workspace tabs that retain it.

use super::{Workspace, editor_close::CloseTarget, layout::Focus};
use crate::grid::windows::{
    PathChange, editor::EditorWindow, manager::ManagerWindow, start::StartPage,
};
use gpui::{Context, Window};
use nanobug::{animation::cursor::CursorConfig, document::Document, i18n};
use std::{
    borrow::Cow,
    fs,
    path::{Path, PathBuf},
};

/// Retained state of a project or standalone file tab.
pub(super) struct Project {
    /// Project root or standalone file's parent, used to resolve relative paths.
    pub(super) root: PathBuf,
    /// Last focused pane in this project.
    pub(super) focus: Focus,
    /// The project's open buffers, cursor positions and scroll state.
    pub(super) editor: EditorWindow,
    /// Explorer state, present only when the tab represents a directory.
    pub(super) manager: Option<ManagerWindow>,
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
        let (doc, status) = if initial.is_file() {
            match Document::open(&initial) {
                Ok(doc) => (doc, String::new()),
                Err(error) => (Document::new(""), format!("{error:#}")),
            }
        } else {
            (Document::new(""), String::new())
        };
        let manager = ManagerWindow::new(root.clone());
        Self::with_editor(root, EditorWindow::new(doc), Some(manager), status)
    }

    /// Initialize the editing tab with its optional Explorer and root-local settings.
    fn with_editor(
        root: PathBuf,
        editor: EditorWindow,
        manager: Option<ManagerWindow>,
        mut status: String,
    ) -> Self {
        let cursor_config = CursorConfig::load(&root).unwrap_or_else(|error| {
            status = i18n::format("cursor.config_error", &[("error", &error)]);
            CursorConfig::default()
        });
        Self {
            manager,
            root,
            focus: Focus::Editor,
            editor,
            hidden_pane: None,
            sidebar: 28,
            status,
            cursor_config,
        }
    }

    /// Directory name for a project, or the filename for a standalone editor.
    pub(super) fn name(&self) -> Cow<'_, str> {
        if self.manager.is_none() {
            return self
                .editor
                .vim()
                .doc
                .path
                .as_deref()
                .and_then(Path::file_name)
                .map_or(Cow::Borrowed(i18n::text("editor.untitled")), |name| {
                    name.to_string_lossy()
                });
        }
        self.root
            .file_name()
            .unwrap_or(self.root.as_os_str())
            .to_string_lossy()
    }

    /// Follow a standalone file's location after Save As or a filesystem move.
    pub(super) fn sync_file_root(&mut self) {
        if self.manager.is_none()
            && let Some(parent) = self.editor.vim().doc.path.as_deref().and_then(Path::parent)
        {
            self.root = parent.to_path_buf();
        }
    }

    /// Cancel pending gestures when leaving the project, retaining document and viewport state.
    fn cancel_pending_input(&mut self) {
        if let Some(manager) = &mut self.manager {
            manager.paste_timer = None;
            manager.count = 0;
            manager.click = None;
        }
        self.editor.dragging = None;
        self.editor.horizontal_wheel = 0.;
        self.editor.composition.clear();
        self.editor.buffers[self.editor.active].vim.cancel_pending();
    }
}

/// The content displayed by a single tab.
enum TabContent {
    /// An independent, non-editable start page.
    Start(StartPage),
    /// A directory project or a standalone file.
    Project(Box<Project>),
}

/// One tab and its position in the activation history.
struct Tab {
    /// Retained state of the page or project.
    content: TabContent,
    /// Revision at which this tab was last selected, or zero if unvisited.
    used: u64,
}

impl Tab {
    /// Borrow the project when this tab contains one.
    fn project(&self) -> Option<&Project> {
        match &self.content {
            TabContent::Project(project) => Some(project),
            TabContent::Start(_) => None,
        }
    }

    /// Mutably borrow the project when this tab contains one.
    fn project_mut(&mut self) -> Option<&mut Project> {
        match &mut self.content {
            TabContent::Project(project) => Some(project),
            TabContent::Start(_) => None,
        }
    }
}

/// Nonempty display order shared by project tabs and start pages.
pub(super) struct WorkspaceTabs {
    /// The strip in left-to-right order.
    tabs: Vec<Tab>,
    /// Index of the tab receiving input.
    pub(super) selected: usize,
    /// Invalidates the rendered strip and orders tab activations.
    pub(super) revision: u64,
}

impl WorkspaceTabs {
    /// Load explicit startup `paths` in order, or create a start page for an empty list.
    pub(super) fn new(paths: Vec<PathBuf>) -> Self {
        let mut tabs = Self {
            tabs: Vec::new(),
            selected: 0,
            revision: 0,
        };
        for (root, file) in paths.into_iter().map(project_path) {
            tabs.add(root, file);
        }
        if tabs.tabs.is_empty() {
            tabs.add_start();
        }
        tabs.tabs[0].used = tabs.revision;
        tabs
    }

    /// Number of open tabs of either kind.
    pub(super) fn len(&self) -> usize {
        self.tabs.len()
    }

    /// Tab labels in strip order, localized for start pages.
    pub(super) fn names(&self) -> impl Iterator<Item = Cow<'_, str>> {
        self.tabs.iter().map(|tab| {
            tab.project()
                .map_or_else(|| i18n::text("start.title").into(), Project::name)
        })
    }

    /// Borrow the selected project when the current tab is a project.
    pub(super) fn active_project(&self) -> Option<&Project> {
        self.tabs[self.selected].project()
    }

    /// Mutably borrow the selected project when the current tab is a project.
    pub(super) fn active_project_mut(&mut self) -> Option<&mut Project> {
        self.tabs[self.selected].project_mut()
    }

    /// Whether the selected tab has an Explorer that can be focused or restored.
    pub(super) fn manager_available(&self) -> bool {
        self.active_project()
            .is_some_and(|project| project.manager.is_some())
    }

    /// Borrow the project in a project-only input or rendering path.
    pub(super) fn active(&self) -> &Project {
        self.active_project().expect("a project tab is selected")
    }

    /// Mutably borrow the project in a project-only input or rendering path.
    pub(super) fn active_mut(&mut self) -> &mut Project {
        self.active_project_mut()
            .expect("a project tab is selected")
    }

    /// Borrow the selected start page, if present.
    pub(super) fn start(&self) -> Option<&StartPage> {
        match &self.tabs[self.selected].content {
            TabContent::Start(page) => Some(page),
            TabContent::Project(_) => None,
        }
    }

    /// Mutably borrow the selected start page, if present.
    pub(super) fn start_mut(&mut self) -> Option<&mut StartPage> {
        match &mut self.tabs[self.selected].content {
            TabContent::Start(page) => Some(page),
            TabContent::Project(_) => None,
        }
    }

    /// Visit projects in strip order, with their tab indices.
    pub(super) fn indexed(&self) -> impl Iterator<Item = (usize, &Project)> {
        self.tabs
            .iter()
            .enumerate()
            .filter_map(|(index, tab)| Some((index, tab.project()?)))
    }

    /// Visit projects in strip order, skipping start pages.
    pub(super) fn ordered(&self) -> impl Iterator<Item = &Project> {
        self.tabs.iter().filter_map(Tab::project)
    }

    /// Visit projects for shared filesystem effects.
    pub(super) fn iter_mut(&mut self) -> impl Iterator<Item = &mut Project> {
        self.tabs.iter_mut().filter_map(Tab::project_mut)
    }

    /// Borrow the project at a `tab` recorded by a modal close request.
    pub(super) fn project(&self, tab: usize) -> &Project {
        self.tabs[tab].project().expect("tab identifies a project")
    }

    /// Mutably borrow the project at a `tab` recorded by a modal close request.
    pub(super) fn project_mut(&mut self, tab: usize) -> &mut Project {
        self.tabs[tab]
            .project_mut()
            .expect("tab identifies a project")
    }

    /// Reuse the tab at `root` or append it with its optional initial `file`.
    /// Return the tab index and whether a new project was loaded.
    pub(super) fn add(&mut self, root: PathBuf, file: Option<PathBuf>) -> (usize, bool) {
        if let Some(index) = self.project_index(&root) {
            return (index, false);
        }
        let index = self.push(TabContent::Project(Box::new(Project::new(root, file))));
        (index, true)
    }

    /// Find the tab whose project is already loaded at canonical `root`.
    fn project_index(&self, root: &Path) -> Option<usize> {
        self.indexed()
            .find(|(_, project)| project.manager.is_some() && project.root == root)
            .map(|(index, _)| index)
    }

    /// Find an editor-only tab displaying this file.
    pub(super) fn file_index(&self, path: &Path) -> Option<usize> {
        self.indexed()
            .find(|(_, tab)| {
                tab.manager.is_none() && tab.editor.vim().doc.path.as_deref() == Some(path)
            })
            .map(|(index, _)| index)
    }

    /// Open a standalone file tab, carrying over a retained buffer if one exists.
    pub(super) fn add_file(&mut self, path: &Path) -> anyhow::Result<usize> {
        if let Some(index) = self.file_index(path) {
            return Ok(index);
        }
        let existing = self
            .indexed()
            .filter_map(|(tab, project)| {
                let buffer = project
                    .editor
                    .buffers
                    .iter()
                    .position(|buffer| buffer.vim.doc.path.as_deref() == Some(path))?;
                let modified = project.editor.buffers[buffer].vim.doc.modified();
                Some((tab, buffer, modified))
            })
            .max_by_key(|&(tab, _, modified)| (modified, tab == self.selected));
        let editor = if let Some((tab, buffer, _)) = existing {
            EditorWindow::with_buffer(self.project_mut(tab).editor.take_buffer(buffer))
        } else {
            EditorWindow::new(Document::open(path)?)
        };
        let root = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        let tab = Project::with_editor(root, editor, None, String::new());
        Ok(self.push(TabContent::Project(Box::new(tab))))
    }

    /// Open `root` in the selected start tab, moving an existing project's retained state if needed.
    fn replace_start_with_project(&mut self, root: PathBuf) {
        debug_assert!(self.start().is_some());
        let mut target = self.selected;
        let content = if let Some(index) = self.project_index(&root) {
            target -= usize::from(index < target);
            self.tabs.remove(index).content
        } else {
            TabContent::Project(Box::new(Project::new(root, None)))
        };
        self.tabs[target].content = content;
        self.activate(target);
    }

    /// Append a fresh start page and return its index.
    pub(super) fn add_start(&mut self) -> usize {
        self.push(TabContent::Start(StartPage::default()))
    }

    /// Append `content` without changing selection, and return its tab index.
    fn push(&mut self, content: TabContent) -> usize {
        let index = self.tabs.len();
        self.tabs.push(Tab { content, used: 0 });
        self.revision += 1;
        index
    }

    /// Switch to `index` while leaving tab order intact. Return whether selection changed.
    pub(super) fn select(&mut self, index: usize) -> bool {
        if index == self.selected || index >= self.len() {
            return false;
        }
        if let Some(project) = self.active_project_mut() {
            project.cancel_pending_input();
        }
        self.activate(index);
        true
    }

    /// Record the tab at `index` as selected and most recently used.
    fn activate(&mut self, index: usize) {
        self.selected = index;
        self.revision += 1;
        self.tabs[index].used = self.revision;
    }

    /// Move the selected tab to `index` without changing its activation history.
    pub(super) fn move_selected(&mut self, index: usize) {
        let index = index.min(self.len() - 1);
        let selected = self.tabs.remove(self.selected);
        self.tabs.insert(index, selected);
        self.selected = index;
        self.revision += 1;
    }

    /// Remove the selected project after confirmation, preferring its right neighbor.
    fn close_project(&mut self) {
        debug_assert!(!self.active().editor.modified());
        self.tabs.remove(self.selected);
        if self.tabs.is_empty() {
            self.add_start();
        }
        self.activate(self.selected.min(self.len() - 1));
    }

    /// Close the selected start page and return to the latest tab.
    /// Return false if this was the sole tab and the caller should close the window.
    pub(super) fn close_start(&mut self) -> bool {
        debug_assert!(self.start().is_some());
        if self.len() == 1 {
            return false;
        }
        self.tabs.remove(self.selected);
        self.activate(self.most_recent());
        true
    }

    /// Find the latest activation in the nonempty strip.
    fn most_recent(&self) -> usize {
        self.tabs
            .iter()
            .enumerate()
            .max_by_key(|(_, tab)| tab.used)
            .expect("at least one tab remains")
            .0
    }

    /// Save buffer `index` from project `tab` to `path` or its filename and invalidate transfers.
    pub(super) fn save_buffer(
        &mut self,
        tab: usize,
        index: usize,
        path: Option<PathBuf>,
    ) -> anyhow::Result<()> {
        let project = self.project_mut(tab);
        if let Some(path) = project.editor.save(index, path)? {
            if project.manager.is_none() {
                project.sync_file_root();
                self.revision += 1;
            }
            for manager in self
                .iter_mut()
                .filter_map(|project| project.manager.as_mut())
            {
                manager.transfers.invalidate(&path);
            }
        }
        Ok(())
    }

    /// Reconcile buffers and update standalone labels after filesystem changes.
    pub(super) fn apply_path_changes(&mut self, changes: &[PathChange]) {
        let mut labels_changed = false;
        for project in self.iter_mut() {
            let file = project.manager.is_none();
            let previous = file.then(|| project.editor.vim().doc.path.clone());
            project.editor.apply_path_changes(changes);
            if let Some(previous) = previous
                && project.editor.vim().doc.path != previous
            {
                project.sync_file_root();
                labels_changed = true;
            }
        }
        self.revision += u64::from(labels_changed);
    }
}

/// Canonicalize a startup `path` and split a file argument into its root and initial file.
fn project_path(path: PathBuf) -> (PathBuf, Option<PathBuf>) {
    let path = path.canonicalize().unwrap_or(path);
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
    /// Activate tab `index` in `window` through `cx`; the caller refreshes its layout.
    pub(super) fn select_tab(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.picker_operation_pending() || !self.projects.select(index) {
            return false;
        }
        self.reset_tab_view(window, cx);
        true
    }

    /// Complete a clean or confirmed close of `target` in `window`, refreshing a changed tab through `cx`.
    pub(super) fn finish_close(
        &mut self,
        target: CloseTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match target {
            CloseTarget::Editor => self.hide_pane(Focus::Editor),
            CloseTarget::Project => {
                self.projects.close_project();
                self.reset_tab_view(window, cx);
            }
            CloseTarget::Quit => window.remove_window(),
        }
    }

    /// Reset transient workspace state and apply the selected tab's cursor settings.
    pub(super) fn reset_tab_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.file_picker = None;
        self.help.close();
        self.help.release_backdrop(window);
        self.prefix = None;
        self.hovered_controls = None;
        self.resizing = false;
        self.wheel = 0.;
        self.last_cursor_space = None;
        let config = self
            .projects
            .active_project()
            .map(|project| project.cursor_config.clone())
            .unwrap_or_default();
        self.cursor
            .update(cx, |cursor, cx| cursor.configure(config, window, cx));
    }

    /// Open canonical `path` in an editor-only tab.
    /// Preserve the active tab on a read or decoding failure.
    pub(super) fn open_standalone_file(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        let index = self.projects.add_file(&path)?;
        self.select_tab(index, window, cx);
        Ok(())
    }

    /// Open directory `path` in `window`, replacing the selected start page when present.
    pub(super) fn open_project(
        &mut self,
        path: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let root = fs::canonicalize(path).and_then(|root| {
            if root.is_dir() {
                Ok(root)
            } else {
                Err(std::io::Error::from(std::io::ErrorKind::NotADirectory))
            }
        });
        match root {
            Ok(root) => self.open_project_path(root, None, window, cx),
            Err(error) => self.set_status(i18n::format(
                "workspace.open_project_failed",
                &[("path", &path.display()), ("error", &error)],
            )),
        }
    }

    /// Open `root` with optional `file` in `window` and record the project in recent history.
    /// Directory actions replace a start page. File actions use the owning project tab.
    pub(super) fn open_project_path(
        &mut self,
        root: PathBuf,
        file: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if file.is_none() && self.projects.start().is_some() {
            self.projects.replace_start_with_project(root.clone());
            self.reset_tab_view(window, cx);
        } else {
            let (index, added) = self.projects.add(root.clone(), file.clone());
            self.select_tab(index, window, cx);
            if !added && let Some(file) = file {
                self.open(file, window, cx);
            }
        }
        self.recent_projects.record(&root);
    }

    /// Report `message` in the current tab's status area.
    pub(super) fn set_status(&mut self, message: String) {
        if let Some(project) = self.projects.active_project_mut() {
            project.status = message;
        } else if let Some(page) = self.projects.start_mut() {
            page.status = message;
        }
    }
}

#[cfg(test)]
mod tests;
