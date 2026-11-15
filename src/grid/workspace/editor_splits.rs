//! The split tree and grid geometry of one Tabpage's editor area.

use crate::grid::render::Rect;

/// An editor window's stable identity within a project.
pub(super) type PaneId = usize;

/// Preferred pane size with room for the border, gutter and visible document text.
const MIN_WIDTH: usize = 12;
const MIN_HEIGHT: usize = 4;
const GAP: usize = 1;

/// Dimension divided between sibling editor panes.
#[derive(Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub(super) enum SplitAxis {
    /// Side-by-side panes, dividing columns.
    Columns,
    /// Stacked panes, dividing rows.
    Rows,
}

impl SplitAxis {
    /// Read the extent of `area` along this axis.
    fn length(self, area: Rect) -> usize {
        match self {
            Self::Columns => area.w,
            Self::Rows => area.h,
        }
    }

    /// Minimum extent for rendering a pane with a usable body.
    fn minimum(self) -> usize {
        match self {
            Self::Columns => MIN_WIDTH,
            Self::Rows => MIN_HEIGHT,
        }
    }

    /// Split `area` into ordered siblings with `first` cells before the separator.
    fn divide(self, area: Rect, first: usize) -> [Rect; 2] {
        match self {
            Self::Columns => [
                Rect { w: first, ..area },
                Rect {
                    x: area.x + first + GAP,
                    w: area.w - first - GAP,
                    ..area
                },
            ],
            Self::Rows => [
                Rect { h: first, ..area },
                Rect {
                    y: area.y + first + GAP,
                    h: area.h - first - GAP,
                    ..area
                },
            ],
        }
    }
}

/// A leaf editor or two neighboring subtrees within this Tabpage.
#[derive(Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
enum Node {
    Pane(PaneId),
    Split {
        axis: SplitAxis,
        children: [Box<Node>; 2],
        /// Explicit first-child extent set by window resize commands.
        first_size: Option<usize>,
        /// Relative extents used until the split is explicitly resized.
        weights: [usize; 2],
    },
}

impl Node {
    /// Whether this subtree includes editor `id`.
    fn contains(&self, id: PaneId) -> bool {
        match self {
            Self::Pane(pane) => *pane == id,
            Self::Split { children, .. } => children.iter().any(|child| child.contains(id)),
        }
    }

    /// Check the sizes read from a Session before using them to divide the window.
    fn has_valid_weights(&self) -> bool {
        match self {
            Self::Pane(_) => true,
            Self::Split {
                children, weights, ..
            } => *weights != [0, 0] && children.iter().all(|child| child.has_valid_weights()),
        }
    }

    /// Apply `visit` to each leaf in top/left to bottom/right tree order.
    fn visit(&self, visit: &mut impl FnMut(PaneId)) {
        match self {
            Self::Pane(id) => visit(*id),
            Self::Split { children, .. } => {
                for child in children {
                    child.visit(visit);
                }
            }
        }
    }

    /// Return the outermost pane on the given side of this subtree.
    fn edge(&self, side: usize) -> PaneId {
        match self {
            Self::Pane(id) => *id,
            Self::Split { children, .. } => children[side].edge(side),
        }
    }

    /// Replace leaf `target` with a split containing `new_id`.
    fn insert(&mut self, target: PaneId, new_id: PaneId, axis: SplitAxis, new_first: bool) {
        match self {
            Self::Pane(id) => {
                let mut children = [Box::new(Self::Pane(*id)), Box::new(Self::Pane(new_id))];
                if new_first {
                    children.swap(0, 1);
                }
                *self = Self::Split {
                    axis,
                    children,
                    first_size: None,
                    weights: [1, 1],
                };
            }
            Self::Split { children, .. } => {
                let side = usize::from(!children[0].contains(target));
                children[side].insert(target, new_id, axis, new_first);
            }
        }
    }

