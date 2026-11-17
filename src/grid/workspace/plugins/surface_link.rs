//! Workspace adapter for surface link.

use super::*;

impl Workspace {
    /// Dismiss the host surface and clear its local input state.
    pub(in crate::grid::workspace) fn close_plugin_surface(&mut self) {
        self.plugins.surface.focused = false;
        self.plugins.surface.key = None;
        self.plugins.surface.viewport = None;
        self.plugins.surface.native_field = None;
        self.plugins.composition.clear();
        self.clear_canvas_focus();
        self.end_plugin_field_drag();
    }

    pub(in crate::grid::workspace) fn plugin_surface_area(&self) -> Option<Rect> {
        let key = self.plugins.surface.key.as_ref()?;
        if Some(key.instance.project) != self.plugins.active || self.modal_open() {
            return None;
        }
        let editor = self.layout.editor;
        let height = editor.h.clamp(4, 10);
        Some(Rect {
            y: editor.y + editor.h.saturating_sub(height),
            h: height,
            ..editor
        })
    }

    /// Resolve a frame whose geometry is still current for this window.
    pub(in crate::grid::workspace) fn current_plugin_surface(
        &self,
    ) -> Option<&nanobug::plugins::session::Surface> {
        let surface = self.displayed_plugin_surface()?;
        surface
            .frame
            .canvas
            .as_ref()
            .and_then(|canvas| canvas.viewport)
            .is_none_or(|id| {
                self.plugins
                    .surface
                    .viewport
                    .is_some_and(|viewport| viewport.id == id)
            })
            .then_some(surface)
    }

    /// Retain the last prepared drawing and its host input during a viewport transition.
    /// Guest actions still require `current_plugin_surface`.
    pub(in crate::grid::workspace) fn displayed_plugin_surface(
        &self,
    ) -> Option<&nanobug::plugins::session::Surface> {
        self.plugins
            .view
            .surfaces
            .get(self.plugins.surface.key.as_ref()?)
    }

    pub(in crate::grid::workspace) fn canvas_surface(
        &self,
    ) -> Option<&nanobug::plugins::session::Surface> {
        let surface = self.current_plugin_surface()?;
        surface.frame.canvas.as_ref()?;
        Some(surface)
    }

    pub(in crate::grid::workspace) fn input_cell(&self, area: Rect) -> (usize, usize, usize) {
        crate::grid::windows::plugin_surface::input_cell(
            area,
            self.metrics,
            self.displayed_plugin_surface(),
        )
    }

    /// Discard only host text layout; the active guest and its draft stay in place.
    pub(in crate::grid::workspace) fn invalidate_plugin_appearance(&mut self) {
        self.plugins.surface.native_field = None;
    }

    /// Publish the current host style and device scale with this surface's geometry.
    pub(in crate::grid::workspace) fn update_plugin_viewport(
        &mut self,
        area: Rect,
        window: &Window,
        cx: &gpui::App,
    ) -> bool {
        let Some(key) = self.plugins.surface.key.clone() else {
            return false;
        };
        let bounds = area.pixels(self.metrics);
        let (width, height, scale) = (
            f32::from(bounds.size.width),
            f32::from(bounds.size.height),
            window.scale_factor(),
        );
        let style = style(cx);
        let appearance = Some(Appearance {
            language: match i18n::locale() {
                i18n::Locale::English => Language::English,
                i18n::Locale::SimplifiedChinese => Language::SimplifiedChinese,
            },
            font_size: self.metrics.font_size,
            base_font_size: Some(self.appearance.base_size),
            font: Some(match self.appearance.font {
                GridFont::RecMono => EditorFont::RecMonoCasual,
                GridFont::MapleMono => EditorFont::MapleMonoNlCn,
            }),
            user_scale: self.appearance.zoom,
            theme: Some(if style.light {
                EditorTheme::Light
            } else {
                EditorTheme::Dark
            }),
            colors: ThemeColors {
                background: style.panel(),
                text: (style.foreground(TEXT) << 8) | 255,
                muted: (style.foreground(MUTED) << 8) | 255,
                accent: (style.foreground(ACCENT) << 8) | 255,
                selection: style.tint(SELECTION),
                warning: (style.foreground(GOLD) << 8) | 255,
                border: Some((style.foreground(MUTED) << 8) | 255),
                error: Some(style.tint(PLUGIN_ERROR)),
            },
        });
        if self.plugins.surface.viewport.is_some_and(|viewport| {
            (
                viewport.width,
                viewport.height,
                viewport.scale,
                viewport.appearance,
            ) == (width, height, scale, appearance)
        }) {
            return false;
        }
        self.plugins.surface.viewport_id += 1;
        let viewport = Viewport {
            id: self.plugins.surface.viewport_id,
            width,
            height,
            scale,
            apple_surface: cfg!(any(target_os = "macos", target_os = "ios")),
            shaped_line: true,
            appearance,
        };
        if self.plugins.send(Action::Resize(key, viewport)) {
            self.plugins.surface.viewport = Some(viewport);
            if let Some(trace) = &self.plugins.trace {
                trace.mark("plugin_viewport", serde_json::json!(viewport));
            }
            return true;
        }
        false
    }
}
