//! Shared palette and bundled font descriptors for the native character grid.

use gpui::{Font, FontFeatures, font};
use std::sync::LazyLock;

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
pub(super) const TREE_GUIDE: u32 = 0x4f627aff;
/// Bundled Rec Mono Casual, with code ligatures disabled to preserve separate cells.
pub static GRID_TEXT_FONT: LazyLock<Font> = LazyLock::new(|| Font {
    features: FontFeatures::disable_ligatures(),
    ..font("Rec Mono Casual")
});

/// Bundled PuHuiTi 3.0 Regular, selected explicitly so it works without a system install.
pub static GRID_CHINESE_FONT: LazyLock<Font> = LazyLock::new(|| font("Alibaba PuHuiTi 3.0"));
