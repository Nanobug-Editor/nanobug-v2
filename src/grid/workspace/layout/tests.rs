//! Pane selection and close button hit testing regressions.

use super::*;

/// The label fits on a narrow pane and activates only inside its small top-right hover region.
#[test]
fn close_button_uses_the_border_cells_and_pixel_tolerance() {
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
    let button = area.close_button().unwrap().pixels(metrics);
    assert_eq!(button.origin, metrics.at(7., 2.));
    assert_eq!(button.center().x, metrics.at(10.5, 2.5).x);
    assert!(button.contains(&metrics.at(8.5, 2.5)));
    assert!(button.contains(&metrics.at(12.5, 2.5)));
    assert!(!button.contains(&metrics.at(14.5, 2.5)));
    for (x, y) in [(12.5, 2.5), (9., 3.1), (6.8, 2.5), (14.2, 2.5)] {
        assert!(near_close_button(area, metrics, metrics.at(x, y)));
    }
    for (x, y) in [(5., 2.5), (15.5, 2.5), (16.2, 6.), (10., 9.5), (10., 5.)] {
        assert!(!near_close_button(area, metrics, metrics.at(x, y)));
    }
    let hidden = Rect { w: 0, ..area };
    assert!(hidden.close_button().is_none());
    assert!(!near_close_button(hidden, metrics, metrics.at(4., 2.)));
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
