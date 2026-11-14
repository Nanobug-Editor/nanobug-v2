//! Pointer hit testing, selection, pane resizing and wheel input.

use super::{
    Workspace,
    layout::{Focus, ShellWindow, near_corner_controls},
};
use crate::grid::windows::editor::{EditorWindow, HeaderTarget};
use gpui::{
    ClickEvent, Context, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollEvent,
    Window, px,
};
use nanobug::animation::scroll::ScrollInput;

impl Workspace {
    /// Find the visible window whose corner controls are near the window-local pointer `position`.
    pub(super) fn corner_controls_at(&self, position: Point<Pixels>) -> Option<ShellWindow> {
        if self.small_modal_open() {
            return None;
        }
        let near = |target: ShellWindow| {
            near_corner_controls(
                self.window_area(target),
                target.has_help(),
                self.metrics,
                position,
            )
        };
        if self.help.is_open() {
            return (!self.help.is_editor_help() && near(ShellWindow::ShortcutHelp))
                .then_some(ShellWindow::ShortcutHelp);
        }
        if self.file_picker.is_some() {
            return (self.file_picker_controls_available() && near(ShellWindow::FilePicker))
                .then_some(ShellWindow::FilePicker);
        }
        if self.projects.start().is_some() {
            return None;
        }
        let manager = ShellWindow::Pane(Focus::Manager);
        near(manager).then_some(manager)
    }

    /// Update the corner controls through `cx` when the hovered `target` changes.
    pub(super) fn set_controls_hover(
        &mut self,
        target: Option<ShellWindow>,
        cx: &mut Context<Self>,
    ) {
        if self.hovered_controls != target {
            self.hovered_controls = target;
            if self.help.is_open() {
                self.refresh_help_panels(cx);
            } else {
                self.refresh_panels(cx);
            }
            cx.notify();
        }
    }

    /// Activate `target`'s close button from `event` and repaint `window` through `cx`.
    pub(super) fn click_close_button(
        &mut self,
        target: ShellWindow,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // After a pane expands, its close button can be under the second press of a double click.
        if self.hovered_controls != Some(target)
            || matches!(event, ClickEvent::Mouse(click) if click.down.click_count > 1)
        {
            return;
        }
        self.close_grid_window(target, window, cx);
        self.sync(window, cx);
        cx.stop_propagation();
    }

