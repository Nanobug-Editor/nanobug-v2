//! Keep a plugin field's committed selection and visible graphemes in the host.

use super::Workspace;
use crate::{
    grid::render::Rect,
    ui::{text::UNITS_PER_COLUMN, theme::SELECTION},
};
use gpui::{Context, MouseMoveEvent, Pixels, Point, Window, px};

pub(super) use crate::grid::text_field::*;

impl Workspace {
    /// Place the host caret from the exact grid positions used to draw this field.
    pub(super) fn press_plugin_field(
        &mut self,
        input: Option<Rect>,
        point: Point<Pixels>,
        shift: bool,
    ) {
        if self.plugins.composition.is_empty() {
            if let Some(native) = self.current_native_field() {
                self.plugins.surface.field.place(
                    &self.plugins.surface.draft,
                    native.byte_at(point),
                    shift,
                );
            } else if let Some(input) = input {
                self.position_plugin_field(input, point, shift);
            }
            self.plugins.surface.field.dragging = true;
        }
    }

    fn position_plugin_field(&mut self, input: Rect, point: Point<Pixels>, extend: bool) {
        let origin = input.pixels(self.metrics).origin;
        let x = f32::from(point.x - origin.x) / self.metrics.cw;
        let byte = self
            .plugins
            .surface
            .field
            .layout(
                &self.plugins.surface.draft,
                &self.plugins.composition,
                input.w,
                &self.spacing,
            )
            .byte_at(x, &self.spacing);
        self.plugins
            .surface
            .field
            .place(&self.plugins.surface.draft, byte, extend);
    }

    pub(in crate::grid::workspace) fn end_plugin_field_drag(&mut self) -> bool {
        std::mem::take(&mut self.plugins.surface.field.dragging)
    }

    pub(in crate::grid::workspace) fn drag_plugin_field(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.plugins.surface.field.dragging {
            return false;
        }
        if !event.dragging()
            || !self.plugins.surface.focused
            || !self.plugins.composition.is_empty()
        {
            self.end_plugin_field_drag();
            return false;
        }
        if let Some(native) = self.current_native_field() {
            self.plugins.surface.field.place(
                &self.plugins.surface.draft,
                native.byte_at(event.position),
                true,
            );
            self.plugin_refresh(window, cx);
            return true;
        }
        let Some(input) = self
            .plugin_surface_area()
            .and_then(|area| self.plugin_surface_input_area(area))
        else {
            self.end_plugin_field_drag();
            return false;
        };
        self.position_plugin_field(input, event.position, true);
        self.plugin_refresh(window, cx);
        true
    }

    /// Paint the selection over the Canvas but behind the host-drawn input text.
    pub(super) fn plugin_selection(
        &self,
        area: Rect,
        input: Rect,
        cx: &gpui::App,
    ) -> Option<gpui::Div> {
        use gpui::{Styled, div, rgba};
        if !self.plugins.surface.focused {
            return None;
        }
        let selected = self
            .plugins
            .surface
            .field
            .layout(
                &self.plugins.surface.draft,
                &self.plugins.composition,
                input.w,
                &self.spacing,
            )
            .selection;
        if selected.is_empty() {
            return None;
        }
        let unit = self.metrics.cw / UNITS_PER_COLUMN as f32;
        Some(
            div()
                .absolute()
                .left(px(
                    (input.x - area.x) as f32 * self.metrics.cw + selected.start as f32 * unit
                ))
                .top(px((input.y - area.y) as f32 * self.metrics.lh))
                .w(px(selected.len() as f32 * unit))
                .h(px(self.metrics.lh))
                .bg(rgba(crate::ui::theme::style(cx).tint(SELECTION))),
        )
    }
}