    /// Remove `target`, promoting its sibling and returning a nearby editor to focus.
    fn remove(&mut self, target: PaneId) -> Option<PaneId> {
        let Self::Split { children, .. } = self else {
            return None;
        };
        let side = usize::from(!children[0].contains(target));
        if matches!(*children[side], Self::Pane(id) if id == target) {
            let other = 1 - side;
            let promoted = std::mem::replace(&mut *children[other], Self::Pane(target));
            let next = promoted.edge(side);
            *self = promoted;
            Some(next)
        } else {
            children[side].remove(target)
        }
    }

    /// Number of leaf extents and internal gaps measured along `dimension`.
    fn units(&self, dimension: SplitAxis) -> usize {
        match self {
            Self::Pane(_) => 1,
            Self::Split { axis, children, .. } => {
                let [a, b] = children.each_ref().map(|child| child.units(dimension));
                if *axis == dimension { a + b } else { a.max(b) }
            }
        }
    }

    /// First-child limits that reserve useful space for both sides of a split.
    fn limits(axis: SplitAxis, children: &[Box<Node>; 2], length: usize) -> Option<(usize, usize)> {
        let available = length.checked_sub(GAP)?;
        let minimum = axis.minimum();
        if available < 2 * minimum {
            return None;
        }
        let [first, second] = children.each_ref().map(|child| {
            let units = child.units(axis);
            units * minimum + (units - 1) * GAP
        });
        if first + second <= available {
            Some((first, available - second))
        } else {
            Some((minimum, available - minimum))
        }
    }

    /// Resolve this split's first-child extent to its allowed cell range.
    fn preferred_first(
        length: usize,
        first_size: Option<usize>,
        weights: [usize; 2],
        (minimum, maximum): (usize, usize),
    ) -> usize {
        let preferred = first_size.unwrap_or_else(|| {
            let [first, second] = weights.map(|weight| weight as u128);
            (((length as u128 + GAP as u128) * first / (first + second)) as usize)
                .saturating_sub(GAP)
        });
        preferred.clamp(minimum, maximum)
    }

    /// Append visible leaf rectangles. If neither sibling can fit, give the area to the active side.
    fn place(&self, area: Rect, active: PaneId, placed: &mut Vec<(PaneId, Rect)>) {
        match self {
            Self::Pane(id) => placed.push((*id, area)),
            Self::Split {
                axis,
                children,
                first_size,
                weights,
            } => {
                let length = axis.length(area);
                if let Some(limits) = Self::limits(*axis, children, length) {
                    let first = Self::preferred_first(length, *first_size, *weights, limits);
                    for (child, area) in children.iter().zip(axis.divide(area, first)) {
                        child.place(area, active, placed);
                    }
                } else {
                    let side = usize::from(children[1].contains(active));
                    children[side].place(area, active, placed);
                }
            }
        }
    }

    /// Resize the closest ancestor of `target` that divides `dimension` and can move.
    fn resize(&mut self, area: Rect, target: PaneId, dimension: SplitAxis, delta: isize) -> bool {
        let Self::Split {
            axis,
            children,
            first_size,
            weights,
        } = self
        else {
            return false;
        };
        let side = usize::from(!children[0].contains(target));
        let length = axis.length(area);
        let Some(limits) = Self::limits(*axis, children, length) else {
            return children[side].resize(area, target, dimension, delta);
        };
        let first = Self::preferred_first(length, *first_size, *weights, limits);
        let child_area = axis.divide(area, first)[side];
        if children[side].resize(child_area, target, dimension, delta) {
            return true;
        }
        if *axis != dimension {
            return false;
        }
        let adjustment = if side == 0 {
            delta
        } else {
            delta.saturating_neg()
        };
        let next = first
            .saturating_add_signed(adjustment)
            .clamp(limits.0, limits.1);
        if first == next {
            return false;
        }
        *first_size = Some(next);
        true
    }

    /// Restore automatic sizing and weight each sibling by its leaf extents along the split axis.
    fn equalize(&mut self) -> bool {
        let Self::Split {
            axis,
            children,
            first_size,
            weights,
        } = self
        else {
            return false;
        };
        let next = children.each_ref().map(|child| child.units(*axis));
        let mut changed = first_size.take().is_some() || next != *weights;
        *weights = next;
        for child in children {
            changed |= child.equalize();
        }
        changed
    }
}

