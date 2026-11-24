//! Search icon and footer input flanked by the shared action buttons.

use super::Frame;
use crate::ui::{
    components::grid_button_row::{ButtonSlot, GridButtonRow},
    icons::Icon,
    text::TextSpacing,
    theme::{ACCENT, TEXT},
};
use std::ops::Range;

/// Magnifying glass drawn at the start of a filter prompt.
pub(in crate::grid) const SEARCH_ICON: Icon =
    Icon::new(include_bytes!("../../../assets/icons/search.svg"), 1.);

/// Input and action slots relative to the enclosing frame.
pub(in crate::grid) struct PromptRow {
    pub(in crate::grid) input: Range<usize>,
    buttons: Vec<ButtonSlot<bool>>,
}

impl PromptRow {
    pub(in crate::grid) fn new(left: usize, right: usize, spacing: &TextSpacing) -> Self {
        let buttons = GridButtonRow::enter_escape().trailing(left..right, 0..1, 1, spacing);
        let end = buttons[0].columns.start.saturating_sub(2).max(left);
        Self {
            input: left..end,
            buttons,
        }
    }
    /// Return the measured apply and cancel columns for the native pointer bridge.
    pub(in crate::grid) fn button_columns(&self) -> [Range<usize>; 2] {
        std::array::from_fn(|i| self.buttons[i].columns.clone())
    }
    pub(in crate::grid) fn draw_buttons(
        &self,
        frame: &mut Frame,
        row: usize,
        spacing: &TextSpacing,
    ) {
        for button in &self.buttons {
            frame.button(
                row,
                button,
                if button.action { ACCENT } else { TEXT },
                spacing,
            );
        }
    }
    pub(in crate::grid) fn cancel_at(&self, column: usize) -> Option<bool> {
        GridButtonRow::hit(&self.buttons, column).map(|accept| !accept)
    }
}
