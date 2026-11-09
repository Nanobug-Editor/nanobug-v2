//! Pointer hit testing, selection, pane resizing and wheel input.

use super::{
    Workspace,
    layout::{Focus, ShellWindow, near_close_button},
};
use gpui::{
    ClickEvent, Context, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollEvent,
    Window, px,
};
use nanobug::animation::scroll::ScrollInput;

impl Workspace {
    /// Find the visible window whose close label is near the window-local pointer `position`.
    pub(super) fn close_button_at(&self, position: Point<Pixels>) -> Option<ShellWindow> {
        if self.small_modal_open() {
            return None;
        }
        let near = |target| near_close_button(self.window_area(target), self.metrics, position);
        if self.help.is_open() {
            return near(ShellWindow::ShortcutHelp).then_some(ShellWindow::ShortcutHelp);
        }
        [Focus::Manager, Focus::Editor]
            .map(ShellWindow::Pane)
            .into_iter()
            .find(|&target| near(target))
    }

    /// Update the close button through `cx` when the hovered `target` changes.
    pub(super) fn set_close_hover(&mut self, target: Option<ShellWindow>, cx: &mut Context<Self>) {
        if self.hovered_close != target {
            self.hovered_close = target;
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
        if self.hovered_close != Some(target)
            || matches!(event, ClickEvent::Mouse(click) if click.down.click_count > 1)
        {
            return;
        }
        self.close_grid_window(target, window);
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
        self.prefix = None;
        self.projects.active.manager.paste_timer = None;
        self.projects.active.manager.count = 0;
        if self.press_editor_close(event.position, window, cx)
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
        self.cursor
            .update(cx, |cursor, cx| cursor.activity(window, cx));
        window.focus(&self.focus_handle, cx);
        let first_click = self.projects.active.manager.click.take();
        let (x, y) = self.metrics.cell(event.position);
        if self.projects.active.hidden_pane.is_none()
            && y >= self.layout.tree.y
            && y < self.layout.tree.y + self.layout.tree.h
            && (x == self.layout.tree.w || x == self.layout.tree.w.saturating_sub(1))
        {
            self.resizing = true;
        } else if self.layout.tree.contains(x, y) {
            self.projects.active.focus = Focus::Manager;
            if let Some(selection) = self.projects.active.manager.press(
                event,
                self.layout.tree,
                self.metrics,
                first_click,
                self.scroll_epoch.elapsed().as_secs_f64(),
            ) {
                if let Some(path) = selection.open {
                    self.open(path);
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
            self.projects.active.focus = Focus::Editor;
            if y == self.layout.editor.y || y == self.layout.editor.y + self.layout.editor.h - 1 {
                self.sync(window, cx);
                return;
            }
            self.projects.active.editor.press(
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
        if !event.dragging() {
            self.resizing = false;
            self.projects.active.editor.dragging = None;
        }
        if self.resizing {
            let (x, _) = self.metrics.cell(event.position);
            self.projects.active.sidebar = x;
        } else if self.projects.active.editor.dragging.is_some() {
            if !self.projects.active.editor.drag(
                event.position,
                self.layout.editor,
                self.metrics,
                &self.spacing,
            ) {
                return;
            }
        } else {
            self.set_close_hover(self.close_button_at(event.position), cx);
            return;
        }
        self.sync(window, cx);
    }

    /// End pointer resizing/selection, including releases outside the native client bounds.
    pub(super) fn mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.resizing = false;
        self.projects.active.editor.dragging = None;
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
        if !self.help.is_open() && self.layout.editor.contains(x, y) && horizontal {
            if self.projects.active.editor.composition.is_empty() {
                // GPUI on Windows has already converted Shift+wheel to an X delta.
                let delta = if delta.x != px(0.) { delta.x } else { delta.y };
                self.projects.active.editor.horizontal_wheel += f32::from(delta) / self.metrics.cw;
                let columns = self.projects.active.editor.horizontal_wheel.trunc() as isize;
                self.projects.active.editor.horizontal_wheel -= columns as f32;
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
        let inputs = if self.layout.tree.contains(x, y) {
            [wheel, ScrollInput::Advance, ScrollInput::Advance]
        } else if self.layout.editor.contains(x, y)
            && self.projects.active.editor.composition.is_empty()
        {
            [ScrollInput::Advance, wheel, ScrollInput::Advance]
        } else {
            return;
        };
        self.scroll_frame(inputs, window, cx);
    }
}
