//! Route the launcher and recent-project picker to workspace actions.

use super::Workspace;
use crate::grid::windows::{
    manager::{Entry, Explorer},
    start::{ProjectPicker, ProjectPickerAction, StartAction, StartHit, StartLayout, StartPage},
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
                preview,
                StartPage::content_metrics(self.metrics),
                &self.spacing,
            );
            self.start_panel
                .update(cx, |panel, cx| panel.set(frame, cx));
        }
    }

    /// Scan the visible selection off the UI thread, replacing stale preview requests.
    pub(super) fn sync_start_preview(&mut self, window: &Window, cx: &mut Context<Self>) {
        let selected = self
            .projects
            .start()
            .and_then(|page| page.picker.as_ref())
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

    /// Handle a non-leader launcher `event` in `window` through `cx`.
    pub(super) fn start_page_key(
        &mut self,
        event: &KeyDownEvent,
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
            StartAction::from_key(&key.key)
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

    /// Apply the picker action for `event` in `window`, updating the view through `cx`.
    /// `character` is the printable ASCII key resolved before native text translation.
    pub(super) fn project_picker_key(
        &mut self,
        event: &KeyDownEvent,
        character: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(StartLayout::Projects(layout)) = self.start_layout() else {
            return;
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
                return;
            }
            ProjectPickerAction::Close => page.picker = None,
            ProjectPickerAction::Ignore => return,
        }
        page.status.clear();
        self.sync(window, cx);
    }

    /// Apply a press to a launcher row, the project filter, or a project row in `window`.
    pub(super) fn press_start_page(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (x, y) = StartPage::content_metrics(self.metrics).cell(event.position);
        let Some(layout) = self.start_layout() else {
            return;
        };
        let page = self
            .projects
            .start_mut()
            .expect("start layout belongs to a start page");
        match (layout, layout.hit(page, x, y), event.click_count) {
            (_, Some(StartHit::Menu(action)), 1) => self.start_action(action, window, cx),
            (StartLayout::Projects(_), Some(StartHit::Filter), 1) => {
                page.picker
                    .as_mut()
                    .expect("visible project picker")
                    .focus_filter();
                self.update_cursor(window, cx);
            }
            (StartLayout::Projects(layout), Some(StartHit::Project(index)), count @ (1 | 2)) => {
                let picker = page.picker.as_mut().expect("visible project picker");
                picker.focus_list();
                if count == 2 && picker.selected() == Some(index) {
                    self.open_selected_project(window, cx);
                } else {
                    picker.select(index, layout.project_rows());
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
        let Some(layout @ StartLayout::Projects(projects)) = self.start_layout() else {
            return;
        };
        let (x, y) = StartPage::content_metrics(self.metrics).cell(position);
        if !layout.area().contains(x, y) {
            return;
        }
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
            picker.scroll_preview(offset, count, projects.preview_rows());
        } else {
            picker.move_selection(offset, projects.project_rows());
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
            StartAction::Directory | StartAction::File => {
                self.open_file_picker(window, cx);
                return;
            }
            StartAction::Config => self.set_status(i18n::text("start.config_unavailable").into()),
            StartAction::Quit => {
                if !self.projects.close_start() {
                    window.remove_window();
                    return;
                }
                self.reset_tab_view(window, cx);
            }
        }
        self.sync(window, cx);
    }
}
