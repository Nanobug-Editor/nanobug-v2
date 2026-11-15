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
    assert_eq!(visible_tabs(&widths, 61, 0, 0), 0..4);
    assert_eq!(visible_tabs(&widths, 62, 0, 4), 1..5);
    assert_eq!(visible_tabs(&widths, 61, 1, 17), 14..18);
    assert_eq!(visible_tabs(&[9, 100, 9], 60, 0, 1), 1..2);
    assert_eq!(visible_tabs(&widths, 400, 14, 17), 0..18);
    assert_eq!(hidden_columns(0), 0);
    assert_eq!(hidden_columns(9), 3);
    assert_eq!(hidden_columns(10), 4);
}

/// Page hit rectangles follow right-aligned numbers and overflow, with room for project names.
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
    assert!(pages::draw(&mut frame, cols, 1, 0, &spacing).is_empty());

    let page_slots = |count, active| {
        pages::draw(&mut Frame::new(cols, 1), cols, count, active, &spacing)
            .into_iter()
            .map(|(index, rect)| (index, rect.x..rect.x + rect.w))
            .collect::<Vec<_>>()
    };
    assert_eq!(page_slots(3, 1), [(0, 31..32), (1, 35..36), (2, 39..40)]);
    let overflow = page_slots(90, 45);
    assert!(overflow[0].1.start >= cols / 2);
    assert!(overflow[0].0 < 45 && overflow.last().unwrap().0 > 45);
    assert!(overflow.iter().any(|(index, _)| *index == 45));
    assert_eq!(overflow.last().unwrap().1.end, cols);
}
