//! Contextual key sequence hints on the shared workspace material.

use super::{
    Workspace,
    keyboard::{LeaderAvailability, LeaderPrefix, PendingPrefix},
    layout::Focus,
};
use crate::{
    grid::{
        render::{Frame, LabelStyle, Metrics, Rect, key_hint_slots},
        windows::start::{StartLayout, StartPage},
    },
    ui::{
        icons::KeyIcon,
        theme::{ACCENT, MUTED, TEXT},
    },
};
use gpui::{
    Bounds, Context, InteractiveElement, IntoElement, KeyDownEvent, Keystroke, MouseButton,
    ParentElement, Pixels, StatefulInteractiveElement, StyleRefinement, Styled, Task, div, point,
    px, size,
};
use nanobug::{
    i18n,
    vim::{SPELL_SUGGESTION_KEYS, SequenceHint, VIEW_CONTINUATIONS, Vim},
};
use std::{borrow::Cow, time::Duration};

/// Keep the hint at the same width when switching between short groups.
const HINT_COLUMNS: usize = 34;

/// Pause after an incomplete key sequence before displaying its continuations.
const SEQUENCE_HINT_DELAY: Duration = Duration::from_millis(150);

/// The pending prefix that owns a delayed popup.
#[derive(PartialEq, Eq)]
pub(super) enum HintPrefix {
    /// Workspace leader group, including its current suffix.
    Leader(LeaderPrefix),
    /// Current pane command and its group translation key.
    Pane(String, &'static str),
}

/// Opening cycle for the continuation popup.
#[derive(Default)]
pub(super) enum HintDelay {
    /// No continuation is waiting to be displayed.
    #[default]
    Idle,
    /// Wait for the current prefix. Dropping the task cancels its timer callback.
    Waiting { prefix: HintPrefix, _task: Task<()> },
    /// Keep an open popup responsive as the user advances through groups.
    Visible,
}

/// A sequence in the shell or in the currently focused command surface.
enum ActiveHint<'a> {
    /// The workspace's Space sequence and its available shell targets.
    Leader(LeaderPrefix, LeaderAvailability),
    /// A command sequence in Explorer or a Vim editor.
    Pane(SequenceHint),
    /// Spelling corrections in order of their selection keys.
    Spelling(&'a [String]),
}

impl<'a> ActiveHint<'a> {
    /// Prefer an active correction list when `vim` is waiting for a candidate key.
    fn for_vim(vim: &'a Vim) -> Option<Self> {
        vim.spelling_suggestions()
            .map(Self::Spelling)
            .or_else(|| vim.sequence_hint().map(Self::Pane))
    }

    /// Return a prefix identity for continuations. A spelling result already needs a choice.
    fn delay_prefix(self) -> Option<HintPrefix> {
        match self {
            Self::Leader(prefix, _) => Some(HintPrefix::Leader(prefix)),
            Self::Pane(hint) => Some(HintPrefix::Pane(hint.prefix, hint.group)),
            Self::Spelling(_) => None,
        }
    }

    /// Count the rows without collecting the shell bindings for positioning.
    fn len(&self) -> usize {
        match self {
            Self::Leader(prefix, availability) => prefix.bindings(*availability).count(),
            Self::Pane(hint) => hint.continuations.len(),
            Self::Spelling(suggestions) => suggestions.len(),
        }
    }

