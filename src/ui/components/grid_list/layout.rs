use nanobug::animation::scroll::{GridScroll, ScrollInput};
use std::ops::Range;

/// A snapshot of equal-height rows visible at the current scroll position.
#[derive(Clone, Copy, Debug)]
pub(crate) struct GridList {
    count: usize,
    height: usize,
    position: f32,
}

impl GridList {
    /// Follow a selected item with the least movement from the previous first row.
    pub(crate) fn reveal(count: usize, height: usize, first: usize, selected: usize) -> Self {
        let selected = selected.min(count.saturating_sub(1));
        let first = first
            .min(selected)
            .max((selected + 1).saturating_sub(height));
        Self::scrolled(count, height, first as f32)
    }

    /// Measure visible rows at a caller-owned scroll animation's current position.
    pub(crate) fn scrolled(count: usize, height: usize, position: f32) -> Self {
        Self {
            count,
            height,
            position: position.clamp(0., count.saturating_sub(height) as f32),
        }
    }

    /// Retarget the shared scroll spring early enough for a neighboring row to slide into view.
    pub(crate) fn follow(
        scroll: &mut GridScroll,
        count: usize,
        height: usize,
        selected: usize,
        now: f64,
    ) {
        scroll.track_rows_with_margin(ScrollInput::Reveal, Some(selected), height, count, now, 1);
    }

    pub(crate) fn height(self) -> usize {
        self.height
    }
    pub(crate) fn at(self, position: f32) -> Self {
        Self::scrolled(self.count, self.height, position)
    }
    pub(crate) fn position(self) -> f32 {
        self.position
    }

    /// Include partially visible rows at either edge of the clipped viewport.
    pub(crate) fn range(self) -> Range<usize> {
        let first = self.position.floor() as usize;
        let end = if self.height == 0 {
            first
        } else {
            (self.position.ceil() as usize)
                .saturating_add(self.height)
                .min(self.count)
        };
        first..end
    }

    /// Convert viewport-relative grid rows to the item under the pointer.
    pub(crate) fn hit(self, row: f32) -> Option<usize> {
        if !(0.0..self.height as f32).contains(&row) {
            return None;
        }
        let index = (row + self.position).floor() as usize;
        (index < self.count).then_some(index)
    }

    /// Move a selection by a signed number of rows, stopping at either end.
    pub(crate) fn moved(selected: usize, delta: isize, count: usize) -> usize {
        selected
            .saturating_add_signed(delta)
            .min(count.saturating_sub(1))
    }
}

#[cfg(test)]
mod tests {
    use super::GridList;

    #[test]
    fn partial_rows_share_paint_and_click_geometry() {
        let list = GridList::scrolled(12, 3, 4.5);
        assert_eq!(list.range(), 4..8);
        assert_eq!(list.hit(0.49), Some(4));
        assert_eq!(list.hit(0.5), Some(5));
        assert_eq!(list.hit(2.99), Some(7));
        assert_eq!(list.hit(3.), None);
        assert_eq!(GridList::reveal(2, 5, 9, 8).range(), 0..2);
        assert_eq!(GridList::reveal(20, 3, 0, 15).range(), 13..16);
        assert!(GridList::scrolled(10, 0, 2.5).range().is_empty());
        assert_eq!(GridList::reveal(0, 10, 1, 2).hit(0.), None);
    }
}
