//! Visible preorder and native pointer hit testing for an already expanded tree.

use super::layout::{TreeRoots, TreeRow};
use gpui::{Bounds, Pixels, Point};

/// Visible nodes form a preorder; parent links here index rows rather than domain nodes.
pub(super) struct Node {
    pub(super) id: usize,
    pub(super) depth: usize,
    pub(super) parent: Option<usize>,
    pub(super) later_sibling: bool,
}

/// Shared tree geometry retained until the owner's hierarchy or expansion state changes.
pub(crate) struct GridTree {
    pub(super) roots: TreeRoots,
    pub(super) nodes: Vec<Node>,
}

/// Node under the pointer and the integer scroll offset that leaves it on the clicked row.
pub(crate) struct TreeHit {
    pub(crate) id: usize,
    pub(crate) aligned_first: usize,
}

impl GridTree {
    /// Index visible (node ID, depth) pairs in preorder. Roots have depth zero and every
    /// child immediately follows its parent or one of its preceding siblings' subtrees.
    pub(crate) fn from_preorder(
        roots: TreeRoots,
        rows: impl IntoIterator<Item = (usize, usize)>,
    ) -> Self {
        let rows = rows.into_iter();
        let mut nodes: Vec<Node> = Vec::with_capacity(rows.size_hint().0);
        let mut ancestors: Vec<usize> = Vec::new();
        for (id, depth) in rows {
            debug_assert!(depth <= ancestors.len());
            if let Some(&previous) = ancestors.get(depth) {
                nodes[previous].later_sibling = true;
            }
            ancestors.truncate(depth);
            let parent = ancestors.last().copied();
            ancestors.push(nodes.len());
            nodes.push(Node {
                id,
                depth,
                parent,
                later_sibling: false,
            });
        }
        Self { roots, nodes }
    }

    /// Flatten (parent ID, expanded) pairs in sibling insertion order. Node IDs are input
    /// indices and parents precede children, including when separate subtrees interleave.
    pub(crate) fn from_parents(
        roots: TreeRoots,
        nodes: impl IntoIterator<Item = (Option<usize>, bool)>,
    ) -> Self {
        let mut children: Vec<(bool, Vec<usize>)> = Vec::new();
        let mut top = Vec::new();
        for (id, (parent, open)) in nodes.into_iter().enumerate() {
            if let Some(parent) = parent {
                children[parent].1.push(id);
            } else {
                top.push(id);
            }
            children.push((open, Vec::new()));
        }
        let mut pending: Vec<_> = top.into_iter().rev().map(|id| (id, 0)).collect();
        let preorder = std::iter::from_fn(|| {
            let (id, depth) = pending.pop()?;
            let (open, descendants) = &children[id];
            if *open {
                pending.extend(descendants.iter().rev().map(|&child| (child, depth + 1)));
            }
            Some((id, depth))
        });
        Self::from_preorder(roots, preorder)
    }

    /// Number of rows contributing to the scroll range.
    pub(crate) fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Find a visible row by its owner's ID, for navigation, caret placement and scroll reveal.
    pub(crate) fn position(&self, id: usize) -> Option<usize> {
        self.nodes.iter().position(|node| node.id == id)
    }

    /// Move from a visible row by a signed offset, stopping at the first or last visible node.
    pub(crate) fn offset(&self, row: usize, delta: isize) -> Option<usize> {
        self.id_at(
            row.saturating_add_signed(delta)
                .min(self.len().saturating_sub(1)),
        )
    }

    /// Return the owner ID of a logical row.
    fn id_at(&self, index: usize) -> Option<usize> {
        self.nodes.get(index).map(|node| node.id)
    }

    /// Yield frame-relative rows, including the partial last row needed by smooth scrolling.
    pub(crate) fn visible_rows(
        &self,
        first: usize,
        height: usize,
    ) -> impl Iterator<Item = (usize, TreeRow<'_>)> {
        let end = first
            .saturating_add(height)
            .saturating_add(usize::from(height > 0))
            .min(self.len());
        (first..end)
            .enumerate()
            .map(|(y, index)| (y, TreeRow { tree: self, index }))
    }

    /// Hit-test the half-open pixel viewport using the current animated offset in rows.
    pub(crate) fn hit(
        &self,
        viewport: Bounds<Pixels>,
        point: Point<Pixels>,
        line_height: f32,
        scroll: f32,
    ) -> Option<TreeHit> {
        if point.x < viewport.left()
            || point.x >= viewport.right()
            || point.y < viewport.top()
            || point.y >= viewport.bottom()
        {
            return None;
        }
        let local = f32::from(point.y - viewport.top()) / line_height;
        let index = (local + scroll).floor() as usize;
        Some(TreeHit {
            id: self.id_at(index)?,
            aligned_first: index.saturating_sub(local.floor() as usize),
        })
    }
}
