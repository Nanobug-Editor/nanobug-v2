//! Project state and the ordered workspace tabs that retain it.

mod buffers;
mod session;

use super::{
    Workspace,
    argument_list::ArgumentList,
    editor_close::CloseTarget,
    editor_splits::{EditorSplits, PaneId, SplitAxis},
    layout::{Focus, Layout, split_panes},
    plugins::PluginWindowsSnapshot,
};
use crate::grid::render::Rect;
use crate::grid::windows::{
    PathChange,
    editor::{Buffer, DirectorySlot, EditorWindow},
    manager::ManagerWindow,
    start::StartPage,
};
use gpui::{Context, Window};
use nanobug::{animation::cursor::CursorConfig, document::Document, i18n};
use std::{
    borrow::Cow,
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

/// Pane geometry and focus to restore when a project tabpage is selected.
#[derive(Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
struct PageLayout {
    /// Focused and previously focused panes.
    focus: Focus,
    previous_focus: Option<Focus>,
    /// Currently hidden pane, if any.
    hidden_pane: Option<Focus>,
    /// Explorer sizing for horizontal and stacked layouts.
    sidebar: usize,
    manager_height: Option<usize>,
    /// Pane orientation and order.
    stacked: bool,
    editor_first: bool,
}

impl Default for PageLayout {
    fn default() -> Self {
        Self {
            focus: Focus::Editor,
            previous_focus: None,
            hidden_pane: None,
            sidebar: 28,
            manager_height: None,
            stacked: false,
            editor_first: false,
        }
    }
}

/// Document to display when opening a new editor split.
pub(super) enum SplitDocument {
    /// Read-only reference page with local mappings for code examples.
    Help(Document),
    /// Read-only output from a Vim command.
    ReadOnly(Document),
    /// Editable generated output in a new buffer.
    Editable(Document),
    /// Display the focused editor's active buffer in an independent view.
    Current,
    /// Start with an untitled, empty buffer.
    Empty,
    /// Open this file and optionally navigate to a one-based line.
    File(PathBuf, Option<usize>),
    /// Display another view of an existing Session buffer by its Ex number.
    SessionBuffer(u64),
    /// Display the previous buffer, or the buffer at the given one-based index.
    Alternate(Option<usize>),
    /// Open the included file (or display the active buffer) at a row and UTF-8 byte column.
    Include(Option<PathBuf>, usize, usize),
    /// Attach a prepared view of an existing Session buffer.
    SharedView(Box<crate::grid::windows::editor::Buffer>),
}

/// A project's tabpage retains its outer layout and its tree of independent editors.
struct EditorPage {
    /// Vim t: variables follow the Tabpage identity through moves and split focus changes.
    script_scope: nanobug::vim::ScriptTabScope,
    /// Explorer and editor-group geometry when this page is selected again.
    layout: PageLayout,
    /// Editor pane placement and the last active pane on this page.
    splits: EditorSplits,
    /// Whether only this page's active editor pane is shown in the editing region.
    editor_zoomed: bool,
    /// Optional editor split reserved for `:pedit` on this page.
    preview: Option<PaneId>,
    /// Ex directory shared by this Tabpage's editor windows.
    directory: DirectorySlot,
}

/// Retained state of a project or standalone file tab.
pub(super) struct Project {
    /// Unique opening used to bind plugin state to this tab's lifetime.
    pub(super) plugin_scope: u64,
    /// Project root or standalone file's parent, used to resolve relative paths.
    pub(super) root: PathBuf,
    /// Last focused pane in this project.
    pub(super) focus: Focus,
    /// Previously focused pane, retained while both panes are visible.
    pub(super) previous_focus: Option<Focus>,
    /// The focused editor, or the last editor focused before entering Explorer.
    pub(super) editor: EditorWindow,
    /// Ordered file arguments, shared by every editor in this project.
    pub(super) arguments: ArgumentList,
    /// Uppercase marks shared by every editor in this project Session.
    pub(super) file_marks: nanobug::vim::FileMarks,
    /// Explorer state, present only when the tab represents a directory.
    pub(super) manager: Option<ManagerWindow>,
    /// Plugin browsing and surface state while a different project owns the native window.
    pub(super) plugin_windows: PluginWindowsSnapshot,
    /// A pane the user closed within this project.
    pub(super) hidden_pane: Option<Focus>,
    /// Preferred Explorer width in grid columns.
    pub(super) sidebar: usize,
    /// Whether Explorer and the editor are stacked instead of side by side.
    pub(super) stacked: bool,
    /// Whether the editor comes before Explorer in the current layout.
    pub(super) editor_first: bool,
    /// Preferred Explorer height in grid rows when the panes are stacked.
    pub(super) manager_height: Option<usize>,
    /// Ordered tabpages, each with its own pane topology.
    pages: Vec<EditorPage>,
    /// Current and most recently departed tabpage indices.
    active_page: usize,
    previous_page: Option<usize>,
    /// Every editor except the one materialized in `editor`, indexed by stable pane ID.
    inactive_editors: HashMap<PaneId, EditorWindow>,
    /// Retain PTYs by Ex buffer number even if their last pane is closed.
    terminal_buffers: HashMap<u64, Buffer>,
    /// Loaded hidden text and unloaded file references keyed by stable Ex number.
    detached_buffers: HashMap<u64, buffers::DetachedBuffer>,
    /// Next available pane ID within this project's lifetime.
    next_pane: PaneId,
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
        mut editor: EditorWindow,
        manager: Option<ManagerWindow>,
        mut status: String,
    ) -> Self {
        static NEXT_SCOPE: AtomicU64 = AtomicU64::new(1);
        let cursor_config = CursorConfig::load(&root).unwrap_or_else(|error| {
            status = i18n::format("cursor.config_error", &[("error", &error)]);
            CursorConfig::default()
        });
        let file_marks = nanobug::vim::FileMarks::default();
        editor.set_file_marks(file_marks.clone());
        Self {
            plugin_scope: NEXT_SCOPE.fetch_add(1, Ordering::Relaxed),
            manager,
            plugin_windows: PluginWindowsSnapshot::default(),
            root,
            focus: Focus::Editor,
            previous_focus: None,
            editor,
            arguments: ArgumentList::default(),
            file_marks,
            hidden_pane: None,
            sidebar: 28,
            stacked: false,
            editor_first: false,
            manager_height: None,
            pages: vec![EditorPage {
                script_scope: Default::default(),
                layout: PageLayout::default(),
                splits: EditorSplits::new(0),
                editor_zoomed: false,
                preview: None,
                directory: DirectorySlot::default(),
            }],
            active_page: 0,
            previous_page: None,
            inactive_editors: HashMap::new(),
            terminal_buffers: HashMap::new(),
            detached_buffers: HashMap::new(),
            next_pane: 1,
            status,
            cursor_config,
        }
    }

    /// Capture the live pane state before changing the active editor tabpage.
    fn page_layout(&self) -> PageLayout {
        PageLayout {
            focus: self.focus,
            previous_focus: self.previous_focus,
            hidden_pane: self.hidden_pane,
            sidebar: self.sidebar,
            manager_height: self.manager_height,
            stacked: self.stacked,
            editor_first: self.editor_first,
        }
    }

    /// Restore the active page's geometry and focus after a switch or removal.
    fn restore_page_layout(&mut self) {
        let PageLayout {
            focus,
            previous_focus,
            hidden_pane,
            sidebar,
            manager_height,
            stacked,
            editor_first,
        } = self.pages[self.active_page].layout;
        self.focus = focus;
        self.previous_focus = previous_focus;
        self.hidden_pane = hidden_pane;
        self.sidebar = sidebar;
        self.manager_height = manager_height;
        self.stacked = stacked;
        self.editor_first = editor_first;
    }

    /// Select page `index`, retaining the departing page's panes and editor view at `now`.
    pub(super) fn select_page(&mut self, index: usize, _now: f64) -> bool {
        let previous = self.active_page;
        if previous == index || index >= self.pages.len() {
            return false;
        }
        self.pages[previous].layout = self.page_layout();
        let old_editor = self.active_editor_id();
        self.active_page = index;
        self.previous_page = Some(previous);
        self.install_editor(old_editor);
        self.restore_page_layout();
        true
    }

    /// Insert a page at `now` for an optional `path`, using the current layout with editor focus.
    pub(super) fn open_page(&mut self, path: Option<PathBuf>, now: f64) -> anyhow::Result<()> {
        let source = path.map_or(SplitDocument::Empty, |path| SplitDocument::File(path, None));
        self.open_page_source(source, self.active_page + 1, now)
    }

    /// Insert a one-window Tabpage displaying `source` at the requested page boundary.
    pub(super) fn open_page_source(
        &mut self,
        source: SplitDocument,
        next: usize,
        now: f64,
    ) -> anyhow::Result<()> {
        let copy_view = matches!(
            source,
            SplitDocument::Current | SplitDocument::SessionBuffer(_)
        );
        let mut editor = self.new_editor(source, now)?;
        if copy_view {
            editor.inherit_window_options(&self.editor);
            editor.inherit_jumps(&self.editor);
        }
        let original = self.active_editor_id();
        let pane = self.allocate_editor(editor);
        self.arguments.inherit_pane(original, pane);
        self.pages.insert(next, self.isolated_page(pane));
        self.pages[next].layout.hidden_pane = None;
        self.active_page += usize::from(next <= self.active_page);
        self.select_page(next, now);
        Ok(())
    }

    /// Move the active editor into a new tabpage before the one-based `count`, or after this page.
    pub(super) fn move_split_to_page(&mut self, count: Option<usize>) -> bool {
        let pane = self.active_editor_id();
        if !self.splits_mut().close_active() {
            return false;
        }
        let previous = self.active_page;
        let next = count.map_or(previous + 1, |index| {
            index.saturating_sub(1).min(self.pages.len())
        });
        self.pages[previous].layout = self.page_layout();
        self.pages[previous].editor_zoomed = false;
        self.pages.insert(next, self.isolated_page(pane));
        // The transferred editor stays materialized; the old page's replacement is already retained.
        self.active_page = next;
        self.previous_page = Some(previous + usize::from(next <= previous));
        self.restore_page_layout();
        true
    }

    /// Arrange a new tabpage around `pane`, inheriting the current Explorer geometry.
    fn isolated_page(&self, pane: PaneId) -> EditorPage {
        let layout = self.page_layout();
        EditorPage {
            script_scope: Default::default(),
            layout: PageLayout {
                focus: Focus::Editor,
                previous_focus: None,
                hidden_pane: layout.hidden_pane.filter(|&pane| pane != Focus::Editor),
                ..layout
            },
            splits: EditorSplits::new(pane),
            editor_zoomed: false,
            preview: None,
            directory: self.tabpage_directory().inherited(),
        }
    }

    /// Close the active page at `now` and restore the neighboring page if one exists.
    pub(super) fn close_page(&mut self, now: f64) -> bool {
        if self.pages.len() == 1 {
            return false;
        }
        let closed = self.active_page;
        let previous = self.previous_page;
        let next = if closed + 1 < self.pages.len() {
            closed + 1
        } else {
            closed - 1
        };
        self.select_page(next, now);
        for id in self.pages.remove(closed).splits.ids() {
            self.inactive_editors.remove(&id);
            self.arguments.forget_pane(id);
        }
        self.active_page -= usize::from(self.active_page > closed);
        self.previous_page = previous
            .filter(|&index| index != closed)
            .map(|index| index - usize::from(index > closed))
            .filter(|&index| index != self.active_page);
        true
    }

    /// Close every inactive Tabpage while retaining the focused page and its complete tree.
    pub(super) fn close_other_pages(&mut self, _now: f64) {
        let pages = std::mem::take(&mut self.pages);
        for (index, page) in pages.into_iter().enumerate() {
            if index == self.active_page {
                self.pages.push(page);
            } else {
                for id in page.splits.ids() {
                    self.inactive_editors.remove(&id);
                    self.arguments.forget_pane(id);
                }
            }
        }
        self.active_page = 0;
        self.previous_page = None;
    }

    /// Count the project's tabpages for the caption and tab commands.
    pub(super) fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// Reorder the active Tabpage to `destination`, preserving its panes and previous-page identity.
    pub(super) fn move_active_page(&mut self, destination: usize) {
        let from = self.active_page;
        if from == destination {
            return;
        }
        let page = self.pages.remove(from);
        self.pages.insert(destination, page);
        self.active_page = destination;
        self.previous_page = self.previous_page.map(|previous| {
            if from < previous && previous <= destination {
                previous - 1
            } else if destination <= previous && previous < from {
                previous + 1
            } else {
                previous
            }
        });
    }

    /// Enumerate editor windows on Tabpage `index` in split order.
    pub(super) fn tabpage_editors(&self, index: usize) -> impl Iterator<Item = &EditorWindow> {
        self.pages[index]
            .splits
            .ids()
            .into_iter()
            .filter_map(|id| self.editor_by_id(id))
    }

    /// Resolve this Tabpage's Explorer and editor regions in the common workspace allocation.
    pub(super) fn pane_regions_for_tabpage(&self, index: usize, layout: Layout) -> (Rect, Rect) {
        let page_layout = if index == self.active_page {
            self.page_layout()
        } else {
            self.pages[index].layout
        };
        let area = layout.pane_area();
        if area.w == 0 || area.h == 0 {
            return (area, area);
        }
        match (&self.manager, page_layout.hidden_pane) {
            (None, _) | (_, Some(Focus::Manager)) => (Rect { w: 0, ..area }, area),
            (_, Some(Focus::Editor)) => (
                area,
                Rect {
                    x: area.w,
                    w: 0,
                    ..area
                },
            ),
            (Some(_), None) => split_panes(
                area,
                page_layout.stacked,
                page_layout.editor_first,
                page_layout.sidebar,
                page_layout.manager_height,
            ),
        }
    }

    /// Lay out a current or retained Tabpage inside its editor region in the native viewport.
    pub(super) fn editor_rects_for_tabpage(
        &self,
        index: usize,
        layout: Layout,
        mut area: Rect,
    ) -> Vec<(PaneId, Rect)> {
        let page = &self.pages[index];
        if page.splits.ids().len() > 1 {
            area = layout.visible_region(area);
        }
        if page.editor_zoomed && area.w > 0 && area.h > 0 {
            return vec![(page.splits.active(), area)];
        }
        page.splits.rects(area)
    }

    /// Return the current tabpage's zero-based caption index.
    pub(super) fn active_page(&self) -> usize {
        self.active_page
    }

    /// Share the active Tabpage's runtime script variables with its focused editor.
    pub(super) fn script_tab_scope(&self) -> nanobug::vim::ScriptTabScope {
        self.pages[self.active_page].script_scope.clone()
    }

    /// Inspect the current Tabpage's Ex directory, which is independent of project root.
    pub(super) fn tabpage_directory(&self) -> &DirectorySlot {
        &self.pages[self.active_page].directory
    }

    /// Update the active Tabpage's working directory override.
    pub(super) fn tabpage_directory_mut(&mut self) -> &mut DirectorySlot {
        &mut self.pages[self.active_page].directory
    }

    /// Return the previously selected tabpage, if it remains open.
    pub(super) fn previous_page(&self) -> Option<usize> {
        self.previous_page
    }

    /// Access the current page's split geometry and pane order.
    pub(super) fn splits(&self) -> &EditorSplits {
        &self.pages[self.active_page].splits
    }

    /// Change current split geometry without changing buffer ownership.
    pub(super) fn splits_mut(&mut self) -> &mut EditorSplits {
        &mut self.pages[self.active_page].splits
    }

    /// Read the current page's temporary editor zoom state.
    pub(super) fn editor_zoomed(&self) -> bool {
        self.pages[self.active_page].editor_zoomed
    }

    /// Toggle temporary editor zoom without changing or discarding the split geometry.
    pub(super) fn toggle_editor_zoom(&mut self) {
        let page = &mut self.pages[self.active_page];
        page.editor_zoomed = !page.editor_zoomed;
    }

    /// Count the editor panes on the current page, including compacted panes.
    pub(super) fn split_count(&self) -> usize {
        self.splits().ids().len()
    }

    /// Return the dedicated preview pane while it remains open on the current page.
    pub(super) fn preview_pane(&self) -> Option<PaneId> {
        self.pages[self.active_page]
            .preview
            .filter(|id| self.splits().ids().contains(id))
    }

    /// Mark the editor `id` as the current page's preview split.
    pub(super) fn mark_preview_pane(&mut self, id: PaneId) {
        self.pages[self.active_page].preview = Some(id);
    }

    /// Identify the editor currently receiving workspace commands.
    pub(super) fn active_editor_id(&self) -> PaneId {
        self.splits().active()
    }

    /// Read editor `id` from any page of this project.
    pub(super) fn editor_by_id(&self, id: PaneId) -> Option<&EditorWindow> {
        if id == self.active_editor_id() {
            Some(&self.editor)
        } else {
            self.inactive_editors.get(&id)
        }
    }

    /// Update editor `id` without changing which pane receives input.
    pub(super) fn editor_by_id_mut(&mut self, id: PaneId) -> Option<&mut EditorWindow> {
        if id == self.active_editor_id() {
            Some(&mut self.editor)
        } else {
            self.inactive_editors.get_mut(&id)
        }
    }

    /// Visit all editors in tabpage and split order for document-wide operations.
    pub(super) fn editors(&self) -> impl Iterator<Item = (PaneId, &EditorWindow)> {
        self.pages
            .iter()
            .flat_map(|page| page.splits.ids())
            .filter_map(|id| self.editor_by_id(id).map(|editor| (id, editor)))
    }

    /// Attach shared Vim `mappings` before any of this project's windows render.
    pub(super) fn set_input_mappings(&mut self, mappings: &nanobug::vim::InputMappings) {
        self.editor.set_input_mappings(mappings.clone());
        for editor in self.inactive_editors.values_mut() {
            editor.set_input_mappings(mappings.clone());
        }
    }

    /// Visit the selected page's editors in split order for drawing and closing.
    pub(super) fn page_editors(&self) -> impl Iterator<Item = (PaneId, &EditorWindow)> {
        self.splits()
            .ids()
            .into_iter()
            .filter_map(|id| self.editor_by_id(id).map(|editor| (id, editor)))
    }

    /// Resolve an asynchronous document result to its originating pane and buffer index.
    pub(super) fn document_owner(&self, opening: u64) -> Option<(PaneId, usize)> {
        self.editors().find_map(|(id, editor)| {
            editor
                .buffers()
                .iter()
                .position(|buffer| buffer.vim.doc.opening == opening)
                .map(|index| (id, index))
        })
    }

    /// Focus editor `id`, selecting its tabpage first if needed.
    pub(super) fn focus_editor_by_id(&mut self, id: PaneId, now: f64) -> bool {
        let Some(index) = self
            .pages
            .iter()
            .position(|page| page.splits.ids().contains(&id))
        else {
            return false;
        };
        let switched = self.select_page(index, now);
        let old = self.active_editor_id();
        let changed = self.splits_mut().focus(id);
        if changed {
            self.pages[self.active_page].editor_zoomed = false;
            self.install_editor(old);
            if !switched && self.focus == Focus::Editor {
                self.previous_focus = Some(Focus::Editor);
            }
        }
        let focused = self.focus == Focus::Editor && self.hidden_pane != Some(Focus::Editor);
        self.hidden_pane = self.hidden_pane.filter(|&pane| pane != Focus::Editor);
        self.focus_pane(Focus::Editor);
        switched || changed || !focused
    }

    /// Split the current editor along `axis` in `area`, preparing `source` at `now`.
    /// `new_first` places the new editor above or to the left of the original.
    /// A failed load or a layout with too little space leaves the previous state intact.
    pub(super) fn open_split(
        &mut self,
        axis: SplitAxis,
        new_first: bool,
        source: SplitDocument,
        area: Rect,
        now: f64,
    ) -> anyhow::Result<bool> {
        self.open_split_positioned(
            super::window_modifiers::SplitPosition {
                axis,
                first: new_first,
                outer: false,
            },
            source,
            area,
            now,
        )
    }

    /// Place an editor at the requested local or outer edge of the current Tabpage.
    pub(super) fn open_split_positioned(
        &mut self,
        position: super::window_modifiers::SplitPosition,
        source: SplitDocument,
        area: Rect,
        now: f64,
    ) -> anyhow::Result<bool> {
        let inherit_jumps = !matches!(source, SplitDocument::ReadOnly(_));
        let mut editor = self.new_editor(source, now)?;
        let old = self.active_editor_id();
        let id = self.next_pane;
        let splits = self.splits_mut();
        let opened = if position.outer {
            splits.split_edge(id, position.axis, position.first, area)
        } else {
            splits.split(id, position.axis, position.first, area)
        };
        if !opened {
            return Ok(false);
        }
        if inherit_jumps {
            editor.inherit_jumps(&self.editor);
        }
        editor.inherit_window_options(&self.editor);
        self.pages[self.active_page].editor_zoomed = false;
        self.allocate_editor(editor);
        self.arguments.inherit_pane(old, id);
        self.install_editor(old);
        self.focus_pane(Focus::Editor);
        Ok(true)
    }

    /// Close the focused split after its buffers have been resolved, restoring its neighbor.
    pub(super) fn close_split(&mut self) -> bool {
        let closed = self.active_editor_id();
        if !self.splits_mut().close_active() {
            return false;
        }
        self.pages[self.active_page].editor_zoomed = false;
        self.install_editor(closed);
        self.inactive_editors.remove(&closed);
        self.arguments.forget_pane(closed);
        true
    }

    /// Keep the focused split after the caller resolves all sibling buffers.
    pub(super) fn only_split(&mut self) {
        self.pages[self.active_page].editor_zoomed = false;
        let active = self.active_editor_id();
        for id in self.splits().ids().into_iter().filter(|&id| id != active) {
            self.inactive_editors.remove(&id);
            self.arguments.forget_pane(id);
        }
        self.splits_mut().retain_active();
    }

    /// Prepare an independent view of the requested `source` at `now`.
    fn new_editor(&mut self, source: SplitDocument, now: f64) -> anyhow::Result<EditorWindow> {
        match source {
            SplitDocument::SharedView(view) => Ok(EditorWindow::with_buffer(*view)),
            SplitDocument::Help(doc) => Ok(EditorWindow::help(doc)),
            SplitDocument::ReadOnly(doc) => Ok(EditorWindow::read_only(doc)),
            SplitDocument::Editable(doc) => Ok(EditorWindow::new(doc)),
            SplitDocument::Current => {
                let buffer = self.editor.buffer();
                Ok(buffer.split_view().map_or_else(
                    || EditorWindow::new(buffer.vim.doc.fork()),
                    EditorWindow::with_buffer,
                ))
            }
            SplitDocument::Empty => Ok(EditorWindow::new(Document::new(""))),
            SplitDocument::File(path, line) => {
                let path = path.canonicalize().unwrap_or(path);
                self.load_detached_path(&path)?;
                let mut editor = match self.shared_file_view(&path) {
                    Some(view) => EditorWindow::with_buffer(view),
                    None => EditorWindow::new(Document::open(&path)?),
                };
                if let Some(line) = line {
                    let row = line.saturating_sub(1).min(editor.vim().line_count() - 1);
                    editor.edit(|vim, _| {
                        vim.place_jump(vim.doc.starts[row]);
                        nanobug::vim::Action::None
                    });
                }
                Ok(editor)
            }
            SplitDocument::SessionBuffer(number) => {
                self.load_detached_buffer(number)?;
                let buffer = self
                    .session_buffer(number)
                    .ok_or_else(|| anyhow::anyhow!("{}", i18n::text("vim_bracket.no_buffer")))?;
                let view = buffer
                    .split_view()
                    .ok_or_else(|| anyhow::anyhow!("{}", i18n::text("editor.read_only_preview")))?;
                Ok(EditorWindow::with_buffer(view))
            }
            SplitDocument::Alternate(count) => {
                let index = match count {
                    Some(count) => Some(count.saturating_sub(1)),
                    None => self
                        .editor
                        .alternate_buffer_id()
                        .and_then(|id| self.editor.buffer_index(id)),
                };
                let buffer = index
                    .and_then(|index| self.editor.buffers().get(index))
                    .ok_or_else(|| anyhow::anyhow!("{}", i18n::text("vim_window.no_alternate")))?;
                Ok(buffer.split_view().map_or_else(
                    || EditorWindow::new(buffer.vim.doc.fork()),
                    EditorWindow::with_buffer,
                ))
            }
            SplitDocument::Include(path, row, column) => {
                let source = path.map_or(SplitDocument::Current, |path| {
                    SplitDocument::File(path, None)
                });
                let mut editor = self.new_editor(source, now)?;
                let row = row.min(editor.vim().line_count() - 1);
                let range = editor.vim().doc.line_range(row);
                editor.edit(|vim, _| {
                    vim.place_jump(range.start + column.min(range.len()));
                    nanobug::vim::Action::None
                });
                Ok(editor)
            }
        }
    }

    /// Assign a stable ID to a new editor before putting it in a page's tree.
    fn allocate_editor(&mut self, mut editor: EditorWindow) -> PaneId {
        let id = self.next_pane;
        self.next_pane += 1;
        editor.set_file_marks(self.file_marks.clone());
        *editor.directory_mut() = self.editor.directory().inherited();
        self.inactive_editors.insert(id, editor);
        id
    }

    /// Replace the Session's file mark bank, binding every restored pane to the same marks.
    pub(super) fn install_file_marks(&mut self, file_marks: nanobug::vim::FileMarks) {
        self.file_marks = file_marks;
        self.editor.set_file_marks(self.file_marks.clone());
        for editor in self.inactive_editors.values_mut() {
            editor.set_file_marks(self.file_marks.clone());
        }
    }

    /// Exchange the active editor with the pane just selected by the current page's tree.
    fn install_editor(&mut self, previous: PaneId) {
        let editor = self
            .inactive_editors
            .remove(&self.active_editor_id())
            .expect("a selected editor is retained by its project");
        let outgoing = std::mem::replace(&mut self.editor, editor);
        self.inactive_editors.insert(previous, outgoing);
    }

    /// Focus `pane`, cancel its predecessor's literal argument, and remember its predecessor for Ctrl+w p.
    pub(super) fn focus_pane(&mut self, pane: Focus) {
        if self.focus != pane {
            if self.focus == Focus::Editor && self.editor.vim().awaits_literal_argument() {
                self.editor.composition_mut().clear();
                self.editor.cancel_pending();
            }
            self.pages[self.active_page].editor_zoomed = false;
            self.previous_focus = Some(self.focus);
            self.focus = pane;
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
            manager.cancel_pending_undo();
            manager.clear_count();
            manager.clear_click();
        }
        self.editor.stop_drag();
        self.editor.reset_wheel();
        self.editor.composition_mut().clear();
        self.editor.cancel_pending();
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
pub(crate) struct WorkspaceTabs {
    /// The strip in left-to-right order.
    tabs: Vec<Tab>,
    /// Index of the tab receiving input.
    pub(super) selected: usize,
    /// Invalidates the rendered strip and orders tab activations.
    pub(super) revision: u64,
    /// Local Session store for directory projects.
    sessions: session::ProjectSessions,
}

impl WorkspaceTabs {
    /// Load the test fixture's startup `paths` without reading or writing local Sessions.
    #[cfg(test)]
    pub(crate) fn new(paths: Vec<PathBuf>) -> Self {
        Self::with_project_sessions(paths, false)
    }

    /// Open startup `paths` in order, restoring Sessions when `enabled` for the profile.
    pub(crate) fn with_project_sessions(paths: Vec<PathBuf>, enabled: bool) -> Self {
        let mut tabs = Self {
            tabs: Vec::new(),
            selected: 0,
            revision: 0,
            sessions: session::ProjectSessions::for_profile(enabled),
        };
        for (root, file) in paths.into_iter().map(project_path) {
            let (index, _) = tabs.add(root, file.clone());
            if let Some(file) = file {
                tabs.project_mut(index).arguments.push_startup_file(file);
            }
        }
        if tabs.tabs.is_empty() {
            tabs.add_start();
        }
        tabs.tabs[0].used = tabs.revision;
        tabs
    }

    /// Reopen the same tab kinds and target startup Ex only at the previous selection.
    pub(crate) fn restore_restart(
        restart: crate::app::Restart,
        enabled: bool,
    ) -> (Self, Option<String>) {
        use crate::app::RestartTab;
        let mut tabs = Self {
            tabs: Vec::new(),
            selected: 0,
            revision: 0,
            sessions: session::ProjectSessions::for_profile(enabled),
        };
        let mut selected = None;
        for (old_index, tab) in restart.tabs.into_iter().enumerate() {
            let index = match tab {
                RestartTab::Start => Some(tabs.add_start()),
                RestartTab::Project(root) if root.is_dir() => Some(tabs.add(root, None).0),
                RestartTab::File(path) if path.is_file() => Document::open(&path)
                    .ok()
                    .filter(|_| path.is_file())
                    .map(|document| {
                        let root = path
                            .parent()
                            .unwrap_or_else(|| Path::new("."))
                            .to_path_buf();
                        let project = Project::with_editor(
                            root,
                            EditorWindow::new(document),
                            None,
                            String::new(),
                        );
                        tabs.push(TabContent::Project(Box::new(project)))
                    }),
                RestartTab::Project(_) | RestartTab::File(_) => None,
            };
            if old_index == restart.selected {
                selected = index;
            }
        }
        if tabs.tabs.is_empty() || selected.is_none() {
            let index = tabs.add_start();
            tabs.activate(index);
        } else if let Some(index) = selected {
            tabs.activate(index);
        }
        let command =
            (selected.is_some() && !restart.command.is_empty()).then_some(restart.command);
        (tabs, command)
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

    /// Find the project with the given live plugin `scope`, even after another tab is selected.
    pub(super) fn project_by_scope_mut(&mut self, scope: u64) -> Option<&mut Project> {
        self.tabs
            .iter_mut()
            .filter_map(Tab::project_mut)
            .find(|project| project.plugin_scope == scope)
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

    /// Update every open project picker to the latest history `roots`.
    pub(super) fn refresh_project_pickers(&mut self, roots: &[PathBuf]) {
        for tab in &mut self.tabs {
            if let TabContent::Start(page) = &mut tab.content
                && let Some(picker) = &mut page.picker
            {
                picker.refresh_history(roots);
            }
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

    /// Visit every dirty named buffer, including editors on inactive pages and projects.
    pub(super) fn dirty_paths(&self) -> impl Iterator<Item = PathBuf> + '_ {
        let labels = self
            .ordered()
            .flat_map(Project::editors)
            .flat_map(|(_, editor)| editor.dirty_paths());
        let detached = self
            .ordered()
            .flat_map(Project::detached_documents)
            .filter(|doc| doc.modified())
            .filter_map(|doc| doc.path.clone());
        labels.chain(detached)
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
        let project = self.sessions.open(root, file);
        let index = self.push(TabContent::Project(Box::new(project)));
        (index, true)
    }

    /// Find the tab whose project is already loaded at canonical `root`.
    fn project_index(&self, root: &Path) -> Option<usize> {
        self.indexed()
            .find(|(_, project)| project.manager.is_some() && project.root == root)
            .map(|(index, _)| index)
    }

    /// Find an editor-only tab retaining `path` in any of its panes.
    pub(super) fn file_index(&self, path: &Path) -> Option<usize> {
        self.indexed()
            .find(|(_, tab)| tab.manager.is_none() && find_open_buffer(tab, path).is_some())
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
                let (pane, buffer, modified) = find_open_buffer(project, path)?;
                Some((tab, pane, buffer, modified))
            })
            .max_by_key(|&(tab, _, _, modified)| (modified, tab == self.selected));
        let editor = if let Some((tab, pane, buffer, _)) = existing {
            let owner = self
                .project_mut(tab)
                .editor_by_id_mut(pane)
                .expect("open editor is retained");
            let mut buffer = owner.take_buffer(buffer);
            buffer.vim.doc.detach_buffer();
            buffer.vim.detach_buffer_options();
            EditorWindow::with_buffer(buffer)
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
            TabContent::Project(Box::new(self.sessions.open(root, None)))
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
        } else if let Some(page) = self.start_mut()
            && let Some(picker) = &mut page.picker
        {
            picker.composition.clear();
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

    /// Index of the most recently used tab other than the selected one.
    pub(super) fn previous_tab_index(&self) -> Option<usize> {
        self.tabs
            .iter()
            .enumerate()
            .filter(|&(index, tab)| index != self.selected && tab.used > 0)
            .max_by_key(|(_, tab)| tab.used)
            .map(|(index, _)| index)
    }

    /// Move the selected tab to `index` without changing its activation history.
    pub(super) fn move_selected(&mut self, index: usize) {
        let index = index.min(self.len() - 1);
        let selected = self.tabs.remove(self.selected);
        self.tabs.insert(index, selected);
        self.selected = index;
        self.revision += 1;
    }

    /// Close the selected tab, preferring its right neighbor for project and file tabs.
    /// Start pages return to the most recently used tab, or return false to close the window.
    fn close_tab(&mut self) -> bool {
        let closing_start = self.start().is_some();
        if closing_start && self.len() == 1 {
            return false;
        }
        debug_assert!(
            self.active_project()
                .is_none_or(|project| project.editors().all(|(_, editor)| !editor.modified()))
        );
        self.tabs.remove(self.selected);
        if self.tabs.is_empty() {
            self.add_start();
        }
        let next = if closing_start {
            self.most_recent()
        } else {
            self.selected.min(self.len() - 1)
        };
        self.activate(next);
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

    /// Save buffer `index` in project `tab`'s focused editor to `path` or its filename.
    #[cfg(test)]
    pub(super) fn save_buffer(
        &mut self,
        tab: usize,
        index: usize,
        path: Option<PathBuf>,
    ) -> anyhow::Result<()> {
        let pane = self.project(tab).active_editor_id();
        self.save_editor_buffer(tab, pane, index, path)
    }

    /// Save buffer `index` from editor `pane` in project `tab` and invalidate transfers.
    /// If supplied, `path` cannot overwrite another open buffer's file by changing this buffer's name.
    pub(super) fn save_editor_buffer(
        &mut self,
        tab: usize,
        pane: usize,
        index: usize,
        path: Option<PathBuf>,
    ) -> anyhow::Result<()> {
        self.save_editor_buffer_to(tab, pane, index, path, false, false)
    }

    /// Write through an Ex bang while keeping filename and filesystem safety checks.
    pub(super) fn save_editor_buffer_forced(
        &mut self,
        tab: usize,
        pane: PaneId,
        index: usize,
        path: Option<PathBuf>,
    ) -> anyhow::Result<()> {
        self.save_editor_buffer_to(tab, pane, index, path, false, true)
    }

    /// Save buffer `index` in project `tab` and editor `pane` to a vacant explicit `path`.
    pub(super) fn save_new_editor_buffer(
        &mut self,
        tab: usize,
        pane: usize,
        index: usize,
        path: PathBuf,
    ) -> anyhow::Result<()> {
        self.save_editor_buffer_to(tab, pane, index, Some(path), true, false)
    }

    /// Save Session-retained text even when none of its pane labels are visible.
    pub(super) fn save_detached_buffer(
        &mut self,
        tab: usize,
        number: u64,
        path: Option<PathBuf>,
    ) -> anyhow::Result<()> {
        let view = self
            .project(tab)
            .session_buffer(number)
            .expect("retained buffer");
        let target = path
            .as_ref()
            .map(|path| path.canonicalize().unwrap_or_else(|_| path.clone()));
        let own_file = target
            .as_ref()
            .is_none_or(|target| view.vim.doc.path.as_ref() == Some(target));
        if own_file && view.vim.file_read_only() {
            anyhow::bail!(i18n::text("ex.read_only_file"));
        }
        if !own_file && let Some(target) = target {
            if self.indexed().any(|(index, project)| {
                project
                    .number_for_path(&target)
                    .is_some_and(|owner| owner != number || index != tab)
            }) {
                anyhow::bail!(i18n::text("editor_close.path_open"));
            }
        }
        let doc = self
            .project_mut(tab)
            .detached_document_mut(number)
            .expect("retained buffer");
        doc.refresh_shared();
        doc.save(path.as_deref())?;
        if let Some(path) = doc.path.clone() {
            for manager in self
                .iter_mut()
                .filter_map(|project| project.manager.as_mut())
            {
                manager.invalidate_history(&path);
            }
        }
        Ok(())
    }

    /// Resolve open-buffer conflicts before saving, then invalidate stale transfer history.
    fn save_editor_buffer_to(
        &mut self,
        tab: usize,
        pane: usize,
        index: usize,
        path: Option<PathBuf>,
        exclusive: bool,
        force: bool,
    ) -> anyhow::Result<()> {
        let owner = self
            .project(tab)
            .editor_by_id(pane)
            .expect("open editor is retained");
        let vim = &owner.buffers()[index].vim;
        let target = path
            .as_ref()
            .map(|path| path.canonicalize().unwrap_or_else(|_| path.clone()));
        let own_file = target
            .as_ref()
            .is_none_or(|target| vim.doc.path.as_ref() == Some(target));
        if own_file && vim.file_read_only() && !force {
            anyhow::bail!(i18n::text("ex.read_only_file"));
        }
        if !own_file && let Some(target) = target {
            if self
                .ordered()
                .any(|project| project.number_for_path(&target).is_some())
            {
                anyhow::bail!(i18n::text("editor_close.path_open"));
            }
        }
        let project = self.project_mut(tab);
        let editor = project
            .editor_by_id_mut(pane)
            .expect("open editor is retained");
        let saved = editor.save(index, path, exclusive)?;
        if own_file && force && editor.buffers()[index].vim.file_read_only() {
            editor.edit_buffer(index, |vim, _| {
                vim.set_file_read_only(false);
                nanobug::vim::Action::None
            });
        }
        if let Some(path) = saved {
            if project.manager.is_none() && pane == project.active_editor_id() {
                project.sync_file_root();
                self.revision += 1;
            }
            for manager in self
                .iter_mut()
                .filter_map(|project| project.manager.as_mut())
            {
                manager.invalidate_history(&path);
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
            let panes = project.editors().map(|(pane, _)| pane).collect::<Vec<_>>();
            for pane in panes {
                project
                    .editor_by_id_mut(pane)
                    .expect("open editor is retained")
                    .apply_path_changes(changes);
            }
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

/// Find the pane and buffer index for `path`, preferring a dirty draft then the focused pane.
fn find_open_buffer(project: &Project, path: &Path) -> Option<(PaneId, usize, bool)> {
    project
        .editors()
        .filter_map(|(pane, editor)| {
            let (index, buffer) = editor
                .buffers()
                .iter()
                .enumerate()
                .find(|(_, buffer)| buffer.vim.doc.path.as_deref() == Some(path))?;
            Some((pane, index, buffer.vim.doc.modified()))
        })
        .max_by_key(|&(pane, _, dirty)| (dirty, pane == project.active_editor_id()))
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
        #[cfg(windows)]
        let cancel_preedit = self
            .projects
            .start()
            .and_then(|page| page.picker.as_ref())
            .is_some_and(|picker| !picker.composition.is_empty());
        if self.picker_operation_pending() || !self.projects.select(index) {
            return false;
        }
        self.reset_tab_view(window, cx);
        #[cfg(windows)]
        if cancel_preedit {
            self.sync_native_ime(window, cx, true);
        }
        true
    }

    /// Complete a clean or confirmed close of `target` in `window`, refreshing a changed tab through `cx`.
    pub(super) fn finish_close(
        &mut self,
        target: CloseTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let CloseTarget::LastEditorTitle(id) = target {
            let now = self.scroll_epoch.elapsed().as_secs_f64();
            self.projects.active_mut().editor.remove_buffer_tab(id, now);
        }
        if target.closes_project() && !self.save_closing_project_sessions(target) {
            self.warn_project_session_close(target, window, cx);
            return;
        }
        self.finish_close_after_session(target, window, cx);
    }

    /// Release the confirmed close `target` after completing the Session write or warning.
    fn finish_close_after_session(
        &mut self,
        target: CloseTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match target {
            CloseTarget::Editor => self.hide_pane(Focus::Editor),
            CloseTarget::Split(pane) => self.finish_close_editor_split(pane, window, cx),
            CloseTarget::Page => self.finish_close_editor_page(window, cx),
            CloseTarget::OtherPages => {
                let now = self.scroll_epoch.elapsed().as_secs_f64();
                self.projects.active_mut().close_other_pages(now);
                self.reset_tab_view(window, cx);
            }
            CloseTarget::OtherSplits => self.finish_only_editor_split(window, cx),
            CloseTarget::Buffer(id) => {
                let now = self.scroll_epoch.elapsed().as_secs_f64();
                if self.projects.active_mut().editor.remove_buffer_tab(id, now) {
                    self.last_cursor_space = None;
                }
            }
            CloseTarget::SessionBuffer(number) => {
                let now = self.scroll_epoch.elapsed().as_secs_f64();
                self.projects
                    .active_mut()
                    .remove_session_buffer(number, now);
                self.last_cursor_space = None;
            }
            CloseTarget::EditorTitle(pane, id) => {
                self.finish_close_editor_title(pane, id, window, cx)
            }
            CloseTarget::BufferRange(reference, range) => {
                self.finish_close_buffer_range(reference, range)
            }
            CloseTarget::Tab | CloseTarget::LastEditorTitle(_) => {
                if let Some(project) = self.projects.active_project() {
                    self.ex_batch_undo.remove(&project.plugin_scope);
                }
                if self.projects.close_tab() {
                    self.reset_tab_view(window, cx);
                } else {
                    self.remove_main_window(window, cx);
                }
            }
            CloseTarget::Quit => self.remove_main_window(window, cx),
            CloseTarget::QuitWithCode(code) => {
                crate::app::set_exit_code(code);
                self.remove_main_window(window, cx);
            }
            CloseTarget::Restart => {
                let command = self.restart_ex_command.take().unwrap_or_default();
                use crate::app::RestartTab;
                let tabs = self
                    .projects
                    .tabs
                    .iter()
                    .map(|tab| match tab.project() {
                        Some(project) if project.manager.is_some() => {
                            RestartTab::Project(project.root.clone())
                        }
                        Some(project) => project
                            .editor
                            .vim()
                            .doc
                            .path
                            .clone()
                            .map_or(RestartTab::Start, RestartTab::File),
                        None => RestartTab::Start,
                    })
                    .collect();
                crate::app::schedule_restart(crate::app::Restart {
                    tabs,
                    selected: self.projects.selected,
                    command,
                });
                self.remove_main_window(window, cx);
            }
        }
    }

    /// Reset transient workspace state and apply the selected tab's cursor settings.
    pub(super) fn reset_tab_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.file_picker = None;
        self.vim_menu = None;
        self.remove_recent_project = None;
        self.help.close();
        self.popup_backdrop.release(window);
        self.prefix = None;
        self.hovered_controls = None;
        self.pane_resize = None;
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
        // Opening globally returns to an existing pane, preferring project drafts.
        let existing = self
            .projects
            .indexed()
            .filter_map(|(index, project)| {
                let (pane, _, modified) = find_open_buffer(project, &path)?;
                Some((index, pane, modified, project.manager.is_some()))
            })
            .max_by_key(|&(index, _, modified, is_project)| {
                (is_project, modified, index == self.projects.selected)
            });
        if let Some((index, pane, _, _)) = existing {
            let now = self.scroll_epoch.elapsed().as_secs_f64();
            let project = self.projects.project_mut(index);
            project.focus_editor_by_id(pane, now);
            project.editor.open(path, now)?;
            project.status.clear();
            self.select_tab(index, window, cx);
            self.last_cursor_space = None;
            return Ok(());
        }
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
        self.input_mappings.remember_ex_message(&message);
        if let Some(project) = self.projects.active_project_mut() {
            project.status = message;
        } else if let Some(page) = self.projects.start_mut() {
            page.status = message;
        }
    }
}

#[cfg(test)]
mod tests;
