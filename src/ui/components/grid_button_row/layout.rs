use crate::ui::{mnemonic::Mnemonic, text::TextSpacing};
use std::ops::Range;

/// A row of visible choices. Keyboard matching, drawing and pointer input share their order.
pub(crate) struct GridButtonRow<A> {
    /// Localized captions paired with the action they activate, in display order.
    choices: Vec<(A, Mnemonic)>,
}

/// An action's painted caption and its half-open local hit target.
pub(crate) struct ButtonSlot<A> {
    /// Returned by both pointer and keyboard activation.
    pub(crate) action: A,
    /// Visible label, including the keyboard shortcut.
    pub(crate) caption: Mnemonic,
    /// Columns occupied by the visible part of this button.
    pub(crate) columns: Range<usize>,
    /// Vertical position relative to the enclosing frame.
    pub(crate) row: usize,
}

/// Placement of a confirmation row within its available columns.
#[derive(Clone, Copy)]
pub(crate) enum ButtonAlign {
    /// Begin at the first available column.
    Start,
    /// Center all choices together, including the gaps.
    Center,
}

impl<A: Copy> GridButtonRow<A> {
    /// Build the active row from `choices`, each pairing an action with a localized caption.
    pub(crate) fn new(choices: impl IntoIterator<Item = (A, Mnemonic)>) -> Self {
        Self {
            choices: choices.into_iter().collect(),
        }
    }

    /// Match a native `key` to a displayed caption without applying any business action.
    pub(crate) fn key(&self, key: &str) -> Option<A> {
        self.choices
            .iter()
            .find_map(|(action, caption)| caption.matches(key).then_some(*action))
    }

    /// Fit buttons within local `columns`, using `align`, `gap` in cells and the drawing `spacing`.
    pub(crate) fn slots(
        &self,
        columns: Range<usize>,
        align: ButtonAlign,
        gap: usize,
        spacing: &TextSpacing,
    ) -> Vec<ButtonSlot<A>> {
        let mut slots: Vec<_> = self
            .choices
            .iter()
            .map(|&(action, caption)| ButtonSlot {
                action,
                caption,
                columns: 0..caption.columns(spacing),
                row: 0,
            })
            .collect();
        let total = slots
            .iter()
            .map(|button| button.columns.len())
            .sum::<usize>()
            .saturating_add(gap.saturating_mul(slots.len().saturating_sub(1)));
        let mut x = columns.start
            + match align {
                ButtonAlign::Start => 0,
                ButtonAlign::Center => columns.len().saturating_sub(total) / 2,
            };
        for button in &mut slots {
            let end = x.saturating_add(button.columns.len()).min(columns.end);
            button.columns = x.min(end)..end;
            x = end.saturating_add(gap);
        }
        slots
    }

    /// Right-align from the last action, wrapping earlier actions upward within the supplied rows.
    pub(crate) fn trailing(
        &self,
        columns: Range<usize>,
        rows: Range<usize>,
        gap: usize,
        spacing: &TextSpacing,
    ) -> Vec<ButtonSlot<A>> {
        if rows.is_empty() {
            return Vec::new();
        }
        let mut available = columns.end;
        let mut y = rows.end - 1;
        let mut slots = Vec::with_capacity(self.choices.len());
        for &(action, caption) in self.choices.iter().rev() {
            let width = caption.columns(spacing);
            if available != columns.end
                && width > available.saturating_sub(columns.start)
                && y > rows.start
            {
                y -= 1;
                available = columns.end;
            }
            let left = available.saturating_sub(width).max(columns.start);
            slots.push(ButtonSlot {
                action,
                caption,
                columns: left..available.max(left),
                row: y,
            });
            available = left.saturating_sub(gap).max(columns.start);
        }
        slots.reverse();
        slots
    }

    /// Resolve a pointer in a multi-row footer to a displayed action.
    pub(crate) fn hit_at(slots: &[ButtonSlot<A>], col: usize, row: usize) -> Option<A> {
        slots
            .iter()
            .find(|button| button.row == row && button.columns.contains(&col))
            .map(|button| button.action)
    }

    /// Return the displayed action hit at local `column` in the measured `slots`.
    pub(crate) fn hit(slots: &[ButtonSlot<A>], column: usize) -> Option<A> {
        slots
            .iter()
            .find(|button| button.columns.contains(&column))
            .map(|button| button.action)
    }
}

impl GridButtonRow<bool> {
    /// Physical Enter/Escape buttons for a prompt, with the existing padded pointer targets.
    /// True accepts and false cancels.
    pub(crate) fn enter_escape() -> Self {
        Self::new([
            (true, Mnemonic::symbol_key("✓", "Enter", "enter")),
            (false, Mnemonic::named_key("✕ ", "Esc", "escape")),
        ])
    }
}
