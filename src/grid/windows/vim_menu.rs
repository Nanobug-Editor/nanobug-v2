//! Draw and navigate one level of the editor's Vim menus.

use crate::ui::components::grid_popup::GridPopup;
use crate::{
    grid::render::{Frame, GridLayer, LabelStyle, Metrics, Panel, Rect},
    ui::{
        text::TextSpacing,
        theme::{ACCENT, CURRENT, MUTED, TEXT, ThemeComponent},
    },
};
use gpui::{App, AppContext, Entity, Pixels, Point};
use nanobug::{
    animation::scroll::{GridScroll, ScrollInput},
    i18n,
    vim::MenuEntry,
};

/// Rows in the scrollable menu viewport before it grows a scroll offset.
const MAX_ROWS: usize = 18;

/// Visible hierarchy level, owned by the editor with matching document identity.
pub(in crate::grid) struct VimMenu {
    /// Document opening whose modal state is used when activating an entry.
    pub(in crate::grid) opening: u64,
    /// Top-left placement preferred by the originating caret or pointer.
    pub(in crate::grid) anchor: (usize, usize),
    /// Menu hierarchy shown on this level. The root path is empty.
    pub(in crate::grid) path: Vec<String>,
    /// Ordered visible rows and their executable paths.
    entries: Vec<MenuEntry>,
    /// Selected enabled entry, if the current mode has one.
    selected: Option<usize>,
    /// Fractional wheel displacement retained between native events.
    wheel: f32,
    /// Shared smooth-scroll model for the menu viewport.
    scroll: GridScroll,
    /// Fixed border and separately clipped scrolling entries.
    panels: [Entity<Panel>; 2],
}

impl VimMenu {
    /// Create a menu level at `anchor` for `opening`, with retained panels in `cx`.
    pub(in crate::grid) fn new(
        opening: u64,
        anchor: (usize, usize),
        path: Vec<String>,
        entries: Vec<MenuEntry>,
        metrics: Metrics,
        cx: &mut App,
    ) -> Self {
        let selected = entries
            .iter()
            .position(|entry| entry.enabled && !entry.separator);
        Self {
            opening,
            anchor,
            path,
            entries,
            selected,
            wheel: 0.,
            scroll: GridScroll::default(),
            panels: std::array::from_fn(|_| cx.new(|_| Panel::new(metrics))),
        }
    }

    /// Fit this menu near its anchor inside the workspace bounds `viewport`.
    pub(in crate::grid) fn area(&self, viewport: Rect, spacing: &TextSpacing) -> Option<Rect> {
        if viewport.w < 10 || viewport.h < 3 {
            return None;
        }
        let width = self
            .entries
            .iter()
            .map(|entry| {
                let label = spacing.columns(&entry.label)
                    + 5
                    + entry
                        .accelerator
                        .as_ref()
                        .map_or(0, |text| spacing.columns(text) + 2);
                label.max(
                    entry
                        .tooltip
                        .as_ref()
                        .map_or(0, |tip| spacing.columns(tip) + 4),
                )
            })
            .max()
            .unwrap_or(0);
        let footer = spacing.columns(i18n::text("vim_menu.hint")) + 6;
        let w = width.max(footer).max(16).min(viewport.w);
        let h = self
            .entries
            .len()
            .clamp(1, MAX_ROWS)
            .saturating_add(2 + usize::from(self.has_tips()))
            .min(viewport.h);
        let (x, y) = self.anchor;
        Some(GridPopup::new(viewport).at(Rect { x, y, w, h }))
    }

    /// Whether this level needs a reserved row to display an entry's menu tip.
    fn has_tips(&self) -> bool {
        self.entries.iter().any(|entry| entry.tooltip.is_some())
    }

    /// Compute the clipped list interior of a menu at `area`.
    fn body(&self, area: Rect) -> Rect {
        Rect {
            x: area.x + 1,
            y: area.y + 1,
            w: area.w - 2,
            h: area.h - 2 - usize::from(area.h >= 4 && self.has_tips()),
        }
    }

    /// Advance the row animation at `now`, revealing keyboard selection when requested by `input`.
    pub(in crate::grid) fn advance(&mut self, input: ScrollInput, area: Rect, now: f64) {
        let height = self.body(area).h;
        let input = if matches!(input, ScrollInput::Reveal) {
            let top = self.scroll.target();
            let target = self
                .selected
                .map_or(top, |row| top.clamp(row.saturating_sub(height - 1), row));
            ScrollInput::Position(target)
        } else {
            input
        };
        // The selection is highlighted in the list and can animate into the viewport.
        self.scroll
            .track_rows(input, None, height, self.entries.len(), now);
    }

