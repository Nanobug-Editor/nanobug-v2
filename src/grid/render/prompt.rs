//! Single-row popup prompts with shared key styling and click targets.

use super::{Frame, LabelStyle};
use crate::ui::{
    icons::Icon,
    text::TextSpacing,
    theme::{ACCENT, TEXT},
};
use std::ops::Range;

/// Magnifying glass drawn at the start of a filter prompt.
pub(in crate::grid) const SEARCH_ICON: Icon =
    Icon::new(include_bytes!("../../../assets/icons/search.svg"), 1.);

/// Apply the prompt with Enter.
const APPLY: PromptButton = PromptButton {
    prefix: &[
        ("[", LabelStyle::Text),
        ("✓", LabelStyle::Symbol),
        (" ", LabelStyle::Text),
    ],
    key: "Enter",
};
/// Cancel the prompt with Escape.
const CANCEL: PromptButton = PromptButton {
    prefix: &[("[✕ ", LabelStyle::Text)],
    key: "Esc",
};

/// Bracketed action with an underlined key name.
struct PromptButton {
    /// Styled spans before the key name.
    prefix: &'static [(&'static str, LabelStyle)],
    /// Visible keyboard key name.
    key: &'static str,
}

impl PromptButton {
    /// Return the width of the action using `spacing`, including its closing bracket.
    fn columns(&self, spacing: &TextSpacing) -> usize {
        let prefix: usize = self
            .prefix
            .iter()
            .map(|(text, _)| spacing.columns(text))
            .sum();
        prefix + spacing.columns(self.key) + 1
    }

    /// Draw the action in `color` at `slot` on `frame`'s `row`, using measured `spacing`.
    fn draw(
        &self,
        frame: &mut Frame,
        row: usize,
        slot: Range<usize>,
        color: u32,
        spacing: &TextSpacing,
    ) {
        let mut start = slot.start;
        for &(text, style) in self.prefix {
            start = frame.styled_label(row, start..slot.end, text, color, spacing, style);
        }
        let end = frame.styled_label(
            row,
            start..slot.end,
            self.key,
            color,
            spacing,
            LabelStyle::Key,
        );
        frame.label(row, end..slot.end, "]", color, spacing);
    }
}

/// Input and action slots in columns relative to the enclosing frame.
pub(in crate::grid) struct PromptRow {
    /// Text area after any leading icon or label.
    pub(in crate::grid) input: Range<usize>,
    /// Apply and Cancel hit targets, in that order.
    pub(in crate::grid) buttons: [Range<usize>; 2],
}

impl PromptRow {
    /// Fit the actions before `right` and start input at `left`, measured with `spacing`.
    pub(in crate::grid) fn new(left: usize, right: usize, spacing: &TextSpacing) -> Self {
        let cancel = right.saturating_sub(CANCEL.columns(spacing));
        let apply_end = cancel.saturating_sub(1);
        let apply = apply_end.saturating_sub(APPLY.columns(spacing));
        Self {
            input: left..apply.saturating_sub(2).max(left),
            buttons: [apply..apply_end, cancel..right],
        }
    }

    /// Draw the actions on `frame`'s `row` using the same `spacing` as the input slots.
    pub(in crate::grid) fn draw_buttons(
        &self,
        frame: &mut Frame,
        row: usize,
        spacing: &TextSpacing,
    ) {
        let [apply, cancel] = self.buttons.clone();
        APPLY.draw(frame, row, apply, ACCENT, spacing);
        CANCEL.draw(frame, row, cancel, TEXT, spacing);
    }

    /// Resolve a click at `column` to an action. `true` means cancel.
    pub(in crate::grid) fn cancel_at(&self, column: usize) -> Option<bool> {
        self.buttons
            .iter()
            .position(|slot| slot.contains(&column))
            .map(|index| index == 1)
    }
}
