//! Character layout, shortcuts, pointer gestures and animated selection for the workspace tab strip.

mod pages;
#[cfg(windows)]
mod windows;

use super::{Workspace, layout::OUTER_INSET_Y, projects::WorkspaceTabs, view::positioned};
use crate::ui::components::grid_tab_bar::{GridTabBar, TabAlign, TabGrowth, TabKind, TabOptions};
use crate::{
    grid::render::{Frame, Metrics, Panel, Rect},
    ui::{
        text::{TextSpacing, UNITS_PER_COLUMN},
        theme::{CURSOR_TEXT, MUTED, TEXT, style},
    },
};
#[cfg(windows)]
use gpui::Point;
use gpui::{
    App, AppContext, Bounds, Context, Entity, Keystroke, ParentElement, Pixels, Styled, Window,
    div, point, px, rgb,
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
/// Logical pixel tolerance for a click with no native window movement.
#[cfg(windows)]
const TAB_CLICK_TOLERANCE: f64 = 4.;

/// Click destinations in the shared row, with zero-based indices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CaptionTarget {
    /// A top-level project or start page.
    Project(usize),
    /// A page within the selected project.
    Page(usize),
}

/// A click candidate recorded before Windows processes a caption press.
#[cfg(windows)]
#[derive(Clone, Copy)]
pub(super) struct TabPress {
    /// Initial pointer position in window-local pixels.
    position: Point<Pixels>,
    /// Visible label under the press.
    target: CaptionTarget,
    /// Tab revision that the index refers to.
    revision: u64,
    /// Page count and selection when the press was captured.
    pages: (usize, usize),
}

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
    /// Locate the tab row above the panes using the workspace's shared grid dimensions.
    pub(super) fn tab_metrics(&self) -> Metrics {
        Metrics {
            origin: point(self.metrics.origin.x, px(OUTER_INSET_Y)),
            ..self.metrics
        }
    }

    /// Capture the tab at window-local `position` before the native caption handles the press.
    #[cfg(windows)]
    pub(super) fn caption_tab_press(&self, position: Point<Pixels>) -> Option<TabPress> {
        if self.modal_open() {
            return None;
        }
        let target = self.tabs.tab_at(position, self.tab_metrics())?;
        let pages = self.tabpage_selection();
        if target == CaptionTarget::Project(self.projects.selected)
            || target == CaptionTarget::Page(pages.1)
        {
            return None;
        }
        Some(TabPress {
            position,
            target,
            revision: self.projects.revision,
            pages,
        })
    }

    /// Switch a caption `press` released at `position` over the same tab in `window` through `cx`.
    #[cfg(windows)]
    pub(super) fn finish_caption_tab_click(
        &mut self,
        press: TabPress,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if press.revision == self.projects.revision
            && press.pages == self.tabpage_selection()
            && (position - press.position).magnitude() <= TAB_CLICK_TOLERANCE
            && self.tabs.tab_at(position, self.tab_metrics()) == Some(press.target)
        {
            self.activate_caption_target(press.target, window, cx);
        }
    }

    /// Activate a caption `target` in `window` through `cx` using its existing command path.
    pub(super) fn activate_caption_target(
        &mut self,
        target: CaptionTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match target {
            CaptionTarget::Project(index) => self.activate_project_tab(index, window, cx),
            CaptionTarget::Page(index) => self.select_editor_page(index, window, cx),
        }
    }

    /// Return the active project's ordered page count and selected index.
    fn tabpage_selection(&self) -> (usize, usize) {
        self.projects.active_project().map_or((0, 0), |project| {
            (project.page_count(), project.active_page())
        })
    }

    /// Activate the tab at `index` in `window` and refresh its layout through `cx`.
    pub(super) fn activate_project_tab(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.modal_open() && self.select_tab(index, window, cx) {
            self.sync(window, cx);
        }
    }

    /// Apply the already-routed `shortcut` in `window`, resetting help on tab switches.
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
                let target = current.saturating_add_signed(offset).min(last);
                if self.select_tab(target, window, cx) {
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
        let pages = self.tabpage_selection();
        self.tabs.update(
            self.layout.visible_cols,
            &self.projects,
            pages,
            &self.spacing,
            cx,
        );
    }
}

/// Two retained text layers and a moving block that masks their contrasting inks.
pub(super) struct TabBar {
    /// Width, project revision, page state, selected standalone buffer and locale at the last layout.
    layout: Option<(usize, u64, (usize, usize), Option<u64>, Locale)>,
    /// Index of the first visible tab, retained until the selected tab needs room.
    first: usize,
    /// Destinations of visible project tabs and their overflow arrows.
    targets: Vec<(usize, Rect)>,
    /// Page labels with adjacent blank cells, and the painted overflow targets.
    page_targets: Vec<(usize, Rect)>,
    /// Width available to the project selection animation, excluding page numbers.
    project_cols: usize,
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
    /// Layout depends on measured glyph positions as well as the number of columns.
    pub(super) fn set_metrics(&mut self, metrics: Metrics, geometry_changed: bool, cx: &mut App) {
        if geometry_changed {
            self.layout = None;
        }
        for panel in [&self.labels, &self.covered] {
            panel.update(cx, |panel, cx| panel.set_appearance(metrics, cx));
        }
    }

