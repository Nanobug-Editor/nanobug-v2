//! Contextual leader hints on the shared workspace material.

use super::{
    Workspace,
    keyboard::{LeaderPrefix, PendingPrefix},
};
use crate::{
    grid::render::{Frame, Rect, key_hint_slots},
    ui::{
        icons::KeyIcon,
        theme::{ACCENT, MUTED, TEXT},
    },
};
use gpui::{
    Bounds, Context, InteractiveElement, IntoElement, MouseButton, ParentElement, Pixels,
    StatefulInteractiveElement, StyleRefinement, Styled, div, point, px,
};
use nanobug::i18n;

/// Keep the hint at the same width when switching between leader groups.
const HINT_COLUMNS: usize = 34;

/// An action available in the hint's footer.
#[derive(Clone, Copy)]
enum HintControl {
    /// Dismiss the entire leader sequence.
    Close,
    /// Remove the last group key.
    Back,
}

/// Footer controls in display order, with their SVG and caption translation key.
const HINT_CONTROLS: [(HintControl, KeyIcon, &str); 2] = [
    (HintControl::Close, KeyIcon::ESC, "leader.close"),
    (HintControl::Back, KeyIcon::BACKSPACE, "leader.back"),
];

/// Height of the `prefix` group, including both borders, the gap and the footer.
fn hint_rows(prefix: LeaderPrefix, project_active: bool, manager_available: bool) -> usize {
    prefix.bindings(project_active, manager_available).count() + 4
}

/// Build the hint title by appending each group key in `suffix`.
fn hint_title(suffix: &str) -> String {
    let mut title = String::from("Leader");
    for key in suffix.chars() {
        title.push_str(" → ");
        title.push(key);
    }
    title
}

/// Resolve the active locale's captions for the leader footer.
fn hint_captions() -> [(KeyIcon, &'static str); 2] {
    HINT_CONTROLS.map(|(_, icon, caption)| (icon, i18n::text(caption)))
}

impl Workspace {
    /// Clear a leader sequence and remove its hint through `cx`.
    pub(super) fn dismiss_leader_hint(&mut self, cx: &mut Context<Self>) {
        if matches!(self.prefix, Some(PendingPrefix::Leader(_))) {
            self.prefix = None;
            cx.notify();
        }
    }

    /// Set the pending `prefix` and its retained hint together before scheduling a render through `cx`.
    pub(super) fn show_leader_hint(&mut self, prefix: LeaderPrefix, cx: &mut Context<Self>) {
        let project_active = self.projects.active_project().is_some();
        let manager_available = self.projects.manager_available();
        let height = hint_rows(prefix, project_active, manager_available);
        let mut frame = Frame::new(HINT_COLUMNS, height);
        let title = hint_title(prefix.suffix());
        frame.border(&title, true, false, &self.spacing);
        let arrow_column = 3 + prefix
            .bindings(project_active, manager_available)
            .map(|(label, _)| self.spacing.columns(label))
            .max()
            .unwrap_or(1);
        for (index, (label, binding)) in prefix
            .bindings(project_active, manager_available)
            .enumerate()
        {
            let row = index + 1;
            frame.label(row, 2..arrow_column, label, ACCENT, &self.spacing);
            frame.label(
                row,
                arrow_column..arrow_column + 2,
                "→",
                MUTED,
                &self.spacing,
            );
            let mut start = arrow_column + 3;
            let end = HINT_COLUMNS - 2;
            let color = if binding.is_group() {
                start = frame.label(row, start..end, "+", ACCENT, &self.spacing);
                ACCENT
            } else {
                TEXT
            };
            frame.label(
                row,
                start..end,
                i18n::text(binding.description_key),
                color,
                &self.spacing,
            );
        }
        frame.key_hints(height - 2, self.metrics.cw, &self.spacing, hint_captions());
        self.prefix = Some(PendingPrefix::Leader(prefix));
        self.popup_panel
            .update(cx, |panel, cx| panel.set(frame, cx));
        cx.notify();
    }

    /// Place the current group's hint at the lower right, above the workspace footer.
    pub(super) fn leader_hint_area(&self) -> Option<Rect> {
        let Some(PendingPrefix::Leader(prefix)) = self.prefix else {
            return None;
        };
        let project_active = self.projects.active_project().is_some();
        Some(self.small_popup_area(
            HINT_COLUMNS,
            hint_rows(prefix, project_active, self.projects.manager_available()),
        ))
    }

    /// Paint the hint in `area`, attaching footer hit targets through `cx`.
    pub(super) fn leader_hint_element(&self, area: Rect, cx: &Context<Self>) -> impl IntoElement {
        let bounds = area.pixels(self.metrics);
        let mut hint = div()
            .id("leader-hint")
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
                cx.listener(|workspace, _, _, cx| {
                    workspace.dismiss_leader_hint(cx);
                    cx.stop_propagation();
                }),
            );
        let slots = key_hint_slots(
            HINT_COLUMNS,
            self.metrics.cw,
            &self.spacing,
            hint_captions(),
        );
        for ((control, _, _), slot) in HINT_CONTROLS.into_iter().zip(slots) {
            hint = hint.child(
                div()
                    .id(("leader-hint-control", slot.start))
                    .absolute()
                    .left(px(slot.start as f32 * self.metrics.cw))
                    .top(px((area.h - 2) as f32 * self.metrics.lh))
                    .w(px(slot.len() as f32 * self.metrics.cw))
                    .h(px(self.metrics.lh))
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |workspace, _, _, cx| {
                        workspace.apply_hint_control(control, cx);
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
            self.dismiss_leader_hint(cx);
        }
    }
}

/// Split the `viewport` around the `popup` so only the window material shows through it.
pub(super) fn exposed_regions(
    viewport: Bounds<Pixels>,
    popup: Bounds<Pixels>,
) -> [Bounds<Pixels>; 4] {
    let hole = viewport.intersect(&popup);
    if hole.is_empty() {
        return [
            viewport,
            Bounds::default(),
            Bounds::default(),
            Bounds::default(),
        ];
    }
    [
        Bounds::from_corners(viewport.origin, point(viewport.right(), hole.top())),
        Bounds::from_corners(
            point(viewport.left(), hole.top()),
            point(hole.left(), hole.bottom()),
        ),
        Bounds::from_corners(
            point(hole.right(), hole.top()),
            point(viewport.right(), hole.bottom()),
        ),
        Bounds::from_corners(
            point(viewport.left(), hole.bottom()),
            viewport.bottom_right(),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::px;

    /// The title separates each group key and omits the separator at the root.
    #[test]
    fn hint_title_lists_each_group_key() {
        assert_eq!(hint_title(""), "Leader");
        assert_eq!(hint_title("q"), "Leader → q");
        assert_eq!(hint_title("gfs"), "Leader → g → f → s");
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
