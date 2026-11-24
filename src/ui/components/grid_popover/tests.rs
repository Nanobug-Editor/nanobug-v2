use super::GridPopover;
use gpui::{Bounds, point, px, size};

/// Sub-row scrolling keeps the selector aligned with its field even in a two-row viewport.
#[test]
fn short_lists_overlay_the_field_only_when_neither_side_has_a_whole_row() {
    let popover = GridPopover::new(10.0..12., 10.25..11.25).unwrap();
    let list = popover.rows(3, true);
    assert_eq!((list.top, list.height, list.inset), (10.25, 1, 0));
    assert!(GridPopover::new(10.0..12., 8.0..10.).is_none());
    let partial = GridPopover::new(10.0..20., 9.5..10.5)
        .unwrap()
        .rows(2, true);
    assert_eq!((partial.top, partial.height, partial.inset), (10.5, 4, 1));
}

/// Editors need a complete caret row, and a wide popover is clipped to the owning editor.
#[test]
fn pixel_bounds_follow_the_visible_caret_and_fit_the_viewport() {
    let body = Bounds::new(point(px(21.), px(30.)), size(px(80.), px(100.)));
    assert!(GridPopover::at_caret(body, point(px(77.), px(115.)), px(20.)).is_none());
    let caret = point(px(89.), px(90.));
    let popover = GridPopover::at_caret(body, caret, px(20.)).unwrap();
    let above = popover.pixel_bounds(body, caret.x, size(px(120.), px(42.)));
    assert_eq!(
        (above.left(), above.right(), above.bottom()),
        (body.left(), body.right(), caret.y)
    );
    let below = popover.pixel_bounds(body, caret.x, size(px(5.), px(18.)));
    assert_eq!((below.left(), below.top()), (caret.x, caret.y + px(20.)));
}