    /// Resolve captions for the rows, retaining candidate text verbatim.
    fn rows(&self) -> Vec<(&str, &str, bool)> {
        match self {
            Self::Leader(prefix, availability) => prefix
                .bindings(*availability)
                .map(|(label, binding)| {
                    (
                        leader_key_label(label),
                        i18n::text(binding.description_key),
                        binding.is_group(),
                    )
                })
                .collect(),
            Self::Pane(hint) => hint
                .continuations
                .iter()
                .map(|&(label, caption, group)| (label, i18n::text(caption), group))
                .collect(),
            Self::Spelling(suggestions) => SPELL_SUGGESTION_KEYS
                .iter()
                .zip(*suggestions)
                .map(|(key, candidate)| {
                    let key = std::str::from_utf8(std::slice::from_ref(key))
                        .expect("ASCII candidate key");
                    (key, candidate.as_str(), false)
                })
                .collect(),
        }
    }
}

/// Grid bounds and the number of binding rows in each column.
struct HintLayout {
    /// Popup bounds within the workspace.
    area: Rect,
    /// Maximum number of bindings stacked above the footer.
    rows: usize,
    /// Number of bindings that fit on a page.
    page_size: usize,
}

/// An action available in the hint's footer.
#[derive(Clone, Copy)]
enum HintControl {
    /// Dismiss the entire sequence.
    Close,
    /// Remove the last group key.
    Back,
}

/// Footer controls in display order, with their SVG and caption translation key.
const HINT_CONTROLS: [(HintControl, KeyIcon, &str); 2] = [
    (HintControl::Close, KeyIcon::ESC, "leader.close"),
    (HintControl::Back, KeyIcon::BACKSPACE, "leader.back"),
];

/// Format the group keys in `suffix` with separators after the Space icon.
fn hint_title_path(suffix: &str) -> String {
    let mut title = String::new();
    for key in suffix.chars() {
        title.push_str(" → ");
        if key == '\t' {
            title.push_str(leader_key_label("\t"));
        } else {
            title.push(key);
        }
    }
    title
}

/// Give the leader's internal Tab token a printable label in the menu.
fn leader_key_label(key: &str) -> &str {
    if key == "\t" { "Tab" } else { key }
}

/// Resolve the active locale's captions for the leader footer.
fn hint_captions() -> [(KeyIcon, &'static str); 2] {
    HINT_CONTROLS.map(|(_, icon, caption)| (icon, i18n::text(caption)))
}

/// Align `popup` to the `editor`'s right and bottom edges when the editor is visible.
fn align_hint(popup: Rect, editor: Rect) -> Rect {
    if editor.w == 0 || editor.h == 0 {
        return popup;
    }
    Rect {
        x: (editor.x + editor.w).saturating_sub(popup.w),
        y: (editor.y + editor.h).saturating_sub(popup.h),
        ..popup
    }
}

impl Workspace {
    /// Clear a leader sequence and remove its hint through `cx`.
    pub(super) fn dismiss_leader_hint(&mut self, cx: &mut Context<Self>) {
        if matches!(self.prefix, Some(PendingPrefix::Leader(_))) {
            self.prefix = None;
            self.hint_delay = HintDelay::Idle;
            cx.notify();
        }
    }

    /// Set the pending `prefix` and its retained hint together before scheduling a render through `cx`.
    pub(super) fn show_leader_hint(&mut self, prefix: LeaderPrefix, cx: &mut Context<Self>) {
        self.prefix = Some(PendingPrefix::Leader(prefix));
        self.refresh_sequence_hint(cx);
        cx.notify();
    }

    /// Read pending keys from the shell or the pane that currently owns keyboard input.
    fn active_sequence_hint(&self) -> Option<ActiveHint<'_>> {
        if self.small_modal_open() || self.help.is_open() || self.plugins.owns_input() {
            return None;
        }
        if let Some(PendingPrefix::Leader(prefix)) = self.prefix {
            let availability = self.leader_availability();
            return prefix
                .bindings(availability)
                .next()
                .map(|_| ActiveHint::Leader(prefix, availability));
        }
        if let Some(PendingPrefix::Window(sequence)) = self.prefix {
            return self.window_sequence_hint(sequence).map(ActiveHint::Pane);
        }
        if let Some(session) = &self.file_picker {
            if session.state.busy {
                return None;
            }
            return ActiveHint::for_vim(session.focused_editor()?.vim());
        }
        let project = self.projects.active_project()?;
        match project.focus {
            Focus::Editor => ActiveHint::for_vim(project.editor.vim()),
            Focus::Manager => {
                let Some(PendingPrefix::Manager(key)) = self.prefix else {
                    return None;
                };
                let (group, continuations) = match key {
                    'g' => (
                        "vim_hint.goto_group",
                        vec![("g", "vim_hint.first_entry", false)],
                    ),
                    'z' => ("vim_hint.scroll_group", VIEW_CONTINUATIONS.to_vec()),
                    _ => return None,
                };
                Some(ActiveHint::Pane(SequenceHint {
                    prefix: key.to_string(),
                    group,
                    continuations,
                }))
            }
        }
    }

    /// Start or cancel the opening timer as prefixes change through `cx`.
    pub(super) fn sync_sequence_hint_delay(&mut self, cx: &mut Context<Self>) {
        let Some(prefix) = self
            .active_sequence_hint()
            .and_then(ActiveHint::delay_prefix)
        else {
            self.hint_delay = HintDelay::Idle;
            return;
        };
        match &self.hint_delay {
            HintDelay::Visible => return,
            HintDelay::Waiting {
                prefix: pending, ..
            } if *pending == prefix => return,
            _ => {}
        }
        let timer = cx.background_executor().timer(SEQUENCE_HINT_DELAY);
        let task = cx.spawn(async move |view, cx| {
            timer.await;
            let _ = view.update(cx, |workspace, cx| {
                let HintDelay::Waiting { prefix, .. } = &workspace.hint_delay else {
                    return;
                };
                let current = workspace
                    .active_sequence_hint()
                    .and_then(ActiveHint::delay_prefix);
                if current.as_ref() == Some(prefix) {
                    workspace.hint_delay = HintDelay::Visible;
                    workspace.draw_sequence_hint(cx);
                    cx.notify();
                } else {
                    workspace.sync_sequence_hint_delay(cx);
                }
            });
        });
        self.hint_delay = HintDelay::Waiting {
            prefix,
            _task: task,
        };
    }

    /// Read the popup only after its delay, or immediately for spelling candidates.
    fn visible_sequence_hint(&self) -> Option<ActiveHint<'_>> {
        self.active_sequence_hint().filter(|hint| {
            matches!(self.hint_delay, HintDelay::Visible) || matches!(hint, ActiveHint::Spelling(_))
        })
    }