/// Independent split geometry and focus of the editor area in one Tabpage.
#[derive(Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub(super) struct EditorSplits {
    root: Node,
    active: PaneId,
    previous: Option<PaneId>,
}

impl Default for EditorSplits {
    fn default() -> Self {
        Self::new(0)
    }
}

impl EditorSplits {
    /// Verify the saved focus and split weights before restoring pane geometry.
    pub(super) fn validate_session(&self) -> bool {
        self.root.contains(self.active)
            && self
                .previous
                .is_none_or(|id| id != self.active && self.root.contains(id))
            && self.root.has_valid_weights()
    }

    /// Create an unsplit area containing editor `root_id`.
    pub(super) fn new(root_id: PaneId) -> Self {
        Self {
            root: Node::Pane(root_id),
            active: root_id,
            previous: None,
        }
    }

    /// Identity of the editor that owns the page's editing input.
    pub(super) fn active(&self) -> PaneId {
        self.active
    }

    /// Last editor that owned the page's editing input and still exists.
    pub(super) fn previous(&self) -> Option<PaneId> {
        self.previous
    }

    /// Editor identities in tree order, including leaves currently clipped by the viewport.
    pub(super) fn ids(&self) -> Vec<PaneId> {
        let mut ids = Vec::new();
        self.root.visit(&mut |id| ids.push(id));
        ids
    }

    /// Change the editing focus to `id` if it belongs to the page.
    pub(super) fn focus(&mut self, id: PaneId) -> bool {
        if id == self.active || !self.root.contains(id) {
            return false;
        }
        self.previous = Some(self.active);
        self.active = id;
        true
    }

    /// Divide the active pane within `area` along `axis` and focus the new pane.
    /// `new_first` chooses the left/top side for `new_id`.
    pub(super) fn split(
        &mut self,
        new_id: PaneId,
        axis: SplitAxis,
        new_first: bool,
        area: Rect,
    ) -> bool {
        let Some(current) = self.rect(self.active, area) else {
            return false;
        };
        if axis.length(current) < 2 * axis.minimum() + GAP || self.root.contains(new_id) {
            return false;
        }
        self.root.insert(self.active, new_id, axis, new_first);
        self.focus(new_id)
    }

    /// Close the active leaf and focus the promoted sibling if it was not the only editor.
    pub(super) fn close_active(&mut self) -> bool {
        let Some(next) = self.root.remove(self.active) else {
            return false;
        };
        self.active = next;
        self.previous = self
            .previous
            .filter(|id| *id != next && self.root.contains(*id));
        true
    }

    /// Remove all editor panes except the active one and clear the previous focus.
    pub(super) fn retain_active(&mut self) {
        self.root = Node::Pane(self.active);
        self.previous = None;
    }

    /// Calculate the nonoverlapping visible pane rectangles within the enclosing editor `area`.
    pub(super) fn rects(&self, area: Rect) -> Vec<(PaneId, Rect)> {
        let mut placed = Vec::new();
        if area.w != 0 && area.h != 0 {
            self.root.place(area, self.active, &mut placed);
        }
        placed
    }

    /// Visible rectangle for editor `id` within the enclosing editor `area`.
    pub(super) fn rect(&self, id: PaneId, area: Rect) -> Option<Rect> {
        self.rects(area)
            .into_iter()
            .find_map(|(pane, rect)| (pane == id).then_some(rect))
    }

