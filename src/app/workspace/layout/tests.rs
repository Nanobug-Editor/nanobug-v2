//! Pane selection, border hit testing and popup geometry regressions.

use super::*;
use gpui::size;

/// The close label mirrors the title inset, and the border hover excludes the body.
#[test]
fn close_button_uses_the_border_cell_and_pixel_tolerance() {
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
    assert_eq!(button.center().x, metrics.at(12.5, 2.5).x);
    assert!(!button.contains(&metrics.at(14.5, 2.5)));
    for (x, y) in [(12.5, 2.5), (9., 3.1), (16.2, 6.), (10., 9.5)] {
        assert!(near_border(area, metrics, metrics.at(x, y)));
    }
    for (x, y) in [(10., 5.), (16.5, 6.), (3.5, 6.)] {
        assert!(!near_border(area, metrics, metrics.at(x, y)));
    }
    let hidden = Rect { w: 0, ..area };
    assert!(hidden.close_button().is_none());
    assert!(!near_border(hidden, metrics, metrics.at(4., 2.)));
}

/// The visible character edges are equally inset despite unequal cell dimensions.
#[test]
fn help_outline_is_even_around_the_character_border() {
    let metrics = Metrics {
        cw: 10.,
        lh: 21.,
        font_size: 16.,
        origin: point(px(0.), px(0.)),
    };
    let grid = Bounds::new(point(px(100.), px(50.)), size(px(640.), px(462.)));
    let outline = help_outline(grid, metrics);
    assert_eq!(
        grid.left() + px(metrics.cw / 2.) - outline.left(),
        px(HELP_BORDER_GAP)
    );
    assert_eq!(
        outline.right() - (grid.right() - px(metrics.cw / 2.)),
        px(HELP_BORDER_GAP)
    );
    assert_eq!(
        grid.top() + px(metrics.lh / 2.) - outline.top(),
        px(HELP_BORDER_GAP)
    );
    assert_eq!(
        outline.bottom() - (grid.bottom() - px(metrics.lh / 2.)),
        px(HELP_BORDER_GAP)
    );
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
