//! Built-in grid windows. Each feature owns its data and derives its own display frames.

pub(crate) mod editor;
pub(in crate::grid) mod file_picker;
pub(crate) mod help;
pub(crate) mod manager;
pub(in crate::grid) mod start;
pub(in crate::grid) mod terminal;
pub(in crate::grid) mod vim_menu;

pub(in crate::grid) mod plugin_manager;
pub(in crate::grid) mod plugin_surface;

mod frame;
pub(in crate::grid) use frame::WindowFrame;

use nanobug::i18n;

/// A help row's key label and the catalog key for its description.
pub(in crate::grid) type Shortcut = (ShortcutKeys, &'static str);

/// Literal keyboard notation or a catalog key for a label that includes words.
pub(in crate::grid) enum ShortcutKeys {
    /// Notation shared across languages, such as `Ctrl+s`.
    Fixed(&'static str),
    /// A label containing translatable text, such as `Shift+Wheel`.
    Localized(&'static str),
}

impl ShortcutKeys {
    /// Return the visible key label in the current locale.
    fn text(&self) -> &'static str {
        match self {
            Self::Fixed(label) => label,
            Self::Localized(key) => i18n::text(key),
        }
    }
}

pub(in crate::grid) use nanobug::filesystem::PathChange;

#[cfg(test)]
mod tests;
