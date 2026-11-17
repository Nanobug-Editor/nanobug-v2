//! Pointer hit testing, selection, pane resizing and wheel input.

#[cfg(all(test, windows))]
mod tests;

use super::{
    Workspace,
    editor_splits::{DividerHit, PaneId},
    layout::{
        Focus, ShellWindow, near_corner_controls, pane_border_offsets, stacked_divider_bounds,
    },
};
use crate::grid::render::Rect;
use crate::grid::windows::editor::{EditorWindow, HeaderTarget, MarkdownPreview};
use crate::grid::windows::help::HelpCommand;
use gpui::{
    Bounds, ClickEvent, Context, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    Point, ScrollEvent, Window, px,
};
use nanobug::animation::scroll::ScrollInput;

/// Divider that owns a pointer resize until the left button is released.
#[derive(Clone, Copy)]
pub(super) enum PaneResize {
    /// Explorer divider and the correction from the mouse-down row to its seam.
    Explorer(usize),
    /// Split divider and the correction from the mouse-down row to its seam.
    Editor(DividerHit, usize),
}

/// Tracks Markdown links in the final painted preview and the current left-button gesture.
#[derive(Default)]
pub(super) struct MarkdownPointer {
    /// Geometry for the frame currently shown to the user.
    painted: Vec<PaintedMarkdownLinks>,
    /// Source identity, destination and position at the start of a click.
    pressed: Option<(u64, String, Point<Pixels>)>,
    /// Whether the most recent render assigned the pointing-hand cursor.
    hovering: bool,
}

/// A preview and the overlay bounds that can cover its links.
struct PaintedMarkdownLinks {
    /// Pane whose buffer produced the preview.
    pane: PaneId,
    /// Opening identity of the source buffer.
    document: u64,
    /// Layout used to paint this frame, including the fractional scroll position.
    preview: MarkdownPreview,
    /// Bounds of contextual overlays above the preview.
    blockers: [Option<Bounds<Pixels>>; 3],
}

impl Workspace {
    /// Publish the visible preview and its overlays. Return whether to paint a hand cursor.
    pub(super) fn set_painted_markdown_preview(
        &mut self,
        previews: Vec<(PaneId, MarkdownPreview)>,
        blockers: [Option<Bounds<Pixels>>; 3],
        window: &Window,
    ) -> bool {
        self.markdown_pointer.painted = previews
            .into_iter()
            .filter_map(|(pane, preview)| {
                let editor = self.projects.active_project()?.editor_by_id(pane)?;
                Some(PaintedMarkdownLinks {
                    pane,
                    document: editor.vim().doc.opening,
                    preview,
                    blockers,
                })
            })
            .collect();
        let hovering =
            window.is_window_hovered() && self.markdown_link_at(window.mouse_position()).is_some();
        self.markdown_pointer.hovering = hovering;
        hovering
    }

    /// Request a frame when the pointer enters or leaves a visible link.
    pub(super) fn update_markdown_pointer(
        &self,
        position: Option<Point<Pixels>>,
        cx: &mut Context<Self>,
    ) {
        let hovering = position.is_some_and(|position| self.markdown_link_at(position).is_some());
        if hovering != self.markdown_pointer.hovering {
            cx.notify();
        }
    }

    /// Resolve a painted link at `position` when its preview still owns that area.
    pub(super) fn markdown_link_at(&self, position: Point<Pixels>) -> Option<(u64, &str)> {
        if self.modal_open() {
            return None;
        }
        let (x, y) = self.metrics.cell(position);
        let (pane, area) = self.editor_at(x, y)?;
        let painted = self
            .markdown_pointer
            .painted
            .iter()
            .find(|painted| painted.pane == pane)?;
        let editor = self.projects.active_project()?.editor_by_id(pane)?;
        if editor.vim().doc.opening != painted.document
            || !editor
                .markdown_preview_area(area)?
                .pixels(self.metrics)
                .contains(&position)
            || painted
                .blockers
                .iter()
                .flatten()
                .any(|bounds| bounds.contains(&position))
        {
            return None;
        }
        painted
            .preview
            .link_at(position)
            .map(|link| (painted.document, link))
    }

