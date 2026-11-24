//! Route the launcher and recent-project picker to workspace actions.

use super::{Workspace, editor_close::CloseTarget};
use crate::grid::{
    render::{GridLayer, Rect},
    windows::{
        manager::{Entry, Explorer},
        start::{
            ProjectPicker, ProjectPickerAction, RemoveProject, StartAction, StartHit, StartLayout,
            StartPage,
        },
    },
};
use gpui::{Context, KeyDownEvent, MouseDownEvent, Pixels, Point, Task, Window};
use nanobug::i18n;
use std::path::{Path, PathBuf};

/// One directory scan for the selected project in the visible picker.
pub(super) struct StartPreview {
    /// Directory whose immediate children belong to this scan.
    root: PathBuf,
    /// Completed rows or the scan error. None while loading.
    rows: Option<Result<Vec<Entry>, String>>,
    /// Cancel delivery of the scan when selection or tab changes.
    _load: Task<()>,
}

impl Workspace {
    /// Open a fresh start tab in `window` and display it through `cx`.
    pub(super) fn open_start_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let index = self.projects.add_start();
        self.select_tab(index, window, cx);
        self.sync(window, cx);
    }

    /// Derive the selected start page's placement and hit regions in its own row geometry.
    pub(super) fn start_layout(&self) -> Option<StartLayout> {
        let metrics = StartPage::content_metrics(self.metrics);
        let rows = (self.layout.rows as f32 * self.metrics.lh / metrics.lh) as usize;
        Some(self.projects.start()?.layout(self.layout.cols, rows))
    }

    /// Update the selected start page's retained frame through `cx`.
    pub(super) fn refresh_start_page(&self, cx: &mut Context<Self>) {
        if let (Some(page), Some(layout)) = (self.projects.start(), self.start_layout()) {
            let preview = self
                .start_preview
                .as_ref()
                .and_then(|preview| preview.rows.as_ref())
                .map(|result| result.as_deref().map_err(String::as_str));
            let frame = page.frame(
                layout,
                self.recent_projects.error.as_deref(),
                StartPage::content_metrics(self.metrics),
                &self.spacing,
            );
            if let StartLayout::Projects(area) = layout
                && let Some(picker) = &page.picker
            {
                for (panel, body) in
                    self.start_bodies
                        .iter()
                        .zip(picker.bodies(area, preview, &self.spacing))
                {
                    if let Some(body) = body {
                        panel.update(cx, |panel, cx| panel.set(body, cx));
                    }
                }
            }
            self.start_panel
                .update(cx, |panel, cx| panel.set(frame, cx));
        }
    }

    /// Sample both picker scroll offsets before drawing or resolving its next pointer event.
    pub(super) fn advance_start_lists(&mut self) {
        let Some(StartLayout::Projects(layout)) = self.start_layout() else {
            return;
        };
        let count = self
            .start_preview
            .as_ref()
            .and_then(|p| p.rows.as_ref()?.as_ref().ok())
            .map_or(0, Vec::len);
        if let Some(picker) = self.projects.start_mut().and_then(|p| p.picker.as_mut()) {
            picker.track(
                layout.project_rows(),
                layout.preview_rows(),
                count,
                self.scroll_epoch.elapsed().as_secs_f64(),
            );
        }
    }
    /// Fixed chrome followed by the clipped bodies that belong to this start tab.
    pub(super) fn start_layers(&self, layout: Option<StartLayout>) -> Vec<GridLayer> {
        let Some(layout) = layout else {
            return Vec::new();
        };
        let metrics = StartPage::content_metrics(self.metrics);
        let mut layers = vec![GridLayer {
            panel: self.start_panel.clone(),
            bounds: layout.frame_area().pixels(metrics),
            clip: None,
        }];
        if let StartLayout::Projects(area) = layout
            && let Some(picker) = self.projects.start().and_then(|p| p.picker.as_ref())
        {
            for (panel, viewport) in self.start_bodies.iter().zip(picker.body_viewports(area)) {
                if let Some((rect, fraction)) = viewport {
                    layers.push(GridLayer::scrolled(panel, rect, fraction, metrics));
                }
            }
        }
        layers
    }

    /// Scan the visible selection off the UI thread, replacing stale preview requests.
    pub(super) fn sync_start_preview(&mut self, window: &Window, cx: &mut Context<Self>) {
        let selected = self
            .projects
            .start()
            .and_then(|page| page.picker.as_ref())
            .filter(|picker| picker.shows_preview())
            .and_then(|picker| picker.selected_root());
        if selected
            == self
                .start_preview
                .as_ref()
                .map(|preview| preview.root.as_path())
        {
            return;
        }
        self.start_preview = selected.map(Path::to_path_buf).map(|root| {
            let requested = root.clone();
            let scan = cx.background_executor().spawn(async move {
                Explorer::preview(requested).map_err(|error| error.root_cause().to_string())
            });
            let requested = root.clone();
            let load = cx.spawn_in(window, async move |view, cx| {
                let rows = scan.await;
                let _ = cx.update(|_, cx| {
                    let _ = view.update(cx, |workspace, cx| {
                        if let Some(preview) = workspace
                            .start_preview
                            .as_mut()
                            .filter(|preview| preview.root == requested)
                        {
                            preview.rows = Some(rows);
                            workspace.refresh_start_page(cx);
                            cx.notify();
                        }
                    });
                });
            });
            StartPreview {
                root,
                rows: None,
                _load: load,
            }
        });
    }

    /// Handle a non-leader launcher `event` and its ASCII `character` in `window` through `cx`.
    pub(super) fn start_page_key(
        &mut self,
        event: &KeyDownEvent,
        character: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = &event.keystroke;
        let mods = key.modifiers;
        if mods.control || mods.alt || mods.platform || mods.function {
            return;
        }
        let Some(page) = self.projects.start_mut() else {
            return;
        };
        let action = if key.key == "enter" {
            Some(page.selected_action())
        } else {
            character.and_then(StartAction::from_key)
        };
        if !event.is_held
            && let Some(action) = action
        {
            if action == StartAction::Quit {
                self.held_close_key = Some(key.key.clone());
            }
            self.start_action(action, window, cx);
            return;
        }
        match key.key.as_str() {
            "up" | "k" => page.move_menu(-1),
            "down" | "j" => page.move_menu(1),
            _ => return,
        }
        self.update_cursor(window, cx);
        cx.notify();
    }

    /// Apply the picker action for `event` in `window` through `cx`, returning whether it consumed the key.
    /// `character` is the printable ASCII command resolved before native text translation.
    pub(super) fn project_picker_key(
        &mut self,
        event: &KeyDownEvent,
        character: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(StartLayout::Projects(layout)) = self.start_layout() else {
            return true;
        };
        let page = self
            .projects
            .start_mut()
            .expect("picker is on a start page");
        let picker = page.picker.as_mut().expect("visible project picker");
        match picker.key(event, character, layout.project_rows()) {
            ProjectPickerAction::Update => {}
            ProjectPickerAction::Open => {
                self.open_selected_project(window, cx);
                return true;
            }
            ProjectPickerAction::ConfirmRemove => {
                self.remove_recent_project = picker.selected_root().map(|root| RemoveProject {
                    root: root.to_path_buf(),
                });
            }
            ProjectPickerAction::Close => page.picker = None,
            ProjectPickerAction::Text => return false,
            ProjectPickerAction::Ignore => return true,
        }
        page.status.clear();
        self.sync(window, cx);
        true
    }

    /// Return the bounds of an active recent-project confirmation in workspace cells.
    pub(super) fn remove_recent_project_area(&self) -> Option<Rect> {
        self.remove_recent_project
            .as_ref()
            .map(|prompt| prompt.area(self.layout.cols, self.layout.rows))
    }

    /// Paint the current recent-project confirmation into the shared popup panel through `cx`.
    pub(super) fn refresh_remove_recent_project(&self, cx: &mut Context<Self>) {
        if let (Some(prompt), Some(area)) = (
            &self.remove_recent_project,
            self.remove_recent_project_area(),
        ) {
            let frame = prompt.frame(area, &self.spacing);
            self.popup_panel
                .update(cx, |panel, cx| panel.set(frame, cx));
        }
    }

    /// Consume `event` while the recent-project confirmation owns input in `window` through `cx`.
    pub(super) fn remove_recent_project_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.remove_recent_project.is_none() {
            return false;
        }
        if let Some(confirm) = RemoveProject::key_choice(event) {
            self.held_close_key = Some(event.keystroke.key.clone());
            self.finish_remove_recent_project(confirm, window, cx);
        }
        true
    }

    /// Use `event` to answer the confirmation in `window` through `cx` and consume clicks behind it.
    pub(super) fn press_remove_recent_project(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(area) = self.remove_recent_project_area() else {
            return false;
        };
        if event.click_count == 1
            && let Some(confirm) =
                RemoveProject::click_choice(area, self.metrics.cell(event.position), &self.spacing)
        {
            self.finish_remove_recent_project(confirm, window, cx);
        }
        true
    }

    /// If `confirm` is true, persist the removal, then restore picker focus in `window` through `cx`.
    pub(super) fn finish_remove_recent_project(
        &mut self,
        confirm: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(prompt) = self.remove_recent_project.take() else {
            return;
        };
        if confirm {
            match self.recent_projects.remove(&prompt.root) {
                Ok(()) => self
                    .projects
                    .refresh_project_pickers(&self.recent_projects.roots),
                Err(error) => {
                    self.set_status(i18n::format("start.remove_failed", &[("error", &error)]))
                }
            }
        }
        self.sync(window, cx);
    }

    /// Close the recent-project pane named by a middle click, returning to the launcher after both close.
    pub(super) fn middle_close_start_title(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(StartLayout::Projects(layout)) = self.start_layout() else {
            return false;
        };
        let page = self
            .projects
            .start_mut()
            .expect("picker is on a start page");
        let picker = page.picker.as_mut().expect("visible project picker");
        let metrics = StartPage::content_metrics(self.metrics);
        match picker.close_title_at(layout, position, metrics, &self.spacing) {
            ProjectPickerAction::Close => {
                page.picker = None;
                page.status.clear();
            }
            ProjectPickerAction::Update => {}
            _ => return false,
        }
        self.sync(window, cx);
        true
    }

    /// Apply a press to a launcher row, the project filter, or a project row in `window`.
    pub(super) fn press_start_page(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let metrics = StartPage::content_metrics(self.metrics);
        let (x, _) = metrics.cell(event.position);
        let y = f32::from(event.position.y - metrics.origin.y) / metrics.lh;
        let Some(layout) = self.start_layout() else {
            return;
        };
        let page = self
            .projects
            .start_mut()
            .expect("start layout belongs to a start page");
        match (layout, layout.hit(page, x, y), event.click_count) {
            (_, Some(StartHit::Menu(action)), 1) => self.start_action(action, window, cx),
            (StartLayout::Projects(layout), Some(StartHit::Filter), 1) => {
                page.picker
                    .as_mut()
                    .expect("visible project picker")
                    .press_filter(
                        layout,
                        event.position,
                        StartPage::content_metrics(self.metrics),
                        &self.spacing,
                        event.modifiers.shift,
                    );
                self.sync(window, cx);
            }
            (StartLayout::Projects(_), Some(StartHit::Project(index)), count @ (1 | 2)) => {
                let picker = page.picker.as_mut().expect("visible project picker");
                picker.focus_list();
                if count == 2 && picker.selected() == Some(index) {
                    self.open_selected_project(window, cx);
                } else {
                    picker.select(index);
                    page.status.clear();
                    self.sync(window, cx);
                }
            }
            _ => {}
        }
    }

    /// Scroll the picker pane under `position` by `offset` rows in `window`.
    pub(super) fn scroll_start_page(
        &mut self,
        position: Point<Pixels>,
        offset: isize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(StartLayout::Projects(projects)) = self.start_layout() else {
            return;
        };
        let (x, y) = StartPage::content_metrics(self.metrics).cell(position);
        let picker = self
            .projects
            .start_mut()
            .and_then(|page| page.picker.as_mut())
            .expect("visible project picker");
        if projects.over_preview(x, y) {
            let count = self
                .start_preview
                .as_ref()
                .and_then(|preview| preview.rows.as_ref())
                .and_then(|result| result.as_ref().ok())
                .map_or(0, Vec::len);
            picker.scroll_preview(
                offset,
                count,
                projects.preview_rows(),
                self.scroll_epoch.elapsed().as_secs_f64(),
            );
        } else if projects.over_list(x, y) {
            picker.move_selection(offset);
        } else {
            return;
        }
        self.sync(window, cx);
    }

    /// Open the highlighted project, leaving the picker visible if its directory is unavailable.
    fn open_selected_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let root = self
            .projects
            .start()
            .and_then(|page| page.picker.as_ref())
            .and_then(|picker| picker.selected_root())
            .map(Path::to_path_buf);
        if let Some(root) = root {
            self.set_status(String::new());
            self.open_project(&root, window, cx);
            self.sync(window, cx);
        }
    }

    /// Execute launcher `action` from its shortcut, Enter or a click in `window`.
    fn start_action(&mut self, action: StartAction, window: &mut Window, cx: &mut Context<Self>) {
        let page = self.projects.start_mut().expect("launcher is visible");
        page.status.clear();
        page.select_action(action);
        match action {
            StartAction::Projects => {
                self.recent_projects.refresh();
                page.picker = Some(Box::new(ProjectPicker::new(&self.recent_projects.roots)));
            }
            StartAction::Open => {
                self.open_file_picker(window, cx);
                return;
            }
            StartAction::Config => {
                self.open_settings(window, cx);
                return;
            }
            StartAction::Quit => self.request_close(CloseTarget::Tab, window, cx),
        }
        self.sync(window, cx);
    }
}
