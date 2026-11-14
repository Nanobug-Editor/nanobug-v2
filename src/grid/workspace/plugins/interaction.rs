//! Dispatch asynchronous callbacks from the visible, retained Canvas scene.

use super::Workspace;
use crate::grid::render::Rect;
use gpui::{
    Context, KeyDownEvent, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    NavigationDirection, Pixels, Point, ScrollEvent, Window, px,
};
use nanobug::plugins::session::{Action, Key};
use nanobug_plugin_sdk::canvas::{EventKind, Interaction, KeyModifiers, LineUnderlines};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

const MOTION_INTERVAL: Duration = Duration::from_millis(16);

/// Focus follows a live module. Frame and motion delivery are bounded separately.
#[derive(Default)]
pub(super) struct State {
    focus: Option<(Key, u64)>,
    last_motion: Option<Instant>,
    last_frame: Option<u64>,
    frame_queued: bool,
}

#[derive(Eq, PartialEq)]
struct Callback {
    key: Key,
    revision: u64,
    viewport: u64,
    action: String,
}

/// Bind one paint's native underline data to the frame that requested it.
pub(super) struct UnderlineReport {
    owner: gpui::WeakEntity<Workspace>,
    callback: Callback,
}

impl UnderlineReport {
    /// Defer one guest callback until after the native draw and recheck the current scene.
    pub(super) fn schedule(self, lines: Vec<LineUnderlines>, window: &Window, cx: &mut gpui::App) {
        let Self { owner, callback } = self;
        window.defer(cx, move |_, cx| {
            let _ = owner.update(cx, |workspace, _| {
                let still_visible = workspace.plugins.interaction.last_frame
                    != Some(callback.revision)
                    && workspace
                        .canvas_callback(|scene| scene.next_frame.as_deref())
                        .as_ref()
                        == Some(&callback);
                if still_visible {
                    workspace.plugins.interaction.last_frame = Some(callback.revision);
                    workspace.send_canvas_callback(
                        callback,
                        EventKind::NextFrame,
                        json!({"event": EventKind::NextFrame, "underlines": lines}),
                    );
                }
            });
        });
    }
}

impl Workspace {
    /// The host input and pointer hit testing use exactly the same visible grid cells.
    pub(super) fn plugin_surface_input_area(&self, area: Rect) -> Option<Rect> {
        if self.current_native_field().is_some() {
            return None;
        }
        if !self.plugins.surface.focused {
            let frame = &self.displayed_plugin_surface()?.frame;
            if frame.input.is_none()
                && !frame
                    .canvas
                    .as_ref()
                    .is_some_and(|canvas| canvas.input.is_some())
            {
                return None;
            }
        }
        let (column, row, width) = self.input_cell(area);
        Some(Rect {
            x: area.x + column,
            y: area.y + row,
            w: width,
            h: 1,
        })
    }

    /// A semantic field uses its exact clipped native bounds, while the old field uses grid cells.
    pub(super) fn plugin_input_bounds(&self, area: Rect) -> Option<gpui::Bounds<Pixels>> {
        self.current_native_field()
            .map(|field| field.clip)
            .or_else(|| {
                self.plugin_surface_input_area(area)
                    .map(|field| field.pixels(self.metrics))
            })
    }

    pub(in crate::grid::workspace) fn clear_canvas_focus(&mut self) {
        self.plugins.interaction.focus = None;
    }

    /// Register extra native button listeners only while a scene wants them.
    pub(in crate::grid::workspace) fn has_canvas_button_callbacks(&self) -> bool {
        self.plugin_surface_area().is_some()
            && self.canvas_surface().is_some_and(|surface| {
                surface.frame.canvas.as_ref().is_some_and(|canvas| {
                    canvas
                        .interaction
                        .pointer
                        .iter()
                        .any(|region| region.mouse_down.is_some() || region.mouse_up.is_some())
                })
            })
    }

