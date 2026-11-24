use super::GridPopup;
use crate::ui::grid_geometry::Rect;

/// Popup origins and sizes stay within the owner even when it starts below or beside another pane.
#[test]
fn fits_anchored_and_oversized_popups_in_the_same_viewport() {
    let viewport = Rect {
        x: 11,
        y: 8,
        w: 18,
        h: 7,
    };
    let popup = GridPopup::new(viewport);
    assert_eq!(
        popup.at(Rect {
            x: 99,
            y: 0,
            w: 6,
            h: 3
        }),
        Rect {
            x: 23,
            y: 8,
            w: 6,
            h: 3
        }
    );
    assert_eq!(
        popup.centered_on_row(30, 6, 3),
        Rect {
            x: 17,
            y: 12,
            w: 6,
            h: 3
        }
    );
    assert_eq!(popup.bottom_right(usize::MAX, usize::MAX), viewport);
    let hidden = Rect {
        w: 0,
        h: 0,
        ..viewport
    };
    assert_eq!(GridPopup::new(hidden).at(viewport), hidden);
}
