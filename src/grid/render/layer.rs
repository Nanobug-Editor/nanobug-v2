//! A retained panel positioned and clipped within the native grid surface.

use super::{Metrics, Panel, Rect};
use gpui::{Bounds, Entity, Pixels, point, px, size};

/// Placement of a retained workspace panel and its optional clip.
pub(in crate::grid) struct GridLayer {
    /// Panel that owns the shaped content.
    pub panel: Entity<Panel>,
    /// Window-local bounds of the panel's content, including its scroll offset.
    pub bounds: Bounds<Pixels>,
    /// Fixed viewport for a scrollable body.
    pub clip: Option<Bounds<Pixels>>,
}

impl GridLayer {
    /// Position `panel` in the `rect` viewport at `fraction` of a row, using `metrics`.
    /// The retained body includes one extra row just below the clip.
    pub(in crate::grid) fn scrolled(
        panel: &Entity<Panel>,
        rect: Rect,
        fraction: f32,
        metrics: Metrics,
    ) -> Self {
        let clip = rect.pixels(metrics);
        Self {
            panel: panel.clone(),
            bounds: Bounds::new(
                clip.origin - point(px(0.), px(fraction * metrics.lh)),
                size(clip.size.width, px((rect.h + 1) as f32 * metrics.lh)),
            ),
            clip: Some(clip),
        }
    }
}