    /// Detect hover over the shortcut popup's close control at the window-local `position`.
    pub(super) fn corner_controls_at(&self, position: Point<Pixels>) -> Option<ShellWindow> {
        (!self.small_modal_open()
            && self.help.is_open()
            && !self.help.is_editor_help()
            && near_corner_controls(self.help_layout(), false, self.metrics, position))
        .then_some(ShellWindow::ShortcutHelp)
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
        if self.vim_menu.is_some() {
            self.click_vim_menu(event.position, window, cx);
            cx.stop_propagation();
            return;
        }
        if let Some(area) = self.command_window_area() {
            self.dismiss_sequence_hint(cx);
            let (x, y) = self.metrics.cell(event.position);
            if EditorWindow::body(area).contains(x, y)
                && let Some(history) = &mut self.command_window
            {
                history
                    .editor
                    .press(event.position, area, self.metrics, &self.spacing);
                self.sync(window, cx);
            } else {
                self.sync_text_input(window, cx);
            }
            cx.stop_propagation();
            return;
        }
        self.markdown_pointer.pressed = None;
        self.pane_resize = None;
        self.dismiss_definition_hover(cx);
        if let Some((menu, ..)) = self.completion_menu(window)
            && let Some(index) = menu.hit(event.position, self.metrics)
        {
            self.accept_completion_at(index, window, cx);
            cx.stop_propagation();
            return;
        }
        self.dismiss_completion(cx);
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
        if let Some((pane, target)) = self.editor_header_target(x, y) {
            match target {
                HeaderTarget::Buffer(id) | HeaderTarget::Overflow(id) => {
                    if !self.focus_editor_pane(pane, window, cx) {
                        self.finish_editor_input(window, cx);
                    }
                    let now = self.scroll_epoch.elapsed().as_secs_f64();
                    let project = self.projects.active_mut();
                    project.editor.select_buffer(id, now);
                    project.focus_pane(Focus::Editor);
                    project.status.clear();
                    self.last_cursor_space = None;
                    self.sync(window, cx);
                }
            }
            cx.stop_propagation();
            return;
        }
        if let Some((divider, row_correction)) = self.editor_divider_at(event.position) {
            self.pane_resize = Some(PaneResize::Editor(divider, row_correction));
            self.set_controls_hover(None, cx);
            cx.notify();
            cx.stop_propagation();
            return;
        }
        let editor_hit = self.editor_at(x, y);
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
        let seam = Rect {
            y: first.y + first.h,
            h: 1,
            ..first
        };
        if project.manager.is_some()
            && project.hidden_pane.is_none()
            && if project.stacked {
                stacked_divider_bounds(seam, self.metrics).contains(&event.position)
            } else {
                y >= first.y
                    && y < first.y + first.h
                    && (x == first.x + first.w || x == first.x + first.w.saturating_sub(1))
            }
        {
            self.pane_resize = Some(PaneResize::Explorer(seam.y.saturating_sub(y)));
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
                self.update_layout(window);
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
        } else if let Some((pane, area)) = editor_hit {
            self.focus_editor_pane(pane, window, cx);
            if y == area.y || y == area.y + area.h - 1 {
                self.sync(window, cx);
                return;
            }
            let body = EditorWindow::body(area);
            if body.contains(x, y)
                && let Some(terminal) = self.projects.active_mut().editor.terminal_mut()
            {
                terminal.click(x - body.x, y - body.y);
                self.sync(window, cx);
                return;
            }
            if self
                .projects
                .active()
                .editor
                .markdown_preview_area(area)
                .is_some_and(|area| area.contains(x, y))
            {
                if event.click_count == 1
                    && let Some((document, destination)) = self.markdown_link_at(event.position)
                {
                    self.markdown_pointer.pressed =
                        Some((document, destination.to_owned(), event.position));
                }
            } else {
                self.projects.active_mut().editor.press(
                    event.position,
                    area,
                    self.metrics,
                    &self.spacing,
                );
            }
        }
        self.sync(window, cx);
    }