    /// Synchronize the opening delay and refresh the visible popup through `cx`.
    pub(super) fn refresh_sequence_hint(&mut self, cx: &mut Context<Self>) {
        self.sync_sequence_hint_delay(cx);
        self.draw_sequence_hint(cx);
    }

    /// Rebuild the retained cells of the visible hint through `cx`.
    fn draw_sequence_hint(&self, cx: &mut Context<Self>) {
        let Some(hint) = self.visible_sequence_hint() else {
            return;
        };
        let layout = self.sequence_hint_layout(&hint);
        let bindings = hint.rows();
        let pages = bindings.len().div_ceil(layout.page_size).max(1);
        let page = self.hint_page.min(pages - 1);
        let rows =
            &bindings[page * layout.page_size..((page + 1) * layout.page_size).min(bindings.len())];
        let Rect {
            w: columns,
            h: height,
            ..
        } = layout.area;
        let mut frame = Frame::new(columns, height);
        frame.border_with_title(true, false, false, |frame, slot, color| match &hint {
            ActiveHint::Leader(prefix, _) => {
                let icon = KeyIcon::SPACE;
                frame.key_icon(slot.start, 0, icon, 0.);
                let start = slot.start + icon.columns(self.metrics.cw, 0.);
                let path = hint_title_path(prefix.suffix());
                frame.label(0, start..slot.end, &path, color, &self.spacing)
            }
            ActiveHint::Pane(hint) => {
                let group = format!("{}[+{}]", hint.prefix, i18n::text(hint.group));
                frame.label(0, slot, &group, color, &self.spacing)
            }
            ActiveHint::Spelling(_) => {
                let group = format!("z=[+{}]", i18n::text("vim_hint.spelling_group"));
                frame.label(0, slot, &group, color, &self.spacing)
            }
        });
        let column_width = columns / rows.len().div_ceil(layout.rows).max(1);
        for (column, bindings) in rows.chunks(layout.rows).enumerate() {
            let left = column * column_width;
            // Keep every key visible even when the display has room for only abbreviated captions.
            let padding = if column_width < 14 { 1 } else { 2 };
            let label_width = bindings
                .iter()
                .map(|(label, ..)| self.spacing.columns(label))
                .max()
                .unwrap_or(1)
                .min(column_width.saturating_sub(10));
            let end = left + column_width - padding;
            for (index, &(label, description, group)) in bindings.iter().enumerate() {
                let row = index + 1;
                let label = if self.spacing.columns(label) > label_width {
                    Cow::Owned(label.replace(' ', ""))
                } else {
                    Cow::Borrowed(label)
                };
                let label_end =
                    frame.label(row, left + padding..end, &label, ACCENT, &self.spacing);
                let arrow = (left + 3 + label_width).max(label_end + 1);
                let arrow_end = frame.label(row, arrow..end, "→", MUTED, &self.spacing);
                let mut start = arrow_end + 1;
                let color = if group {
                    start = frame.label(row, start..end, "+", ACCENT, &self.spacing);
                    ACCENT
                } else {
                    TEXT
                };
                let caption = if !matches!(hint, ActiveHint::Leader(..)) {
                    self.spacing
                        .ellipsize(description, end.saturating_sub(start))
                } else {
                    Cow::Borrowed(description)
                };
                frame.label(row, start..end, &caption, color, &self.spacing);
            }
        }
        if matches!(hint, ActiveHint::Leader(..)) {
            frame.key_hints(height - 2, self.metrics.cw, &self.spacing, hint_captions());
        } else {
            frame.key_hints(height - 2, self.metrics.cw, &self.spacing, cancel_hint());
        }
        if pages > 1 {
            let status = format!(
                "{} {}/{}",
                i18n::text("vim_hint.page_keys"),
                page + 1,
                pages
            );
            let [cancel] = key_hint_slots(columns, self.metrics.cw, &self.spacing, cancel_hint());
            frame.styled_label(
                height - 2,
                1..cancel.start.saturating_sub(1),
                &status,
                TEXT,
                &self.spacing,
                LabelStyle::Dimmed,
            );
        }
        self.popup_panel
            .update(cx, |panel, cx| panel.set(frame, cx));
    }

