//! Floating value selectors share placement between painting, clipping, cursor and pointer input.

use super::{Editor, Kind, Settings, view};
use crate::ui::components::grid_list::GridList;
use crate::ui::components::grid_popover::GridPopover;
use crate::{
    grid::render::{Frame, GridLayer, Metrics, OutlineStyle, Rect},
    ui::{
        text::TextSpacing,
        theme::{ACCENT, SELECTION, TEXT},
    },
};
use gpui::{App, Bounds, Pixels, Point, px};

pub(super) struct SelectorLayout {
    count: usize,
    frame: Rect,
    pub(super) rows: Rect,
    /// Fractional row shared by the frame and its contents while the anchor is scrolling.
    pub(super) offset_y: f32,
}

impl SelectorLayout {
    /// Prefer opening below the field. Near the bottom edge use the side with more space.
    fn at(body: Rect, anchor: f32, count: usize) -> Option<Self> {
        if body.w == 0 || count == 0 {
            return None;
        }
        let popover = GridPopover::new(0.0..body.h as f32, anchor..anchor + 1.)?;
        let list = popover.rows(count, body.w >= 4);
        let (top, height, border) = (list.top, list.height, list.inset);
        let width = (body.w - view::value_column(body.w)).max(12.min(body.w));
        let frame = Rect {
            x: body.x + body.w - width,
            y: body.y + top.floor() as usize,
            w: width,
            h: height,
        };
        Some(Self {
            count,
            frame,
            rows: Rect {
                x: frame.x + border,
                y: frame.y + border,
                w: frame.w - 2 * border,
                h: frame.h - 2 * border,
            },
            offset_y: top.fract(),
        })
    }

    fn bounds(&self, rect: Rect, metrics: Metrics) -> Bounds<Pixels> {
        let mut bounds = rect.pixels(metrics);
        bounds.origin.y += px(self.offset_y * metrics.lh);
        bounds
    }

    pub(super) fn contains(&self, point: Point<Pixels>, metrics: Metrics) -> bool {
        self.bounds(self.frame, metrics).contains(&point)
    }

    /// Resolve the visible option under the pointer, accounting for both scrolling surfaces.
    pub(super) fn hit(&self, point: Point<Pixels>, metrics: Metrics, scroll: f32) -> Option<usize> {
        let clip = self.bounds(self.rows, metrics);
        if !clip.contains(&point) {
            return None;
        }
        self.list(scroll)
            .hit(f32::from(point.y - clip.top()) / metrics.lh)
    }

    fn list(&self, scroll: f32) -> GridList {
        GridList::scrolled(self.count, self.rows.h, scroll)
    }
}

impl Settings {
    pub(super) fn selector_layout(&self, body: Rect) -> Option<SelectorLayout> {
        if !matches!(&self.editor, Some(Editor::List { .. })) {
            return None;
        }
        let field = self
            .body_rows()
            .iter()
            .position(|r| *r == view::BodyRow::Field(self.row))?;
        SelectorLayout::at(
            body,
            field as f32 - self.form_scroll.position(),
            self.choices().len(),
        )
    }

    pub(super) fn refresh_selector(&self, body: Rect, spacing: &TextSpacing, cx: &mut App) {
        let Some(Editor::List { cursor, value }) = &self.editor else {
            return;
        };
        let Some(layout) = self.selector_layout(body) else {
            return;
        };
        let mut frame = Frame::new(layout.frame.w, layout.frame.h);
        frame.hide_theme_background();
        if layout.rows.h != layout.frame.h {
            frame.outline(
                Rect {
                    x: 0,
                    y: 0,
                    w: layout.frame.w,
                    h: layout.frame.h,
                },
                ACCENT,
                OutlineStyle::Solid,
                [],
            );
        }
        let mut items = Frame::new(layout.rows.w, layout.rows.h + 1);
        items.hide_theme_background();
        let multi = matches!(self.field().map(|f| &f.kind), Some(Kind::Multi(_)));
        let rows = layout.list(self.selector_scroll.position()).range();
        let first = rows.start;
        for (y, choice) in self.choices()[rows].iter().enumerate() {
            let selected = if multi {
                value
                    .as_array()
                    .is_some_and(|v| v.iter().any(|it| it == &choice.id))
            } else {
                value == &choice.id
            };
            let marker = match (multi, selected) {
                (true, true) => "[✓]",
                (true, false) => "[ ]",
                (false, true) => "●",
                (false, false) => "○",
            };
            if *cursor == first + y {
                items.tint(y, 0, layout.rows.w, SELECTION);
            }
            items.label(
                y,
                1..layout.rows.w,
                &format!("{marker} {}", choice.label),
                if selected { ACCENT } else { TEXT },
                spacing,
            );
        }
        self.panels[3].update(cx, |panel, cx| panel.set(frame, cx));
        self.panels[4].update(cx, |panel, cx| panel.set(items, cx));
    }

    /// The bounds cut a hole in underlying form rows. Only the options move inside that hole.
    pub(in crate::grid) fn selector_layers(
        &self,
        area: Rect,
        metrics: Metrics,
    ) -> Option<(Bounds<Pixels>, [GridLayer; 2])> {
        let layout = self.selector_layout(view::pane_bounds(area).1)?;
        let bounds = layout.bounds(layout.frame, metrics);
        Some((
            bounds,
            [
                GridLayer {
                    panel: self.panels[3].clone(),
                    bounds,
                    clip: None,
                },
                GridLayer::scrolled_bounds(
                    &self.panels[4],
                    layout.bounds(layout.rows, metrics),
                    self.selector_scroll.fraction(),
                    metrics.lh,
                ),
            ],
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selector_follows_fractional_anchor_and_flips_above_near_bottom() {
        let body = Rect {
            x: 16,
            y: 6,
            w: 50,
            h: 15,
        };
        let down = SelectorLayout::at(body, 2.75, 3).unwrap();
        assert_eq!((down.frame.y, down.offset_y, down.rows.h), (9, 0.75, 3));
        let up = SelectorLayout::at(body, 12.75, 20).unwrap();
        assert_eq!((up.frame.y, up.offset_y, up.rows.h), (6, 0.75, 10));
        let metrics = Metrics {
            cw: 8.,
            lh: 16.,
            font_size: 14.,
            origin: gpui::point(px(3.), px(6.)),
        };
        let x = (up.rows.x + 1) as f32;
        // Both motions affect item picking. The border consumes its click without choosing a row.
        let border = metrics.at(x, up.frame.y as f32 + up.offset_y + 0.5);
        assert!(up.contains(border, metrics));
        assert_eq!(up.hit(border, metrics, 4.5), None);
        let second = metrics.at(x, up.rows.y as f32 + up.offset_y + 0.75);
        assert_eq!(up.hit(second, metrics, 4.5), Some(5));
    }

    #[test]
    fn clipped_selector_in_narrow_or_short_views_always_exposes_a_choice_inside_the_form() {
        for w in 1..15 {
            for h in 1..12 {
                let body = Rect { x: 7, y: 11, w, h };
                for quarter in -3..(4 * h as i32) {
                    let anchor = quarter as f32 / 4.;
                    let menu = SelectorLayout::at(body, anchor, 20).unwrap();
                    assert!(menu.rows.w >= 1 && menu.rows.h >= 1);
                    assert!(body.contains(menu.frame.x, menu.frame.y));
                    assert!(menu.frame.x + menu.frame.w <= body.x + w);
                    assert!(
                        menu.offset_y + (menu.frame.y + menu.frame.h) as f32 <= (body.y + h) as f32
                    );
                }
            }
        }
    }
}