    /// Identify a visible editor title entry at the same grid cells used to draw it.
    fn editor_header_target(&self, x: usize, y: usize) -> Option<(PaneId, HeaderTarget)> {
        let (pane, area) = self.editor_at(x, y)?;
        if y != area.y {
            return None;
        }
        self.projects
            .active_project()?
            .editor_by_id(pane)?
            .header(area, &self.spacing, self.layout.visible_cols)
            .target_at(x, y)
            .map(|target| (pane, target))
    }

    /// Close the title under a middle-click, preserving Canvas input outside the title.
    pub(super) fn middle_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.vim_menu.is_some() {
            cx.stop_propagation();
            return;
        }
        self.dismiss_definition_hover(cx);
        self.dismiss_completion(cx);
        if !self.modal_open()
            && self.projects.active_project().is_some_and(|project| {
                project.focus == Focus::Editor && project.editor.vim().awaits_bracket_paste()
            })
            && EditorWindow::body(self.active_editor_area())
                .pixels(self.metrics)
                .contains(&event.position)
        {
            self.run_vim(|vim, register| vim.key("MiddleMouse", register), window, cx);
            self.sync(window, cx);
            cx.stop_propagation();
            return;
        }
        if event.click_count == 1 && self.middle_close_title(event.position, window, cx) {
            cx.stop_propagation();
            return;
        }
        if self.plugin_canvas_down(event) {
            cx.stop_propagation();
        }
    }

    /// Let the topmost modal own title presses.
    fn middle_close_title(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.plugin_popup_area().is_some() {
            return self.middle_close_plugin_title(position, window, cx);
        }
        if self.help.is_open() {
            let Some(scope) = self.help.scope_at(
                self.help_layout(),
                self.metrics,
                &self.spacing,
                self.hovered_controls == Some(ShellWindow::ShortcutHelp),
                position,
            ) else {
                return false;
            };
            match self
                .help
                .close_scope(scope, self.scroll_epoch.elapsed().as_secs_f64())
            {
                HelpCommand::Close => {
                    self.close_grid_window(ShellWindow::ShortcutHelp, window, cx);
                    self.sync(window, cx);
                }
                HelpCommand::Update(input) => self.repaint_help(input, window, cx),
                _ => {}
            }
            return true;
        }
        let popup_title = |area| {
            self.popup_panel
                .read(cx)
                .title_at(area, position, self.metrics)
        };
        if let Some(area) = self.editor_close_area() {
            let hit = popup_title(area);
            if hit {
                self.cancel_editor_close(window, cx);
            }
            return hit;
        }
        if let Some(area) = self.remove_recent_project_area() {
            let hit = popup_title(area);
            if hit {
                self.finish_remove_recent_project(false, window, cx);
            }
            return hit;
        }
        if let Some(area) = self.language_picker_area() {
            let hit = popup_title(area);
            if hit {
                self.finish_language_picker(None, window, cx);
            }
            return hit;
        }
        if let Some(area) = self.file_picker_prompt_area() {
            let hit = popup_title(area);
            if hit {
                self.finish_picker_prompt(true, window, cx);
            }
            return hit;
        }
        if let Some(session) = &self.file_picker {
            let hit = session.panel.read(cx).title_at(
                self.file_picker_layout().area,
                position,
                self.metrics,
            );
            if hit {
                self.close_focused_grid(window, cx);
            }
            return hit;
        }
        if self.plugins.open {
            return self.middle_close_plugin_title(position, window, cx);
        }
        if self.plugins.file_busy {
            return false;
        }
        if let Some(area) = self.sequence_hint_area()
            && area.pixels(self.metrics).contains(&position)
        {
            let hit = popup_title(area);
            if hit {
                self.dismiss_sequence_hint(cx);
                self.sync_text_input(window, cx);
            }
            return hit;
        }
        if let Some(area) = self.plugin_surface_area()
            && area.pixels(self.metrics).contains(&position)
        {
            let hit = self
                .plugin_panel()
                .read(cx)
                .title_at(area, position, self.metrics);
            if hit {
                self.close_plugin_surface();
                self.sync(window, cx);
            }
            return hit;
        }
        if self.projects.start().is_some() {
            return self.middle_close_start_title(position, window, cx);
        }
        if self.panels[0]
            .read(cx)
            .title_at(self.layout.tree, position, self.metrics)
        {
            self.close_grid_window(ShellWindow::Pane(Focus::Manager), window, cx);
            self.sync(window, cx);
            return true;
        }
        let Some((pane, id)) = self.editor_title_at(position) else {
            return false;
        };
        self.close_editor_title(pane, id, window, cx);
        true
    }

    /// Match file title cells after the same seam offset used to paint stacked panes.
    fn editor_title_at(&self, position: Point<Pixels>) -> Option<(PaneId, u64)> {
        let project = self.projects.active_project()?;
        let panes = self.workspace_pane_areas();
        self.editor_rects().into_iter().find_map(|(pane, area)| {
            let mut title = Rect { h: 1, ..area }.pixels(self.metrics);
            title.origin.y += pane_border_offsets(area, &panes, self.metrics).0;
            if !title.contains(&position) {
                return None;
            }
            let (x, _) = self.metrics.cell(position);
            let target = project
                .editor_by_id(pane)?
                .header(area, &self.spacing, self.layout.visible_cols)
                .target_at(x, area.y)?;
            match target {
                HeaderTarget::Buffer(id) => Some((pane, id)),
                HeaderTarget::Overflow(_) => None,
            }
        })
    }

    /// Update close button hover from `event` or continue resizing or selection in `window`.
    pub(super) fn mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.vim_menu.is_some() {
            self.hover_vim_menu(event.position, cx);
            cx.stop_propagation();
            return;
        }
        if let Some(area) = self.command_window_area()
            && let Some(history) = &mut self.command_window
        {
            if event.pressed_button != Some(MouseButton::Left) {
                history.editor.stop_drag();
            } else if history
                .editor
                .drag(event.position, area, self.metrics, &self.spacing)
            {
                self.sync(window, cx);
            }
            cx.stop_propagation();
            return;
        }
        if let Some(PaneResize::Editor(divider, row_correction)) = self.pane_resize {
            if event.pressed_button == Some(MouseButton::Left) && !self.modal_open() {
                let (x, y) = self.metrics.cell(event.position);
                if self.move_editor_divider(divider, x, y.saturating_add(row_correction)) {
                    self.sync(window, cx);
                }
                cx.stop_propagation();
                return;
            }
            self.pane_resize = None;
            cx.notify();
        }
        self.update_markdown_pointer(Some(event.position), cx);
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
        let active_area = self.active_editor_area();
        let Some(project) = self.projects.active_project_mut() else {
            return;
        };
        if !event.dragging() {
            self.pane_resize = None;
            project.editor.stop_drag();
        }
        if let Some(PaneResize::Explorer(row_correction)) = self.pane_resize {
            let (x, y) = self.metrics.cell(event.position);
            if project.stacked {
                let first_rows = y.saturating_add(row_correction).saturating_sub(1);
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
            if !project
                .editor
                .drag(event.position, active_area, self.metrics, &self.spacing)
            {
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
        if let Some(history) = &mut self.command_window {
            history.editor.stop_drag();
            cx.stop_propagation();
            return;
        }
        let resize = self.pane_resize.take();
        let mut native_drag = self.end_plugin_field_drag() || resize.is_some();
        if let Some(PaneResize::Editor(divider, row_correction)) = resize {
            let (x, y) = self.metrics.cell(event.position);
            if self.move_editor_divider(divider, x, y.saturating_add(row_correction)) {
                self.sync(window, cx);
            } else {
                cx.notify();
            }
            cx.stop_propagation();
        }
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
        let pressed = self.markdown_pointer.pressed.take();
        if !native_drag
            && let Some((document, destination, position)) = pressed
            && f32::from(position.x - event.position.x)
                .abs()
                .max(f32::from(position.y - event.position.y).abs())
                < 4.
            && self.markdown_link_at(event.position) == Some((document, destination.as_str()))
        {
            self.open_markdown_link(&destination, window, cx);
            cx.stop_propagation();
        } else if !native_drag {
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
        if self.vim_menu.is_some() {
            self.wheel_vim_menu(event, window, cx);
            cx.stop_propagation();
            return;
        }
        if self.command_window.is_some() {
            self.wheel_command_window(event, window, cx);
            cx.stop_propagation();
            return;
        }
        self.dismiss_completion(cx);
        if self.small_modal_open() {
            cx.stop_propagation();
            return;
        }
        if self.plugin_canvas_scroll(event) {
            cx.stop_propagation();
            return;
        }
        if self.wheel_definition_hover(event, window, cx) {
            cx.stop_propagation();
            return;
        }
        self.cursor
            .update(cx, |cursor, cx| cursor.activity(window, cx));
        let (x, y) = self.metrics.cell(event.position);
        let delta = event.delta.pixel_delta(px(self.metrics.cw));
        let horizontal = event.modifiers.shift || (delta.x != px(0.) && delta.y == px(0.));
        let preview_delta = event.delta.pixel_delta(px(self.metrics.lh)).y;
        if self.wheel_markdown_preview(event.position, preview_delta, horizontal, window, cx) {
            cx.stop_propagation();
            return;
        }
        if self.file_picker.is_none()
            && !self.help.is_open()
            && let Some((pane, area)) = self.editor_at(x, y)
            && pane != self.projects.active().active_editor_id()
        {
            self.wheel_inactive_editor(pane, area, event, horizontal, window, cx);
            cx.stop_propagation();
            return;
        }
        let horizontal_area = if self.file_picker.is_some() {
            EditorWindow::body(self.file_picker_layout().preview_editor_area())
        } else {
            self.active_editor_area()
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
        } else if self.active_editor_area().contains(x, y)
            && self.projects.active().editor.composition().is_empty()
        {
            [ScrollInput::Advance, wheel, ScrollInput::Advance]
        } else {
            return;
        };
        self.scroll_frame(inputs, window, cx);
    }

    /// Scroll an unfocused editor's own viewport while keeping keyboard input in the active pane.
    fn wheel_inactive_editor(
        &mut self,
        pane: PaneId,
        area: crate::grid::render::Rect,
        event: &ScrollEvent,
        horizontal: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editor = self
            .projects
            .active_mut()
            .editor_by_id_mut(pane)
            .expect("visible editor");
        let now = self.scroll_epoch.elapsed().as_secs_f64();
        let inputs = if horizontal {
            let delta = event.delta.pixel_delta(px(self.metrics.cw));
            let delta = if delta.x != px(0.) { delta.x } else { delta.y };
            let columns = editor.wheel_columns(f32::from(delta) / self.metrics.cw);
            [
                ScrollInput::Advance,
                ScrollInput::Wheel(columns.saturating_neg()),
            ]
        } else {
            self.wheel +=
                f32::from(event.delta.pixel_delta(px(self.metrics.lh)).y) / self.metrics.lh;
            let rows = self.wheel.trunc() as isize;
            self.wheel -= rows as f32;
            [
                ScrollInput::Wheel(rows.saturating_neg()),
                ScrollInput::Advance,
            ]
        };
        let (changed, _, _) = editor.track_view(area, false, &self.spacing, inputs, now);
        if changed {
            self.refresh_panels(cx);
        }
        self.scroll_frame([ScrollInput::Advance; 3], window, cx);
    }
}