    /// Fit `hint` above the footer and wrap longer pane lists across columns.
    fn sequence_hint_layout(&self, hint: &ActiveHint<'_>) -> HintLayout {
        let pane_hint = !matches!(hint, ActiveHint::Leader(..));
        let anchor = if pane_hint
            && self
                .file_picker
                .as_ref()
                .is_some_and(|session| session.previewing_file())
        {
            self.file_picker_layout().preview_editor_area()
        } else {
            self.active_editor_area()
        };
        let mut rows = hint.len().max(1);
        let mut columns = HINT_COLUMNS;
        let mut page_size = rows;
        if pane_hint {
            // Balance the bindings when the viewport cannot fit a single column.
            let available = self
                .layout
                .visible_rows
                .min(self.layout.rows.saturating_sub(1));
            let available = available.saturating_sub(5).max(1);
            let max_columns = (self.layout.visible_cols / 12).max(1);
            let column_count = rows.div_ceil(available).min(max_columns);
            rows = rows.div_ceil(column_count).min(available);
            page_size = rows * column_count;
            columns = if column_count > 1 {
                40 * column_count
            } else if rows > 8 {
                46
            } else {
                HINT_COLUMNS
            };
            columns = columns.min(self.layout.visible_cols.max(16));
        }
        // Leave a blank row between the bindings and the footer.
        let mut area = align_hint(self.small_popup_area(columns, rows + 4), anchor);
        if pane_hint {
            area.x = area.x.min(self.layout.visible_cols.saturating_sub(area.w));
            area.y = area
                .y
                .min(self.layout.visible_rows.saturating_sub(area.h))
                .max(1);
        }
        HintLayout {
            area,
            rows,
            page_size,
        }
    }

    /// Page a hint by `delta` when its list cannot fit in the current grid.
    pub(super) fn page_sequence_hint(&mut self, delta: isize, cx: &mut Context<Self>) -> bool {
        let Some(hint) = self.visible_sequence_hint() else {
            return false;
        };
        let layout = self.sequence_hint_layout(&hint);
        let pages = hint.len().div_ceil(layout.page_size);
        if pages <= 1 {
            return false;
        }
        self.hint_page = self.hint_page.saturating_add_signed(delta).min(pages - 1);
        self.draw_sequence_hint(cx);
        cx.notify();
        true
    }

