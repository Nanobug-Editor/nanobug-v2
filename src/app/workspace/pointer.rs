//! Pointer hit testing, selection, pane resizing and wheel input.

use super::{
    Editor,
    layout::{Focus, ShellWindow, near_border},
};
use gpui::{
    ClickEvent, Context, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollEvent,
    Window, px,
};
use nanobug::{animation::scroll::ScrollInput, editor::Mode};

impl Editor {
    /// Hit-test the actually displayed fractional viewport at native `position`.
    fn editor_offset(&self, position: gpui::Point<gpui::Pixels>) -> usize {
        let buffer = &self.buffers[self.active];
        let body = self.editor_text();
        let row = (f32::from(position.y - self.metrics.origin.y) / self.metrics.lh - body.y as f32
            + buffer.scroll.position())
        .floor()
        .max(0.) as usize;
        let row = row.min(buffer.vim.doc.starts.len() - 1);
        let x = f32::from(position.x - self.metrics.at(body.x as f32, 0.).x) / self.metrics.cw;
        let (line, preedit) = buffer.displayed_line(row, &self.composition);
        let visual_x =
            f64::from(x.max(0.) + buffer.columns.fraction()) + buffer.columns.first_cell() as f64;
        let mut byte = self.spacing.byte_at(&line, visual_x);
        if let Some(mark) = preedit {
            byte -= byte.saturating_sub(mark.start).min(mark.len());
        }
        buffer.vim.doc.starts[row] + byte
    }

    /// Find the visible window whose border is near the window-local pointer `position`.
    pub(super) fn border_at(&self, position: Point<Pixels>) -> Option<ShellWindow> {
        let near = |target| near_border(self.window_area(target), self.metrics, position);
        if self.help_top.is_some() {
            return near(ShellWindow::ShortcutHelp).then_some(ShellWindow::ShortcutHelp);
        }
        [Focus::Manager, Focus::Editor]
            .map(ShellWindow::Pane)
            .into_iter()
            .find(|&target| near(target))
    }

    /// Update the close glyph and its hit target through `cx` when the hovered `target` changes.
    pub(super) fn set_border_hover(&mut self, target: Option<ShellWindow>, cx: &mut Context<Self>) {
        if self.hovered_border != target {
            self.hovered_border = target;
            self.refresh_panels(cx);
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
        if self.hovered_border != Some(target)
            || matches!(event, ClickEvent::Mouse(click) if click.down.click_count > 1)
        {
            return;
        }
        self.prefix = None;
        self.paste_timer = None;
        self.tree_count = 0;
        self.tree_click = None;
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
        self.paste_timer = None;
        self.tree_count = 0;
        if self.help_top.is_some() {
            cx.stop_propagation();
            return;
        }
        self.cursor
            .update(cx, |cursor, cx| cursor.activity(window, cx));
        window.focus(&self.focus_handle, cx);
        let first_click = self.tree_click.take();
        let (x, y) = self.metrics.cell(event.position);
        if self.hidden_pane.is_none()
            && y >= self.layout.tree.y
            && y < self.layout.tree.y + self.layout.tree.h
            && (x == self.layout.tree.w || x == self.layout.tree.w.saturating_sub(1))
        {
            self.resizing = true;
        } else if self.layout.tree.contains(x, y) {
            self.focus = Focus::Manager;
            if self.layout.tree_body().contains(x, y) {
                let body = self.layout.tree_body();
                let index = (f32::from(event.position.y - self.metrics.origin.y) / self.metrics.lh
                    - body.y as f32
                    + self.tree_scroll.position())
                .floor()
                .max(0.) as usize;
                if let Some(entry) = self.tree.entries.get(index) {
                    let activate =
                        event.click_count == 2 && first_click.as_ref() == Some(&entry.path);
                    if event.click_count == 1 {
                        self.tree_click = Some(entry.path.clone());
                    }
                    self.tree.selected = index;
                    // Keep the clicked row under the pointer as a fractional scroll settles.
                    let top = index - (y - body.y);
                    self.tree_scroll
                        .snap(top, self.scroll_epoch.elapsed().as_secs_f64());
                    if activate && let Some(path) = self.tree.activate() {
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
            }
        } else if self.layout.editor.contains(x, y) {
            self.focus = Focus::Editor;
            if y == self.layout.editor.y || y == self.layout.editor.y + self.layout.editor.h - 1 {
                self.sync(window, cx);
                return;
            }
            let offset = self.editor_offset(event.position);
            self.buffers[self.active].vim.set_cursor(offset);
            self.dragging = Some((self.vim().doc.cursor, event.position));
        }
        self.sync(window, cx);
    }

    /// Update border hover from `event` or continue resizing or selection in `window` through `cx`.
    pub(super) fn mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !event.dragging() {
            self.resizing = false;
            self.dragging = None;
        }
        if self.resizing {
            let (x, _) = self.metrics.cell(event.position);
            self.sidebar = x;
        } else if let Some((anchor, origin)) = self.dragging {
            if f32::from(event.position.x - origin.x)
                .abs()
                .max(f32::from(event.position.y - origin.y).abs())
                < 4.
            {
                return;
            }
            let at = self.editor_offset(event.position);
            let vim = &mut self.buffers[self.active].vim;
            if at != anchor {
                vim.mode = Mode::Visual;
                vim.anchor = anchor;
                vim.set_cursor(at);
            }
        } else {
            self.set_border_hover(self.border_at(event.position), cx);
            return;
        }
        self.sync(window, cx);
    }

    /// End pointer resizing/selection, including releases outside the native client bounds.
    pub(super) fn mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.resizing = false;
        self.dragging = None;
    }

    /// Route `event` to the panel under the pointer and retain partial steps on each axis.
    /// Shift pans editor text horizontally. `window` and `cx` drive viewport animation.
    pub(super) fn scroll(
        &mut self,
        event: &ScrollEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cursor
            .update(cx, |cursor, cx| cursor.activity(window, cx));
        let (x, y) = self.metrics.cell(event.position);
        let delta = event.delta.pixel_delta(px(self.metrics.cw));
        let horizontal = event.modifiers.shift || (delta.x != px(0.) && delta.y == px(0.));
        if self.help_top.is_none() && self.layout.editor.contains(x, y) && horizontal {
            if self.composition.is_empty() {
                // GPUI on Windows has already converted Shift+wheel to an X delta.
                let delta = if delta.x != px(0.) { delta.x } else { delta.y };
                self.horizontal_wheel += f32::from(delta) / self.metrics.cw;
                let columns = self.horizontal_wheel.trunc() as isize;
                self.horizontal_wheel -= columns as f32;
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
        if self.help_top.is_some() {
            self.scroll_help(rows.saturating_neg(), cx);
            cx.stop_propagation();
            return;
        }
        let wheel = ScrollInput::Wheel(rows.saturating_neg());
        let inputs = if self.layout.tree.contains(x, y) {
            [wheel, ScrollInput::Advance, ScrollInput::Advance]
        } else if self.layout.editor.contains(x, y) && self.composition.is_empty() {
            [ScrollInput::Advance, wheel, ScrollInput::Advance]
        } else {
            return;
        };
        self.scroll_frame(inputs, window, cx);
    }
}
