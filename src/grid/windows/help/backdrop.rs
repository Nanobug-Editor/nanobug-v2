//! Help-window backdrop reuse and native image lifetime management.

use super::{HelpBackdrop, HelpWindow, view::HELP_CORNER_RADIUS};
use crate::{grid::render::GridLayer, ui::backdrop::BackdropRaster};
use gpui::{App, Bounds, Pixels, PlatformTextSystem, RenderImage, Window};
use std::sync::Arc;

impl HelpWindow {
    /// Release the cached native image in `window` once the popup is closed.
    pub(in crate::grid) fn release_backdrop(&mut self, window: &mut Window) {
        if let Some(backdrop) = self.backdrop.take() {
            let _ = window.drop_image(backdrop.image);
        }
    }

    /// Reuse the blurred `layers` while the `outline` and `window` size match.
    /// `glyphs` supplies font rasters; `cx` supplies retained panels and the SVG renderer.
    pub(in crate::grid) fn backdrop_image(
        &mut self,
        layers: &[GridLayer],
        outline: Bounds<Pixels>,
        glyphs: &dyn PlatformTextSystem,
        window: &mut Window,
        cx: &App,
    ) -> Arc<RenderImage> {
        let viewport = window.viewport_size();
        if let Some(backdrop) = &self.backdrop
            && backdrop.viewport == viewport
            && backdrop.outline == outline
        {
            return backdrop.image.clone();
        }
        let mut raster = BackdropRaster::new(viewport, glyphs, cx.svg_renderer());
        for layer in layers {
            let clip = layer.clip.unwrap_or(layer.bounds);
            if !clip.is_empty() {
                layer
                    .panel
                    .read(cx)
                    .stamp_backdrop(layer.bounds, clip, &mut raster, window);
            }
        }
        let image = raster.finish(outline, HELP_CORNER_RADIUS);
        if let Some(previous) = self.backdrop.replace(HelpBackdrop {
            viewport,
            outline,
            image: image.clone(),
        }) {
            let _ = window.drop_image(previous.image);
        }
        image
    }
}
