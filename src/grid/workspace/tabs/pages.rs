//! Fit and draw the numbered pages beside the project tabs in the window caption.

use crate::ui::components::grid_tab_bar::{GridTabBar, TabAlign, TabGrowth, TabKind, TabOptions};
use crate::{
    grid::render::{Frame, Rect, SHORTCUT_SEPARATOR},
    ui::{
        text::TextSpacing,
        theme::{ACCENT, MUTED, TEXT},
    },
};

/// Minimum number of blank columns between project tabs and page numbers.
const PAGE_GAP: usize = 2;

/// Draw `count` one-based labels, highlighting `active`, at the right of a `cols`-column row.
/// Return the width available for projects and the click targets for numbers and overflow arrows.
pub(super) fn draw(
    frame: &mut Frame,
    cols: usize,
    count: usize,
    active: usize,
    spacing: &TextSpacing,
) -> (usize, Vec<(usize, Rect)>) {
    if count < 2 {
        return (cols, Vec::new());
    }
    // Keep at least half the caption available for project names.
    let budget = cols / 2;
    let separator = spacing.columns(SHORTCUT_SEPARATOR);
    let width = |index: usize| index.saturating_add(1).ilog10() as usize + 1;
    if width(active) > budget {
        return (cols, Vec::new());
    }
    let bar = GridTabBar::fit(
        count,
        width,
        active,
        cols - budget..cols,
        TabOptions {
            gap: separator,
            hit_padding: 1,
            minimum: width(active),
            growth: TabGrowth::LeftFirst,
            align: TabAlign::End,
        },
    );
    let project_cols = bar
        .slots
        .first()
        .map_or(cols, |slot| slot.columns.start.saturating_sub(PAGE_GAP));
    let mut targets = Vec::new();
    for slot in bar.slots {
        let (text, ink) = match slot.kind {
            TabKind::Gap => (SHORTCUT_SEPARATOR.to_owned(), MUTED),
            TabKind::Before { count, .. } => (format!("{count}← "), MUTED),
            TabKind::After { count, .. } => (format!(" →{count}"), MUTED),
            TabKind::Tab(index) => (
                (index + 1).to_string(),
                if index == active { ACCENT } else { TEXT },
            ),
        };
        if let Some(index) = slot.target() {
            targets.push((
                index,
                Rect {
                    x: slot.hit.start,
                    y: 0,
                    w: slot.hit.len(),
                    h: 1,
                },
            ));
        }
        frame.label(0, slot.columns, &text, ink, spacing);
    }
    (project_cols, targets)
}
