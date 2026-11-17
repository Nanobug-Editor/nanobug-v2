//! Route commands and pointing into the independent editors on the selected tabpage.

use super::{
    Workspace,
    editor_close::CloseTarget,
    editor_splits::{DividerHit, PaneId, SplitAxis},
    layout::{Focus, stacked_divider_bounds},
    projects::SplitDocument,
};
use crate::grid::render::Rect;
use gpui::{Context, Pixels, Point, Window};
use nanobug::i18n;

impl Workspace {
    /// Clip split geometry to whole visible cells when zoom exceeds the base layout minimum.
    pub(super) fn visible_editor_region(&self) -> Rect {
        self.layout.visible_region(self.layout.editor)
    }

    /// Lay out the current tabpage's editor panes in the region alongside Explorer.
    pub(super) fn editor_rects(&self) -> Vec<(PaneId, Rect)> {
        self.projects
            .active_project()
            .map_or_else(Vec::new, |project| {
                project.editor_rects_for_tabpage(
                    project.active_page(),
                    self.layout,
                    self.layout.editor,
                )
            })
    }

    /// Return the selected editor's rectangle for input, the cursor and its transient overlays.
    pub(super) fn active_editor_area(&self) -> Rect {
        self.projects
            .active_project()
            .and_then(|project| {
                self.editor_rects()
                    .into_iter()
                    .find_map(|(id, area)| (id == project.active_editor_id()).then_some(area))
            })
            .unwrap_or(Rect {
                w: 0,
                h: 0,
                ..self.layout.editor
            })
    }

    /// Locate the editor owning grid cell (`x`, `y`) and return its full rectangle.
    pub(super) fn editor_at(&self, x: usize, y: usize) -> Option<(PaneId, Rect)> {
        self.editor_rects()
            .into_iter()
            .find(|(_, rect)| rect.contains(x, y))
    }

    /// Return all visible divider cells that can start a drag when the editor owns pointer input.
    pub(super) fn editor_dividers(&self) -> Vec<(SplitAxis, Rect)> {
        if self.modal_open() {
            return Vec::new();
        }
        self.projects
            .active_project()
            .filter(|project| !project.editor_zoomed())
            .map_or_else(Vec::new, |project| {
                project.splits().divider_rects(self.visible_editor_region())
            })
    }

    /// Identify the divider at `position` and the row correction when it is grabbed above its seam.
    pub(super) fn editor_divider_at(&self, position: Point<Pixels>) -> Option<(DividerHit, usize)> {
        let (x, y) = self.metrics.cell(position);
        let (axis, row) = self.editor_dividers().into_iter().find_map(|(axis, gap)| {
            if axis == SplitAxis::Rows {
                stacked_divider_bounds(gap, self.metrics)
                    .contains(&position)
                    .then_some((axis, gap.y))
            } else {
                gap.contains(x, y).then_some((axis, y))
            }
        })?;
        let hit = self.projects.active_project()?.splits().divider_at(
            self.visible_editor_region(),
            axis,
            x,
            row,
        )?;
        Some((hit, row.saturating_sub(y)))
    }

    /// Move the captured `divider` to the relevant coordinate of grid cell (`x`, `y`).
    pub(super) fn move_editor_divider(&mut self, divider: DividerHit, x: usize, y: usize) -> bool {
        let area = self.visible_editor_region();
        !self.modal_open()
            && self
                .projects
                .active_project_mut()
                .filter(|project| !project.editor_zoomed())
                .is_some_and(|project| project.splits_mut().move_divider(area, divider, x, y))
    }

    /// Focus pane `id` after committing the outgoing editor's modal input.
    /// The caller can immediately send a pointer action before repainting.
    pub(super) fn focus_editor_pane(
        &mut self,
        id: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let project = self.projects.active();
        if project.active_editor_id() == id && project.focus == Focus::Editor {
            return false;
        }
        self.finish_editor_input(window, cx);
        let now = self.scroll_epoch.elapsed().as_secs_f64();
        if !self.projects.active_mut().focus_editor_by_id(id, now) {
            return false;
        }
        self.reset_tab_view(window, cx);
        true
    }

