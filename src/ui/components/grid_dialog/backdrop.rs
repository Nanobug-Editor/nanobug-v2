//! Optional cached backdrop for a dialog over retained window content.

use crate::ui::{backdrop::BackdropRaster, theme::style};
use gpui::{
    AnyElement, App, Bounds, Corners, IntoElement, Pixels, PlatformTextSystem, RenderImage, Size,
    Styled, Window, canvas, point, px,
};
use std::sync::Arc;

/// Distance from the grid border's centerline to the rounded cutout in logical pixels.
const BORDER_GAP: f32 = 12.;
/// Radius of the dialog's outer corners in logical pixels.
const CORNER_RADIUS: f32 = 14.;

/// Retain the window-local backdrop while a dialog is open.
#[derive(Default)]
pub(crate) struct DialogBackdrop {
    /// Current image, cleared when the workspace is visible again.
    cached: Option<CachedBackdrop>,
}

/// Snapshot and placement used to decide when the backdrop can be reused.
struct CachedBackdrop {
    /// Logical size of the window the image covers.
    viewport: Size<Pixels>,
    /// Rounded dialog bounds reserved in the image.
    outline: Bounds<Pixels>,
    /// Whether the image still reflects the window's appearance.
    valid: bool,
    /// Downsampled, blurred workspace texture.
    image: Arc<RenderImage>,
}

impl DialogBackdrop {
    /// Leave an even logical-pixel gap from the character border of `grid` to the rounded cutout.
    pub(crate) fn outline(
        grid: Bounds<Pixels>,
        cell_width: f32,
        row_height: f32,
    ) -> Bounds<Pixels> {
        let padding = point(
            px(BORDER_GAP - cell_width * 0.5),
            px(BORDER_GAP - row_height * 0.5),
        );
        Bounds::from_corners(grid.origin - padding, grid.bottom_right() + padding)
    }

    /// Rebuild the image from the reflowed panels on the next render.
    pub(crate) fn invalidate(&mut self) {
        if let Some(backdrop) = &mut self.cached {
            backdrop.valid = false;
        }
    }

    /// Release the cached native image in `window` once the dialog is closed.
    pub(crate) fn release(&mut self, window: &mut Window) {
        if let Some(backdrop) = self.cached.take() {
            let _ = window.drop_image(backdrop.image);
        }
    }

    /// Stamp the visible content into a new backdrop only when the cache needs rebuilding.
    pub(crate) fn element(
        &mut self,
        outline: Bounds<Pixels>,
        glyphs: &dyn PlatformTextSystem,
        window: &mut Window,
        cx: &App,
        stamp: impl FnOnce(&mut BackdropRaster<'_>, &mut Window),
    ) -> AnyElement {
        let viewport = window.viewport_size();
        if !self.cached.as_ref().is_some_and(|backdrop| {
            backdrop.valid && backdrop.viewport == viewport && backdrop.outline == outline
        }) {
            let mut raster = BackdropRaster::new(viewport, glyphs, cx.svg_renderer());
            stamp(&mut raster, window);
            let image = raster.finish(outline, CORNER_RADIUS, style(cx).background());
            if let Some(previous) = self.cached.replace(CachedBackdrop {
                viewport,
                outline,
                valid: true,
                image,
            }) {
                let _ = window.drop_image(previous.image);
            }
        }
        let image = self.cached.as_ref().expect("dialog backdrop").image.clone();
        canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                let _ = window.paint_image(bounds, bounds, Corners::default(), image, 0, false);
            },
        )
        .absolute()
        .left_0()
        .top_0()
        .size_full()
        .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::size;

    /// The visible character edges are equally inset despite unequal cell dimensions.
    #[test]
    fn dialog_outline_is_even_around_the_character_border() {
        let cell_width = 10.;
        let row_height = 21.;
        let grid = Bounds::new(point(px(100.), px(50.)), size(px(640.), px(462.)));
        let outline = DialogBackdrop::outline(grid, cell_width, row_height);
        assert_eq!(
            grid.left() + px(cell_width / 2.) - outline.left(),
            px(BORDER_GAP)
        );
        assert_eq!(
            outline.right() - (grid.right() - px(cell_width / 2.)),
            px(BORDER_GAP)
        );
        assert_eq!(
            grid.top() + px(row_height / 2.) - outline.top(),
            px(BORDER_GAP)
        );
        assert_eq!(
            outline.bottom() - (grid.bottom() - px(row_height / 2.)),
            px(BORDER_GAP)
        );
    }
}
