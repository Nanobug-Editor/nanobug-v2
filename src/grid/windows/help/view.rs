//! Shortcut layout and the visible help frame.

use super::{
    HelpWindow,
    list::{HelpList, Scope},
};
use crate::{
    grid::{
        render::{Frame, LabelStyle, Metrics, Rect},
        windows::WindowFrame,
    },
    ui::{
        icons::Icon,
        text::{TextSpacing, UNITS_PER_COLUMN},
        theme::{ACCENT, GOLD, MUTED, SELECTION, TEXT},
    },
};

use gpui::{Bounds, Pixels, point, px, size};
use nanobug::i18n;
use std::ops::Range;

/// Distance from the character border's centerline to the rounded cutout, in logical pixels.
pub(super) const HELP_BORDER_GAP: f32 = 12.;
/// Radius of the shortcut popup's outer corners, in logical pixels.
pub(super) const HELP_CORNER_RADIUS: f32 = 14.;
/// One grid row per horizontal border.
const BORDER_ROWS: usize = 2;
/// Lower the content to give the title space above the first full-height selection.
const CONTENT_OFFSET_Y: f32 = 4.;
/// Extra offset for the bottom gap between the search glyphs and their row edge.
const SEARCH_OFFSET_Y: f32 = 2.;
/// First text column after the search icon.
const SEARCH_TEXT_COLUMN: usize = 2;
/// Magnifying glass drawn in the filter status row.
const SEARCH_ICON: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/icons/search.svg"
));
/// Apply filter with Enter.
const SEARCH_APPLY: SearchButton = SearchButton {
    prefix: &[
        ("[", LabelStyle::Text),
        ("✓", LabelStyle::Symbol),
        (" ", LabelStyle::Text),
    ],
    key: "Enter",
};
/// Cancel filter input with Escape.
const SEARCH_CANCEL: SearchButton = SearchButton {
    prefix: &[("[✕ ", LabelStyle::Text)],
    key: "Esc",
};

/// One bracketed text button with an underlined key name.
struct SearchButton {
    /// Styled spans for the opening bracket, action symbol and separating space.
    prefix: &'static [(&'static str, LabelStyle)],
    /// Visible keyboard key name.
    key: &'static str,
}

impl SearchButton {
    /// Return the button width including its closing bracket using `spacing`.
    fn columns(&self, spacing: &TextSpacing) -> usize {
        let prefix: usize = self
            .prefix
            .iter()
            .map(|(text, _)| spacing.columns(text))
            .sum();
        prefix + spacing.columns(self.key) + 1
    }

    /// Draw this button in `color` at `slot` on the search `frame`, using measured `spacing`.
    fn draw(&self, frame: &mut Frame, slot: Range<usize>, color: u32, spacing: &TextSpacing) {
        let mut start = slot.start;
        for &(text, style) in self.prefix {
            start = frame.styled_label(0, start..slot.end, text, color, spacing, style);
        }
        let end = frame.styled_label(
            0,
            start..slot.end,
            self.key,
            color,
            spacing,
            LabelStyle::Key,
        );
        frame.label(0, end..slot.end, "]", color, spacing);
    }
}

/// Search fields in columns relative to the single search row.
struct SearchSlots {
    /// Query after the magnifying glass, with room for the caret.
    query: Range<usize>,
    /// Apply button at the right of the query.
    apply: Range<usize>,
    /// Cancel button after Apply.
    cancel: Range<usize>,
}

impl SearchSlots {
    /// Fit the two buttons at the right of `width` columns using measured `spacing`.
    fn new(width: usize, spacing: &TextSpacing) -> Self {
        let cancel_start = width.saturating_sub(SEARCH_CANCEL.columns(spacing));
        let apply_end = cancel_start.saturating_sub(1);
        let apply_start = apply_end.saturating_sub(SEARCH_APPLY.columns(spacing));
        Self {
            query: SEARCH_TEXT_COLUMN..apply_start.saturating_sub(2).max(SEARCH_TEXT_COLUMN),
            apply: apply_start..apply_end,
            cancel: cancel_start..width,
        }
    }
}

