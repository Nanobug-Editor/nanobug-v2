//! Route commands and pointing into the independent editors on the selected tabpage.

use super::{
    Workspace,
    editor_close::CloseTarget,
    editor_splits::{PaneId, SplitAxis},
    layout::Focus,
};
use crate::grid::render::Rect;
use gpui::{Context, Window};
use nanobug::i18n;

impl Workspace {
    /// Clip split geometry to whole visible cells when zoom exceeds the base layout minimum.
    fn visible_editor_region(&self) -> Rect {
        let area = self.layout.editor;
        Rect {
            w: area.w.min(self.layout.visible_cols.saturating_sub(area.x)),
            h: area.h.min(self.layout.visible_rows.saturating_sub(area.y)),
            ..area
        }
    }

    /// Lay out the current tabpage's editor panes in the region alongside Explorer.
    pub(super) fn editor_rects(&self) -> Vec<(PaneId, Rect)> {
        self.projects
            .active_project()
            .map_or_else(Vec::new, |project| {
                let area = if project.split_count() == 1 {
                    self.layout.editor
                } else {
                    self.visible_editor_region()
                };
                project.splits().rects(area)
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
        if self.modal_open() || self.projects.active_project().is_none() {
            return;
        }
        self.finish_editor_input(window, cx);
        let now = self.scroll_epoch.elapsed().as_secs_f64();
        let path = file.map(|file| self.projects.active().root.join(file));
        let axis = if stacked {
            SplitAxis::Rows
        } else {
            SplitAxis::Columns
        };
        let area = self.visible_editor_region();
        match self.projects.active_mut().open_split(axis, path, area, now) {
            Ok(true) => {
                self.projects.active_mut().status.clear();
                self.reset_tab_view(window, cx);
            }
            Ok(false) => self.set_status(i18n::text("editor.split_no_space").into()),
            Err(error) => self.set_status(format!("{error:#}")),
        }
        self.sync(window, cx);
    }

    /// Move to the nearest editor in the requested grid direction when one is visible.
    pub(super) fn focus_editor_split(
        &mut self,
        horizontal: isize,
        vertical: isize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let project = self.projects.active();
        if project.focus != Focus::Editor {
            return false;
        }
        let Some(id) =
            project
                .splits()
                .neighbor(self.visible_editor_region(), horizontal, vertical)
        else {
            return false;
        };
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

    /// Restore balanced sizes inside the editor group, leaving the Explorer divider in place.
    pub(super) fn equalize_editor_splits(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> bool {
        let project = self.projects.active_mut();
        if project.split_count() < 2 {
            return false;
        }
        project.splits_mut().equalize();
        true
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
