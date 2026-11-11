//! Built-in grid windows. Each feature owns its data and derives its own display frames.

pub(crate) mod editor;
pub(in crate::grid) mod file_picker;
pub(crate) mod help;
pub(crate) mod manager;
pub(in crate::grid) mod start;

mod frame;
pub(in crate::grid) use frame::WindowFrame;

use nanobug::i18n;
use std::path::PathBuf;

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

/// Change to a visible path that open buffers must follow.
pub(in crate::grid) struct PathChange {
    /// Previous visible path, or a newly created path whose old cached buffer is stale.
    pub from: PathBuf,
    /// New path, or None when the old buffer should be closed.
    pub to: Option<PathBuf>,
}

#[cfg(test)]
mod tests;
