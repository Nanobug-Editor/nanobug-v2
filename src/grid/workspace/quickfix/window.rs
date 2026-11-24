//! Display result lists as read-only editor splits and jump back to their source buffers.

use crate::grid::{
    windows::editor::{Buffer, ResultList},
    workspace::{
        Workspace,
        editor_close::CloseTarget,
        editor_splits::PaneId,
        projects::{Project, SplitDocument},
    },
};
use gpui::{Context, Window};
use nanobug::{
    document::{Document, DocumentAnchor},
    i18n,
    vim::QuickfixCommand,
};

impl Project {
    /// The global list or the location list attached to the source of the active window.
    pub(super) fn result_list_kind(&self, location: bool) -> ResultList {
        if !location {
            return ResultList::Quickfix;
        }
        match self.editor.buffer().list_owner() {
            Some((kind @ ResultList::Location(_), _)) => kind,
            _ => ResultList::Location(self.result_source_pane()),
        }
    }

    /// Resolve the source pane even when the caret is in a result list.
    pub(in crate::grid::workspace) fn result_source_pane(&self) -> PaneId {
        self.editor
            .buffer()
            .list_owner()
            .and_then(|(kind, source)| {
                let previous = match kind {
                    ResultList::Quickfix | ResultList::Diagnostics => self.splits().previous(),
                    _ => None,
                };
                previous
                    .into_iter()
                    .chain([source])
                    .find(|&id| {
                        self.editor_by_id(id)
                            .is_some_and(|editor| editor.buffer().list_owner().is_none())
                    })
                    .or_else(|| {
                        self.page_editors()
                            .find(|(_, editor)| editor.buffer().list_owner().is_none())
                            .map(|(id, _)| id)
                    })
            })
            .unwrap_or_else(|| self.active_editor_id())
    }

    /// Find an open window for this list on the current tabpage.
    pub(super) fn result_pane(&self, kind: ResultList) -> Option<PaneId> {
        self.page_editors()
            .find(|(_, editor)| {
                editor
                    .buffer()
                    .list_owner()
                    .is_some_and(|(list, _)| list == kind)
            })
            .map(|(id, _)| id)
    }
}

impl Workspace {
    /// Materialize the selected results in each open view, preserving independent scroll positions.
    pub(super) fn update_result_list_windows(&mut self, kind: ResultList, row: Option<usize>) {
        let project = self.projects.active_mut();
        let Some(list) = project.result_lists.current(kind) else {
            return;
        };
        let panes: Vec<_> = project
            .editors()
            .filter(|(_, editor)| editor.buffer().list_owner().is_some_and(|(k, _)| k == kind))
            .map(|(id, _)| id)
            .collect();
        if panes.is_empty() {
            return;
        }
        let rows = list.rows(&project.root);
        for pane in panes {
            if let Some(editor) = project.editor_by_id_mut(pane) {
                editor.update_result_rows(&rows, row);
            }
        }
    }

    /// Focus or create a read-only bottom split with the requested number of text rows.
    pub(super) fn open_result_list(
        &mut self,
        kind: ResultList,
        height: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let project = self.projects.active();
        if let Some(id) = project.result_pane(kind) {
            self.focus_editor_pane(id, window, cx);
            if let Some(rows) = height {
                let area = self.active_editor_area();
                let vertical = self.active_ex_window_modifiers.orientation
                    == Some(nanobug::vim::WindowOrientation::Vertical);
                let extent = if vertical { area.w } else { area.h };
                let delta =
                    rows.saturating_add(2).min(isize::MAX as usize) as isize - extent as isize;
                self.resize_editor_split(vertical, delta, window, cx);
            }
        } else {
            let source = project.result_source_pane();
            let rows = project
                .result_lists
                .current(kind)
                .map(|list| list.rows(&project.root))
                .unwrap_or_default();
            let buffer = Buffer::result_list(kind, source, &rows);
            let mut modifiers = self.active_ex_window_modifiers.clone();
            if !matches!(kind, ResultList::Location(_)) && modifiers.placement.is_none() {
                modifiers.placement = Some(nanobug::vim::WindowPlacement::Last);
            }
            self.with_ex_window_modifiers(modifiers, |workspace| {
                workspace.open_editor_split_at(
                    true,
                    false,
                    SplitDocument::SharedView(Box::new(buffer)),
                    Some(height.unwrap_or(10).saturating_add(2)),
                    window,
                    cx,
                );
            });
        }
        if self
            .projects
            .active()
            .editor
            .buffer()
            .list_owner()
            .is_some_and(|(list, _)| list == kind)
        {
            let current = self
                .projects
                .active()
                .result_lists
                .current(kind)
                .map_or(0, |list| list.current);
            let editor = &mut self.projects.active_mut().editor;
            editor.edit(|vim, _| {
                vim.set_cursor(vim.doc.starts[current.min(vim.doc.starts.len() - 1)]);
                nanobug::vim::Action::None
            });
        }
        self.sync(window, cx);
    }

