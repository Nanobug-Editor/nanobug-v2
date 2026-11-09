//! Editing modes and their status-line labels.

use crate::i18n;

/// Editing mode, independent of native focus and cursor animation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Character motions and editing operators.
    Normal,
    /// Native text entry and IME composition.
    Insert,
    /// Inclusive character selection.
    Visual,
    /// Whole-line selection.
    VisualLine,
    /// A short Ex command entered after `:`.
    Command,
    /// Literal text search entered after `/`.
    Search,
}

impl Mode {
    /// Localized short mode name displayed in the status line.
    pub fn label(self) -> &'static str {
        i18n::text(match self {
            Self::Normal => "mode.normal",
            Self::Insert => "mode.insert",
            Self::Visual => "mode.visual",
            Self::VisualLine => "mode.visual_line",
            Self::Command => "mode.command",
            Self::Search => "mode.search",
        })
    }
}
