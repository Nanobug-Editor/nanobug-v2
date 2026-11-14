//! Character frames and text slots for a published plugin scene.
use super::SurfaceWindow;
use crate::{
    grid::render::{Frame, Rect},
    ui::theme::{GOLD, TEXT},
};
impl SurfaceWindow {
    /// Render the published grid scene, adding host text only when no native field draws it.
    pub(in crate::grid) fn frame(
        &self,
        area: Rect,
        view: &nanobug::plugins::session::View,
        composition: &crate::ui::ime::Composition,
        metrics: crate::grid::render::Metrics,
        spacing: &crate::ui::text::TextSpacing,
        native: bool,
    ) -> Frame {
        let mut frame = Frame::new(area.w, area.h);
        let surface = self.key.as_ref().and_then(|key| view.surfaces.get(key));
        let title = surface.map_or("Plugin", |surface| surface.frame.title.as_str());
        frame.border(title, self.focused, false, spacing);
        let right = area.w.saturating_sub(2);
        if let Some(surface) = surface {
            for (index, line) in surface.frame.lines.iter().take(area.h - 3).enumerate() {
                frame.label(1 + index, 2..right, line, TEXT, spacing);
            }
        } else {
            let state = self.key.as_ref().and_then(|key| view.modules.get(key));
            frame.label(
                1,
                2..right,
                state.map_or("Module unavailable", |state| {
                    state.label().lines().next().unwrap_or("")
                }),
                GOLD,
                spacing,
            );
        }
        if !native
            && (self.focused
                || surface.is_some_and(|surface| {
                    surface.frame.input.is_some()
                        || surface
                            .frame
                            .canvas
                            .as_ref()
                            .is_some_and(|canvas| canvas.input.is_some())
                }))
        {
            let (column, row, width) = input_cell(area, metrics, surface);
            let text = self.field.layout(&self.draft, composition, width, spacing);
            frame.label(row, column..column + width, text.text(), TEXT, spacing);
        }
        frame
    }
}
/// Resolve a canvas input's grid cell bounds, falling back to the standard footer field.
pub(in crate::grid) fn input_cell(
    area: Rect,
    metrics: crate::grid::render::Metrics,
    surface: Option<&nanobug::plugins::session::Surface>,
) -> (usize, usize, usize) {
    if let Some([x, y, width, _]) = surface.and_then(|surface| surface.frame.canvas.as_ref()?.input)
    {
        let column = (x / metrics.cw).ceil().max(2.) as usize;
        let row = (y / metrics.lh).ceil().max(1.) as usize;
        if column < area.w.saturating_sub(3) && row < area.h - 1 {
            return (
                column,
                row,
                ((width / metrics.cw).floor().max(1.) as usize).min(area.w - column - 2),
            );
        }
    }
    (2, area.h - 2, area.w.saturating_sub(4))
}