    /// Close this tabpage's list pane, preserving the saved result index.
    pub(in crate::grid::workspace) fn close_result_list(
        &mut self,
        kind: ResultList,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let project = self.projects.active();
        if let Some(pane) = project.result_pane(kind) {
            if project.split_count() == 1 {
                self.set_status(i18n::text("quickfix.last_window").into());
                return;
            }
            let source = project.result_source_pane();
            let focused = project.active_editor_id() == pane;
            self.request_close(CloseTarget::Split(pane), window, cx);
            if focused && self.projects.active().editor_by_id(pane).is_none() {
                self.focus_editor_pane(source, window, cx);
                self.sync(window, cx);
            }
        }
    }

    /// Select an entry, reveal its source and update the list index only after a successful jump.
    pub(super) fn navigate_result(
        &mut self,
        kind: ResultList,
        command: QuickfixCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let project = self.projects.active();
        let Some(list) = project.result_lists.current(kind) else {
            self.set_status(i18n::text("quickfix.empty").into());
            return;
        };
        let index = match list.destination(command) {
            Ok(i) => i,
            Err(key) => {
                self.set_status(i18n::text(key).into());
                return;
            }
        };
        let entry = list.entries[index].clone();
        let position = list
            .anchors
            .get(index)
            .and_then(Option::as_ref)
            .map(DocumentAnchor::position);
        let path = position
            .as_ref()
            .and_then(|pos| pos.path.as_deref())
            .or(entry.path.as_deref());
        let resolve = |doc: &Document| {
            if let Some(pos) = &position {
                if doc.buffer_number() == pos.number {
                    return Some(pos.offset);
                }
                let range = doc.line_range(pos.location.0.min(doc.starts.len() - 1));
                return Some(
                    doc.text
                        .floor_char_boundary(range.start + pos.location.1.min(range.len())),
                );
            }
            (doc.buffer_number() == entry.buffer
                && doc.revision == entry.revision
                && doc.path == entry.path)
                .then_some(entry.offset)
        };
        let source = project.result_source_pane();
        if project.active_editor_id() != source {
            self.focus_editor_pane(source, window, cx);
        }
        let project = self.projects.active_mut();
        let now = self.scroll_epoch.elapsed().as_secs_f64();
        let jump = if let Some(path) = path {
            project.jump_file(path, true, now, resolve)
        } else if project
            .session_buffer(entry.buffer)
            .is_some_and(|b| resolve(&b.vim.doc).is_some())
            && project.select_session_buffer(entry.buffer, now)
        {
            let offset = resolve(&project.editor.vim().doc);
            if let Some(offset) = offset {
                project.editor.jump_to(offset);
            }
            Ok(offset.is_some())
        } else {
            Ok(false)
        };
        match jump {
            Ok(true) => {
                project
                    .result_lists
                    .history(kind)
                    .current_mut()
                    .unwrap()
                    .current = index;
                project.status.clear();
                project.editor.edit(|vim, _| {
                    vim.publish_ex_message("", false);
                    nanobug::vim::Action::None
                });
                self.update_result_list_windows(kind, Some(index));
                self.capture_ex_message(&entry.message);
            }
            Ok(false) => self.set_status(i18n::text("quickfix.stale").into()),
            Err(err) => self.set_status(format!("{err:#}")),
        }
        self.sync(window, cx);
    }

    /// Follow the result row under the caret after Enter or a double click.
    pub(in crate::grid::workspace) fn activate_result_row(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let project = self.projects.active();
        let Some((kind, _)) = project.editor.buffer().list_owner() else {
            return false;
        };
        let row = project
            .editor
            .vim()
            .doc
            .location(project.editor.vim().doc.cursor)
            .0;
        self.navigate_result(kind, QuickfixCommand::Current(Some(row + 1)), window, cx);
        true
    }
}