    /// Position the active hint in workspace coordinates against the lower-right pane edge.
    pub(super) fn sequence_hint_area(&self) -> Option<Rect> {
        let hint = self.visible_sequence_hint()?;
        Some(self.sequence_hint_layout(&hint).area)
    }

    /// Cancel the incomplete sequence and remove its hint through `cx`.
    pub(super) fn dismiss_sequence_hint(&mut self, cx: &mut Context<Self>) {
        self.hint_page = 0;
        self.hint_delay = HintDelay::Idle;
        if matches!(self.prefix, Some(PendingPrefix::Leader(_))) {
            self.dismiss_leader_hint(cx);
        } else if matches!(
            self.prefix,
            Some(PendingPrefix::Window(_) | PendingPrefix::MarkdownNavigation(_))
        ) {
            self.prefix = None;
            cx.notify();
        } else if matches!(self.prefix, Some(PendingPrefix::Manager(_))) {
            self.prefix = None;
            if let Some(manager) = self
                .projects
                .active_project_mut()
                .and_then(|project| project.manager.as_mut())
            {
                manager.cancel_pending_undo();
                manager.clear_count();
            }
            cx.notify();
        } else if matches!(
            self.active_sequence_hint(),
            Some(ActiveHint::Pane(_) | ActiveHint::Spelling(_))
        ) {
            if let Some(session) = &mut self.file_picker {
                if let Some(editor) = session.focused_editor_mut() {
                    editor.cancel_pending();
                }
            } else {
                self.cancel_pending_pane_input();
            }
            cx.notify();
        }
    }

    /// Cut out `popup` and leave clearance from the borders it covers in the active surface.
    /// `start` describes the visible start page when the file picker is closed.
    pub(super) fn leader_cutouts(
        &self,
        popup: Bounds<Pixels>,
        start: Option<StartLayout>,
    ) -> Vec<Bounds<Pixels>> {
        let mut cuts = vec![popup];
        let metrics = self.metrics;
        if self.file_picker.is_some() {
            let layout = self.file_picker_layout();
            cuts.extend(border_gaps(
                layout.area.pixels(metrics),
                popup,
                metrics,
                metrics,
            ));
            cuts.extend(layout.dividers().into_iter().filter_map(|divider| {
                let bounds = Rect {
                    x: layout.area.x + divider.x,
                    y: layout.area.y + divider.y,
                    ..divider
                }
                .pixels(metrics);
                border_gap(bounds, popup, metrics)
            }));
        } else if let Some(StartLayout::Projects(layout)) = start {
            let pane_metrics = StartPage::content_metrics(metrics);
            for pane in layout.pane_areas() {
                cuts.extend(border_gaps(
                    pane.pixels(pane_metrics),
                    popup,
                    pane_metrics,
                    metrics,
                ));
            }
        } else if self.projects.active_project().is_some() {
            for pane in std::iter::once(self.layout.tree)
                .chain(self.editor_rects().into_iter().map(|(_, area)| area))
            {
                cuts.extend(border_gaps(pane.pixels(metrics), popup, metrics, metrics));
            }
            // The footer can start within the bottom border row, below its visible stroke.
            let footer_top = self.footer_bounds().top();
            for cut in &mut cuts {
                cut.size.height = cut.size.height.min((footer_top - cut.top()).max(px(0.)));
            }
        }
        cuts
    }