    /// Open a pane above or left of the selected editor, optionally displaying `file`.
    pub(super) fn open_editor_split(
        &mut self,
        stacked: bool,
        file: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(_) = self.projects.active_project() else {
            return;
        };
        let source = if let Some(file) = file {
            let Some(path) = self.file_operand(&file) else {
                self.set_status(nanobug::i18n::format(
                    "editor.invalid_ex_argument",
                    &[("argument", &file)],
                ));
                return;
            };
            SplitDocument::File(path, None)
        } else {
            SplitDocument::Current
        };
        self.open_editor_split_at(stacked, true, source, None, window, cx);
    }

    /// Open `file` in this page's preview split, retaining focus and its existing buffers.
    /// Uses `window` and `cx` for split layout and viewport updates.
    pub(super) fn open_vim_preview(
        &mut self,
        file: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.projects.active_project() else {
            return;
        };
        if self.modal_open() {
            return;
        }
        let Some(path) = self.file_operand(&file) else {
            self.set_status(nanobug::i18n::format(
                "editor.invalid_ex_argument",
                &[("argument", &file)],
            ));
            return;
        };
        if self.ex_tab_insertion().is_some() {
            self.open_editor_split_at(
                true,
                false,
                SplitDocument::File(path, None),
                None,
                window,
                cx,
            );
            return;
        }
        let now = self.scroll_epoch.elapsed().as_secs_f64();
        if let Some(id) = project.preview_pane() {
            let project = self.projects.active_mut();
            let preview = project
                .editor_by_id_mut(id)
                .expect("retained preview split");
            if let Err(error) = preview.open(path, now) {
                self.set_status(format!("{error:#}"));
            } else {
                project.status.clear();
                self.sync(window, cx);
            }
            return;
        }
        let original = project.active_editor_id();
        let area = self.visible_editor_region();
        let position = self.ex_split_position(SplitAxis::Rows, false);
        self.finish_editor_input(window, cx);
        let project = self.projects.active_mut();
        match project.open_split_positioned(position, SplitDocument::File(path, None), area, now) {
            Ok(true) => {
                let preview = project.active_editor_id();
                project.mark_preview_pane(preview);
                project.focus_editor_by_id(original, now);
                project.status.clear();
                self.reset_tab_view(window, cx);
            }
            Ok(false) => self.set_status(i18n::text("editor.split_no_space").into()),
            Err(error) => self.set_status(format!("{error:#}")),
        }
        self.sync(window, cx);
    }

