//! Keep editor appearance, retained panel geometry and pointer coordinates in sync.

use super::{
    Workspace,
    layout::{OUTER_INSET_Y, TAB_GAP_Y},
};
use crate::{
    grid::{render::Metrics, windows::start::StartPage},
    ui::{
        text::TextSpacing,
        theme::{AppearanceSettings, GRID_CHINESE_FONT, GRID_RULE_FONT_FAMILY, style},
    },
};
use gpui::{Context, PlatformTextSystem, Window, font, point, px};
use std::sync::Arc;

/// Measure bundled grid faces in logical pixels and snap their advances to this display.
pub(super) fn measure(
    window: &Window,
    glyphs: &Arc<dyn PlatformTextSystem>,
    appearance: AppearanceSettings,
) -> (Metrics, TextSpacing) {
    let system = window.text_system();
    let font_size = appearance.base_size * appearance.zoom;
    let scale = window.scale_factor();
    let text = system.resolve_font(&appearance.font.descriptor());
    let rules = system.resolve_font(&font(GRID_RULE_FONT_FAMILY));
    let advance = f32::from(
        system
            .advance(text, px(font_size), 'M')
            .expect("font advance")
            .width,
    );
    let height = f32::from(system.ascent(rules, px(font_size))).abs()
        + f32::from(system.descent(rules, px(font_size))).abs();
    let metrics = Metrics {
        cw: (advance * scale).round().max(1.) / scale,
        lh: (height * scale).ceil() / scale,
        font_size,
        origin: point(px(14.), px(OUTER_INSET_Y + TAB_GAP_Y)),
    };
    let chinese = system.resolve_font(&GRID_CHINESE_FONT);
    let spacing = TextSpacing::new(glyphs.clone(), chinese, font_size, metrics.cw);
    (metrics, spacing)
}

impl Workspace {
    /// Update the retained entities in place before the next grid frame and caret are derived.
    pub(super) fn update_display_scale(&mut self, window: &Window, cx: &mut Context<Self>) -> bool {
        let scale = window.scale_factor();
        if self.display_scale == scale {
            return false;
        }
        self.display_scale = scale;
        self.last_cursor_space = None;
        self.help.invalidate_backdrop();
        self.refresh_grid_appearance(window, cx, true);
        true
    }

    /// Apply live settings without recreating tabs, popups or plugin sessions.
    pub(super) fn apply_appearance(
        &mut self,
        next: AppearanceSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = style(cx);
        let updated = next.style(window.appearance());
        let font_changed = current.font != updated.font;
        let layout_changed = font_changed
            || self.appearance.base_size != next.base_size
            || self.appearance.zoom != next.zoom;
        self.appearance = next;
        if current != updated {
            cx.set_global(updated);
        }
        if current != updated || layout_changed {
            self.help.invalidate_backdrop();
            self.invalidate_plugin_appearance();
            if layout_changed {
                self.last_cursor_space = None;
            }
            self.refresh_grid_appearance(window, cx, layout_changed);
            if layout_changed {
                self.sync(window, cx);
            } else {
                self.update_cursor(window, cx);
                cx.notify();
            }
        }
    }

    fn refresh_grid_appearance(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
        remeasure: bool,
    ) {
        if remeasure {
            (self.metrics, self.spacing) = measure(window, &self.glyphs, self.appearance);
        }
        let metrics = self.metrics;
        self.tabs.set_metrics(metrics, remeasure, cx);
        self.start_panel.update(cx, |panel, cx| {
            panel.set_appearance(StartPage::content_metrics(metrics), cx)
        });
        for panel in self
            .panels
            .iter()
            .chain(&self.bodies)
            .chain([
                &self.tree_name_preview,
                &self.language_list,
                &self.popup_panel,
                &self.help.panel,
                &self.help.body,
                &self.help.dim_title,
                &self.help.search,
                self.plugin_panel(),
            ])
            .chain(
                self.file_picker
                    .as_ref()
                    .into_iter()
                    .flat_map(|picker| picker.panels()),
            )
        {
            panel.update(cx, |panel, cx| panel.set_appearance(metrics, cx));
        }
        #[cfg(feature = "plugin-probe")]
        if let Some(probe) = &self.probe {
            probe
                .panel
                .update(cx, |panel, cx| panel.set_appearance(metrics, cx));
        }
    }
}
