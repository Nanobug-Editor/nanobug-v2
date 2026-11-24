//! Project shortcuts and strip overflow regressions.

use super::*;

/// Both directional pairs map to the same project action, with exact modifiers.
#[test]
fn project_shortcuts_select_and_reorder_independently() {
    for (keys, expected) in [
        (["alt-h", "alt-k"], Some(ProjectShortcut::Select(-1))),
        (["alt-l", "alt-j"], Some(ProjectShortcut::Select(1))),
        (
            ["ctrl-alt-h", "ctrl-alt-k"],
            Some(ProjectShortcut::Move(-1)),
        ),
        (["ctrl-alt-l", "ctrl-alt-j"], Some(ProjectShortcut::Move(1))),
        (["alt-shift-h", "ctrl-l"], None),
        (["alt-home", "alt-end"], None),
    ] {
        for key in keys {
            assert_eq!(
                ProjectShortcut::resolve(&Keystroke::parse(key).unwrap()),
                expected
            );
        }
    }
    assert_eq!(
        ProjectShortcut::resolve(&Keystroke::parse("ctrl-alt-home").unwrap()),
        Some(ProjectShortcut::First)
    );
    assert_eq!(
        ProjectShortcut::resolve(&Keystroke::parse("ctrl-alt-end").unwrap()),
        Some(ProjectShortcut::Last)
    );
}

/// Reserve room for both indicators and keep the active name visible even when it must clip.
#[test]
fn tab_range_accounts_for_hidden_counts_on_both_sides() {
    let widths = [9; 18];
    assert_eq!(project_tabs(&widths, 61, 0, 0).visible, 0..4);
    assert_eq!(project_tabs(&widths, 62, 0, 4).visible, 1..5);
    assert_eq!(project_tabs(&widths, 61, 1, 17).visible, 14..18);
    assert_eq!(project_tabs(&[9, 100, 9], 60, 0, 1).visible, 1..2);
    assert_eq!(project_tabs(&widths, 400, 14, 17).visible, 0..18);
}

/// Page hits cover the blanks beside right-aligned numbers and leave the pipe between them free.
#[test]
fn pages_use_the_right_side_of_the_caption() {
    let spacing = TextSpacing::new(
        std::sync::Arc::new(gpui::NoopTextSystem::new()),
        gpui::FontId(0),
        16.,
        9.,
    );
    let cols = 40;
    let mut frame = Frame::new(cols, 1);
    let (project_cols, targets) = pages::draw(&mut frame, cols, 1, 0, &spacing);
    assert_eq!(project_cols, cols);
    assert!(targets.is_empty());

    let page_slots = |count, active| {
        let (project_cols, targets) =
            pages::draw(&mut Frame::new(cols, 1), cols, count, active, &spacing);
        let ranges = targets
            .into_iter()
            .map(|(index, rect)| (index, rect.x..rect.x + rect.w))
            .collect::<Vec<_>>();
        (project_cols, ranges)
    };
    let (project_cols, ranges) = page_slots(3, 1);
    assert_eq!(project_cols, 29);
    assert_eq!(ranges, [(0, 30..33), (1, 34..37), (2, 38..41)]);

    let (_, overflow) = page_slots(90, 45);
    assert!(overflow[0].1.start >= cols / 2);
    assert!(overflow[0].0 < 45 && overflow.last().unwrap().0 > 45);
    assert!(overflow.iter().any(|(index, _)| *index == 45));
    assert_eq!(overflow.last().unwrap().1.end, cols);
    assert!(
        overflow
            .windows(2)
            .all(|pair| pair[0].1.end <= pair[1].1.start)
    );
}
