//! Connect native focus, IME and cursor effects to the surface-owned text layout.
use super::Workspace;
pub(super) use crate::grid::windows::plugin_surface::native_field::{NativeField, element};
use crate::{grid::render::Rect, ui::theme::Style};
use gpui::{AnyElement, Bounds, Pixels, Window};
use std::rc::Rc;
impl Workspace {
    /// Native IME and the animated cursor use the same logical text anchor.
    pub(in crate::grid::workspace) fn canvas_input_caret_bounds(&self) -> Option<Bounds<Pixels>> {
        if self.plugins.open || !self.plugins.surface.focused {
            return None;
        }
        Some(self.current_native_field()?.caret_bounds())
    }

    pub(super) fn current_native_field(&self) -> Option<&Rc<NativeField>> {
        self.plugins.surface.current_field(
            self.displayed_plugin_surface()?,
            self.plugin_surface_area()?,
            self.metrics,
        )
    }

    /// Resolve exact field geometry and refresh when its scene, text or scroll position changes.
    pub(in crate::grid::workspace) fn update_canvas_text_layout(&mut self, window: &Window) {
        self.plugins.surface.native_field = self.plugin_surface_area().and_then(|panel| {
            self.plugins.surface.build_layout(
                panel,
                self.metrics,
                self.displayed_plugin_surface()?,
                &self.plugins.composition,
                self.appearance,
                window,
            )
        });
    }

    pub(super) fn canvas_text_element(&self, panel: Rect, style: Style) -> Option<AnyElement> {
        let field = Rc::clone(self.current_native_field()?);
        let cursor = self.plugins.surface.focused.then(|| self.cursor.clone());
        Some(element(
            field,
            panel.pixels(self.metrics).origin,
            cursor,
            style,
        ))
    }
}
