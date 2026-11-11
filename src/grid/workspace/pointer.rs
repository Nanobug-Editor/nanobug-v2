//! Pointer hit testing, selection, pane resizing and wheel input.

use super::{
    Workspace,
    layout::{Focus, ShellWindow, near_corner_controls},
};
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
            return near(ShellWindow::ShortcutHelp).then_some(ShellWindow::ShortcutHelp);
        }
        if self.file_picker.is_some() {
            return (self.file_picker_controls_available() && near(ShellWindow::FilePicker))
                .then_some(ShellWindow::FilePicker);
        }
        if self.projects.start().is_some() {
            return None;
        }
        [Focus::Manager, Focus::Editor]
            .map(ShellWindow::Pane)
            .into_iter()
            .find(|&target| near(target))
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
        #[cfg(windows)]
        if event.position.y < self.native_caption_height() {
            // GPUI forwards caption presses here before Windows starts its native move.
            return;
        }
        self.dismiss_leader_hint(cx);
        self.prefix = None;
        if let Some(manager) = self
            .projects
            .active_project_mut()
            .and_then(|project| project.manager.as_mut())
        {
            manager.paste_timer = None;
            manager.count = 0;
        }
        if self.press_editor_close(event, window, cx)
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
        let Some(project) = self.projects.active_project_mut() else {
            self.press_start_page(event, window, cx);
            cx.stop_propagation();
            return;
        };
        let first_click = project
            .manager
            .as_mut()
            .and_then(|manager| manager.click.take());
        if project.manager.is_some()
            && project.hidden_pane.is_none()
            && y >= self.layout.tree.y
            && y < self.layout.tree.y + self.layout.tree.h
            && (x == self.layout.tree.w || x == self.layout.tree.w.saturating_sub(1))
        {
            self.resizing = true;
        } else if let Some(manager) = &mut project.manager
            && self.layout.tree.contains(x, y)
        {
            project.focus = Focus::Manager;
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
            project.focus = Focus::Editor;
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

    /// Update close button hover from `event` or continue resizing or selection in `window`.
    pub(super) fn mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.file_picker.is_some() {
            self.set_controls_hover(self.corner_controls_at(event.position), cx);
            return;
        }
        let Some(project) = self.projects.active_project_mut() else {
            return;
        };
        if !event.dragging() {
            self.resizing = false;
            project.editor.dragging = None;
        }
        if self.resizing {
            let (x, _) = self.metrics.cell(event.position);
            project.sidebar = x;
        } else if project.editor.dragging.is_some() {
            if !project.editor.drag(
                event.position,
                self.layout.editor,
                self.metrics,
                &self.spacing,
            ) {
                return;
            }
        } else {
            self.set_controls_hover(self.corner_controls_at(event.position), cx);
            return;
        }
        self.sync(window, cx);
    }

    /// End pointer resizing/selection, including releases outside the native client bounds.
    pub(super) fn mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.resizing = false;
        if let Some(project) = self.projects.active_project_mut() {
            project.editor.dragging = None;
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
        self.cursor
            .update(cx, |cursor, cx| cursor.activity(window, cx));
        let (x, y) = self.metrics.cell(event.position);
        let delta = event.delta.pixel_delta(px(self.metrics.cw));
        let horizontal = event.modifiers.shift || (delta.x != px(0.) && delta.y == px(0.));
        if !self.help.is_open()
            && self.file_picker.is_none()
            && self.layout.editor.contains(x, y)
            && horizontal
        {
            let editor = &mut self.projects.active_mut().editor;
            if editor.composition.is_empty() {
                // GPUI on Windows has already converted Shift+wheel to an X delta.
                let delta = if delta.x != px(0.) { delta.x } else { delta.y };
                editor.horizontal_wheel += f32::from(delta) / self.metrics.cw;
                let columns = editor.horizontal_wheel.trunc() as isize;
                editor.horizontal_wheel -= columns as f32;
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
            && self.projects.active().editor.composition.is_empty()
        {
            [ScrollInput::Advance, wheel, ScrollInput::Advance]
        } else {
            return;
        };
        self.scroll_frame(inputs, window, cx);
    }
}
