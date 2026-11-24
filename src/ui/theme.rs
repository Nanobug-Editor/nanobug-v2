//! Editor palettes and bundled monospace font descriptors shared by native and SDK drawing.

mod highlight;
mod palette;
mod preferences;
mod syntax;
pub(crate) use syntax::{syntax_example, syntax_section};

pub(crate) use palette::{
    ACCENT, ACTIVE_FILE, CURRENT, CURSOR_TEXT, ERROR_BORDER, ERROR_TEXT, ERROR_UNDERLINE,
    FOOTER_BLUE, FOOTER_DETAILS, FOOTER_INSERT, FOOTER_VISUAL, GOLD, MUTED, PLUGIN_ERROR, PREEDIT,
    SELECTION, TEXT, TREE_GUIDE, ThemeComponent, WARNING_BORDER, WARNING_TEXT, WARNING_UNDERLINE,
    load_for_editor,
};

pub(crate) use preferences::AppearanceChange;

use gpui::{App, Font, FontFeatures, Global, PlatformTextSystem, WindowAppearance, font};
use std::{borrow::Cow, sync::LazyLock};

/// Bundled face for box-drawing glyphs and the grid's row-height metrics.
pub const GRID_RULE_FONT_FAMILY: &str = "Nanobug Grid";
/// Editor and chrome font size in logical pixels.
pub const GRID_FONT_SIZE: f32 = 16.;
/// Resolve plugin syntax `kind` to a host palette role used by retained editor cells.
pub(crate) use palette::syntax_color;

/// Bundled Rec Mono Casual, with code ligatures disabled to preserve separate cells.
pub static GRID_TEXT_FONT: LazyLock<Font> = LazyLock::new(|| Font {
    features: FontFeatures::disable_ligatures(),
    ..font("Rec Mono Casual")
});

/// Bundled PuHuiTi 3.0 Regular, selected explicitly so it works without a system install.
pub static GRID_CHINESE_FONT: LazyLock<Font> = LazyLock::new(|| font("Alibaba PuHuiTi 3.0"));

/// The editor uses these tested, bundled monospace faces for the grid.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum GridFont {
    #[default]
    RecMono,
    MapleMono,
}

impl GridFont {
    /// Register the alternative bundled font when the selected appearance uses it.
    pub(crate) fn ensure_maple(text: &dyn PlatformTextSystem) -> gpui::Result<()> {
        if text.font_id(&Self::MapleMono.descriptor()).is_err() {
            text.add_fonts(vec![Cow::Borrowed(include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/fonts/MapleMonoNL-CN-Regular.ttf"
            )))])?;
        }
        Ok(())
    }

    /// Return the same family name in the native shaper and in the plugin appearance.
    pub(crate) fn family(self) -> &'static str {
        match self {
            Self::RecMono => "Rec Mono Casual",
            Self::MapleMono => "Maple Mono NL CN",
        }
    }

    /// Select the grid face with ligatures disabled so each cell remains separately addressable.
    pub(crate) fn descriptor(self) -> Font {
        match self {
            Self::RecMono => GRID_TEXT_FONT.clone(),
            Self::MapleMono => Font {
                features: FontFeatures::disable_ligatures(),
                ..font(self.family())
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ThemeChoice {
    /// Keep the original Nanobug palette unless the user changes the theme.
    #[default]
    Dark,
    Light,
    System,
}

/// Validated editor-wide preferences for this window. Device DPI is separate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct AppearanceSettings {
    pub(crate) theme: ThemeChoice,
    pub(crate) font: GridFont,
    pub(crate) base_size: f32,
    pub(crate) zoom: f32,
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        Self {
            theme: ThemeChoice::Dark,
            font: GridFont::RecMono,
            base_size: GRID_FONT_SIZE,
            zoom: 1.,
        }
    }
}

/// Resolved colors and font used by both retained controls and native plugin drawing.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Style {
    pub(crate) light: bool,
    pub(crate) font: GridFont,
    /// Component whose color overrides take precedence over the global palette.
    component: Option<ThemeComponent>,
}

impl Global for Style {}

impl AppearanceSettings {
    /// Resolve the user palette against the system setting only in follow-system mode.
    pub(crate) fn style(self, system: WindowAppearance) -> Style {
        Style {
            light: match self.theme {
                ThemeChoice::Light => true,
                ThemeChoice::Dark => false,
                ThemeChoice::System => {
                    matches!(
                        system,
                        WindowAppearance::Light | WindowAppearance::VibrantLight
                    )
                }
            },
            font: self.font,
            component: None,
        }
    }
}

/// Native and test windows without an explicit style keep the bundled defaults.
pub(crate) fn style(cx: &App) -> Style {
    cx.try_global::<Style>().copied().unwrap_or_default()
}

impl Style {
    /// Apply the color overrides for `component` while retaining the theme and font.
    pub(crate) fn for_component(self, component: ThemeComponent) -> Self {
        Self {
            component: Some(component),
            ..self
        }
    }

    /// Return the nearest configured component background over the existing window material.
    pub(crate) fn component_background(self) -> Option<u32> {
        palette::component_color(self.light, self.component?, palette::BACKGROUND)
    }

    /// Resolve a retained RGB role using this palette.
    pub(crate) fn foreground(self, role: u32) -> u32 {
        palette::resolve(self.light, self.component, role)
    }

    /// Resolve a retained RGBA role using this palette.
    pub(crate) fn tint(self, role: u32) -> u32 {
        palette::resolve(self.light, self.component, role)
    }

    /// Return the translucent material behind the editor.
    pub(crate) fn background(self) -> u32 {
        self.tint(palette::BACKGROUND)
    }

    /// Return the opaque background used by plugin panels.
    pub(crate) fn panel(self) -> u32 {
        self.background() | 0xff
    }

    /// Return RGB cursor fill, with opacity determined by the animation.
    pub(crate) fn cursor(self) -> u32 {
        self.foreground(palette::CURSOR_FILL)
    }

    /// Return RGBA ink for text shadows.
    pub(crate) fn shadow(self) -> u32 {
        self.tint(palette::SHADOW)
    }
}
