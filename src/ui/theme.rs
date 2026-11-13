//! Editor palettes and bundled monospace font descriptors shared by native and SDK drawing.

use gpui::{App, Font, FontFeatures, Global, PlatformTextSystem, WindowAppearance, font};
use std::{borrow::Cow, sync::LazyLock};

/// Bundled face whose box glyphs define the pane rules and row height.
pub const GRID_RULE_FONT_FAMILY: &str = "Nanobug Grid";
/// Editor and chrome font size in logical pixels.
pub const GRID_FONT_SIZE: f32 = 16.;
/// Foreground color of normal text.
pub const TEXT: u32 = 0xc8d3f5;
/// Foreground color of secondary text.
pub const MUTED: u32 = 0x8a9eb3;
/// Accent for focused panes, mode labels and text waves.
pub const ACCENT: u32 = 0x9df0d4;
/// Warm accent for project names and the active line number.
pub const GOLD: u32 = 0xf2d09b;
/// Shared material tint: approximately 84% opacity (16% transparent), independent of text.
pub const BACKGROUND: u32 = 0x0b1624d6;
/// Base RGB fill of the cursor and its motion effects.
pub const CURSOR_FILL: u32 = 0x9df0d4;
/// RGB ink for the character covered by a block cursor.
pub const CURSOR_TEXT: u32 = BACKGROUND >> 8;
/// Selection tint, matching the Glass Editor reference.
pub const SELECTION: u32 = 0x65baa558;
/// Subtle current-line/status tint above the panel's material.
pub const CURRENT: u32 = 0x85bcd522;
/// Low-contrast RGBA ink for Explorer hierarchy guides.
pub(crate) const TREE_GUIDE: u32 = 0x4f627aff;
/// Resolve plugin syntax `kind` to a host palette role used by retained editor cells.
pub(crate) fn syntax_color(kind: nanobug_plugin_sdk::language::SyntaxKind) -> u32 {
    use nanobug_plugin_sdk::language::SyntaxKind::*;
    match kind {
        Keyword => 0xc4a7f4,
        Type => 0x89c8f2,
        Function => 0x97b8ff,
        String => 0xa9d69b,
        Constant => 0xe9ab8c,
        Comment => MUTED,
        Attribute => GOLD,
    }
}

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
    /// Register the alternative bundled font on demand, outside the startup path.
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
        }
    }
}

/// Native and test windows without an explicit style keep the bundled defaults.
pub(crate) fn style(cx: &App) -> Style {
    cx.try_global::<Style>().copied().unwrap_or_default()
}

impl Style {
    /// Resolve only editor palette roles. Explicit SDK colors bypass this mapping.
    pub(crate) fn foreground(self, role: u32) -> u32 {
        if !self.light {
            return role;
        }
        match role {
            0xc4a7f4 => 0x70439c,
            0x89c8f2 => 0x235a82,
            0x97b8ff => 0x344aa2,
            0xa9d69b => 0x376431,
            0xe9ab8c => 0x8c3e27,
            TEXT => 0x263454,
            MUTED => 0x495c73,
            ACCENT => 0x146858,
            GOLD => 0x895218,
            _ => role,
        }
    }

    pub(crate) fn tint(self, role: u32) -> u32 {
        if !self.light {
            return role;
        }
        match role {
            SELECTION => 0x579a935c,
            CURRENT => 0x546c9b22,
            TREE_GUIDE => 0x7c90a9ff,
            _ => role,
        }
    }

    pub(crate) fn background(self) -> u32 {
        if self.light { 0xe8eef9ef } else { BACKGROUND }
    }

    pub(crate) fn panel(self) -> u32 {
        self.background() | 0xff
    }

    pub(crate) fn cursor(self) -> u32 {
        if self.light { 0x91d7c2 } else { CURSOR_FILL }
    }

    pub(crate) fn shadow(self) -> u32 {
        if self.light { 0x00000000 } else { 0x00000070 }
    }
}
