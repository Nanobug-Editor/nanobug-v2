//! Shared popup backdrop and rounded detail outline.

use super::{
    BORDER_LABEL_INSET, GridLayer, Metrics, OutlineStyle, Rect,
    outline::{cell_center_bounds, outline_path},
};
use crate::ui::{
    backdrop::BackdropRaster,
    theme::{MUTED, style},
};
use gpui::{
    AnyElement, App, Bounds, Corners, IntoElement, Pixels, PlatformTextSystem, RenderImage, Size,
    Styled, Window, canvas, point, px, rgb,
};
use std::{ops::Range, sync::Arc};

/// Distance from the grid border's centerline to the rounded cutout in logical pixels.
const BORDER_GAP: f32 = 12.;
/// Radius of the popup's outer corners in logical pixels.
const CORNER_RADIUS: f32 = 14.;

/// Return a rounded cutout's bounds with an even gap around the grid border inside `grid`.
/// `metrics` locates the centerline in the first and last rows and columns.
pub(in crate::grid) fn popup_outline(grid: Bounds<Pixels>, metrics: Metrics) -> Bounds<Pixels> {
    let padding = point(
        px(BORDER_GAP - metrics.cw * 0.5),
        px(BORDER_GAP - metrics.lh * 0.5),
    );
    Bounds::from_corners(grid.origin - padding, grid.bottom_right() + padding)
}

/// Retain the window-local backdrop while a popup is open.
#[derive(Default)]
pub(in crate::grid) struct PopupBackdrop {
    /// Current image, cleared when the workspace is visible again.
    cached: Option<CachedBackdrop>,
}

/// Snapshot and placement used to decide when the backdrop can be reused.
struct CachedBackdrop {
    /// Logical size of the window the image covers.
    viewport: Size<Pixels>,
    /// Rounded popup bounds reserved in the image.
    outline: Bounds<Pixels>,
    /// Whether the image still reflects the window's appearance.
    valid: bool,
    /// Downsampled, blurred workspace texture.
    image: Arc<RenderImage>,
}

impl PopupBackdrop {
    /// Rebuild the image from the reflowed panels on the next render.
    pub(in crate::grid) fn invalidate(&mut self) {
        if let Some(backdrop) = &mut self.cached {
            backdrop.valid = false;
        }
    }

    /// Release the cached native image in `window` once the popup is closed.
    pub(in crate::grid) fn release(&mut self, window: &mut Window) {
        if let Some(backdrop) = self.cached.take() {
            let _ = window.drop_image(backdrop.image);
        }
    }

    /// Paint blurred `layers` outside `outline` in `window`, reusing the image between renders.
    /// `glyphs` supplies font rasters; `cx` supplies retained panels and the SVG renderer.
    pub(in crate::grid) fn element<'a>(
        &mut self,
        layers: impl IntoIterator<Item = &'a GridLayer>,
        outline: Bounds<Pixels>,
        glyphs: &dyn PlatformTextSystem,
        window: &mut Window,
        cx: &App,
    ) -> AnyElement {
        let viewport = window.viewport_size();
        if !self.cached.as_ref().is_some_and(|backdrop| {
            backdrop.valid && backdrop.viewport == viewport && backdrop.outline == outline
        }) {
            let mut raster = BackdropRaster::new(viewport, glyphs, cx.svg_renderer());
            for layer in layers {
                let clip = layer.clip.unwrap_or(layer.bounds);
                if !clip.is_empty() {
                    layer.panel.read(cx).stamp_backdrop(
                        layer.bounds,
                        clip,
                        &mut raster,
                        window,
                        cx,
                    );
                }
            }
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
        let image = self.cached.as_ref().expect("popup backdrop").image.clone();
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

/// Place a dashed outline along `area`'s edge cells using `metrics`.
/// `title` is the visible span in local columns, with a cell of padding added on each side.
pub(in crate::grid) fn dashed_outline_element(
    area: Rect,
    metrics: Metrics,
    title: Range<usize>,
) -> AnyElement {
    let bounds = cell_center_bounds(area.pixels(metrics), metrics);
    let title_gap = (!title.is_empty()).then(|| {
        let column = |local| metrics.at((area.x + local) as f32 - 0.5, 0.).x;
        column(BORDER_LABEL_INSET)..column(title.end + 2)
    });
    canvas(
        |_, _, _| (),
        move |_, _, window, cx| paint_dashed_outline(bounds, title_gap, window, cx),
    )
    .absolute()
    .left_0()
    .top_0()
    .size_full()
    .into_any_element()
}

/// Paint equal-length dashes around rounded `bounds` using the palette from `cx`.
/// `title_gap` optionally interrupts the top edge of the path drawn in `window`.
pub(in crate::grid) fn paint_dashed_outline(
    bounds: Bounds<Pixels>,
    title_gap: Option<Range<Pixels>>,
    window: &mut Window,
    cx: &App,
) {
    let gaps = title_gap.as_slice();
    if let Some(path) = outline_path(bounds, OutlineStyle::Dashed, 0., gaps, &[]) {
        window.paint_path(path, rgb(style(cx).foreground(MUTED)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::size;

    /// The visible character edges are equally inset despite unequal cell dimensions.
    #[test]
    fn popup_outline_is_even_around_the_character_border() {
        let metrics = Metrics {
            cw: 10.,
            lh: 21.,
            font_size: 16.,
            origin: point(px(0.), px(0.)),
        };
        let grid = Bounds::new(point(px(100.), px(50.)), size(px(640.), px(462.)));
        let outline = popup_outline(grid, metrics);
        assert_eq!(
            grid.left() + px(metrics.cw / 2.) - outline.left(),
            px(BORDER_GAP)
        );
        assert_eq!(
            outline.right() - (grid.right() - px(metrics.cw / 2.)),
            px(BORDER_GAP)
        );
        assert_eq!(
            grid.top() + px(metrics.lh / 2.) - outline.top(),
            px(BORDER_GAP)
        );
        assert_eq!(
            outline.bottom() - (grid.bottom() - px(metrics.lh / 2.)),
            px(BORDER_GAP)
        );
    }
}
