//! Editing modes and their status-line labels.

use crate::i18n;

/// Editing mode, independent of native focus and cursor animation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Character motions and editing operators.
    Normal,
    /// Native text entry and IME composition.
    Insert,
    /// Replace characters at the caret using native text input.
    Replace,
    /// Inclusive character selection.
    Visual,
    /// Whole-line selection.
    VisualLine,
    /// Rectangular selection over multiple display columns and lines.
    VisualBlock,
    /// A short Ex command entered after `:`.
    Command,
    /// Pattern search entered after `/` or `?`.
    Search,
}

impl Mode {
    /// Whether committed text edits the document in this mode.
    pub fn is_insert(self) -> bool {
        matches!(self, Self::Insert | Self::Replace)
    }

    /// Whether the native IME and clipboard can commit text in this mode.
    pub fn accepts_text(self) -> bool {
        self.is_insert() || matches!(self, Self::Command | Self::Search)
    }

    /// Whether document motions extend a selection in this mode.
    pub fn is_visual(self) -> bool {
        matches!(self, Self::Visual | Self::VisualLine | Self::VisualBlock)
    }

    /// Localized short mode name displayed in the status line.
    pub fn label(self) -> &'static str {
        i18n::text(match self {
            Self::Normal => "mode.normal",
            Self::Insert => "mode.insert",
            Self::Replace => "mode.replace",
            Self::Visual => "mode.visual",
            Self::VisualLine => "mode.visual_line",
            Self::VisualBlock => "mode.visual_block",
            Self::Command => "mode.command",
            Self::Search => "mode.search",
        })
    }
}
