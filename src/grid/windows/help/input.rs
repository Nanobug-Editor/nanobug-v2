//! Modal keyboard and pointer routing for the help popup.

use super::{
    HelpWindow,
    list::{HelpList, Scope},
};
use crate::{
    grid::render::{Metrics, Rect},
    ui::keys::normal_character,
};
use gpui::{KeyDownEvent, Pixels, Point};
use nanobug::animation::scroll::ScrollInput;

/// Effect of a help key on the workspace and its native input handler.
pub(in crate::grid) enum HelpCommand {
    /// Close the popup and return input to its originating pane.
    Close,
    /// Repaint help after applying the given scroll intent.
    Update(ScrollInput),
    /// Let GPUI deliver committed or composing text to the filter.
    Text,
    /// Commit clipboard text to the filter.
    Paste,
    /// Leave the filter prompt, restoring the previous query if `cancel` is true.
    FinishFilter { cancel: bool },
    /// Consume a key without changing the view.
    Ignore,
}

impl HelpWindow {
    /// Handle `event` in a viewport of `height` list rows at animation time `now`.
    pub(in crate::grid) fn key(
        &mut self,
        event: &KeyDownEvent,
        height: usize,
        now: f64,
    ) -> HelpCommand {
        self.list.as_mut().map_or(HelpCommand::Ignore, |list| {
            list.key(event, height, now, !self.composition.is_empty())
        })
    }

    /// End search at animation time `now`; restore the old query when `cancel` is true.
    /// Return whether a prompt was open and needs repainting.
    pub(in crate::grid) fn finish_filter(&mut self, cancel: bool, now: f64) -> bool {
        let Some(list) = self.list.as_mut().filter(|list| list.is_filtering()) else {
            return false;
        };
        let preedit = self.composition.take();
        if !cancel {
            list.commit_filter(&preedit, now);
        }
        list.finish_filter(cancel, now);
        true
    }

    /// Select the row at window-local `position` inside `area`, using `metrics` for hit testing.
    /// Snap at `now` so the clicked row stays under the pointer as it settles.
    pub(in crate::grid) fn press(
        &mut self,
        position: Point<Pixels>,
        area: Rect,
        metrics: Metrics,
        now: f64,
    ) -> bool {
        let body = self.body(area);
        let Some(list) = &mut self.list else {
            return false;
        };
        list.cancel_navigation();
        let metrics = Self::content_metrics(metrics);
        let (x, y) = metrics.cell(position);
        if !body.contains(x, y) || !self.composition.is_empty() {
            return false;
        }
        let local = f32::from(position.y - metrics.origin.y) / metrics.lh - body.y as f32;
        let row = (local + list.scroll.position()).floor() as usize;
        if row >= list.len() {
            return false;
        }
        list.finish_filter(false, now);
        list.selected = row;
        list.scroll.snap(row.saturating_sub(y - body.y), now);
        true
    }
}

impl HelpList {
    /// Resolve `event` using the visible `height`, monotonic `now` and whether IME is `composing`.
    pub(super) fn key(
        &mut self,
        event: &KeyDownEvent,
        height: usize,
        now: f64,
        composing: bool,
    ) -> HelpCommand {
        use HelpCommand::{Close, FinishFilter, Ignore, Paste, Text, Update};

        let key = &event.keystroke;
        let raw = key.key.as_str();
        let modifiers = key.modifiers;
        let modified = modifiers.alt || modifiers.platform || modifiers.function;
        if self.is_filtering() {
            if raw == "v" && (modifiers.control || modifiers.platform) && !modifiers.alt {
                return Paste;
            }
            if composing || modified || modifiers.control || modifiers.shift {
                return Text;
            }
            return match raw {
                "enter" | "escape" => FinishFilter {
                    cancel: raw == "escape",
                },
                "backspace" => {
                    self.filter_backspace(now);
                    Update(ScrollInput::Advance)
                }
                _ => Text,
            };
        }

        if modified || modifiers.control {
            self.cancel_navigation();
            return Ignore;
        }
        let character = normal_character(key);
        let printable = character.as_deref().unwrap_or(raw);
        if event.is_held && matches!(printable, "q" | "escape" | "/") {
            return Ignore;
        }
        if let [digit @ b'0'..=b'9'] = printable.as_bytes()
            && (*digit != b'0' || self.count != 0)
        {
            if self.count < 1000 {
                self.count = self.count * 10 + usize::from(*digit - b'0');
            }
            return Ignore;
        }
        let count = std::mem::take(&mut self.count);
        let pending_g = self.pending_g.take();
        let rows = count.max(1) as isize;
        match printable {
            "escape" | "q" if !modifiers.shift => Close,
            "/" => {
                self.start_filter();
                Update(ScrollInput::Advance)
            }
            "h" | "left" | "l" | "right" => {
                let scope = if matches!(printable, "h" | "left") {
                    Scope::Window
                } else {
                    Scope::Global
                };
                self.select_scope(scope, now);
                Update(ScrollInput::Advance)
            }
            "g" => {
                if let Some(prefix) = pending_g {
                    Update(self.select_row(prefix * count.max(1) - 1))
                } else {
                    self.pending_g = Some(count.max(1));
                    Ignore
                }
            }
            _ if pending_g.is_some() => Ignore,
            "G" => Update(self.select_row(if count == 0 {
                self.len().saturating_sub(1)
            } else {
                count - 1
            })),
            "j" | "down" => Update(self.move_selection(rows)),
            "k" | "up" => Update(self.move_selection(-rows)),
            "pageup" => Update(self.move_selection(-(height as isize))),
            "pagedown" => Update(self.move_selection(height as isize)),
            "n" | "N" => Update(self.repeat_search(printable == "N")),
            _ => Ignore,
        }
    }
}
