//! Pane selection, corner hit testing and footer clearance regressions.

use super::*;

/// Stacked and side-by-side strokes have equal clearance, with less space on the title side.
#[test]
fn stacked_path_clearance_matches_side_by_side_outlines() {
    let upper = Rect {
        x: 0,
        y: 1,
        w: 26,
        h: 12,
    };
    let lower = Rect { y: 13, ..upper };
    let side = Rect {
        x: 27,
        h: 25,
        ..upper
    };
    let panes = [upper, lower, side];
    for (cw, lh, font_size) in [(9., 21., 16.), (9.5, 22., 16.), (13.5, 30.5, 25.)] {
        let metrics = Metrics {
            cw,
            lh,
            font_size,
            origin: point(px(14.), px(9.)),
        };
        let stroke = solid_stroke_width(font_size);
        let (upper_top, upper_bottom) = pane_border_offsets(upper, &panes, metrics);
        let (lower_top, lower_bottom) = pane_border_offsets(lower, &panes, metrics);
        let seam = lower.pixels(metrics).top();
        let bottom = seam - px(lh * 0.5) + stroke * 0.5 + upper_bottom;
        let top = seam + px(lh * 0.5) - stroke * 0.5 + lower_top;
        let side_gap =
            side.pixels(metrics).left() - upper.pixels(metrics).right() + px(cw) - stroke;
        assert!(f32::from(top - bottom - side_gap).abs() < 0.001);
        assert!(top - seam < seam - bottom);
        assert!(lower_bottom > px(0.));
        assert_eq!(
            pane_border_offsets(side, &panes, metrics),
            (px(0.), lower_bottom)
        );
        assert_eq!(upper_top, px(0.));
    }
}

/// Split panes preserve their outer bounds and smallest usable size when moved or resized.
#[test]
fn split_panes_fit_the_retained_grid() {
    let area = Rect {
        x: 0,
        y: 1,
        w: 60,
        h: 14,
    };
    for editor_first in [false, true] {
        let (manager, editor) = split_panes(area, false, editor_first, usize::MAX, None);
        assert_eq!((manager.w, editor.w), (30, 29));
        assert_eq!(manager.y, editor.y);
        let (first, last) = if editor_first {
            (editor, manager)
        } else {
            (manager, editor)
        };
        assert_eq!(first.x + first.w + 1, last.x);
        assert_eq!(last.x + last.w, area.w);

        let (manager, editor) = split_panes(area, true, editor_first, 28, Some(0));
        assert_eq!((manager.h, editor.h), (4, 10));
        assert_eq!(manager.x, editor.x);
        let (first, last) = if editor_first {
            (editor, manager)
        } else {
            (manager, editor)
        };
        assert_eq!(first.y + first.h, last.y);
        assert_eq!(last.y + last.h, area.y + area.h);

        let short = Rect { h: 7, ..area };
        let (manager, editor) = split_panes(short, true, editor_first, 28, Some(usize::MAX));
        assert_eq!((manager.h, editor.h), (4, 3));
        assert_eq!(
            (manager.y + manager.h).max(editor.y + editor.h),
            short.y + short.h
        );
    }
}

/// Resizing matches the visible title gap above the pane to the outline gap below it.
#[test]
fn footer_and_tabs_keep_centered_panes_across_window_heights() {
    for (line_height, font_size) in [(12., 9.), (20., 15.), (22., 16.), (22.4, 16.5), (48., 36.)] {
        for height in 480..=840 {
            let height = height as f32;
            let (rows, grid_origin_y, footer_top) = vertical_geometry(px(height), line_height);
            let metrics = Metrics {
                cw: 10.,
                lh: line_height,
                font_size,
                origin: point(px(14.), grid_origin_y),
            };
            let pane = Rect {
                x: 0,
                y: 1,
                w: 60,
                h: rows - 1 - FOOTER_ROWS,
            };
            let bounds = pane.pixels(metrics);
            let (_, bottom_offset) = pane_border_offsets(pane, &[pane], metrics);
            let tab_bottom = OUTER_INSET_Y + line_height;
            let footer_badge_top = f32::from(footer_top) + FOOTER_BADGE_INSET_Y;
            let grid_gap = f32::from(bounds.top()) - tab_bottom;
            let stroke_inset = (line_height - f32::from(solid_stroke_width(font_size))) * 0.5;
            let top_gap = grid_gap + (line_height - font_size) * 0.5;
            let bottom_gap =
                footer_badge_top - (f32::from(bounds.bottom() + bottom_offset) - stroke_inset);
            assert!(
                (top_gap - bottom_gap).abs() < 0.001,
                "height {height}, row height {line_height}: top gap {top_gap}, bottom gap {bottom_gap}"
            );
            assert!(grid_gap + 0.001 >= PANE_GAP_Y);
            assert!(grid_gap < PANE_GAP_Y + line_height * 0.5 + 0.001);
            let footer_bottom = f32::from(footer_top) + line_height;
            assert!(footer_bottom < height);
            assert!(
                (height - (footer_bottom - FOOTER_BADGE_INSET_Y) - OUTER_INSET_Y).abs() < 0.001
            );
        }
    }
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
