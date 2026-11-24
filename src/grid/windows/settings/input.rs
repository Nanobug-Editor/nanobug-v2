//! Directory navigation and control editing share the same actions for pointer and keyboard.

use super::*;
use crate::{
    grid::render::{Metrics, Rect},
    ui::text::TextSpacing,
};
use gpui::{KeyDownEvent, Pixels, Point};
use nanobug::animation::scroll::ScrollInput;
use serde_json::json;

/// Validate typed color overrides and numeric controls without lossy conversion.
fn parse_input(kind: &Kind, text: &str) -> Option<Result<Value, String>> {
    if matches!(kind, Kind::Color { .. }) {
        let value = text.trim();
        return Some(
            if value.is_empty()
                || value
                    .strip_prefix('#')
                    .is_some_and(|s| s.len() == 6 && s.bytes().all(|b| b.is_ascii_hexdigit()))
            {
                Ok(Value::from(value.to_ascii_lowercase()))
            } else {
                Err(i18n::text("settings_core.invalid_fg").into())
            },
        );
    }
    let (min, max, number) = match kind {
        Kind::OptionalNumber { .. } if text.trim().is_empty() => return Some(Ok(Value::Null)),
        Kind::Number { min, max } | Kind::OptionalNumber { min, max } => (
            min,
            max,
            text.trim()
                .parse::<i64>()
                .ok()
                .filter(|v| (*min..=*max).contains(v))
                .map(Value::from),
        ),
        Kind::Decimal { min, max } => (
            min,
            max,
            text.trim()
                .parse::<f64>()
                .ok()
                .filter(|v| ((*min as f64)..=(*max as f64)).contains(v))
                .map(Value::from),
        ),
        _ => return None,
    };
    Some(number.ok_or_else(|| i18n::format("settings.number_range", &[("min", min), ("max", max)])))
}

impl Settings {
    /// Native text is accepted only while a value editor has focus.
    pub(in crate::grid) fn accepts_text(&self) -> bool {
        matches!(self.editor, Some(Editor::Input { .. }))
    }

    pub(in crate::grid) fn insert_text(&mut self, insert: &str) {
        if let Some(Editor::Input { text, field }) = &mut self.editor {
            if insert.contains(['\n', '\r']) {
                self.error = i18n::text("settings.single_line").into();
            } else if field.insert(text, insert) {
                self.validate_input();
            }
            self.composition.clear();
        }
    }

    fn validate_input(&mut self) {
        let Some(Editor::Input { text, .. }) = &self.editor else {
            return;
        };
        self.error = self
            .field()
            .and_then(|field| parse_input(&field.kind, text))
            .and_then(Result::err)
            .unwrap_or_default();
    }