    /// Close the current page's preview via `window` and `cx`, resolving dirty buffers first.
    pub(super) fn close_vim_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.projects.active().preview_pane() {
            self.finish_editor_input(window, cx);
            self.request_close(CloseTarget::Split(id), window, cx);
        }
    }

    /// Split the focused editor below or to its right, using the LazyVim Leader placement.
    pub(super) fn open_leader_editor_split(
        &mut self,
        stacked: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .projects
            .active_project()
            .is_some_and(|project| project.focus == Focus::Editor)
        {
            self.open_editor_split_at(stacked, false, SplitDocument::Current, None, window, cx);
        }
    }

    /// Split above or below when `stacked`, with `new_first` choosing the top or left side.
    /// Open `source`, then apply its optional `size` in rows or columns in `window` through `cx`.
    pub(super) fn open_editor_split_at(
        &mut self,
        stacked: bool,
        new_first: bool,
        source: SplitDocument,
        size: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal_open() || self.projects.active_project().is_none() {
            return;
        }
        self.finish_editor_input(window, cx);
        let now = self.scroll_epoch.elapsed().as_secs_f64();
        if let Some(tab) = self.ex_tab_insertion() {
            if !self.projects.manager_available() {
                self.set_status(i18n::text("editor.tabpage_requires_project").into());
                return;
            }
            match self
                .projects
                .active_mut()
                .open_page_source(source, tab, now)
            {
                Ok(()) => {
                    self.set_status(String::new());
                    self.reset_tab_view(window, cx);
                }
                Err(error) => self.set_status(format!("{error:#}")),
            }
            self.sync(window, cx);
            return;
        }
        let position = self.ex_split_position(
            if stacked {
                SplitAxis::Rows
            } else {
                SplitAxis::Columns
            },
            new_first,
        );
        let axis = position.axis;
        let area = self.visible_editor_region();
        match self
            .projects
            .active_mut()
            .open_split_positioned(position, source, area, now)
        {
            Ok(true) => {
                let project = self.projects.active_mut();
                if let Some(size) = size
                    && let Some(pane) = project.splits().rect(project.active_editor_id(), area)
                {
                    let extent = if axis == SplitAxis::Rows {
                        pane.h
                    } else {
                        pane.w
                    };
                    let delta = size.min(isize::MAX as usize) as isize - extent as isize;
                    project.splits_mut().resize(area, axis, delta);
                }
                project.status.clear();
                self.reset_tab_view(window, cx);
            }
            Ok(false) => self.set_status(i18n::text("editor.split_no_space").into()),
            Err(error) => self.set_status(format!("{error:#}")),
        }
        self.sync(window, cx);
    }

    /// Transfer the current editor to a new tabpage at `count`, then refresh `window` through `cx`.
    pub(super) fn move_editor_split_to_page(
        &mut self,
        count: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.finish_editor_input(window, cx);
        let project = self.projects.active_mut();
        if project.move_split_to_page(count) {
            project.status.clear();
            self.reset_tab_view(window, cx);
            self.sync(window, cx);
        }
    }

    /// Whether the current editor can temporarily fill its Tabpage's editor region.
    pub(super) fn can_zoom_leader_window(&self) -> bool {
        self.projects
            .active_project()
            .is_some_and(|project| project.focus == Focus::Editor && project.split_count() > 1)
    }

    /// Show only the focused editor or restore the current page's split layout.
    pub(super) fn toggle_leader_window_zoom(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.modal_open() && self.can_zoom_leader_window() {
            self.pane_resize = None;
            self.projects.active_mut().toggle_editor_zoom();
            self.sync(window, cx);
        }
    }

    /// Follow up to `count` visible editor neighbors in the requested grid direction.
    pub(super) fn focus_editor_split(
        &mut self,
        horizontal: isize,
        vertical: isize,
        count: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let project = self.projects.active();
        if project.focus != Focus::Editor {
            return false;
        }
        let area = self.visible_editor_region();
        let splits = project.splits();
        let Some(mut id) = splits.neighbor(area, horizontal, vertical) else {
            return false;
        };
        for _ in 1..count {
            let Some(next) = splits.neighbor_from(id, area, horizontal, vertical) else {
                break;
            };
            id = next;
        }
        self.focus_editor_pane(id, window, cx)
    }

    /// Move to the previous editor pane unless the previous window was Explorer.
    pub(super) fn focus_previous_editor_split(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let project = self.projects.active();
        if project.focus != Focus::Editor || project.previous_focus == Some(Focus::Manager) {
            return false;
        }
        let Some(id) = project.splits().previous() else {
            return false;
        };
        self.focus_editor_pane(id, window, cx)
    }

    /// Cycle through editor panes and Explorer, or select an explicit one-based `number`.
    pub(super) fn cycle_editor_split(
        &mut self,
        reverse: bool,
        number: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let project = self.projects.active();
        if project.split_count() < 2 || self.layout.editor.w == 0 {
            return false;
        }
        let mut panes: Vec<_> = project.splits().ids().into_iter().map(Some).collect();
        if self.layout.tree.w > 0 {
            if project.editor_first {
                panes.push(None);
            } else {
                panes.insert(0, None);
            }
        }
        let selected = (project.focus == Focus::Editor).then(|| project.active_editor_id());
        let current = panes.iter().position(|pane| *pane == selected).unwrap_or(0);
        let index = number.map_or_else(
            || (current + if reverse { panes.len() - 1 } else { 1 }) % panes.len(),
            |number| number.saturating_sub(1).min(panes.len() - 1),
        );
        if let Some(id) = panes[index] {
            self.focus_editor_pane(id, window, cx);
        } else if project.focus == Focus::Editor {
            self.finish_editor_input(window, cx);
            self.projects.active_mut().focus_pane(Focus::Manager);
            self.last_cursor_space = None;
        }
        true
    }

    /// Resize the selected editor by `delta` grid cells, claiming the command when its axis exists.
    pub(super) fn resize_editor_split(
        &mut self,
        horizontal: bool,
        delta: isize,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> bool {
        let area = self.visible_editor_region();
        let active = self.active_editor_area();
        if self.projects.active().focus != Focus::Editor
            || (if horizontal {
                active.w >= area.w
            } else {
                active.h >= area.h
            })
        {
            return false;
        }
        let axis = if horizontal {
            SplitAxis::Columns
        } else {
            SplitAxis::Rows
        };
        self.projects
            .active_mut()
            .splits_mut()
            .resize(area, axis, delta);
        true
    }

    /// Equalize the active page's visible editor splits within their existing editor area.
    pub(super) fn equalize_visible_editors(&mut self) {
        let visible = |area: Rect| {
            area.w > 0
                && area.h > 0
                && area.x < self.layout.visible_cols
                && area.y < self.layout.visible_rows
        };
        let editors = self
            .editor_rects()
            .into_iter()
            .filter_map(|(id, area)| visible(area).then_some(id))
            .collect::<Vec<_>>();
        if editors.len() > 1 {
            let dimension = match self.active_ex_window_modifiers.orientation {
                Some(nanobug::vim::WindowOrientation::Vertical) => Some(SplitAxis::Rows),
                Some(nanobug::vim::WindowOrientation::Horizontal) => Some(SplitAxis::Columns),
                None => None,
            };
            self.projects
                .active_mut()
                .splits_mut()
                .equalize_axis(&editors, dimension);
        }
    }

    /// Request closing the current editor or an explicit one-based editor `number`.
    pub(super) fn close_editor_split(
        &mut self,
        number: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let project = self.projects.active();
        if project.split_count() < 2 || (number.is_none() && project.focus != Focus::Editor) {
            return false;
        }
        let id = if let Some(number) = number {
            let Some(id) = project
                .splits()
                .ids()
                .get(number.saturating_sub(1))
                .copied()
            else {
                return true;
            };
            id
        } else {
            project.active_editor_id()
        };
        self.finish_editor_input(window, cx);
        self.request_close(CloseTarget::Split(id), window, cx);
        true
    }

    /// Guard all sibling buffers before retaining only the selected editor in this page.
    pub(super) fn only_editor_split(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let project = self.projects.active();
        if project.split_count() < 2 || project.focus != Focus::Editor {
            return false;
        }
        self.finish_editor_input(window, cx);
        self.request_close(CloseTarget::OtherSplits, window, cx);
        true
    }

    /// Close `id` after resolving its buffers and restore the surviving editor's input route.
    pub(super) fn finish_close_editor_split(
        &mut self,
        id: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let now = self.scroll_epoch.elapsed().as_secs_f64();
        let project = self.projects.active_mut();
        let original = project.active_editor_id();
        if project.split_count() > 1 && project.splits().ids().contains(&id) {
            project.focus_editor_by_id(id, now);
            project.close_split();
            if original != id {
                project.focus_editor_by_id(original, now);
            }
            self.reset_tab_view(window, cx);
        }
    }

    /// Remove the resolved tabpage and restore its neighbor via `window` and `cx`.
    pub(super) fn finish_close_editor_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let now = self.scroll_epoch.elapsed().as_secs_f64();
        if self.projects.active_mut().close_page(now) {
            self.projects.active_mut().status.clear();
            self.reset_tab_view(window, cx);
        }
    }

    /// Remove all resolved siblings of the current editor within this tabpage.
    pub(super) fn finish_only_editor_split(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.projects.active_mut().only_split();
        self.reset_tab_view(window, cx);
    }
}
