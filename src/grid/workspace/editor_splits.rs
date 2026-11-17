//! The split tree and grid geometry of one Tabpage's editor area.

use crate::grid::render::Rect;

/// An editor window's stable identity within a project.
pub(super) type PaneId = usize;

/// Preferred pane size with room for the border, gutter and visible document text.
const MIN_WIDTH: usize = 12;
const MIN_HEIGHT: usize = 4;
/// Grid columns reserved between side-by-side editor panes.
const COLUMN_GAP: usize = 1;

/// Dimension divided between sibling editor panes.
#[derive(Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub(super) enum SplitAxis {
    /// Side-by-side panes, dividing columns.
    Columns,
    /// Stacked panes, dividing rows.
    Rows,
}

impl SplitAxis {
    /// Return the cells reserved between siblings. Stacked panes share a row seam.
    pub(super) fn gap(self) -> usize {
        match self {
            Self::Columns => COLUMN_GAP,
            Self::Rows => 0,
        }
    }

    /// Read the extent of `area` along this axis.
    fn length(self, area: Rect) -> usize {
        match self {
            Self::Columns => area.w,
            Self::Rows => area.h,
        }
    }

    /// Divide `length` by the leading and trailing span `weights`, accounting for divider cells.
    fn weighted_first(self, length: usize, weights: [usize; 2]) -> usize {
        let [first, second] = weights.map(|weight| weight as u128);
        let gap = self.gap() as u128;
        ((((length as u128 + gap) * first / (first + second)).saturating_sub(gap)) as usize)
            .min(length)
    }

    /// Minimum extent for rendering a pane with a usable body.
    fn minimum(self) -> usize {
        match self {
            Self::Columns => MIN_WIDTH,
            Self::Rows => MIN_HEIGHT,
        }
    }

    /// Measure pointer cell `(x, y)` from the leading edge of `area` along this axis.
    fn offset(self, area: Rect, x: usize, y: usize) -> usize {
        match self {
            Self::Columns => x.saturating_sub(area.x),
            Self::Rows => y.saturating_sub(area.y),
        }
    }

    /// Locate the divider after `first` cells in `area`, using the lower pane's first row for a seam.
    fn divider(self, area: Rect, first: usize) -> Rect {
        match self {
            Self::Columns => Rect {
                x: area.x + first,
                w: COLUMN_GAP,
                ..area
            },
            Self::Rows => Rect {
                y: area.y + first,
                h: 1,
                ..area
            },
        }
    }

    /// Split `area` into ordered siblings with `first` cells before the separator.
    fn divide(self, area: Rect, first: usize) -> [Rect; 2] {
        match self {
            Self::Columns => [
                Rect { w: first, ..area },
                Rect {
                    x: area.x + first + COLUMN_GAP,
                    w: area.w - first - COLUMN_GAP,
                    ..area
                },
            ],
            Self::Rows => [
                Rect { h: first, ..area },
                Rect {
                    y: area.y + first,
                    h: area.h - first,
                    ..area
                },
            ],
        }
    }
}

/// A captured divider in a page's split tree, identified by its two sibling branches.
#[derive(Clone, Copy)]
pub(super) struct DividerHit {
    /// Axis along which the divider moves.
    pub(super) axis: SplitAxis,
    /// One pane from each branch in leading-to-trailing order.
    anchors: [PaneId; 2],
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

    /// Relabel leaf panes according to `mapping`, keeping sizes and tree geometry.
    fn relabel(&mut self, mapping: &[(PaneId, PaneId)]) {
        match self {
            Self::Pane(id) => {
                if let Some((_, destination)) = mapping.iter().find(|(source, _)| source == id) {
                    *id = *destination;
                }
            }
            Self::Split { children, .. } => {
                for child in children {
                    child.relabel(mapping);
                }
            }
        }
    }