    /// Dispatch one modal key. Printable text goes through the workspace native input handler.
    pub(in crate::grid) fn key(&mut self, event: &KeyDownEvent, area: Rect) -> Option<Action> {
        let key = &event.keystroke;
        let raw = key.key.as_str();
        if self.accepts_text() {
            if (key.modifiers.control
                || key.modifiers.platform
                || key.modifiers.alt
                || key.modifiers.function)
                && let Some(Editor::Input { text, field }) = &mut self.editor
            {
                field.edit(text, key);
                return None;
            }
            match raw {
                "escape" => self.clear_editor(),
                "enter" | "tab" => {
                    self.finish_editor();
                }
                _ => {
                    if let Some(Editor::Input { text, field }) = &mut self.editor
                        && field.edit(text, key) == Some(true)
                    {
                        self.validate_input();
                    }
                }
            }
            return None;
        }
        if key.modifiers.control
            || key.modifiers.alt
            || key.modifiers.platform
            || key.modifiers.function
        {
            return None;
        }
        if self.editor.is_some() {
            let count = self.choices().len();
            let step = self
                .selector_layout(view::pane_bounds(area).1)
                .map_or(1, |menu| menu.rows.h);
            if let Some(Editor::List { cursor, .. }) = &mut self.editor {
                match raw {
                    "up" | "k" | "down" | "j" | "pageup" | "pagedown" => {
                        let delta = match raw {
                            "up" | "k" => -1,
                            "down" | "j" => 1,
                            "pageup" => -(step as isize),
                            _ => step as isize,
                        };
                        *cursor = crate::ui::components::grid_list::GridList::moved(
                            *cursor, delta, count,
                        );
                    }
                    "home" => *cursor = 0,
                    "end" => *cursor = count.saturating_sub(1),
                    "escape" => self.clear_editor(),
                    "enter" | "tab" if !event.is_held => {
                        self.finish_editor();
                    }
                    "space" if !event.is_held => self.choose(),
                    _ => {}
                }
            }
            self.reveal = true;
            return None;
        }
        if event.is_held
            && !matches!(
                raw,
                "up" | "down" | "left" | "right" | "j" | "k" | "h" | "l" | "pageup" | "pagedown"
            )
        {
            return None;
        }
        if let Some(button) = buttons().key(raw) {
            return self.press(button);
        }
        self.reveal = true;
        match raw {
            "escape" => return Some(Action::Cancel),
            "tab" => {
                self.pane = if self.pane == Pane::Tree {
                    Pane::Form
                } else {
                    Pane::Tree
                };
            }
            "up" | "k" => self.move_row(-1),
            "down" | "j" => self.move_row(1),
            "pageup" => self.move_row(-5),
            "pagedown" => self.move_row(5),
            "home" => self.move_row(isize::MIN),
            "end" => self.move_row(isize::MAX),
            "left" | "h" if self.pane == Pane::Tree => {
                if !self.set_expanded(self.tree_row, false)
                    && let Some(parent) = self.nodes.get(self.tree_row).and_then(|n| n.parent)
                {
                    self.select_node(parent);
                }
            }
            "right" | "l" if self.pane == Pane::Tree => {
                if self.nodes.get(self.tree_row).is_some_and(|n| n.branch) {
                    if !self.set_expanded(self.tree_row, true) {
                        self.move_row(1);
                    }
                } else {
                    self.pane = Pane::Form;
                }
            }
            "enter" | "space" => {
                if self.pane == Pane::Tree {
                    self.activate_node(self.tree_row);
                } else {
                    self.start_editor();
                }
            }
            _ => {}
        }
        None
    }

    fn move_row(&mut self, delta: isize) {
        if self.pane == Pane::Tree {
            let current = self.visible.position(self.tree_row).unwrap_or(0);
            if let Some(id) = self.visible.offset(current, delta) {
                self.select_node(id);
            }
        } else {
            self.row = self
                .row
                .saturating_add_signed(delta)
                .min(self.forms[self.page].page.fields.len().saturating_sub(1));
        }
    }

    fn select_node(&mut self, id: usize) {
        self.tree_row = id;
        if let Some(index) = self.nodes.get(id).and_then(|node| node.page)
            && index != self.page
        {
            self.page = index;
            self.row = 0;
            self.clear_editor();
        }
        self.reveal = true;
    }

    fn activate_node(&mut self, id: usize) {
        self.select_node(id);
        if self.nodes.get(id).is_some_and(|node| node.branch) {
            self.set_expanded(id, !self.expanded.contains(&id));
        } else {
            self.pane = Pane::Form;
        }
    }

