//! Character layout, shortcuts and animated selection for the workspace tab strip.

use super::{Workspace, projects::WorkspaceTabs, view::positioned};
use crate::{
    grid::render::{Frame, Metrics, Panel, Rect},
    ui::{
        text::{TextSpacing, UNITS_PER_COLUMN},
        theme::{CURSOR_FILL, CURSOR_TEXT, MUTED, TEXT},
    },
};
use gpui::{
    App, AppContext, Bounds, Context, Entity, Keystroke, ParentElement, Pixels, Styled, Window,
    div, px, rgb,
};
use nanobug::{
    animation::cursor::Spring,
    i18n::{self, Locale},
};
use std::{ops::Range, time::Instant};

/// Space on each side of a tab name, in narrow grid columns.
const TAB_PADDING: usize = 2;
/// Width of the pipe separator between two tabs.
const TAB_SEPARATOR: usize = 1;
/// Time for the selection's edges to settle, in seconds.
const TAB_SETTLE: f64 = 0.24;

/// Keyboard actions that operate on tab order without focusing the strip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ProjectShortcut {
    /// Select the tab at a relative offset.
    Select(isize),
    /// Move the active tab by a relative offset.
    Move(isize),
    /// Move the active tab to the first position.
    First,
    /// Move the active tab to the last position.
    Last,
}

impl ProjectShortcut {
    /// Match only the project chords in `key`, leaving other modifier combinations to the panes.
    pub(super) fn resolve(key: &Keystroke) -> Option<Self> {
        let modifiers = key.modifiers;
        if !modifiers.alt || modifiers.shift || modifiers.platform || modifiers.function {
            return None;
        }
        let offset = match (modifiers.control, key.key.as_str()) {
            (_, "h" | "k") => -1,
            (_, "l" | "j") => 1,
            (true, "home") => return Some(Self::First),
            (true, "end") => return Some(Self::Last),
            _ => return None,
        };
        Some(if modifiers.control {
            Self::Move(offset)
        } else {
            Self::Select(offset)
        })
    }
}

impl Workspace {
    /// Apply `shortcut` to the tab order in `window` through `cx`.
    pub(super) fn project_shortcut(
        &mut self,
        shortcut: ProjectShortcut,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = self.projects.selected;
        let last = self.projects.len() - 1;
        let target = match shortcut {
            ProjectShortcut::Select(offset) => {
                if self.select_tab(current.saturating_add_signed(offset).min(last), window, cx) {
                    self.sync(window, cx);
                }
                return;
            }
            ProjectShortcut::Move(offset) => current.saturating_add_signed(offset).min(last),
            ProjectShortcut::First => 0,
            ProjectShortcut::Last => last,
        };
        if target != current {
            self.projects.move_selected(target);
            self.refresh_tab_bar(cx);
            cx.notify();
        }
    }

    /// Refresh the strip from the current grid and open tabs in `cx`.
    pub(super) fn refresh_tab_bar(&mut self, cx: &mut Context<Self>) {
        self.tabs
            .update(self.layout.cols, &self.projects, &self.spacing, cx);
    }
}

/// Two retained text layers and a moving block that masks their contrasting inks.
pub(super) struct TabBar {
    /// The column count, tab revision and locale at the last layout.
    layout: Option<(usize, u64, Locale)>,
    /// Index of the first visible tab, retained until the selected tab needs room.
    first: usize,
    /// Normal text layer for the full strip.
    labels: Entity<Panel>,
    /// Dark text shown only inside the selection block.
    covered: Entity<Panel>,
    /// Left and right edges of the selected tab in grid columns.
    target: [f64; 2],
    /// Animated positions of the two block edges, in the same units as `target`.
    edges: [Spring; 2],
    /// Monotonic timestamp of the last animation sample or retarget.
    sampled_at: Instant,
}

impl TabBar {
    /// Create the retained strip at the workspace's `metrics` in `cx`.
    pub(super) fn new(metrics: Metrics, cx: &mut App) -> Self {
        Self {
            layout: None,
            first: 0,
            labels: cx.new(|_| Panel::new(metrics)),
            covered: cx.new(|_| Panel::new(metrics)),
            target: [0.; 2],
            edges: [Spring::default(); 2],
            sampled_at: Instant::now(),
        }
    }

