use crate::ui::components::grid_window::title_hit_columns;
use crate::ui::text::{TextSpacing, UNITS_PER_COLUMN};
use std::ops::Range;

/// Measured title tabs whose labels and adjacent blank cells share a pointer target.
pub(crate) struct TitleTabs<const N: usize> {
    /// Captions in stable tab order. Empty labels hide their slot.
    captions: [&'static str; N],
    /// Painted columns in the containing frame.
    slots: [Range<usize>; N],
    /// Pipe separator between visible captions, with optional whitespace.
    separator: &'static str,
}

impl<const N: usize> TitleTabs<N> {
    /// Fit `captions` into the label `columns` with `spacing`. Leave a blank cell
    /// outside each end of that range. A `compact` strip uses a bare pipe between tabs.
    pub(crate) fn new(
        columns: Range<usize>,
        captions: [&'static str; N],
        compact: bool,
        spacing: &TextSpacing,
    ) -> Self {
        let separator = if compact { "|" } else { " | " };
        let gap = spacing.columns(separator);
        let mut start = columns.start;
        let slots = captions.map(|caption| {
            let available = columns.end.saturating_sub(start) * UNITS_PER_COLUMN;
            let width = spacing
                .fitting_cells(caption, available)
                .last()
                .map_or(0, |cell| cell.end.div_ceil(UNITS_PER_COLUMN));
            let slot = start..start + width;
            if width > 0 {
                start = (slot.end + gap).min(columns.end);
            }
            slot
        });
        Self {
            captions,
            slots,
            separator,
        }
    }

    /// The text drawn between visible tabs.
    pub(crate) fn separator(&self) -> &'static str {
        self.separator
    }

    /// Yield visible captions with their original index and measured columns.
    pub(crate) fn visible(&self) -> impl Iterator<Item = (usize, &'static str, Range<usize>)> + '_ {
        self.captions
            .into_iter()
            .zip(&self.slots)
            .enumerate()
            .filter_map(|(index, (caption, columns))| {
                (!columns.is_empty()).then(|| (index, caption, columns.clone()))
            })
    }

    /// Pointer columns of the tab at `index`, preserving the pipe as a divider.
    pub(crate) fn hit_columns(&self, index: usize) -> Range<usize> {
        let slot = &self.slots[index];
        let mut hit = title_hit_columns(slot.clone());
        if self.separator == "|" && !slot.is_empty() {
            if self.slots[..index].iter().any(|slot| !slot.is_empty()) {
                hit.start = slot.start;
            }
            if self.slots[index + 1..].iter().any(|slot| !slot.is_empty()) {
                hit.end = slot.end;
            }
        }
        hit
    }

    /// Resolve `column` to its visible tab index, including the caption's padding.
    pub(crate) fn tab_at(&self, column: usize) -> Option<usize> {
        (0..N).find(|&index| self.hit_columns(index).contains(&column))
    }
}
