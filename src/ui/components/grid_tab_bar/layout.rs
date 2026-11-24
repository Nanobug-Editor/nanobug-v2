use std::ops::Range;

/// Which adjacent tab to admit first when there is space on both sides.
pub(crate) enum TabGrowth {
    Narrowest,
    LeftFirst,
    /// Preserve the previous first tab until it crowds the active one.
    Retain(usize),
}

pub(crate) enum TabAlign {
    Start,
    End,
    Fill,
}

/// Placement policies chosen by the enclosing tab strip.
pub(crate) struct TabOptions {
    pub(crate) gap: usize,
    /// Extra blank cells on each side that belong to a tab's pointer target.
    pub(crate) hit_padding: usize,
    /// Leave this much of the active caption visible before offering overflow arrows.
    pub(crate) minimum: usize,
    pub(crate) growth: TabGrowth,
    pub(crate) align: TabAlign,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TabKind {
    Tab(usize),
    Gap,
    Before { count: usize, target: usize },
    After { count: usize, target: usize },
}

/// A caption, separator or overflow arrow. Geometry is in the enclosing frame's columns.
pub(crate) struct TabSlot {
    pub(crate) kind: TabKind,
    pub(crate) columns: Range<usize>,
    pub(crate) hit: Range<usize>,
}

impl TabSlot {
    pub(crate) fn target(&self) -> Option<usize> {
        match self.kind {
            TabKind::Tab(index) => Some(index),
            TabKind::Before { target, .. } | TabKind::After { target, .. } => Some(target),
            TabKind::Gap => None,
        }
    }
}

/// One measured strip for drawing, hit testing and retaining the first visible tab.
pub(crate) struct GridTabBar {
    pub(crate) visible: Range<usize>,
    pub(crate) slots: Vec<TabSlot>,
}

/// Decimal count, arrow and one separating blank.
fn counter(count: usize) -> usize {
    count.checked_ilog10().map_or(0, |power| power as usize + 3)
}

impl GridTabBar {
    /// Measure only adjacent candidate widths; the active tab may be shortened in a narrow viewport.
    pub(crate) fn fit(
        count: usize,
        width: impl Fn(usize) -> usize,
        active: usize,
        columns: Range<usize>,
        options: TabOptions,
    ) -> Self {
        if count == 0 || columns.is_empty() {
            return Self {
                visible: 0..0,
                slots: Vec::new(),
            };
        }
        let budget = columns.len();
        let gap = options.gap;
        let active = active.min(count - 1);
        let mut start = match options.growth {
            TabGrowth::Retain(first) => first.min(active),
            _ => active,
        };
        let mut end = active + 1;
        let mut used = (start..end).map(&width).sum::<usize>() + (end - start - 1) * gap;
        let fits = |start: usize, end: usize, used: usize| {
            used + counter(start) + counter(count - end) <= budget
        };
        while start < active && !fits(start, end, used) {
            used -= width(start) + gap;
            start += 1;
        }
        loop {
            let left = start
                .checked_sub(1)
                .map(&width)
                .filter(|add| fits(start - 1, end, used + gap + add));
            let right = (end < count)
                .then(|| width(end))
                .filter(|add| fits(start, end + 1, used + gap + add));
            let take_left = match (left, right) {
                (None, None) => break,
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (Some(a), Some(b)) => match options.growth {
                    TabGrowth::Narrowest => a <= b,
                    TabGrowth::LeftFirst => true,
                    TabGrowth::Retain(_) => false,
                },
            };
            if take_left {
                start -= 1;
                used += gap + width(start);
            } else {
                used += gap + width(end);
                end += 1;
            }
        }

        let room = budget.saturating_sub(options.minimum.min(used));
        let left = counter(start);
        let left = if left <= room { left } else { 0 };
        let right = counter(count - end);
        let right = if right <= room - left { right } else { 0 };
        let mut x = match options.align {
            TabAlign::End => columns
                .end
                .saturating_sub(used.min(budget - left - right) + left + right),
            _ => columns.start,
        };
        let limit = columns.end - right;
        let mut slots = Vec::new();
        if left > 0 {
            let columns = x..x + left;
            slots.push(TabSlot {
                kind: TabKind::Before {
                    count: start,
                    target: start - 1,
                },
                hit: columns.clone(),
                columns,
            });
            x += left;
        }
        for index in start..end {
            if index != start {
                slots.push(TabSlot {
                    kind: TabKind::Gap,
                    columns: x..x + gap,
                    hit: x..x,
                });
                x += gap;
            }
            let next = (x + width(index)).min(limit);
            // Keep the separator's middle cell inert even when the caller requests extra hit padding.
            let inside = options.hit_padding.min(gap.saturating_sub(1) / 2);
            let before = if index > start {
                inside
            } else if left == 0 {
                options.hit_padding
            } else {
                0
            };
            let after = if index + 1 < end {
                inside
            } else if right == 0 {
                options.hit_padding
            } else {
                0
            };
            let begin = x.saturating_sub(before);
            let stop = next.saturating_add(after);
            slots.push(TabSlot {
                kind: TabKind::Tab(index),
                columns: x..next,
                hit: begin..stop,
            });
            x = next;
        }
        if right > 0 {
            if matches!(options.align, TabAlign::Fill) {
                x = limit;
            }
            let columns = x..x + right;
            slots.push(TabSlot {
                kind: TabKind::After {
                    count: count - end,
                    target: end,
                },
                hit: columns.clone(),
                columns,
            });
        }
        Self {
            visible: start..end,
            slots,
        }
    }
}