    /// Forward `event` to its Canvas callback, with primary clicks also updating focus.
    pub(in crate::grid::workspace) fn plugin_canvas_down(
        &mut self,
        event: &MouseDownEvent,
    ) -> bool {
        let callback = self.canvas_pointer_callback(EventKind::MouseDown, event.position);
        if event.button == MouseButton::Left {
            let focus = self.plugins.surface.key.as_ref().and_then(|key| {
                let surface = self.canvas_surface()?;
                let interaction = &surface.frame.canvas.as_ref()?.interaction;
                (!interaction.keys.is_empty()).then(|| (key.clone(), surface.generation))
            });
            if callback.is_some() || focus.is_some() {
                self.plugins.surface.focused = false;
                self.plugins.composition.clear();
            }
            self.plugins.interaction.focus = focus;
        }
        let Some((callback, [x, y])) = callback else {
            return false;
        };
        self.send_canvas_callback(
            callback,
            EventKind::MouseDown,
            json!({
                "event": EventKind::MouseDown, "x": x, "y": y,
                "button": button_name(event.button), "click_count": event.click_count,
                "modifiers": modifiers(event.modifiers),
            }),
        );
        true
    }

    pub(in crate::grid::workspace) fn plugin_canvas_aux_down(
        &mut self,
        event: &MouseDownEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.plugin_canvas_down(event) {
            cx.stop_propagation();
        }
    }

    pub(in crate::grid::workspace) fn plugin_canvas_up(
        &mut self,
        event: &MouseUpEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((callback, [x, y])) =
            self.canvas_pointer_callback(EventKind::MouseUp, event.position)
        else {
            return;
        };
        self.send_canvas_callback(
            callback,
            EventKind::MouseUp,
            json!({
                "event": EventKind::MouseUp, "x": x, "y": y,
                "button": button_name(event.button), "click_count": event.click_count,
                "modifiers": modifiers(event.modifiers),
            }),
        );
        cx.stop_propagation();
    }

    pub(in crate::grid::workspace) fn plugin_canvas_move(&mut self, event: &MouseMoveEvent) {
        let now = Instant::now();
        if self
            .plugins
            .interaction
            .last_motion
            .is_some_and(|previous| now.duration_since(previous) < MOTION_INTERVAL)
        {
            return;
        }
        let Some((callback, [x, y])) =
            self.canvas_pointer_callback(EventKind::MouseMove, event.position)
        else {
            return;
        };
        self.plugins.interaction.last_motion = Some(now);
        self.send_canvas_callback(
            callback,
            EventKind::MouseMove,
            json!({
                "event": EventKind::MouseMove, "x": x, "y": y,
                "button": event.pressed_button.map(button_name),
                "modifiers": modifiers(event.modifiers),
            }),
        );
    }

    /// A registered wheel target owns the gesture instead of the editor behind it.
    pub(in crate::grid::workspace) fn plugin_canvas_scroll(&mut self, event: &ScrollEvent) -> bool {
        let Some((callback, [x, y])) =
            self.canvas_pointer_callback(EventKind::Wheel, event.position)
        else {
            return false;
        };
        let delta = event.delta.pixel_delta(px(self.metrics.lh));
        self.send_canvas_callback(
            callback,
            EventKind::Wheel,
            json!({
                "event": EventKind::Wheel, "x": x, "y": y,
                "delta_x": f32::from(delta.x), "delta_y": f32::from(delta.y),
                "precise": event.delta.precise(), "modifiers": modifiers(event.modifiers),
            }),
        );
        true
    }

    /// Called after the workspace has given native text and global commands priority.
    pub(in crate::grid::workspace) fn plugin_canvas_key(&mut self, event: &KeyDownEvent) -> bool {
        let Some((owner, generation)) = self.plugins.interaction.focus.as_ref() else {
            return false;
        };
        if self.plugins.surface.key.as_ref() != Some(owner)
            || !self
                .canvas_surface()
                .is_some_and(|surface| surface.generation == *generation)
        {
            self.clear_canvas_focus();
            return false;
        }
        let key = &event.keystroke.key;
        let modifiers = modifiers(event.keystroke.modifiers);
        let Some(callback) =
            self.canvas_callback(|interaction| interaction.key_action(key, modifiers))
        else {
            return false;
        };
        self.send_canvas_callback(
            callback,
            EventKind::KeyDown,
            json!({
                "event": EventKind::KeyDown, "key": key, "held": event.is_held,
                "modifiers": modifiers,
            }),
        );
        true
    }