    /// Move by `delta` enabled items, keeping the current item at the bounds.
    pub(in crate::grid) fn move_selection(&mut self, delta: isize) {
        let candidates = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| (entry.enabled && !entry.separator).then_some(index))
            .collect::<Vec<_>>();
        let Some(&start) = candidates.first() else {
            return;
        };
        let current = self
            .selected
            .and_then(|index| candidates.binary_search(&index).ok());
        let next = current.map_or(if delta < 0 { candidates.len() - 1 } else { 0 }, |index| {
            index.saturating_add_signed(delta).min(candidates.len() - 1)
        });
        self.selected = Some(candidates.get(next).copied().unwrap_or(start));
    }

    /// Select the visible enabled item under `point` using the animated row bounds.
    pub(in crate::grid) fn select_point(
        &mut self,
        point: Point<Pixels>,
        area: Rect,
        metrics: Metrics,
    ) -> bool {
        let body = self.body(area).pixels(metrics);
        if !body.contains(&point) {
            return false;
        }
        let row = (f32::from(point.y - body.origin.y) / metrics.lh + self.scroll.fraction()).floor()
            as usize;
        let index = self.scroll.first_cell() + row;
        if self
            .entries
            .get(index)
            .is_some_and(|entry| entry.enabled && !entry.separator)
        {
            self.selected = Some(index);
            return true;
        }
        false
    }

    /// Resolve an unambiguous `letter` in this level's enabled entries.
    pub(in crate::grid) fn select_mnemonic(&mut self, letter: char) -> bool {
        let mut matches = self.entries.iter().enumerate().filter(|(_, entry)| {
            entry.enabled
                && entry
                    .mnemonic
                    .is_some_and(|key| key.eq_ignore_ascii_case(&letter))
        });
        if let Some((index, _)) = matches.next()
            && matches.next().is_none()
        {
            self.selected = Some(index);
            return true;
        }
        false
    }

    /// Return the current selectable menu action or submenu.
    pub(in crate::grid) fn selected(&self) -> Option<&MenuEntry> {
        self.selected.and_then(|index| self.entries.get(index))
    }

    /// Accumulate fractional wheel `rows`, returning the signed whole-row update.
    pub(in crate::grid) fn wheel_rows(&mut self, rows: f32) -> isize {
        self.wheel += rows;
        let whole = self.wheel.trunc() as isize;
        self.wheel -= whole as f32;
        whole
    }

    /// Whether menu scroll still needs native frame updates.
    pub(in crate::grid) fn scrolling(&self) -> bool {
        self.scroll.is_active()
    }

    /// Update retained panel metrics during a workspace DPI or theme change.
    pub(in crate::grid) fn set_appearance(&self, metrics: Metrics, cx: &mut App) {
        for panel in &self.panels {
            panel.update(cx, |panel, cx| panel.set_appearance(metrics, cx));
        }
    }

    /// Compose the fixed menu frame and its visible scrolled area using measured `spacing`.
    pub(in crate::grid) fn refresh(&self, area: Rect, spacing: &TextSpacing, cx: &mut App) {
        let body = self.body(area);
        let title = if self.path.is_empty() {
            i18n::text("vim_menu.title").into()
        } else {
            self.path.join(" › ")
        };
        let mut chrome = Frame::titled(area, &title, spacing);
        chrome.set_theme_component(ThemeComponent::Editor);
        chrome.exclude_theme_background(Rect {
            x: 1,
            y: 1,
            w: body.w,
            h: body.h,
        });
        chrome.styled_label(
            area.h - 1,
            3..area.w.saturating_sub(3),
            i18n::text("vim_menu.hint"),
            MUTED,
            spacing,
            LabelStyle::Dimmed,
        );
        if body.h + 2 < area.h
            && let Some(tip) = self.selected().and_then(|entry| entry.tooltip.as_deref())
        {
            chrome.styled_label(
                body.h + 1,
                2..area.w - 2,
                tip,
                MUTED,
                spacing,
                LabelStyle::Dimmed,
            );
        }
        let height = body.h + usize::from(self.scroll.fraction() > 0.);
        let mut rows = Frame::new(body.w, height);
        rows.set_theme_component(ThemeComponent::Editor);
        let first = self.scroll.first_cell();
        for (y, entry) in self.entries.iter().skip(first).take(height).enumerate() {
            if entry.separator {
                rows.text(
                    1,
                    y,
                    &"─".repeat(body.w.saturating_sub(2)),
                    MUTED,
                    body.w.saturating_sub(2),
                );
                continue;
            }
            let color = if !entry.enabled {
                MUTED
            } else if self.selected == Some(first + y) {
                rows.tint(y, 0, body.w, CURRENT);
                ACCENT
            } else {
                TEXT
            };
            let right = body.w.saturating_sub(1);
            let suffix = if entry.submenu {
                Some("›")
            } else {
                entry.accelerator.as_deref()
            };
            let label_end = if let Some(suffix) = suffix {
                let suffix_width = spacing.columns(suffix).min(right / 3);
                let suffix_start = right.saturating_sub(suffix_width);
                rows.styled_label(
                    y,
                    suffix_start..right,
                    suffix,
                    MUTED,
                    spacing,
                    LabelStyle::Dimmed,
                );
                suffix_start.saturating_sub(2)
            } else {
                right
            };
            let label = &entry.label;
            if let Some((index, ch)) = entry.mnemonic.and_then(|key| {
                label
                    .char_indices()
                    .find(|(_, ch)| ch.eq_ignore_ascii_case(&key))
            }) {
                let before = rows.label(y, 1..label_end, &label[..index], color, spacing);
                let end = index + ch.len_utf8();
                let after = rows.styled_label(
                    y,
                    before..label_end,
                    &label[index..end],
                    color,
                    spacing,
                    LabelStyle::Key,
                );
                rows.label(y, after..label_end, &label[end..], color, spacing);
            } else {
                rows.label(y, 1..label_end, label, color, spacing);
            }
        }
        for (panel, frame) in self.panels.iter().zip([chrome, rows]) {
            panel.update(cx, |panel, cx| panel.set(frame, cx));
        }
    }

    /// Position the retained layers inside `area` with the body's smooth scroll transform.
    pub(in crate::grid) fn layers(&self, area: Rect, metrics: Metrics) -> [GridLayer; 2] {
        [
            GridLayer {
                panel: self.panels[0].clone(),
                bounds: area.pixels(metrics),
                clip: None,
            },
            GridLayer::scrolled(
                &self.panels[1],
                self.body(area),
                self.scroll.fraction(),
                metrics,
            ),
        ]
    }
}