    fn start_editor(&mut self) {
        let Some(control) = self.field() else { return };
        let value = &self.forms[self.page].draft[self.row];
        self.editor = match &control.kind {
            Kind::Notice => return,
            Kind::Toggle => {
                self.forms[self.page].draft[self.row] = json!(!value.as_bool().unwrap_or(false));
                None
            }
            Kind::Select(choices) | Kind::Multi(choices) => {
                if choices.is_empty() {
                    return;
                }
                let cursor = choices
                    .iter()
                    .position(|c| value == &c.id)
                    .unwrap_or_default();
                Some(Editor::List {
                    cursor,
                    value: value.clone(),
                })
            }
            Kind::Text
            | Kind::Color { .. }
            | Kind::Number { .. }
            | Kind::OptionalNumber { .. }
            | Kind::Decimal { .. } => {
                let text = if value.is_null() {
                    String::new()
                } else {
                    value
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| value.to_string())
                };
                let mut field = GridTextField::default();
                field.place(&text, 0, false);
                field.place(&text, text.len(), true);
                Some(Editor::Input { text, field })
            }
        };
        self.error.clear();
        self.reveal = true;
    }

    pub(super) fn choices(&self) -> &[Choice] {
        match self.field().map(|field| &field.kind) {
            Some(Kind::Select(choices) | Kind::Multi(choices)) => choices,
            _ => &[],
        }
    }

    /// Toggle the pointed checklist item without dismissing the menu; single choice finishes immediately.
    fn choose(&mut self) {
        let Some(Editor::List { cursor, .. }) = self.editor.as_ref() else {
            return;
        };
        let Some(choice) = self.choices().get(*cursor).map(|choice| json!(choice.id)) else {
            return;
        };
        if matches!(self.field().map(|f| &f.kind), Some(Kind::Multi(_))) {
            if let Some(Editor::List { value, .. }) = &mut self.editor
                && let Some(list) = value.as_array_mut()
            {
                if let Some(i) = list.iter().position(|v| v == &choice) {
                    list.remove(i);
                } else {
                    list.push(choice);
                }
            }
        } else {
            self.finish_editor();
        }
    }

    /// Resolve an active editor once for the dirty indicator, preview and eventual commit.
    pub(super) fn editing_value(&self) -> Option<Result<Value, String>> {
        let editor = self.editor.as_ref()?;
        let field = self.field()?;
        let next = match (editor, &field.kind) {
            (Editor::Input { text, .. }, kind) => {
                return Some(parse_input(kind, text).unwrap_or_else(|| Ok(json!(text))));
            }
            (Editor::List { cursor, .. }, Kind::Select(choices)) => {
                json!(choices.get(*cursor)?.id)
            }
            (Editor::List { value, .. }, Kind::Multi(choices)) => {
                let selected = value.as_array().map(Vec::as_slice).unwrap_or_default();
                Value::Array(
                    choices
                        .iter()
                        .filter(|choice| selected.iter().any(|s| s == &choice.id))
                        .map(|choice| json!(choice.id))
                        .collect(),
                )
            }
            _ => return None,
        };
        Some(Ok(next))
    }

    /// Commit the active control into the page draft. Invalid numeric input keeps focus.
    fn finish_editor(&mut self) -> bool {
        let next = match self.editing_value() {
            Some(Ok(value)) => value,
            Some(Err(error)) => {
                self.error = error;
                return false;
            }
            None => return self.editor.is_none(),
        };
        self.forms[self.page].draft[self.row] = next;
        self.clear_editor();
        self.reveal = true;
        true
    }

    fn press(&mut self, button: Button) -> Option<Action> {
        match button {
            Button::Cancel => {
                self.clear_editor();
                Some(Action::Cancel)
            }
            Button::Reset | Button::Restore => {
                self.clear_editor();
                let form = &mut self.forms[self.page];
                for (field, draft) in form.page.fields.iter().zip(&mut form.draft) {
                    draft.clone_from(if matches!(button, Button::Reset) {
                        &field.default
                    } else {
                        &field.value
                    });
                }
                None
            }
            Button::Apply | Button::Confirm if self.finish_editor() => {
                Some(if matches!(button, Button::Apply) {
                    Action::Apply
                } else {
                    Action::Confirm
                })
            }
            _ => None,
        }
    }

    /// Hit-test the same measured rows and footer slots used to draw the dialog.
    pub(in crate::grid) fn click(
        &mut self,
        point: Point<Pixels>,
        area: Rect,
        metrics: Metrics,
        spacing: &TextSpacing,
    ) -> Option<Action> {
        let (col, row) = metrics.cell(point);
        if !area.contains(col, row) {
            return None;
        }
        let layout = view::Layout::new(area, spacing);
        if let Some(menu) = self.selector_layout(layout.body)
            && menu.contains(point, metrics)
        {
            if let Some(item) = menu.hit(point, metrics, self.selector_scroll.position())
                && let Some(Editor::List { cursor, .. }) = &mut self.editor
            {
                *cursor = item;
                self.choose();
            }
            return None;
        }
        if let Some(button) = GridButtonRow::hit_at(&layout.buttons, col - area.x, row - area.y) {
            return self.press(button);
        }
        if layout.tree.contains(col, row) {
            if !self.finish_editor() {
                return None;
            }
            if let Some(hit) = self.visible.hit(
                layout.tree.pixels(metrics),
                point,
                metrics.lh,
                self.tree_scroll.position(),
            ) {
                self.pane = Pane::Tree;
                if self.nodes[hit.id].branch {
                    self.activate_node(hit.id);
                } else {
                    self.select_node(hit.id);
                }
            }
            return None;
        }
        if !layout.body.contains(col, row) {
            return None;
        }
        let index = view::hit_row(point, layout.body, metrics, &self.form_scroll);
        let rows = self.body_rows();
        let value_start = layout.body.x + view::value_column(layout.body.w);
        match rows.get(index) {
            Some(view::BodyRow::Field(field)) => {
                if self.row == *field && self.accepts_text() {
                    if col > value_start
                        && let Some(Editor::Input { text, field }) = &mut self.editor
                    {
                        let x = f32::from(point.x - metrics.at((value_start + 1) as f32, 0.).x)
                            / metrics.cw;
                        let byte = field
                            .layout(
                                text,
                                &self.composition,
                                view::input_width(layout.body.w),
                                spacing,
                            )
                            .byte_at(x, spacing);
                        if self.composition.is_empty() {
                            field.place(text, byte, false);
                        }
                    }
                } else if self.row == *field && self.editor.is_some() {
                    self.finish_editor();
                } else if self.finish_editor() {
                    self.pane = Pane::Form;
                    self.row = *field;
                    if col >= value_start {
                        self.start_editor();
                    }
                    self.reveal = true;
                }
            }
            _ => {
                self.finish_editor();
            }
        }
        None
    }

    /// A wheel gesture scrolls only its own viewport and retains partial touchpad rows.
    pub(in crate::grid) fn wheel(
        &mut self,
        point: Point<Pixels>,
        rows: f32,
        area: Rect,
        metrics: Metrics,
        now: f64,
    ) {
        let (col, row) = metrics.cell(point);
        let (tree, body) = view::pane_bounds(area);
        let (side, height, count) = if let Some(menu) = self.selector_layout(body)
            && menu.contains(point, metrics)
        {
            (2, menu.rows.h, self.choices().len())
        } else if tree.contains(col, row) {
            (0, tree.h, self.visible.len())
        } else if body.contains(col, row) {
            (1, body.h, self.body_rows().len())
        } else {
            return;
        };
        self.wheel_remainder[side] += rows;
        let whole = self.wheel_remainder[side].trunc() as isize;
        self.wheel_remainder[side] -= whole as f32;
        self.reveal = false;
        let scroll = match side {
            0 => &mut self.tree_scroll,
            1 => &mut self.form_scroll,
            _ => &mut self.selector_scroll,
        };
        scroll.track_rows(ScrollInput::Wheel(whole), None, height, count, now);
    }

    pub(in crate::grid) fn scrolling(&self) -> bool {
        self.tree_scroll.is_active()
            || self.form_scroll.is_active()
            || self.selector_scroll.is_active()
    }

    /// Keep the focused row in view after navigation, advancing the selector within its own clip.
    pub(in crate::grid) fn advance(&mut self, area: Rect, now: f64) {
        let (tree, body) = view::pane_bounds(area);
        let tree_at = self.visible.position(self.tree_row).unwrap_or(0);
        let body_rows = self.body_rows();
        let form_at = body_rows
            .iter()
            .position(|item| *item == view::BodyRow::Field(self.row))
            .unwrap_or(0);
        let track = |scroll: &mut GridScroll, at: usize, height: usize, len: usize| {
            let input = if self.reveal {
                ScrollInput::Position(
                    scroll
                        .target()
                        .clamp(at.saturating_sub(height.saturating_sub(1)), at),
                )
            } else {
                ScrollInput::Advance
            };
            scroll.track_rows(input, None, height, len, now);
        };
        track(&mut self.tree_scroll, tree_at, tree.h, self.visible.len());
        track(&mut self.form_scroll, form_at, body.h, body_rows.len());
        if let Some(Editor::List { cursor, .. }) = self.editor {
            if let Some(menu) = self.selector_layout(body) {
                let count = self.choices().len();
                track(&mut self.selector_scroll, cursor, menu.rows.h, count);
            } else {
                // The anchor may temporarily be offscreen. A manual form scroll keeps the
                // selection intact, and a keyboard reveal resumes when the field comes into view.
                self.selector_scroll.advance(now);
                self.reveal &= self.form_scroll.is_active();
                return;
            }
        }
        self.reveal = false;
    }
}
