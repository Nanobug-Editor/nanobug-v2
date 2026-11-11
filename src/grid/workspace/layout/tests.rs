//! Pane selection, corner hit testing and footer clearance regressions.

use super::*;

/// Whole pane rows leave clearance above the footer while it stays aligned to the outer inset.
#[test]
fn footer_keeps_clearance_across_window_heights() {
    for line_height in [20., 21., 22.] {
        for height in 480..=840 {
            let height = height as f32;
            let (rows, offset) = footer_geometry(px(height), line_height);
            let origin = OUTER_INSET_Y + TAB_GAP_Y;
            let footer = origin + (rows - FOOTER_ROWS) as f32 * line_height + f32::from(offset);
            let border = origin + ((rows - FOOTER_ROWS) as f32 - 0.5) * line_height;
            assert!(footer + FOOTER_BADGE_INSET_Y - border >= FOOTER_GAP_Y);
            let badge_bottom = footer + line_height - FOOTER_BADGE_INSET_Y;
            assert_eq!(height - badge_bottom, OUTER_INSET_Y);
        }
    }
    assert_eq!(footer_geometry(px(840.), 22.).0, 38);
}

/// Pane controls include a separate help target, while a popup has only its close target.
#[test]
fn corner_controls_use_adjacent_border_slots() {
    let metrics = Metrics {
        cw: 10.,
        lh: 20.,
        font_size: 16.,
        origin: point(px(14.), px(5.)),
    };
    let area = Rect {
        x: 4,
        y: 2,
        w: 12,
        h: 8,
    };
    let controls = area.corner_controls(true).unwrap();
    let help = controls.help.unwrap().pixels(metrics);
    let button = controls.close.pixels(metrics);
    assert_eq!(help.right(), button.left());
    assert_eq!(help.center(), metrics.at(9.5, 2.5));
    assert!(near_corner_controls(area, true, metrics, help.center()));
    assert!(area.corner_controls(false).unwrap().help.is_none());
    assert!(!near_corner_controls(area, false, metrics, help.center()));
    assert_eq!(button.origin, metrics.at(11., 2.));
    assert_eq!(button.center().x, metrics.at(12.5, 2.5).x);
    assert!(!button.contains(&metrics.at(10.8, 2.5)));
    assert!(button.contains(&metrics.at(12.5, 2.5)));
    assert!(!button.contains(&metrics.at(14.5, 2.5)));
    for (x, y) in [(12.5, 2.5), (12.5, 3.1), (10.8, 2.5), (14.2, 2.5)] {
        assert!(near_corner_controls(area, true, metrics, metrics.at(x, y)));
    }
    for (x, y) in [(7., 2.5), (15.5, 2.5), (16.2, 6.), (10., 9.5), (10., 5.)] {
        assert!(!near_corner_controls(area, true, metrics, metrics.at(x, y)));
    }
    let hidden = Rect { w: 0, ..area };
    assert!(hidden.corner_controls(true).is_none());
    assert!(!near_corner_controls(
        hidden,
        true,
        metrics,
        metrics.at(4., 2.)
    ));
}

/// A shortcut popup owns the selection while open and returns it to its originating pane.
#[test]
fn shortcut_help_is_the_only_selected_window() {
    let windows = [
        ShellWindow::Pane(Focus::Manager),
        ShellWindow::Pane(Focus::Editor),
        ShellWindow::ShortcutHelp,
    ];
    for (pane, pane_selection) in [
        (Focus::Manager, [true, false, false]),
        (Focus::Editor, [false, true, false]),
    ] {
        assert_eq!(
            [false, true, false]
                .map(|help_open| windows.map(|window| window.is_selected(pane, help_open))),
            [pane_selection, [false, false, true], pane_selection],
        );
    }
}
