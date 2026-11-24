//! Paint retained workspace layers behind dialogs and rounded outlines around detail popovers.

use super::{
    BORDER_LABEL_INSET, GridLayer, Metrics, OutlineStyle, Rect,
    outline::{cell_center_bounds, outline_path},
};
use crate::ui::{
    components::grid_dialog::DialogBackdrop,
    theme::{MUTED, style},
};
use gpui::{
    AnyElement, App, Bounds, IntoElement, Pixels, PlatformTextSystem, Styled, Window, canvas, rgb,
};
use std::ops::Range;

/// Stamp the visible portions of the grid layers when the dialog needs a new background image.
pub(in crate::grid) fn dialog_backdrop<'a>(
    backdrop: &mut DialogBackdrop,
    layers: impl IntoIterator<Item = &'a GridLayer>,
    outline: Bounds<Pixels>,
    glyphs: &dyn PlatformTextSystem,
    window: &mut Window,
    cx: &App,
) -> AnyElement {
    backdrop.element(outline, glyphs, window, cx, |raster, window| {
        for layer in layers {
            let clip = layer.clip.unwrap_or(layer.bounds);
            if !clip.is_empty() {
                layer
                    .panel
                    .read(cx)
                    .stamp_backdrop(layer.bounds, clip, raster, window, cx);
            }
        }
    })
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
