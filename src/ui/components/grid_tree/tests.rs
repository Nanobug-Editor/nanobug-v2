use super::*;
use gpui::{Bounds, point, px, size};

/// Children of each root arrive interleaved, as backend settings pages can.
fn forest(roots: TreeRoots, open: bool) -> GridTree {
    GridTree::from_parents(
        roots,
        [
            (None, open),
            (Some(0), true),
            (None, true),
            (Some(2), true),
            (Some(1), false),
            (Some(0), false),
            (Some(3), false),
        ],
    )
}

#[test]
fn guides_follow_visible_siblings_and_the_root_style() {
    let separate = forest(TreeRoots::Separate, true);
    let connected = forest(TreeRoots::Connected, true);
    let ids = |tree: &GridTree| {
        tree.visible_rows(0, tree.len())
            .map(|(_, row)| row.id())
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&separate), [0, 1, 4, 5, 2, 3, 6]);
    assert_eq!(ids(&forest(TreeRoots::Separate, false)), [0, 2, 3, 6]);

    let guides = |tree: &GridTree, index, limit| {
        let (_, row) = tree.visible_rows(index, 1).next().unwrap();
        let mut lines = Vec::new();
        row.guides(row.control_column().min(limit), |col, continues, arm| {
            lines.push((col, continues, arm))
        });
        lines.sort_by_key(|line| line.0);
        (row.control_column(), lines)
    };
    assert_eq!(guides(&separate, 0, 20), (1, vec![]));
    assert_eq!(guides(&connected, 0, 20), (3, vec![(1, true, Some(3))]));
    assert_eq!(
        guides(&separate, 2, 20),
        (5, vec![(1, true, None), (3, false, Some(5))])
    );
    assert_eq!(guides(&connected, 2, 3), (7, vec![(1, true, None)]));
}

/// The partially visible last row shares its ID with keyboard navigation and pointer hits.
#[test]
fn fractional_hits_respect_viewport_edges_and_visible_ids() {
    let tree = forest(TreeRoots::Separate, true);
    let bounds = Bounds::new(point(px(20.), px(40.)), size(px(80.), px(20.)));
    let hit = |x, y| {
        tree.hit(bounds, point(px(x), px(y)), 10., 1.5)
            .map(|hit| (hit.id, hit.aligned_first))
    };
    assert_eq!(
        tree.visible_rows(1, 2)
            .map(|(y, row)| (y, row.id()))
            .collect::<Vec<_>>(),
        [(0, 1), (1, 4), (2, 5)]
    );
    assert_eq!(hit(20., 40.), Some((1, 1)));
    assert_eq!(hit(20., 44.9), Some((1, 1)));
    assert_eq!(hit(20., 45.), Some((4, 2)));
    assert_eq!(hit(99., 59.9), Some((5, 2)));
    for (x, y) in [(19., 40.), (100., 40.), (20., 39.), (20., 60.)] {
        assert_eq!(hit(x, y), None);
    }
    assert_eq!(tree.offset(tree.position(4).unwrap(), 1), Some(5));
    assert_eq!(tree.offset(1, isize::MAX), Some(6));
    assert_eq!(tree.visible_rows(0, 0).count(), 0);
    assert!(tree.hit(bounds, point(px(20.), px(50.)), 10., 6.).is_none());
}