/// Independently retained parts of the shortcut popup.
pub(in crate::grid) struct HelpFrames {
    /// Fixed title and border.
    pub chrome: Frame,
    /// Clipped shortcut rows.
    pub body: Frame,
    /// Inactive scope title and its separator, composited at half opacity.
    pub dim_title: Frame,
    /// Search icon, query and buttons while the filter prompt is active.
    pub search: Option<Frame>,
}

impl HelpWindow {
    /// Center the popup in `cols` by `rows` at the same size in browsing and search.
    pub(in crate::grid) fn layout(&self, cols: usize, rows: usize) -> Rect {
        let Some(list) = &self.list else {
            return Rect::default();
        };
        let w = cols.saturating_sub(4).min(74);
        let h = rows.saturating_sub(4).min(list.capacity() + BORDER_ROWS);
        Rect {
            x: (cols - w) / 2,
            y: (rows - h) / 2,
            w,
            h,
        }
    }

    /// Shortcut viewport inside `area`; clip its final row while search occupies that space.
    pub(in crate::grid) fn body(&self, area: Rect) -> Rect {
        Rect {
            x: area.x + 1,
            y: area.y + 1,
            w: area.w.saturating_sub(2),
            h: area
                .h
                .saturating_sub(BORDER_ROWS + usize::from(self.is_filtering())),
        }
    }

    /// Shift the origin in `metrics` for the popup contents, leaving its border on the grid.
    pub(in crate::grid) fn content_metrics(mut metrics: Metrics) -> Metrics {
        metrics.origin.y += px(CONTENT_OFFSET_Y);
        metrics
    }

    /// Place the single search row inside `area` in workspace grid cells.
    fn search_area(area: Rect) -> Rect {
        Rect {
            x: area.x + 2,
            y: area.y + area.h.saturating_sub(2),
            w: area.w.saturating_sub(4),
            h: 1,
        }
    }

    /// Place the search row within `area` using `metrics` and the footer's optical offset.
    pub(in crate::grid) fn search_bounds(area: Rect, metrics: Metrics) -> Bounds<Pixels> {
        let mut bounds = Self::search_area(area).pixels(Self::content_metrics(metrics));
        bounds.origin.y += px(SEARCH_OFFSET_Y);
        bounds
    }

    /// Return the Apply and Cancel hit boxes in `area`, using `metrics` and measured `spacing`.
    pub(in crate::grid) fn search_buttons(
        area: Rect,
        metrics: Metrics,
        spacing: &TextSpacing,
    ) -> [Bounds<Pixels>; 2] {
        let bounds = Self::search_bounds(area, metrics);
        let slots = SearchSlots::new(Self::search_area(area).w, spacing);
        [slots.apply, slots.cancel].map(|slot| {
            Bounds::new(
                bounds.origin + point(px(slot.start as f32 * metrics.cw), px(0.)),
                size(px(slot.len() as f32 * metrics.cw), bounds.size.height),
            )
        })
    }

    /// Build the border, split title, scrollable list and optional search row for `context`.
    pub(in crate::grid) fn frames(&self, context: WindowFrame<'_>) -> Option<HelpFrames> {
        let list = self.list.as_ref()?;
        let area = context.area;
        let body = self.body(area);
        let spacing = context.spacing;
        let first = list.scroll.first_cell();
        let mut rows = Frame::viewport(body.w, body.h + 1, first);
        if list.len() == 0 {
            rows.label(
                0,
                1..body.w.saturating_sub(1),
                i18n::text("help.no_matches"),
                MUTED,
                spacing,
            );
        }
        for (row, (key, description)) in list.rows.iter().skip(first).take(body.h + 1).enumerate() {
            let selected = first + row == list.selected;
            if selected {
                rows.tint(row, 0, body.w, SELECTION);
            }
            rows.label(
                row,
                1..21,
                key,
                if selected { ACCENT } else { GOLD },
                spacing,
            );
            rows.label(
                row,
                22..body.w.saturating_sub(1),
                description,
                if selected { ACCENT } else { TEXT },
                spacing,
            );
        }

        let mut chrome = Frame::new(area.w, area.h);
        let mut dim_title = Frame::new(area.w, 1);
        let global = list.scope() == Scope::Global;
        chrome.border_with_title(
            context.selected,
            context.close_hovered,
            |frame, slot, color| {
                let mut start = slot.start;
                for (text, selected) in [
                    (list.window_title(), !global),
                    (" | ", false),
                    (i18n::text("help.global_title"), global),
                ] {
                    let title = if selected {
                        &mut *frame
                    } else {
                        &mut dim_title
                    };
                    start = title.label(0, start..slot.end, text, color, spacing);
                }
                start
            },
        );
        Some(HelpFrames {
            chrome,
            body: rows,
            dim_title,
            search: list
                .is_filtering()
                .then(|| self.search_frame(area, list, spacing)),
        })
    }

