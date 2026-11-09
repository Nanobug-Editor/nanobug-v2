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
