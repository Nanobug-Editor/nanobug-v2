//! Shortcut layout and the visible help frame.

use super::{
    HelpWindow,
    list::{HelpList, Scope},
};
use crate::ui::components::grid_dialog::GridDialog;
use crate::ui::components::grid_tab_bar::TitleTabs;
use crate::{
    grid::{
        render::{CursorCell, Frame, Metrics, PromptRow, Rect, SEARCH_ICON},
        windows::WindowFrame,
    },
    ui::{
        text::{TextSpacing, UNITS_PER_COLUMN},
        theme::{ACCENT, GOLD, MUTED, SELECTION, TEXT},
    },
};

use gpui::{AnyElement, Bounds, Pixels, Point, point, px, size};
use nanobug::i18n;
use std::ops::Range;

/// One grid row per horizontal border.
const BORDER_ROWS: usize = 2;
/// Lower the content to give the title space above the first full-height selection.
const CONTENT_OFFSET_Y: f32 = 4.;
/// Extra offset for the bottom gap between the search glyphs and their row edge.
const SEARCH_OFFSET_Y: f32 = 2.;
/// First text column after the search icon.
const SEARCH_TEXT_COLUMN: usize = 2;

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

impl HelpList {
    /// Fit the localized titles and separator into `slot` using their measured `spacing`.
    fn titles(&self, slot: Range<usize>, spacing: &TextSpacing) -> TitleTabs<2> {
        let window = self.has_scope(Scope::Window).then(|| self.window_title());
        let global = self
            .has_scope(Scope::Global)
            .then(|| i18n::text("help.global_title"));
        TitleTabs::new(
            slot,
            [window.unwrap_or_default(), global.unwrap_or_default()],
            false,
            spacing,
        )
    }

    /// Place each tab's click target in `area` using the same `metrics` and `spacing` as the title.
    /// Reserve the close button when `controls_hovered` is set.
    pub(in crate::grid) fn tab_targets(
        &self,
        area: Rect,
        metrics: Metrics,
        spacing: &TextSpacing,
        controls_hovered: bool,
    ) -> [(Scope, Bounds<Pixels>); 2] {
        let controls = if controls_hovered {
            area.corner_controls(false)
        } else {
            None
        };
        let titles = self.titles(area.border_title_columns(controls.as_ref()), spacing);
        [Scope::Window, Scope::Global].map(|scope| {
            let slot = titles.hit_columns(usize::from(scope == Scope::Global));
            let bounds = Rect {
                x: area.x + slot.start,
                y: area.y,
                w: slot.len(),
                h: 1,
            }
            .pixels(metrics);
            (scope, bounds)
        })
    }
}

impl HelpWindow {
    /// Find the visible help title at `position` using the same layout as the border.
    pub(in crate::grid) fn scope_at(
        &self,
        area: Rect,
        metrics: Metrics,
        spacing: &TextSpacing,
        controls_hovered: bool,
        position: Point<Pixels>,
    ) -> Option<Scope> {
        self.list
            .as_ref()?
            .tab_targets(area, metrics, spacing, controls_hovered)
            .into_iter()
            .find_map(|(scope, bounds)| {
                (!bounds.is_empty() && bounds.contains(&position)).then_some(scope)
            })
    }

    /// Center the popup in `cols` by `rows` at the same size in browsing and search.
    pub(in crate::grid) fn layout(&self, cols: usize, rows: usize) -> Rect {
        let Some(list) = &self.list else {
            return Rect::default();
        };
        let (width, height) = if self.keyboard.is_some() {
            (
                super::keyboard::KEYBOARD_COLUMNS,
                super::keyboard::KEYBOARD_BODY_ROWS,
            )
        } else {
            (74, list.capacity())
        };
        let w = cols.saturating_sub(4).min(width);
        let h = rows.saturating_sub(4).min(height + BORDER_ROWS);
        GridDialog::new(Rect {
            w: cols,
            h: rows,
            ..Rect::default()
        })
        .centered(w, h)
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
        let slots = PromptRow::new(SEARCH_TEXT_COLUMN, Self::search_area(area).w, spacing);
        slots.button_columns().map(|slot| {
            Bounds::new(
                bounds.origin + point(px(slot.start as f32 * metrics.cw), px(0.)),
                size(px(slot.len() as f32 * metrics.cw), bounds.size.height),
            )
        })
    }

    /// Paint the keycaps and example outline when the editor shortcut tab is visible.
    pub(in crate::grid) fn keyboard_element(
        &self,
        area: Rect,
        metrics: Metrics,
    ) -> Option<AnyElement> {
        self.keyboard
            .as_ref()
            .filter(|_| self.keyboard_visible())
            .map(|keyboard| keyboard.element(self.body(area), Self::content_metrics(metrics)))
    }

    /// Locate the first binding character when the keyboard tab is visible in `area`.
    pub(in crate::grid) fn keyboard_caret(
        &self,
        area: Rect,
        metrics: Metrics,
    ) -> Option<CursorCell> {
        if !self.keyboard_visible() {
            return None;
        }
        super::keyboard::Keyboard::caret(self.body(area), Self::content_metrics(metrics))
    }

    /// Build the border, title and example text or scrollable list for `context`.
    pub(in crate::grid) fn frames(&self, context: WindowFrame<'_>) -> Option<HelpFrames> {
        let list = self.list.as_ref()?;
        let area = context.area;
        let body = self.body(area);
        let spacing = context.spacing;
        let rows =
            if let Some(keyboard) = self.keyboard.as_ref().filter(|_| self.keyboard_visible()) {
                keyboard.frame(body, spacing)
            } else {
                list_frame(list, body, spacing)
            };

        let mut dim_title = Frame::new(area.w, 1);
        let global = list.scope() == Scope::Global;
        let chrome = Frame::with_title(
            area,
            context.selected,
            context.controls_hovered,
            |frame, slot, color| {
                let start = slot.start;
                let titles = list.titles(slot, spacing);
                let mut end = None;
                for (index, text, part) in titles.visible() {
                    if let Some(previous) = end {
                        dim_title.label(
                            0,
                            previous..part.start,
                            titles.separator(),
                            color,
                            spacing,
                        );
                    }
                    let title = if index == usize::from(global) {
                        &mut *frame
                    } else {
                        &mut dim_title
                    };
                    end = Some(title.label(0, part, text, color, spacing));
                }
                end.unwrap_or(start)
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
        let slots = PromptRow::new(SEARCH_TEXT_COLUMN, bar.w, spacing);
        let mut frame = Frame::new(bar.w, 1);
        let query = self.filter_text(list, slots.input.len(), spacing);
        frame.icon(0, 0, SEARCH_ICON);
        if query.is_empty() {
            frame.label(
                0,
                slots.input.clone(),
                i18n::text("help.filter_placeholder"),
                MUTED,
                spacing,
            );
        } else {
            frame.label(0, slots.input.clone(), &query, TEXT, spacing);
        }
        slots.draw_buttons(&mut frame, 0, spacing);
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
        let query = PromptRow::new(SEARCH_TEXT_COLUMN, bar.w, spacing).input;
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

/// Draw the retained rows of `list` inside `body` with measured `spacing`.
fn list_frame(list: &HelpList, body: Rect, spacing: &TextSpacing) -> Frame {
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
    rows
}