    /// At most one native frame callback is queued. A hidden or stale scene cannot run it.
    pub(super) fn schedule_canvas_frame(&mut self, window: &Window, cx: &mut Context<Self>) {
        // A geometry report must wait for the real paint callback to fill its segments.
        if self.canvas_reports_underlines() {
            return;
        }
        let Some(callback) = self.canvas_callback(|scene| scene.next_frame.as_deref()) else {
            return;
        };
        let state = &mut self.plugins.interaction;
        if state.frame_queued || state.last_frame == Some(callback.revision) {
            return;
        }
        state.frame_queued = true;
        let weak = cx.entity().downgrade();
        window.on_next_frame(move |window, cx| {
            let _ = weak.update(cx, |workspace, cx| {
                workspace.plugins.interaction.frame_queued = false;
                let Some(current) = workspace.canvas_callback(|scene| scene.next_frame.as_deref())
                else {
                    return;
                };
                if current != callback {
                    workspace.schedule_canvas_frame(window, cx);
                    return;
                }
                workspace.plugins.interaction.last_frame = Some(callback.revision);
                workspace.send_canvas_callback(
                    callback,
                    EventKind::NextFrame,
                    json!({"event": EventKind::NextFrame}),
                );
            });
        });
    }

    /// Offer one visible Canvas a geometry report. Regular frame callbacks keep their fast path.
    pub(super) fn canvas_underline_report(&self, cx: &Context<Self>) -> Option<UnderlineReport> {
        if !self.canvas_reports_underlines() {
            return None;
        }
        let callback = self.canvas_callback(|scene| scene.next_frame.as_deref())?;
        if self.plugins.interaction.last_frame == Some(callback.revision) {
            return None;
        }
        Some(UnderlineReport {
            owner: cx.entity().downgrade(),
            callback,
        })
    }

    fn canvas_reports_underlines(&self) -> bool {
        self.canvas_surface()
            .and_then(|surface| surface.drawing.as_ref())
            .is_some_and(|drawing| drawing.reports_underlines)
    }

    fn canvas_pointer_callback(
        &self,
        kind: EventKind,
        position: Point<Pixels>,
    ) -> Option<(Callback, [f32; 2])> {
        let area = self.plugin_surface_area()?;
        let bounds = area.pixels(self.metrics);
        if !bounds.contains(&position)
            || self
                .plugin_input_bounds(area)
                .is_some_and(|input| input.contains(&position))
        {
            return None;
        }
        let position = position - bounds.origin;
        let position = [f32::from(position.x), f32::from(position.y)];
        let callback = self.canvas_callback(|scene| scene.pointer_action(kind, position))?;
        Some((callback, position))
    }

    fn canvas_callback(
        &self,
        resolve: impl FnOnce(&Interaction) -> Option<&str>,
    ) -> Option<Callback> {
        self.plugin_surface_area()?;
        let surface = self.canvas_surface()?;
        let interaction = &surface.frame.canvas.as_ref()?.interaction;
        Some(Callback {
            action: resolve(interaction)?.to_owned(),
            key: self.plugins.surface.key.clone()?,
            revision: surface.revision,
            viewport: self.plugins.surface.viewport?.id,
        })
    }

    fn send_canvas_callback(&mut self, callback: Callback, kind: EventKind, argument: Value) {
        let request = Action::CanvasAction {
            key: callback.key,
            revision: callback.revision,
            viewport: callback.viewport,
            action: callback.action,
            kind,
            argument,
        };
        if kind.is_passive() {
            if let Some(session) = &self.plugins.session {
                let _ = session.send(request);
            }
        } else if !self.plugins.send(request)
            && let Some(message) = self.plugins.manager.feedback.clone()
        {
            self.set_status(message);
        }
    }
}

fn modifiers(value: Modifiers) -> KeyModifiers {
    KeyModifiers {
        control: value.control,
        alt: value.alt,
        shift: value.shift,
        platform: value.platform,
        function: value.function,
    }
}

fn button_name(button: MouseButton) -> &'static str {
    match button {
        MouseButton::Left => "left",
        MouseButton::Right => "right",
        MouseButton::Middle => "middle",
        MouseButton::Navigate(NavigationDirection::Back) => "back",
        MouseButton::Navigate(NavigationDirection::Forward) => "forward",
    }
}