    /// Paint the hint in `area`, attaching footer hit targets through `cx`.
    pub(super) fn sequence_hint_element(&self, area: Rect, cx: &Context<Self>) -> impl IntoElement {
        let bounds = area.pixels(self.metrics);
        let mut hint = div()
            .id("sequence-hint")
            .absolute()
            .left(bounds.origin.x)
            .top(bounds.origin.y)
            .w(bounds.size.width)
            .h(bounds.size.height)
            .child(
                self.popup_panel
                    .clone()
                    .cached(StyleRefinement::default().size_full()),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|workspace, _, window, cx| {
                    workspace.dismiss_sequence_hint(cx);
                    workspace.sync_text_input(window, cx);
                    cx.stop_propagation();
                }),
            );
        if let Some(ActiveHint::Spelling(suggestions)) = self.visible_sequence_hint() {
            let layout = self.sequence_hint_layout(&ActiveHint::Spelling(suggestions));
            let pages = suggestions.len().div_ceil(layout.page_size).max(1);
            let start = self.hint_page.min(pages - 1) * layout.page_size;
            let count = (suggestions.len() - start).min(layout.page_size);
            let rows = layout.rows;
            let column_width = area.w / count.div_ceil(rows).max(1);
            for (index, &key) in SPELL_SUGGESTION_KEYS
                .iter()
                .skip(start)
                .take(count)
                .enumerate()
            {
                let col = index / rows * column_width;
                let row = index % rows + 1;
                hint = hint.child(
                    div()
                        .id(("spelling-suggestion", index))
                        .absolute()
                        .left(px((col + 1) as f32 * self.metrics.cw))
                        .top(px(row as f32 * self.metrics.lh))
                        .w(px(column_width.saturating_sub(2) as f32 * self.metrics.cw))
                        .h(px(self.metrics.lh))
                        .cursor_pointer()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |workspace, _, window, cx| {
                            let key = char::from(key).to_string();
                            workspace.key_down(
                                &KeyDownEvent {
                                    keystroke: Keystroke {
                                        modifiers: Default::default(),
                                        key_char: Some(key.clone()),
                                        key,
                                    },
                                    is_held: false,
                                    prefer_character_input: false,
                                },
                                window,
                                cx,
                            );
                            cx.stop_propagation();
                        })),
                );
            }
        }
        let controls = if matches!(self.prefix, Some(PendingPrefix::Leader(_))) {
            let slots = key_hint_slots(area.w, self.metrics.cw, &self.spacing, hint_captions());
            HINT_CONTROLS
                .map(|(control, _, _)| control)
                .into_iter()
                .zip(slots)
                .collect()
        } else {
            let [slot] = key_hint_slots(area.w, self.metrics.cw, &self.spacing, cancel_hint());
            vec![(HintControl::Close, slot)]
        };
        for (control, slot) in controls {
            hint = hint.child(
                div()
                    .id(("sequence-hint-control", slot.start))
                    .absolute()
                    .left(px(slot.start as f32 * self.metrics.cw))
                    .top(px((area.h - 2) as f32 * self.metrics.lh))
                    .w(px(slot.len() as f32 * self.metrics.cw))
                    .h(px(self.metrics.lh))
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |workspace, _, window, cx| {
                        workspace.apply_hint_control(control, cx);
                        workspace.sync_text_input(window, cx);
                        cx.stop_propagation();
                    })),
            );
        }
        hint
    }

    /// Apply the clicked `control` to the active group through `cx`.
    fn apply_hint_control(&mut self, control: HintControl, cx: &mut Context<Self>) {
        if let HintControl::Back = control
            && let Some(PendingPrefix::Leader(prefix)) = self.prefix
            && let Some(parent) = prefix.parent()
        {
            self.show_leader_hint(parent, cx);
        } else {
            self.dismiss_sequence_hint(cx);
        }
    }
}