    /// Build the search row inside `area` for `list` using measured `spacing`.
    fn search_frame(&self, area: Rect, list: &HelpList, spacing: &TextSpacing) -> Frame {
        let bar = Self::search_area(area);
        let slots = SearchSlots::new(bar.w, spacing);
        let mut frame = Frame::new(bar.w, 1);
        let query = self.filter_text(list, slots.query.len(), spacing);
        frame.icon(0, 0, Icon::new(SEARCH_ICON, 1.));
        if query.is_empty() {
            frame.label(
                0,
                slots.query,
                i18n::text("help.filter_placeholder"),
                MUTED,
                spacing,
            );
        } else {
            frame.label(0, slots.query, &query, TEXT, spacing);
        }
        SEARCH_APPLY.draw(&mut frame, slots.apply, ACCENT, spacing);
        SEARCH_CANCEL.draw(&mut frame, slots.cancel, TEXT, spacing);
        frame
    }

    /// Locate the filter caret in grid coordinates using `area`, `spacing` and pixel `metrics`.
    pub(in crate::grid) fn filter_caret(
        &self,
        area: Rect,
        spacing: &TextSpacing,
        metrics: Metrics,
    ) -> Option<(f32, f32)> {
        let list = self.list.as_ref().filter(|list| list.is_filtering())?;
        let bar = Self::search_area(area);
        let query = SearchSlots::new(bar.w, spacing).query;
        let text = self.filter_text(list, query.len(), spacing);
        let units = spacing.cells(&text).last().map_or(0, |cell| cell.end);
        let column = query.start as f32 + units as f32 / UNITS_PER_COLUMN as f32;
        let y =
            f32::from(Self::search_bounds(area, metrics).origin.y - metrics.origin.y) / metrics.lh;
        Some((bar.x as f32 + column, y))
    }

    /// Keep `list`'s query and preedit within `columns` using `spacing`, with room for the caret.
    fn filter_text(&self, list: &HelpList, columns: usize, spacing: &TextSpacing) -> String {
        let text = format!("{}{}", list.query, self.composition.text);
        let width = spacing.cells(&text).last().map_or(0, |cell| cell.end);
        let available = columns.saturating_sub(1) * UNITS_PER_COLUMN;
        if width <= available {
            return text;
        }
        let tail_width = available.saturating_sub(UNITS_PER_COLUMN);
        let skip = width.saturating_sub(tail_width);
        let start = spacing
            .cells(&text)
            .find(|cell| cell.start >= skip)
            .map_or(text.len(), |cell| cell.byte);
        format!("…{}", &text[start..])
    }
}

/// Return the backdrop cutout bounds with an even gap around the character border inside `grid`.
/// `metrics` locates the centerline in the first and last rows and columns.
pub(in crate::grid) fn help_outline(grid: Bounds<Pixels>, metrics: Metrics) -> Bounds<Pixels> {
    let padding = point(
        px(HELP_BORDER_GAP - metrics.cw * 0.5),
        px(HELP_BORDER_GAP - metrics.lh * 0.5),
    );
    Bounds::from_corners(grid.origin - padding, grid.bottom_right() + padding)
}