    /// Find the split axis directly dividing leaf `pane` from its immediate sibling.
    fn parent_axis(&self, pane: PaneId) -> Option<SplitAxis> {
        match self {
            Self::Pane(_) => None,
            Self::Split { axis, children, .. } => {
                let child = &children[usize::from(!children[0].contains(pane))];
                match **child {
                    Self::Pane(_) => Some(*axis),
                    Self::Split { .. } => child.parent_axis(pane),
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

    /// Count pane spans along `dimension`, restricting the count to `visible` IDs if supplied.
    fn units(&self, dimension: SplitAxis, visible: Option<&[PaneId]>) -> usize {
        match self {
            Self::Pane(id) => usize::from(visible.is_none_or(|panes| panes.contains(id))),
            Self::Split { axis, children, .. } => {
                let [a, b] = children
                    .each_ref()
                    .map(|child| child.units(dimension, visible));
                if *axis == dimension { a + b } else { a.max(b) }
            }
        }
    }

    /// Minimum extent containing every leaf and divider along `axis`.
    fn minimum_extent(&self, axis: SplitAxis) -> usize {
        let units = self.units(axis, None);
        units * axis.minimum() + (units - 1) * axis.gap()
    }

    /// First-child limits that reserve useful space for both sides of a split.
    fn limits(axis: SplitAxis, children: &[Box<Node>; 2], length: usize) -> Option<(usize, usize)> {
        let gap = axis.gap();
        let available = length.checked_sub(gap)?;
        let minimum = axis.minimum();
        if available < 2 * minimum {
            return None;
        }
        let [first, second] = children.each_ref().map(|child| child.minimum_extent(axis));
        if first + second <= available {
            Some((first, available - second))
        } else {
            Some((minimum, available - minimum))
        }
    }

    /// Resolve this split's first-child extent to its allowed cell range.
    fn preferred_first(
        length: usize,
        axis: SplitAxis,
        first_size: Option<usize>,
        weights: [usize; 2],
        (minimum, maximum): (usize, usize),
    ) -> usize {
        let preferred = first_size.unwrap_or_else(|| axis.weighted_first(length, weights));
        preferred.clamp(minimum, maximum)
    }

    /// Visit visible editors and their dividers. If neither sibling fits, give `area` to the active side.
    fn place(
        &self,
        area: Rect,
        active: PaneId,
        pane: &mut impl FnMut(PaneId, Rect),
        divider: &mut impl FnMut(SplitAxis, Rect, &[Box<Node>; 2]),
    ) {
        match self {
            Self::Pane(id) => pane(*id, area),
            Self::Split {
                axis,
                children,
                first_size,
                weights,
            } => {
                let length = axis.length(area);
                if let Some(limits) = Self::limits(*axis, children, length) {
                    let first = Self::preferred_first(length, *axis, *first_size, *weights, limits);
                    divider(*axis, axis.divider(area, first), children);
                    for (child, area) in children.iter().zip(axis.divide(area, first)) {
                        child.place(area, active, pane, divider);
                    }
                } else {
                    let side = usize::from(children[1].contains(active));
                    children[side].place(area, active, pane, divider);
                }
            }
        }
    }

    /// Move `hit` toward pointer cell `(x, y)` within this subtree's visible `area`.
    fn move_divider(
        &mut self,
        area: Rect,
        active: PaneId,
        hit: DividerHit,
        x: usize,
        y: usize,
    ) -> bool {
        let Self::Split {
            axis,
            children,
            first_size,
            weights,
        } = self
        else {
            return false;
        };
        let side_of = |id| children.iter().position(|child| child.contains(id));
        let (Some(before), Some(after)) = (side_of(hit.anchors[0]), side_of(hit.anchors[1])) else {
            return false;
        };
        let length = axis.length(area);
        let Some(limits) = Self::limits(*axis, children, length) else {
            // A compacted split has no divider, but the visible branch can have its own.
            let visible = usize::from(children[1].contains(active));
            return before == visible
                && after == visible
                && children[visible].move_divider(area, active, hit, x, y);
        };
        let first = Self::preferred_first(length, *axis, *first_size, *weights, limits);
        if before == after {
            let child_area = axis.divide(area, first)[before];
            return children[before].move_divider(child_area, active, hit, x, y);
        }
        if before != 0 || *axis != hit.axis {
            return false;
        }
        let next = axis.offset(area, x, y).clamp(limits.0, limits.1);
        if next == first {
            return false;
        }
        *first_size = Some(next);
        true
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
        let child = usize::from(!children[0].contains(target));
        let length = axis.length(area);
        let Some(limits) = Self::limits(*axis, children, length) else {
            return children[child].resize(area, target, dimension, delta);
        };
        let first = Self::preferred_first(length, *axis, *first_size, *weights, limits);
        let child_area = axis.divide(area, first)[child];
        if children[child].resize(child_area, target, dimension, delta) {
            return true;
        }
        if *axis != dimension {
            return false;
        }
        let adjustment = if child == 0 {
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

    /// Reset sizing where both branches contain `visible` editors, weighted by their spans.
    fn equalize(&mut self, visible: &[PaneId], dimension: Option<SplitAxis>) -> bool {
        let Self::Split {
            axis,
            children,
            first_size,
            weights,
        } = self
        else {
            return false;
        };
        let next = children
            .each_ref()
            .map(|child| child.units(*axis, Some(visible)));
        let mut changed = false;
        if dimension.is_none_or(|dimension| dimension == *axis) && !next.contains(&0) {
            changed = first_size.take().is_some() || next != *weights;
            *weights = next;
        }
        for child in children {
            changed |= child.equalize(visible, dimension);
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
        if axis.length(current) < 2 * axis.minimum() + axis.gap() || self.root.contains(new_id) {
            return false;
        }
        self.root.insert(self.active, new_id, axis, new_first);
        self.focus(new_id)
    }

    /// Place a new pane at an outer edge while preserving the existing split tree.
    pub(super) fn split_edge(
        &mut self,
        new_id: PaneId,
        axis: SplitAxis,
        first: bool,
        area: Rect,
    ) -> bool {
        let reserve = self.root.minimum_extent(axis) + axis.minimum() + axis.gap();
        if axis.length(area) < reserve || self.root.contains(new_id) {
            return false;
        }
        let old = std::mem::replace(&mut self.root, Node::Pane(new_id));
        let mut children = [Box::new(Node::Pane(new_id)), Box::new(old)];
        if !first {
            children.swap(0, 1);
        }
        self.root = Node::Split {
            axis,
            children,
            first_size: None,
            weights: [1, 1],
        };
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

    /// Rotate panes in the active editor's row or column `count` positions within `area`.
    /// When `reverse` is true, rotate towards the leading edge.
    pub(super) fn rotate(&mut self, area: Rect, count: usize, reverse: bool) -> bool {
        let panes = self.aligned_panes(area);
        if panes.len() < 2 {
            return false;
        }
        let mut reordered = panes.clone();
        let amount = count % panes.len();
        if reverse {
            reordered.rotate_left(amount);
        } else {
            reordered.rotate_right(amount);
        }
        self.root
            .relabel(&panes.into_iter().zip(reordered).collect::<Vec<_>>());
        true
    }

    /// Exchange the active pane with its neighbor or one-based `number` within `area`.
    /// Returns the exchanged pane ID for the caller to focus.
    pub(super) fn exchange(&mut self, area: Rect, number: Option<usize>) -> Option<PaneId> {
        let panes = self.aligned_panes(area);
        let current = panes.iter().position(|&id| id == self.active)?;
        let target = number
            .map_or_else(
                || {
                    panes
                        .get(current + 1)
                        .or_else(|| current.checked_sub(1).and_then(|i| panes.get(i)))
                },
                |number| number.checked_sub(1).and_then(|index| panes.get(index)),
            )
            .copied()
            .filter(|&id| id != self.active)?;
        self.root
            .relabel(&[(self.active, target), (target, self.active)]);
        Some(target)
    }

    /// List panes sharing the active editor's extent across its parent's split axis in `area`.
    fn aligned_panes(&self, area: Rect) -> Vec<PaneId> {
        let Some(axis) = self.root.parent_axis(self.active) else {
            return Vec::new();
        };
        let panes = self.rects(area);
        let Some((_, active)) = panes.iter().find(|(id, _)| *id == self.active) else {
            return Vec::new();
        };
        let active = *active;
        panes
            .into_iter()
            .filter(|(_, rect)| match axis {
                SplitAxis::Rows => rect.x == active.x && rect.w == active.w,
                SplitAxis::Columns => rect.y == active.y && rect.h == active.h,
            })
            .map(|(id, _)| id)
            .collect()
    }

    /// Move the active pane to span the full `axis` of the enclosing area.
    /// Choose the leading edge when `first` is true.
    pub(super) fn move_to_edge(&mut self, axis: SplitAxis, first: bool) -> bool {
        if self.root.remove(self.active).is_none() {
            return false;
        }
        let pane = Node::Pane(self.active);
        let rest = std::mem::replace(&mut self.root, Node::Pane(self.active));
        let mut children = [Box::new(pane), Box::new(rest)];
        if !first {
            children.swap(0, 1);
        }
        self.root = Node::Split {
            axis,
            children,
            first_size: None,
            weights: [1, 1],
        };
        true
    }

    /// Calculate the nonoverlapping visible pane rectangles within the enclosing editor `area`.
    pub(super) fn rects(&self, area: Rect) -> Vec<(PaneId, Rect)> {
        let mut placed = Vec::new();
        self.visit_layout(area, |id, area| placed.push((id, area)), |_, _, _| {});
        placed
    }

    /// Editor divider locators within `area`, in parent-before-child order.
    pub(super) fn divider_rects(&self, area: Rect) -> Vec<(SplitAxis, Rect)> {
        let mut dividers = Vec::new();
        self.visit_layout(area, |_, _| {}, |axis, area, _| dividers.push((axis, area)));
        dividers
    }

    /// Identify the divider of `orientation` at grid cell `(x, y)` within `area`.
    pub(super) fn divider_at(
        &self,
        area: Rect,
        orientation: SplitAxis,
        x: usize,
        y: usize,
    ) -> Option<DividerHit> {
        let mut hit = None;
        self.visit_layout(
            area,
            |_, _| {},
            |axis, divider, branches| {
                if axis == orientation && divider.contains(x, y) {
                    hit = Some(DividerHit {
                        axis,
                        anchors: [branches[0].edge(1), branches[1].edge(0)],
                    });
                }
            },
        );
        hit
    }

    /// Place the captured `hit` at pointer cell `(x, y)` along its axis, inside `area`'s limits.
    pub(super) fn move_divider(&mut self, area: Rect, hit: DividerHit, x: usize, y: usize) -> bool {
        area.w != 0 && area.h != 0 && self.root.move_divider(area, self.active, hit, x, y)
    }

    /// Visit pane and divider geometry only when the editor group has space to render.
    fn visit_layout(
        &self,
        area: Rect,
        mut pane: impl FnMut(PaneId, Rect),
        mut divider: impl FnMut(SplitAxis, Rect, &[Box<Node>; 2]),
    ) {
        if area.w != 0 && area.h != 0 {
            self.root.place(area, self.active, &mut pane, &mut divider);
        }
    }

    /// Visible rectangle for editor `id` within the enclosing editor `area`.
    pub(super) fn rect(&self, id: PaneId, area: Rect) -> Option<Rect> {
        self.rects(area)
            .into_iter()
            .find_map(|(pane, rect)| (pane == id).then_some(rect))
    }

    /// Closest visible editor in the cardinal direction `(dx, dy)`, sharing the cross-axis span.
    pub(super) fn neighbor(&self, area: Rect, dx: isize, dy: isize) -> Option<PaneId> {
        self.neighbor_from(self.active, area, dx, dy)
    }

    /// Closest visible editor to `pane` in direction `(dx, dy)` within `area`.
    pub(super) fn neighbor_from(
        &self,
        pane: PaneId,
        area: Rect,
        dx: isize,
        dy: isize,
    ) -> Option<PaneId> {
        let rectangles = self.rects(area);
        let current = rectangles.iter().find(|(id, _)| *id == pane)?.1;
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

    /// Change the size of `pane` without focusing it or moving its caret.
    pub(super) fn resize_pane(
        &mut self,
        pane: PaneId,
        area: Rect,
        axis: SplitAxis,
        delta: isize,
    ) -> bool {
        self.root.resize(area, pane, axis, delta)
    }

    /// Rebalance splits among `visible` panes and return whether a size preference changed.
    pub(super) fn equalize(&mut self, visible: &[PaneId]) -> bool {
        self.equalize_axis(visible, None)
    }

    /// Equalize only dividers of the requested axis, preserving the other explicit sizes.
    pub(super) fn equalize_axis(
        &mut self,
        visible: &[PaneId],
        dimension: Option<SplitAxis>,
    ) -> bool {
        self.root.equalize(visible, dimension)
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

    /// Keep pane IDs and focus stable while rotating, exchanging and moving mixed layouts.
    #[test]
    fn reorder_split_panes_without_losing_neighbors() {
        let mut splits = EditorSplits::new(0);
        assert!(splits.split(1, SplitAxis::Rows, false, AREA));
        assert!(splits.split(2, SplitAxis::Rows, false, AREA));
        assert_eq!(splits.ids(), [0, 1, 2]);
        assert!(splits.rotate(AREA, 1, false));
        assert_eq!(splits.ids(), [2, 0, 1]);
        assert_eq!(splits.active(), 2);
        assert!(splits.rotate(AREA, 1, true));
        assert_eq!(splits.ids(), [0, 1, 2]);
        assert_eq!(splits.exchange(AREA, Some(1)), Some(0));
        assert_eq!(splits.ids(), [2, 1, 0]);
        assert!(splits.move_to_edge(SplitAxis::Columns, false));
        assert_eq!(splits.ids(), [1, 0, 2]);
        assert_eq!(splits.rect(2, AREA).unwrap().h, AREA.h);
        assert!(splits.focus(1));
        assert_eq!(splits.exchange(AREA, None), Some(0));
        assert_eq!(splits.ids(), [0, 1, 2]);
    }

    #[test]
    fn mixed_splits_restore_sibling_and_preserve_editor_ids() {
        let mut splits = EditorSplits::new(7);
        assert!(splits.split(25, SplitAxis::Columns, false, AREA));
        assert!(splits.split(42, SplitAxis::Rows, true, AREA));
        assert_eq!(splits.ids(), [7, 42, 25]);
        let upper = splits.rect(42, AREA).unwrap();
        let lower = splits.rect(25, AREA).unwrap();
        assert_eq!(upper.y + upper.h, lower.y);
        assert_eq!(upper.h + lower.h, AREA.h);
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
        assert!(splits.equalize(&[0, 1]));
        assert_eq!(splits.rect(1, AREA).unwrap().w, start);
    }

    /// Moving a divider keeps its original branches even when the pointer crosses another pane.
    #[test]
    fn divider_drag_targets_nested_branches_and_clamps_each_axis() {
        let mut splits = EditorSplits::default();
        assert!(splits.split(1, SplitAxis::Columns, false, AREA));
        assert!(splits.split(2, SplitAxis::Rows, true, AREA));
        let dividers = splits.divider_rects(AREA);
        assert_eq!(dividers.len(), 2);
        let column = dividers[0].1;
        let row = dividers[1].1;
        assert_eq!((column.w, column.h), (COLUMN_GAP, AREA.h));
        assert_eq!(
            (row.y, row.h, row.x),
            (AREA.y + 12, 1, column.x + COLUMN_GAP)
        );
        assert!(
            splits
                .divider_at(AREA, SplitAxis::Rows, row.x - 2, row.y)
                .is_none()
        );
        let column = splits
            .divider_at(AREA, SplitAxis::Columns, column.x, column.y)
            .unwrap();
        let row = splits
            .divider_at(AREA, SplitAxis::Rows, row.x, row.y)
            .unwrap();
        assert!(column.axis == SplitAxis::Columns && row.axis == SplitAxis::Rows);

        let left = splits.rect(0, AREA).unwrap();
        assert!(splits.move_divider(AREA, row, AREA.x, 0));
        assert!(splits.rect(0, AREA).unwrap() == left);
        assert_eq!(splits.rect(2, AREA).unwrap().h, MIN_HEIGHT);
        assert!(!splits.move_divider(AREA, row, AREA.x, 0));

        assert!(splits.move_divider(AREA, column, usize::MAX, AREA.y + AREA.h));
        assert_eq!(splits.rect(1, AREA).unwrap().w, MIN_WIDTH);
        assert_eq!(splits.rect(2, AREA).unwrap().h, MIN_HEIGHT);
        assert!(splits.move_divider(AREA, row, 0, AREA.y + 9));
        assert_eq!(splits.rect(2, AREA).unwrap().h, 9);
        assert_eq!(splits.active(), 2);

        let compact = Rect {
            w: MIN_WIDTH,
            ..AREA
        };
        assert_eq!(splits.divider_rects(compact).len(), 1);
        assert!(!splits.move_divider(compact, column, 0, 0));
        assert!(splits.move_divider(compact, row, 0, AREA.y + 10));
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