    /// Fit `projects` into `cols`, using `spacing` for actual glyph positions and `cx` for panels.
    fn update(
        &mut self,
        cols: usize,
        projects: &WorkspaceTabs,
        spacing: &TextSpacing,
        cx: &mut App,
    ) {
        let key = (cols, projects.revision, i18n::locale());
        if self.layout == Some(key) {
            return;
        }
        let names: Vec<_> = projects.names().collect();
        let widths: Vec<_> = names.iter().map(|name| spacing.columns(name)).collect();
        let visible = visible_tabs(&widths, cols, self.first, projects.selected);
        self.first = visible.start;
        let hidden_left = visible.start;
        let hidden_right = names.len() - visible.end;
        let mut column = hidden_columns(hidden_left);
        let limit = cols.saturating_sub(hidden_columns(hidden_right));
        let mut labels = Frame::new(cols, 1);
        let mut covered = Frame::new(cols, 1);
        let mut label = |slot: Range<usize>, text: &str, color| {
            labels.label(0, slot.clone(), text, color, spacing);
            covered.label(0, slot, text, CURSOR_TEXT, spacing);
        };
        if hidden_left > 0 {
            label(0..column - 1, &format!("←{hidden_left}"), MUTED);
        }
        for i in visible.clone() {
            let start = column + TAB_PADDING;
            let width = widths[i].min(limit.saturating_sub(start + TAB_PADDING));
            let end = start + width;
            label(start..end, &names[i], TEXT);
            if i == projects.selected {
                let units = spacing
                    .fitting_cells(&names[i], width * UNITS_PER_COLUMN)
                    .last()
                    .map_or(0, |cell| cell.end);
                let target = [
                    column as f64,
                    (start + TAB_PADDING) as f64 + units as f64 / UNITS_PER_COLUMN as f64,
                ];
                if self.layout.is_none() {
                    for (edge, goal) in self.edges.iter_mut().zip(target) {
                        edge.snap(goal);
                    }
                }
                if self.target != target {
                    self.target = target;
                    self.sampled_at = Instant::now();
                }
            }
            column = end + TAB_PADDING;
            if i + 1 < visible.end {
                label(column..column + TAB_SEPARATOR, "|", MUTED);
                column += TAB_SEPARATOR;
            }
        }
        if hidden_right > 0 {
            label(limit + 1..cols, &format!("{hidden_right}→"), MUTED);
        }
        self.layout = Some(key);
        self.labels.update(cx, |panel, cx| panel.set(labels, cx));
        self.covered.update(cx, |panel, cx| panel.set(covered, cx));
    }

    /// Draw the block and masked text at `metrics`, advancing animation in `window` through `cx`.
    pub(super) fn element(
        &mut self,
        metrics: Metrics,
        window: &Window,
        cx: &App,
    ) -> impl gpui::IntoElement {
        let cols = self.layout.map_or(0, |layout| layout.0);
        let now = Instant::now();
        let elapsed = now.duration_since(self.sampled_at).as_secs_f64();
        self.sampled_at = now;
        let mut animating = false;
        for (edge, goal) in self.edges.iter_mut().zip(self.target) {
            edge.advance(goal, elapsed, TAB_SETTLE, 1.);
            if cx.reduce_motion()
                || ((edge.value - goal).abs() < 0.002 && edge.velocity.abs() < 0.02)
            {
                edge.snap(goal);
            } else {
                animating = true;
            }
        }
        if animating {
            window.request_animation_frame();
        }

        let header = Rect {
            x: 0,
            y: 0,
            w: cols,
            h: 1,
        }
        .pixels(metrics);
        let left = self.edges[0].value.clamp(0., cols as f64) as f32;
        let right = self.edges[1].value.clamp(left as f64, cols as f64) as f32;
        let block = Bounds::<Pixels>::from_corners(metrics.at(left, 0.), metrics.at(right, 1.));
        let masked = Bounds::new(header.origin - block.origin, header.size);
        div()
            .absolute()
            .left_0()
            .top_0()
            .w_full()
            .h(header.bottom())
            .child(self.labels.clone().cached(positioned(header)))
            .child(
                div()
                    .absolute()
                    .left(block.origin.x)
                    .top(block.origin.y)
                    .w(px((right - left) * metrics.cw))
                    .h(block.size.height)
                    .bg(rgb(CURSOR_FILL))
                    .overflow_hidden()
                    .child(self.covered.clone().cached(positioned(masked))),
            )
    }
}

/// Columns needed for a `count` indicator, including its arrow and inner gap.
fn hidden_columns(count: usize) -> usize {
    count.checked_ilog10().map_or(0, |power| power as usize + 3)
}

/// Fit a contiguous range of tab `widths` in `cols`, always including `selected`.
/// Keep the previous `first` until it crowds the selection, then backfill any unused space.
fn visible_tabs(widths: &[usize], cols: usize, first: usize, selected: usize) -> Range<usize> {
    let mut edges = Vec::with_capacity(widths.len() + 1);
    edges.push(0);
    for width in widths {
        edges.push(edges.last().unwrap() + width + 2 * TAB_PADDING + TAB_SEPARATOR);
    }
    let fits = |start, end| {
        edges[end] - edges[start] - TAB_SEPARATOR
            + hidden_columns(start)
            + hidden_columns(widths.len() - end)
            <= cols
    };
    let mut start = first.min(selected);
    let mut end = selected + 1;
    while start < selected && !fits(start, end) {
        start += 1;
    }
    while end < widths.len() && fits(start, end + 1) {
        end += 1;
    }
    while start > 0 && fits(start - 1, end) {
        start -= 1;
    }
    start..end
}

#[cfg(test)]
mod tests;
