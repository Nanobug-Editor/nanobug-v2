//! Modal language selection on the shared lower-right popup surface.

use super::Workspace;
use crate::{
    grid::render::{Frame, LabelStyle, Metrics, Rect, key_hint_slots},
    ui::{
        icons::KeyIcon,
        theme::{ACCENT, SELECTION, TEXT},
    },
};
use gpui::{Bounds, Context, KeyDownEvent, Pixels, Point, Window, px};
use nanobug::{
    animation::scroll::ScrollInput,
    i18n::{self, Locale},
};

/// Supported choices in display order, with labels written in their own languages.
const LANGUAGES: [(Locale, &str); 2] = [
    (Locale::English, "language.english"),
    (Locale::SimplifiedChinese, "language.chinese"),
];
/// Width of the picker in grid columns.
const PICKER_COLUMNS: usize = 28;
/// Total vertical padding in grid rows, split evenly above and below the choices.
const LIST_SPACER_ROWS: usize = 1;
/// Footer row after the title, choices and both halves of the padding.
const PICKER_FOOTER_ROW: usize = 1 + LANGUAGES.len() + LIST_SPACER_ROWS;
/// Total height through the footer and bottom border.
const PICKER_ROWS: usize = PICKER_FOOTER_ROW + 2;

/// Position the choice rows inside `area` with half the padding above them, using `metrics`.
pub(super) fn language_list_bounds(area: Rect, metrics: Metrics) -> Bounds<Pixels> {
    let mut bounds = area.pixels(metrics);
    bounds.origin.y += px((1. + LIST_SPACER_ROWS as f32 / 2.) * metrics.lh);
    bounds.size.height = px(LANGUAGES.len() as f32 * metrics.lh);
    bounds
}

/// Return the apply and close hints in footer order with their active captions.
fn picker_hints() -> [(KeyIcon, &'static str); 2] {
    [
        (KeyIcon::ENTER, i18n::text("language.apply")),
        (KeyIcon::ESC, i18n::text("language.close")),
    ]
}

impl Workspace {
    /// Open the picker in `window` with the active language highlighted, then repaint through `cx`.
    pub(super) fn open_language_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.language_selection = Some(i18n::locale());
        self.hovered_close = None;
        self.resizing = false;
        self.projects.active.editor.dragging = None;
        self.projects.active.manager.click = None;
        self.repaint([ScrollInput::Advance; 3], window, cx);
    }

    /// Return the picker's grid rectangle while it owns input.
    pub(super) fn language_picker_area(&self) -> Option<Rect> {
        self.language_selection
            .map(|_| self.small_popup_area(PICKER_COLUMNS, PICKER_ROWS))
    }

    /// Return the selected choice's row-start caret in workspace grid coordinates.
    pub(super) fn language_caret(&self) -> Option<(f32, f32)> {
        let area = self.language_picker_area()?;
        let row = LANGUAGES
            .iter()
            .position(|&(locale, _)| self.language_selection == Some(locale))?;
        let list = language_list_bounds(area, self.metrics);
        let top = f32::from(list.origin.y - self.metrics.origin.y) / self.metrics.lh;
        Some(((area.x + 1) as f32, top + row as f32))
    }

    /// Retain the picker border and footer separately from the vertically inset choices through `cx`.
    pub(super) fn refresh_language_picker(&self, cx: &mut Context<Self>) {
        let Some(selected) = self.language_selection else {
            return;
        };
        let current = i18n::locale();
        let spacing = &self.spacing;
        let mut frame = Frame::new(PICKER_COLUMNS, PICKER_ROWS);
        frame.border(i18n::text("language.title"), true, false, spacing);
        frame.key_hints(PICKER_FOOTER_ROW, self.metrics.cw, spacing, picker_hints());
        self.popup_panel
            .update(cx, |panel, cx| panel.set(frame, cx));

        let mut choices = Frame::new(PICKER_COLUMNS, LANGUAGES.len());
        for (row, (locale, key)) in LANGUAGES.into_iter().enumerate() {
            let color = if locale == selected {
                choices.tint(row, 1, PICKER_COLUMNS - 1, SELECTION);
                ACCENT
            } else {
                TEXT
            };
            choices.label(row, 3..PICKER_COLUMNS - 4, i18n::text(key), color, spacing);
            if locale == current {
                choices.styled_label(
                    row,
                    PICKER_COLUMNS - 3..PICKER_COLUMNS - 1,
                    "✓",
                    ACCENT,
                    spacing,
                    LabelStyle::Symbol,
                );
            }
        }
        self.language_list
            .update(cx, |panel, cx| panel.set(choices, cx));
    }

    /// Route `event` while `selected` is highlighted in the modal picker in `window` through `cx`.
    pub(super) fn language_picker_key(
        &mut self,
        selected: Locale,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let modifiers = event.keystroke.modifiers;
        if modifiers.control
            || modifiers.alt
            || modifiers.platform
            || modifiers.function
            || modifiers.shift
        {
            return;
        }
        let raw = event.keystroke.key.as_str();
        let next = match raw {
            "up" | "k" => Locale::English,
            "down" | "j" => Locale::SimplifiedChinese,
            "enter" | "escape" if !event.is_held => {
                self.held_close_key = Some(raw.into());
                self.finish_language_picker((raw == "enter").then_some(selected), window, cx);
                return;
            }
            _ => return,
        };
        if next != selected {
            self.language_selection = Some(next);
            self.refresh_language_picker(cx);
            self.update_cursor(window, cx);
            cx.notify();
        }
    }

    /// Handle a left press at window-local `position`, returning whether the picker consumed it.
    /// Activate a clicked option or footer control, or dismiss on an outside click in `window` through `cx`.
    pub(super) fn press_language_picker(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(area) = self.language_picker_area() else {
            return false;
        };
        let (x, y) = self.metrics.cell(position);
        if !area.contains(x, y) {
            self.finish_language_picker(None, window, cx);
            return true;
        }
        let row = y - area.y;
        let column = x - area.x;
        if row == PICKER_FOOTER_ROW {
            let [apply, close] = key_hint_slots(
                PICKER_COLUMNS,
                self.metrics.cw,
                &self.spacing,
                picker_hints(),
            );
            if apply.contains(&column) {
                self.finish_language_picker(self.language_selection, window, cx);
            } else if close.contains(&column) {
                self.finish_language_picker(None, window, cx);
            }
        } else if column > 0 && column < area.w - 1 {
            let list = language_list_bounds(area, self.metrics);
            if list.contains(&position) {
                let index = (f32::from(position.y - list.origin.y) / self.metrics.lh) as usize;
                if let Some(&(locale, _)) = LANGUAGES.get(index) {
                    self.finish_language_picker(Some(locale), window, cx);
                }
            }
        }
        true
    }

    /// Apply `choice` if present and repaint `window` through `cx` with pane input restored.
    fn finish_language_picker(
        &mut self,
        choice: Option<Locale>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.language_selection = None;
        if let Some(locale) = choice
            && i18n::set_locale(locale)
        {
            // Status feedback is already formatted in the previous language.
            for project in self.projects.iter_mut() {
                project.status.clear();
                for buffer in &mut project.editor.buffers {
                    buffer.vim.message.clear();
                }
            }
        }
        self.repaint([ScrollInput::Advance; 3], window, cx);
    }
}
