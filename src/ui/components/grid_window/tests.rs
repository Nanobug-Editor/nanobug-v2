use super::GridWindow;
use crate::ui::grid_geometry::Rect;

/// A split's origin is preserved in every region, including the optional fixed rows.
#[test]
fn regions_follow_the_window_origin() {
    let bounds = Rect {
        x: 8,
        y: 4,
        w: 16,
        h: 12,
    };
    let window = GridWindow::new(bounds, 2, 3);
    assert_eq!(window.bounds(), bounds);
    assert_eq!(window.title(), Rect { h: 1, ..bounds });
    assert_eq!(
        window.header(),
        Rect {
            x: 9,
            y: 5,
            w: 14,
            h: 2
        }
    );
    assert_eq!(
        window.body(),
        Rect {
            x: 9,
            y: 7,
            w: 14,
            h: 5
        }
    );
    assert_eq!(
        window.footer(),
        Rect {
            x: 9,
            y: 12,
            w: 14,
            h: 3
        }
    );
    assert!(window.body().contains(22, 11));
    assert!(!window.body().contains(9, 12));
}

/// Hidden and undersized panes never overlap fixed rows or put a hit target outside the window.
#[test]
fn small_windows_clip_fixed_rows_before_exposing_content() {
    for w in 0..=4 {
        for h in 0..=6 {
            let bounds = Rect { x: 5, y: 7, w, h };
            let window = GridWindow::new(bounds, 2, usize::MAX);
            let body = window.body();
            let interior_height = h.saturating_sub(2);
            assert_eq!(window.header().h, 2.min(interior_height));
            assert_eq!(window.footer().h, interior_height - window.header().h);
            assert_eq!(body.h, 0);
            assert_eq!(
                window.title().contains(bounds.x, bounds.y),
                w >= 2 && h >= 2
            );
            for region in [window.title(), window.header(), body, window.footer()] {
                assert!(region.x >= bounds.x && region.x + region.w <= bounds.x + w);
                assert!(region.y >= bounds.y && region.y + region.h <= bounds.y + h);
            }
        }
    }
}