    /// Closest visible editor in the cardinal direction `(dx, dy)`, sharing the cross-axis span.
    pub(super) fn neighbor(&self, area: Rect, dx: isize, dy: isize) -> Option<PaneId> {
        let rectangles = self.rects(area);
        let current = rectangles.iter().find(|(id, _)| *id == self.active)?.1;
        let candidate = |(id, rect): (PaneId, Rect)| {
            let (gap, first, second) = match (dx.signum(), dy.signum()) {
                (-1, 0) => (
                    current.x.checked_sub(rect.x + rect.w),
                    (current.y, current.h),
                    (rect.y, rect.h),
                ),
                (1, 0) => (
                    rect.x.checked_sub(current.x + current.w),
                    (current.y, current.h),
                    (rect.y, rect.h),
                ),
                (0, -1) => (
                    current.y.checked_sub(rect.y + rect.h),
                    (current.x, current.w),
                    (rect.x, rect.w),
                ),
                (0, 1) => (
                    rect.y.checked_sub(current.y + current.h),
                    (current.x, current.w),
                    (rect.x, rect.w),
                ),
                _ => return None,
            };
            if first.0 >= second.0 + second.1 || second.0 >= first.0 + first.1 {
                return None;
            }
            let center_distance = (2 * first.0 + first.1).abs_diff(2 * second.0 + second.1);
            Some((gap?, center_distance, id))
        };
        rectangles
            .into_iter()
            .filter_map(candidate)
            .min()
            .map(|(_, _, id)| id)
    }

    /// Grow the active editor by `delta` cells along `axis`. Negative values shrink it.
    pub(super) fn resize(&mut self, area: Rect, axis: SplitAxis, delta: isize) -> bool {
        self.root.resize(area, self.active, axis, delta)
    }

    /// Restore automatic sizing for all splits and return whether a size preference changed.
    pub(super) fn equalize(&mut self) -> bool {
        self.root.equalize()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect {
        x: 28,
        y: 1,
        w: 63,
        h: 24,
    };

    #[test]
    fn mixed_splits_restore_sibling_and_preserve_editor_ids() {
        let mut splits = EditorSplits::new(7);
        assert!(splits.split(25, SplitAxis::Columns, false, AREA));
        assert!(splits.split(42, SplitAxis::Rows, true, AREA));
        assert_eq!(splits.ids(), [7, 42, 25]);
        assert!(splits.rect(42, AREA).unwrap().y < splits.rect(25, AREA).unwrap().y);
        assert_eq!(splits.active(), 42);
        assert_eq!(splits.previous(), Some(25));
        assert_eq!(splits.neighbor(AREA, -1, 0), Some(7));
        assert_eq!(splits.neighbor(AREA, 0, 1), Some(25));

        assert!(splits.close_active());
        assert_eq!(splits.active(), 25);
        assert_eq!(splits.ids(), [7, 25]);
        assert_eq!(splits.rect(25, AREA).unwrap().h, AREA.h);
        splits.retain_active();
        assert_eq!(splits.ids(), [25]);
        assert!(splits.rect(25, AREA).unwrap() == AREA);
        assert!(!splits.close_active());
    }

    #[test]
    fn resize_and_equalize_reuse_the_split() {
        let mut splits = EditorSplits::default();
        assert!(splits.split(1, SplitAxis::Columns, false, AREA));
        let start = splits.rect(1, AREA).unwrap().w;
        assert!(splits.resize(AREA, SplitAxis::Columns, 3));
        assert_eq!(splits.rect(1, AREA).unwrap().w, start + 3);
        assert!(splits.equalize());
        assert_eq!(splits.rect(1, AREA).unwrap().w, start);
    }

    #[test]
    fn narrow_viewport_preserves_topology_and_keeps_the_focused_pane_visible() {
        let mut splits = EditorSplits::default();
        let narrow = Rect {
            w: MIN_WIDTH,
            ..AREA
        };
        assert!(!splits.split(1, SplitAxis::Columns, true, narrow));
        assert!(splits.split(1, SplitAxis::Columns, true, AREA));
        assert!(splits.rect(1, narrow).unwrap() == narrow);
        assert_eq!(splits.rects(narrow).len(), 1);
        assert!(splits.focus(0));
        assert!(splits.rect(0, narrow).unwrap() == narrow);
        assert_eq!(splits.rects(AREA).len(), 2);
    }
}
