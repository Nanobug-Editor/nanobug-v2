//! Fit and draw the numbered pages beside the project tabs in the window caption.

use super::hidden_columns;
use crate::{
    grid::render::{Frame, Rect, SHORTCUT_SEPARATOR},
    ui::{
        text::TextSpacing,
        theme::{ACCENT, MUTED, TEXT},
    },
};
use std::ops::Range;

/// Minimum number of blank columns between project tabs and page numbers.
pub(super) const PAGE_GAP: usize = 2;

/// Draw `count` one-based labels, highlighting `active`, at the right of a `cols`-column row.
/// The returned rectangles identify the same cells used to draw page numbers and overflow arrows.
pub(super) fn draw(
    frame: &mut Frame,
    cols: usize,
    count: usize,
    active: usize,
    spacing: &TextSpacing,
) -> Vec<(usize, Rect)> {
    if count < 2 {
        return Vec::new();
    }
    // Keep at least half the caption available for project names.
    let budget = cols / 2;
    let separator = spacing.columns(SHORTCUT_SEPARATOR);
    let width = |index: usize| index.saturating_add(1).ilog10() as usize + 1;
    let mut range = active..active + 1;
    let mut text_width = width(active);
    if text_width > budget {
        return Vec::new();
    }
    let indicators =
        |range: &Range<usize>| hidden_columns(range.start) + hidden_columns(count - range.end);
    loop {
        let left = range
            .start
            .checked_sub(1)
            .map(|left| (left..range.end, width(left)));
        let right = (range.end < count).then(|| (range.start..range.end + 1, width(range.end)));
        let Some((next, added)) = [left, right]
            .into_iter()
            .flatten()
            .find(|(next, added)| text_width + separator + added + indicators(next) <= budget)
        else {
            break;
        };
        range = next;
        text_width += separator + added;
    }
    let hidden_left = range.start;
    let hidden_right = count - range.end;
    let left_width = hidden_columns(hidden_left);
    let right_width = hidden_columns(hidden_right);
    let left_width = if text_width + left_width <= budget {
        left_width
    } else {
        0
    };
    let right_width = if text_width + left_width + right_width <= budget {
        right_width
    } else {
        0
    };
    let mut column = cols - text_width - left_width - right_width;
    let mut targets = Vec::new();
    let mut label = |index, text: &str, span: usize, color| {
        frame.label(0, column..column + span, text, color, spacing);
        let rect = Rect {
            x: column,
            y: 0,
            w: span,
            h: 1,
        };
        column += span;
        if let Some(index) = index {
            targets.push((index, rect));
        }
    };
    if left_width > 0 {
        label(
            Some(range.start - 1),
            &format!("{hidden_left}← "),
            left_width,
            MUTED,
        );
    }
    for index in range.clone() {
        if index != range.start {
            // A separator occupies its own slot and never receives a page target.
            label(None, SHORTCUT_SEPARATOR, separator, MUTED);
        }
        label(
            Some(index),
            &(index + 1).to_string(),
            width(index),
            if index == active { ACCENT } else { TEXT },
        );
    }
    if right_width > 0 {
        label(
            Some(range.end),
            &format!(" →{hidden_right}"),
            right_width,
            MUTED,
        );
    }
    targets
}