    /// Create the retained strip at the workspace's `metrics` in `cx`.
    pub(super) fn new(metrics: Metrics, cx: &mut App) -> Self {
        Self {
            layout: None,
            first: 0,
            targets: Vec::new(),
            page_targets: Vec::new(),
            project_cols: 0,
            labels: cx.new(|_| Panel::new(metrics)),
            covered: cx.new(|_| Panel::new(metrics)),
            target: [0.; 2],
            edges: [Spring::default(); 2],
            sampled_at: Instant::now(),
        }
    }

    /// Fit `projects` and their active `pages` into `cols`, measuring labels with `spacing`.
    /// Publish the normal and selected text panels through `cx`.
    fn update(
        &mut self,
        cols: usize,
        projects: &WorkspaceTabs,
        pages: (usize, usize),
        spacing: &TextSpacing,
        cx: &mut App,
    ) {
        let standalone_buffer = projects
            .active_project()
            .filter(|project| project.manager.is_none())
            .map(|project| project.editor.buffer().id);
        let key = (
            cols,
            projects.revision,
            pages,
            standalone_buffer,
            i18n::locale(),
        );
        if self.layout == Some(key) {
            return;
        }
        let names: Vec<_> = projects.names().collect();
        let widths: Vec<_> = names.iter().map(|name| spacing.columns(name)).collect();
        let mut labels = Frame::new(cols, 1);
        let mut covered = Frame::new(cols, 1);
        let (cols, page_targets) = pages::draw(&mut labels, cols, pages.0, pages.1, spacing);
        self.page_targets = page_targets;
        self.project_cols = cols;
        let bar = project_tabs(&widths, cols, self.first, projects.selected);
        self.first = bar.visible.start;
        self.targets.clear();
        let mut label = |slot: Range<usize>, text: &str, color| {
            let slot = slot.start..slot.end.min(cols);
            labels.label(0, slot.clone(), text, color, spacing);
            covered.label(0, slot, text, CURSOR_TEXT, spacing);
        };
        for slot in bar.slots {
            let cells = slot.columns.clone();
            if let Some(index) = slot.target() {
                self.targets.push((
                    index,
                    Rect {
                        x: slot.hit.start,
                        y: 0,
                        w: slot.hit.len(),
                        h: 1,
                    },
                ));
            }
            match slot.kind {
                TabKind::Gap => label(cells, "|", MUTED),
                TabKind::Before { count, .. } => {
                    label(cells.start..cells.end - 1, &format!("←{count}"), MUTED)
                }
                TabKind::After { count, .. } => {
                    label(cells.start + 1..cells.end, &format!("{count}→"), MUTED)
                }
                TabKind::Tab(index) => {
                    // Reserve a visible name cell even when the entire project slot is narrower than its padding.
                    let padding = TAB_PADDING.min(cells.len().saturating_sub(1) / 2);
                    let start = cells.start + padding;
                    let end = cells.end.saturating_sub(padding);
                    label(start..end, &names[index], TEXT);
                    if index == projects.selected {
                        let units = spacing
                            .fitting_cells(&names[index], (end - start) * UNITS_PER_COLUMN)
                            .last()
                            .map_or(0, |cell| cell.end);
                        let target = [
                            cells.start as f64,
                            (start + padding) as f64 + units as f64 / UNITS_PER_COLUMN as f64,
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
                }
            }
        }
        self.layout = Some(key);
        self.labels.update(cx, |panel, cx| panel.set(labels, cx));
        self.covered.update(cx, |panel, cx| panel.set(covered, cx));
    }

    /// Return the visible project or page hit by `position` in the caption row's `metrics`.
    #[cfg(windows)]
    fn tab_at(&self, position: Point<Pixels>, metrics: Metrics) -> Option<CaptionTarget> {
        self.click_targets(metrics)
            .find(|(_, bounds)| bounds.contains(&position))
            .map(|(index, _)| index)
    }

    /// Iterate visible project and page hit bounds using the caption row's `metrics`.
    pub(super) fn click_targets(
        &self,
        metrics: Metrics,
    ) -> impl Iterator<Item = (CaptionTarget, Bounds<Pixels>)> + '_ {
        self.targets
            .iter()
            .map(|&(index, target)| (CaptionTarget::Project(index), target))
            .chain(
                self.page_targets
                    .iter()
                    .map(|&(index, target)| (CaptionTarget::Page(index), target)),
            )
            .filter(|(_, target)| target.w > 0)
            .map(move |(target, rect)| (target, rect.pixels(metrics)))
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
        let left = self.edges[0].value.clamp(0., self.project_cols as f64) as f32;
        let right = self.edges[1]
            .value
            .clamp(left as f64, self.project_cols as f64) as f32;
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
                    .bg(rgb(style(cx).cursor()))
                    .overflow_hidden()
                    .child(self.covered.clone().cached(positioned(masked))),
            )
    }
}

/// Fit project names with symmetric tab padding and preserve the first visible project on navigation.
fn project_tabs(widths: &[usize], cols: usize, first: usize, active: usize) -> GridTabBar {
    GridTabBar::fit(
        widths.len(),
        |i| widths[i] + 2 * TAB_PADDING,
        active,
        0..cols,
        TabOptions {
            gap: TAB_SEPARATOR,
            hit_padding: 0,
            minimum: 1,
            growth: TabGrowth::Retain(first),
            align: TabAlign::Fill,
        },
    )
}

#[cfg(test)]
mod tests;