    /// Use `event` to select or activate an item, place the editor cursor, or begin resizing.
    /// `window` and `cx` update the focus and repaint the selected viewport.
    pub(super) fn mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.plugin_click(event, window, cx) {
            cx.stop_propagation();
            return;
        }
        #[cfg(feature = "plugin-probe")]
        if self.probe_click(event.position, window, cx) {
            cx.stop_propagation();
            return;
        }
        #[cfg(windows)]
        if event.position.y < self.native_caption_height() {
            // GPUI forwards caption presses here before Windows starts its native move.
            return;
        }
        self.dismiss_sequence_hint(cx);
        self.prefix = None;
        if let Some(manager) = self
            .projects
            .active_project_mut()
            .and_then(|project| project.manager.as_mut())
        {
            manager.cancel_pending_undo();
            manager.clear_count();
        }
        if self.press_editor_close(event, window, cx)
            || self.press_remove_recent_project(event, window, cx)
            || self.press_language_picker(event.position, window, cx)
        {
            cx.stop_propagation();
            return;
        }
        if self.help.is_open() {
            let area = self.help_layout();
            if self.help.press(
                event.position,
                area,
                self.metrics,
                self.scroll_epoch.elapsed().as_secs_f64(),
            ) {
                self.cursor
                    .update(cx, |cursor, cx| cursor.activity(window, cx));
                self.repaint_help(ScrollInput::Advance, window, cx);
            }
            cx.stop_propagation();
            return;
        }
        window.focus(&self.focus_handle, cx);
        self.cursor
            .update(cx, |cursor, cx| cursor.activity(window, cx));
        if self.file_picker.is_some() {
            self.press_file_picker(event, window, cx);
            cx.stop_propagation();
            return;
        }
        let (x, y) = self.metrics.cell(event.position);
        if let Some(target) = self.editor_header_target(x, y) {
            match target {
                HeaderTarget::Buffer(id) | HeaderTarget::Overflow(id) => {
                    self.finish_editor_input(window, cx);
                    let now = self.scroll_epoch.elapsed().as_secs_f64();
                    let project = self.projects.active_mut();
                    project.editor.select_buffer(id, now);
                    project.focus_pane(Focus::Editor);
                    project.status.clear();
                    self.last_cursor_space = None;
                    self.sync(window, cx);
                }
                HeaderTarget::Tabpage(index) => self.select_editor_page(index, window, cx),
            }
            cx.stop_propagation();
            return;
        }
        let Some(project) = self.projects.active_project_mut() else {
            self.press_start_page(event, window, cx);
            cx.stop_propagation();
            return;
        };
        let first_click = project
            .manager
            .as_mut()
            .and_then(|manager| manager.take_click());
        let first = if project.editor_first {
            self.layout.editor
        } else {
            self.layout.tree
        };
        if project.manager.is_some()
            && project.hidden_pane.is_none()
            && if project.stacked {
                x >= first.x && x < first.x + first.w && y == first.y + first.h
            } else {
                y >= first.y
                    && y < first.y + first.h
                    && (x == first.x + first.w || x == first.x + first.w.saturating_sub(1))
            }
        {
            self.resizing = true;
        } else if project.manager.is_some() && self.layout.tree.contains(x, y) {
            project.focus_pane(Focus::Manager);
            let manager = project.manager.as_mut().expect("visible Explorer");
            if let Some(selection) = manager.press(
                event,
                self.layout.tree,
                self.metrics,
                first_click,
                self.scroll_epoch.elapsed().as_secs_f64(),
            ) {
                if let Some(path) = selection.open {
                    self.open(path, window, cx);
                }
                self.layout = self.derive_layout(window);
                self.repaint(
                    [
                        ScrollInput::Advance,
                        ScrollInput::Reveal,
                        ScrollInput::Reveal,
                    ],
                    window,
                    cx,
                );
                return;
            }
        } else if self.layout.editor.contains(x, y) {
            project.focus_pane(Focus::Editor);
            if y == self.layout.editor.y || y == self.layout.editor.y + self.layout.editor.h - 1 {
                self.sync(window, cx);
                return;
            }
            project.editor.press(
                event.position,
                self.layout.editor,
                self.metrics,
                &self.spacing,
            );
        }
        self.sync(window, cx);
    }

    /// Identify a visible editor title entry at the same grid cells used to draw it.
    fn editor_header_target(&self, x: usize, y: usize) -> Option<HeaderTarget> {
        let area = self.layout.editor;
        if y != area.y || !area.contains(x, y) {
            return None;
        }
        self.projects
            .active_project()?
            .editor
            .header(area, &self.spacing, self.layout.visible_cols)
            .target_at(x, y)
    }

    /// Remove a file label on middle-click, while preserving Canvas input and modal ownership.
    pub(super) fn middle_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.plugin_canvas_down(event) {
            cx.stop_propagation();
            return;
        }
        if self.modal_open() || event.click_count > 1 || self.plugins.file_busy {
            return;
        }
        let (x, y) = self.metrics.cell(event.position);
        if let Some(HeaderTarget::Buffer(id)) = self.editor_header_target(x, y) {
            self.finish_editor_input(window, cx);
            self.request_close(super::editor_close::CloseTarget::Buffer(id), window, cx);
            self.sync(window, cx);
            cx.stop_propagation();
        }
    }

    /// Update close button hover from `event` or continue resizing or selection in `window`.
    pub(super) fn mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.file_picker.is_some() {
            if !self.help.is_open() && !self.small_modal_open() {
                let area = self.file_picker_layout().preview_editor_area();
                if let Some(editor) = self
                    .file_picker
                    .as_mut()
                    .and_then(|session| session.focused_editor_mut())
                {
                    if !event.dragging() {
                        editor.stop_drag();
                    } else if editor.drag(event.position, area, self.metrics, &self.spacing) {
                        self.sync(window, cx);
                        return;
                    }
                }
            }
            self.set_controls_hover(self.corner_controls_at(event.position), cx);
            return;
        }
        if self.drag_plugin_field(event, window, cx) {
            cx.stop_propagation();
            return;
        }
        let Some(project) = self.projects.active_project_mut() else {
            return;
        };
        if !event.dragging() {
            self.resizing = false;
            project.editor.stop_drag();
        }
        if self.resizing {
            let (x, y) = self.metrics.cell(event.position);
            if project.stacked {
                let first_rows = y.saturating_sub(1);
                let rows = self.layout.tree.h + self.layout.editor.h;
                project.manager_height = Some(if project.editor_first {
                    rows.saturating_sub(first_rows)
                } else {
                    first_rows
                });
            } else {
                project.sidebar = if project.editor_first {
                    self.layout.cols.saturating_sub(x + 1)
                } else {
                    x
                };
            }
        } else if project.editor.is_dragging() {
            if !project.editor.drag(
                event.position,
                self.layout.editor,
                self.metrics,
                &self.spacing,
            ) {
                return;
            }
        } else {
            self.plugin_canvas_move(event);
            self.set_controls_hover(self.corner_controls_at(event.position), cx);
            return;
        }
        self.sync(window, cx);
    }

    /// End pointer resizing/selection, including releases outside the native client bounds.
    pub(super) fn mouse_up(
        &mut self,
        event: &MouseUpEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut native_drag = self.end_plugin_field_drag() || self.resizing;
        self.resizing = false;
        if let Some(editor) = self
            .file_picker
            .as_mut()
            .and_then(|session| session.focused_editor_mut())
        {
            editor.stop_drag();
        }
        if let Some(project) = self.projects.active_project_mut() {
            native_drag |= project.editor.stop_drag();
        }
        if !native_drag {
            self.plugin_canvas_up(event, window, cx);
        }
    }

    /// Route `event` to the panel under the pointer and retain partial steps on each axis.
    /// Shift pans editor text horizontally. `window` and `cx` drive viewport animation.
    pub(super) fn scroll(
        &mut self,
        event: &ScrollEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.small_modal_open() {
            cx.stop_propagation();
            return;
        }
        if self.plugin_canvas_scroll(event) {
            cx.stop_propagation();
            return;
        }
        self.cursor
            .update(cx, |cursor, cx| cursor.activity(window, cx));
        let (x, y) = self.metrics.cell(event.position);
        let delta = event.delta.pixel_delta(px(self.metrics.cw));
        let horizontal = event.modifiers.shift || (delta.x != px(0.) && delta.y == px(0.));
        let horizontal_area = if self.file_picker.is_some() {
            EditorWindow::body(self.file_picker_layout().preview_editor_area())
        } else {
            self.layout.editor
        };
        if !self.help.is_open()
            && horizontal
            && horizontal_area.contains(x, y)
            && let Some(editor) = if let Some(session) = &mut self.file_picker {
                session.focused_editor_mut()
            } else {
                self.projects
                    .active_project_mut()
                    .map(|project| &mut project.editor)
            }
        {
            if editor.composition().is_empty() {
                // GPUI on Windows has already converted Shift+wheel to an X delta.
                let delta = if delta.x != px(0.) { delta.x } else { delta.y };
                let columns = editor.wheel_columns(f32::from(delta) / self.metrics.cw);
                if columns != 0 {
                    self.scroll_frame(
                        [
                            ScrollInput::Advance,
                            ScrollInput::Advance,
                            ScrollInput::Wheel(columns.saturating_neg()),
                        ],
                        window,
                        cx,
                    );
                }
            }
            cx.stop_propagation();
            return;
        }
        self.wheel += f32::from(event.delta.pixel_delta(px(self.metrics.lh)).y) / self.metrics.lh;
        let rows = self.wheel.trunc() as isize;
        self.wheel -= rows as f32;
        if rows == 0 {
            return;
        }
        let wheel = ScrollInput::Wheel(rows.saturating_neg());
        if self.help.is_open() {
            self.repaint_help(wheel, window, cx);
            cx.stop_propagation();
            return;
        }
        if self.file_picker.is_some() {
            self.scroll_file_picker(x, y, rows.saturating_neg(), window, cx);
            cx.stop_propagation();
            return;
        }
        if self.projects.start().is_some() {
            self.scroll_start_page(event.position, rows.saturating_neg(), window, cx);
            cx.stop_propagation();
            return;
        }
        let inputs = if self.layout.tree.contains(x, y) {
            [wheel, ScrollInput::Advance, ScrollInput::Advance]
        } else if self.layout.editor.contains(x, y)
            && self.projects.active().editor.composition().is_empty()
        {
            [ScrollInput::Advance, wheel, ScrollInput::Advance]
        } else {
            return;
        };
        self.scroll_frame(inputs, window, cx);
    }
}
