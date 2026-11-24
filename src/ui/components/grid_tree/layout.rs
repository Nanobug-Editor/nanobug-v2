//! Indentation and connector geometry for a single visible row.

use super::tree::GridTree;

/// Connect top-level entries to an implicit root, or show them as independent roots.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum TreeRoots {
    Connected,
    Separate,
}

/// A visible row borrows the same tree that supplies pointer and caret indices.
pub(crate) struct TreeRow<'a> {
    pub(super) tree: &'a GridTree,
    pub(super) index: usize,
}

impl TreeRow<'_> {
    /// Stable owner identifier for this visible row.
    pub(crate) fn id(&self) -> usize {
        self.tree.nodes[self.index].id
    }

    /// Column of the icon or disclosure arrow, leaving a one-cell leading gutter.
    pub(crate) fn control_column(&self) -> usize {
        1 + 2
            * (self.tree.nodes[self.index].depth
                + usize::from(self.tree.roots == TreeRoots::Connected))
    }

    /// Emit the visible connector columns and their optional arm ending before the control.
    /// A narrow viewport can move the control left; guides stop at its actual column.
    pub(crate) fn guides(&self, control: usize, mut paint: impl FnMut(usize, bool, Option<usize>)) {
        let mut current = Some(self.index);
        while let Some(index) = current {
            let node = &self.tree.nodes[index];
            let level = node.depth + usize::from(self.tree.roots == TreeRoots::Connected);
            if level == 0 {
                break;
            }
            let column = 1 + 2 * (level - 1);
            let arm = (index == self.index).then_some(control);
            if column < control && (arm.is_some() || node.later_sibling) {
                paint(column, node.later_sibling, arm);
            }
            current = node.parent;
        }
    }
}