/// Resolve the pane hint's single Escape caption in the active locale.
fn cancel_hint() -> [(KeyIcon, &'static str); 1] {
    [(KeyIcon::ESC, i18n::text("vim_hint.cancel"))]
}

/// Clear `border` where it crosses `popup`, using the popup's `metrics` to match the visible gaps.
fn border_gap(
    border: Bounds<Pixels>,
    popup: Bounds<Pixels>,
    metrics: Metrics,
) -> Option<Bounds<Pixels>> {
    if border.intersect(&popup).is_empty() {
        return None;
    }
    // The horizontal cut leaves one column plus half a column to the visible rule.
    // Subtract the top rule's half-row inset to get the vertical padding.
    let clearance = metrics.cw + metrics.cw / 2.;
    let padding = point(px(metrics.cw), px((clearance - metrics.lh / 2.).max(0.)));
    let padded = Bounds::from_corners(popup.origin - padding, popup.bottom_right() + padding);
    Some(border.intersect(&padded))
}

/// Use `pane_metrics` for `pane`'s border cells and `popup_metrics` for clearance from `popup`.
pub(super) fn border_gaps(
    pane: Bounds<Pixels>,
    popup: Bounds<Pixels>,
    pane_metrics: Metrics,
    popup_metrics: Metrics,
) -> impl Iterator<Item = Bounds<Pixels>> {
    let row = size(pane.size.width, px(pane_metrics.lh));
    let column = size(px(pane_metrics.cw), pane.size.height);
    [
        Bounds::new(pane.origin, row),
        Bounds::new(point(pane.left(), pane.bottom() - row.height), row),
        Bounds::new(pane.origin, column),
        Bounds::new(point(pane.right() - column.width, pane.top()), column),
    ]
    .into_iter()
    .filter(move |_| !pane.is_empty())
    .filter_map(move |border| border_gap(border, popup, popup_metrics))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::render::exposed_regions;
    use gpui::px;

    /// The text after the Space icon starts with a separator only in a group.
    #[test]
    fn hint_title_lists_each_group_key() {
        assert_eq!(hint_title_path(""), "");
        assert_eq!(hint_title_path("p"), " → p");
        assert_eq!(hint_title_path("gfs"), " → g → f → s");
    }

    /// The editor anchors the popup's corner even when the popup extends over another pane.
    #[test]
    fn hint_uses_editor_border_coordinates() {
        let editor = Rect {
            x: 36,
            y: 1,
            w: 29,
            h: 25,
        };
        let fallback = Rect {
            x: 30,
            y: 14,
            w: HINT_COLUMNS,
            h: 11,
        };
        let metrics = Metrics {
            cw: 9.6,
            lh: 22.,
            font_size: 16.,
            origin: point(px(5.), px(9.)),
        };
        let hint = align_hint(fallback, editor);
        let delta = hint.pixels(metrics).bottom_right() - editor.pixels(metrics).bottom_right();
        assert!(f32::from(delta.x).abs() < 0.001 && f32::from(delta.y).abs() < 0.001);
        assert!(hint.x < editor.x);
        assert!(align_hint(fallback, Rect { w: 0, ..editor }) == fallback);
    }

    /// The visible rules have equal horizontal and vertical clearance with unequal cell dimensions.
    #[test]
    fn hint_cuts_only_overlapping_borders() {
        let metrics = Metrics {
            cw: 10.,
            lh: 20.,
            font_size: 16.,
            origin: point(px(0.), px(0.)),
        };
        let bounds = |left, top, right, bottom| {
            Bounds::from_corners(point(px(left), px(top)), point(px(right), px(bottom)))
        };
        let pane = bounds(0., 0., 600., 400.);
        let hint = bounds(300., 200., 600., 400.);
        let cuts = border_gaps(pane, hint, metrics, metrics).collect::<Vec<_>>();
        assert_eq!(
            cuts,
            [
                bounds(290., 380., 600., 400.),
                bounds(590., 195., 600., 400.)
            ],
        );
        let horizontal_gap = hint.left() + px(metrics.cw / 2.) - cuts[0].left();
        let vertical_gap = hint.top() + px(metrics.lh / 2.) - cuts[1].top();
        assert_eq!(vertical_gap, horizontal_gap);
        let interior = bounds(300., 200., 590., 380.);
        assert!(
            border_gaps(pane, interior, metrics, metrics)
                .next()
                .is_none()
        );
    }

    /// The four clips remove the hint's area while retaining the footer and pane borders.
    #[test]
    fn hint_cutout_preserves_the_surrounding_grid() {
        let bounds = |left, top, right, bottom| {
            Bounds::from_corners(point(px(left), px(top)), point(px(right), px(bottom)))
        };
        let viewport = bounds(0., 0., 800., 420.);
        let hint = bounds(490., 310., 770., 398.);
        assert_eq!(
            exposed_regions(viewport, hint),
            [
                bounds(0., 0., 800., 310.),
                bounds(0., 310., 490., 398.),
                bounds(770., 310., 800., 398.),
                bounds(0., 398., 800., 420.),
            ]
        );
    }
}

#[cfg(all(test, windows))]
mod native_tests;
