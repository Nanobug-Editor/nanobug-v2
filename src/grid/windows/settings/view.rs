//! Fit and draw the settings frame, two independent smooth viewports, and value editors.

use super::*;
use crate::{
    grid::render::{Frame, GridLayer, LabelStyle, Metrics, OutlineStyle, Rect},
    ui::{
        text::{TextSpacing, UNITS_PER_COLUMN},
        theme::{self, ACCENT, ACTIVE_FILE, CURRENT, ERROR_TEXT, GOLD, MUTED, SELECTION, TEXT},
    },
};
use gpui::{App, Pixels, Point};
use nanobug::vim::HighlightStyle;
use nanobug_plugin_sdk::language::SyntaxKind;

pub(super) struct Layout {
    pub(super) tree: Rect,
    pub(super) body: Rect,
    pub(super) buttons: Vec<(Button, Rect)>,
}

/// Very small or unmeasured surfaces cannot fit both columns and the fixed action bar.
fn fits_form(area: Rect) -> bool {
    area.w >= 32 && area.h >= 9
}

/// Reserve a second button row on narrow grids while keeping the final action at bottom right.
fn footer_rows(width: usize) -> usize {
    if width < 68 { 5 } else { 4 }
}

pub(super) fn pane_bounds(area: Rect) -> (Rect, Rect) {
    if !fits_form(area) {
        let empty = Rect {
            x: area.x,
            y: area.y,
            w: 0,
            h: 0,
        };
        return (empty, empty);
    }
    let split = (area.w * 30 / 100).max(12).min(area.w.saturating_sub(10));
    let top = 1;
    let body_top = if area.h >= 13 { 4 } else { 2 };
    let bottom = area.h.saturating_sub(footer_rows(area.w));
    (
        Rect {
            x: area.x + 1,
            y: area.y + top,
            w: split.saturating_sub(1),
            h: bottom.saturating_sub(top),
        },
        Rect {
            x: area.x + split + 1,
            y: area.y + body_top,
            w: area.w.saturating_sub(split + 3),
            h: bottom.saturating_sub(body_top),
        },
    )
}

impl Layout {
    pub(super) fn new(area: Rect, spacing: &TextSpacing) -> Self {
        let (tree, body) = pane_bounds(area);
        let mut slots = Vec::new();
        let mut right = area.w.saturating_sub(2);
        let mut y = area.h.saturating_sub(3);
        let mut all: Vec<_> = buttons().collect();
        if !fits_form(area) {
            return Self {
                tree,
                body,
                buttons: slots,
            };
        }
        // When the window is narrow the actions wrap upward, preserving their reading order.
        while let Some((button, mnemonic)) = all.pop() {
            let width = mnemonic
                .spans()
                .iter()
                .map(|(s, _)| spacing.columns(s))
                .sum::<usize>();
            if right < 1 + width {
                y = y.saturating_sub(1);
                right = area.w.saturating_sub(2);
            }
            let x = right.saturating_sub(width).max(1);
            slots.push((
                button,
                Rect {
                    x: area.x + x,
                    y: area.y + y,
                    w: right.saturating_sub(x),
                    h: 1,
                },
            ));
            right = x.saturating_sub(1);
        }
        Self {
            tree,
            body,
            buttons: slots,
        }
    }
}

/// Use the animated pixel position rather than simply the integer target row for clicks.
pub(super) fn hit_row(
    point: Point<Pixels>,
    area: Rect,
    metrics: Metrics,
    scroll: &GridScroll,
) -> usize {
    let offset = f32::from(point.y - area.pixels(metrics).origin.y) / metrics.lh;
    (offset + scroll.position()).floor().max(0.) as usize
}

pub(super) fn value_column(width: usize) -> usize {
    width.saturating_mul(56) / 100
}
pub(super) fn input_width(width: usize) -> usize {
    width.saturating_sub(value_column(width) + 3).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn spacing() -> TextSpacing {
        TextSpacing::new(
            Arc::new(gpui::NoopTextSystem::new()),
            gpui::FontId(0),
            16.,
            8.,
        )
    }

    #[test]
    fn toggle_caption_marks_the_current_draft() {
        let field = Field {
            id: "number".into(),
            label: "行号".into(),
            kind: Kind::Toggle,
            value: Value::Bool(true),
            default: Value::Bool(false),
        };
        for (on, prefix) in [(true, "[ ✓ ] "), (false, "[ × ] ")] {
            let caption = control_caption(&field, &Value::Bool(on), false, 24, &spacing());
            assert!(caption.starts_with(prefix));
        }
    }

    #[test]
    fn every_footer_slot_is_within_the_dialog_at_small_measured_and_unmeasured_sizes() {
        let spacing = spacing();
        for w in 0..82 {
            for h in 0..24 {
                let area = Rect { x: 19, y: 8, w, h };
                let layout = Layout::new(area, &spacing);
                for (_, slot) in &layout.buttons {
                    assert!(area.contains(slot.x, slot.y) && slot.x + slot.w <= area.x + area.w);
                    assert!(slot.y >= layout.tree.y + layout.tree.h);
                }
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum BodyRow {
    Notice(usize, usize),
    Field(usize),
    Invalid,
    Space,
    PreviewTitle,
    PreviewLine(usize),
    ValueInfo,
    Empty,
}

impl Settings {
    /// Leave a narrow rim of the editor visible, expanding toward the edges on smaller grids.
    pub(in crate::grid) fn area(viewport: Rect) -> Rect {
        let mx = if viewport.w >= 76 { viewport.w / 18 } else { 0 };
        let my = if viewport.h >= 20 { viewport.h / 15 } else { 0 };
        Rect {
            x: viewport.x + mx,
            y: viewport.y + my,
            w: viewport.w.saturating_sub(2 * mx),
            h: viewport.h.saturating_sub(2 * my),
        }
    }

    /// Logical form rows stay fixed while a selector covers part of their viewport.
    pub(super) fn body_rows(&self) -> Vec<BodyRow> {
        let form = &self.forms[self.page];
        let mut rows = Vec::new();
        for row in 0..form.page.fields.len() {
            if matches!(form.page.fields[row].kind, Kind::Notice) {
                let count = form.page.fields[row]
                    .value
                    .as_str()
                    .map_or(0, |s| s.lines().count());
                rows.extend((0..count).map(|line| BodyRow::Notice(row, line)));
                continue;
            }
            rows.push(BodyRow::Field(row));
            if self.row == row && self.accepts_text() && !self.error.is_empty() {
                rows.push(BodyRow::Invalid);
            }
        }
        if rows.is_empty() {
            rows.push(BodyRow::Empty);
            return rows;
        }
        if form
            .page
            .fields
            .iter()
            .all(|field| matches!(field.kind, Kind::Notice))
        {
            return rows;
        }
        rows.push(BodyRow::Space);
        let preview = if form.page.id == "core.vim.indent" {
            3
        } else if form.page.id.starts_with("core.highlight.") {
            1
        } else {
            0
        };
        if preview > 0 {
            rows.push(BodyRow::PreviewTitle);
            rows.extend((0..preview).map(BodyRow::PreviewLine));
            rows.push(BodyRow::Space);
        }
        rows.push(BodyRow::ValueInfo);
        rows
    }

    pub(in crate::grid) fn set_appearance(&self, metrics: Metrics, cx: &mut App) {
        for panel in &self.panels {
            panel.update(cx, |panel, cx| panel.set_appearance(metrics, cx));
        }
    }

    /// Draw fixed chrome separately from the cropped, translated rows to keep text off the border.
    pub(in crate::grid) fn refresh(&self, area: Rect, spacing: &TextSpacing, cx: &mut App) {
        let layout = Layout::new(area, spacing);
        let mut chrome = Frame::new(area.w, area.h);
        chrome.hide_theme_background();
        chrome.border(i18n::text("settings.title"), true, false, spacing);
        if !fits_form(area) {
            if area.h > 2 {
                chrome.label(
                    1,
                    1..area.w.saturating_sub(1),
                    i18n::text("settings.enlarge"),
                    MUTED,
                    spacing,
                );
            }
            self.panels[0].update(cx, |panel, cx| panel.set(chrome, cx));
            return;
        }
        let split = layout.tree.w + 1;
        for y in 1..area.h.saturating_sub(footer_rows(area.w)) {
            chrome.vertical_rule(split, y, MUTED);
        }
        let left = layout.body.x - area.x + 1;
        let right = area.w.saturating_sub(2);
        let page = &self.forms[self.page].page;
        if area.h >= 13 {
            chrome.label(1, left..right, &page.path.join(" / "), MUTED, spacing);
            let title = page.path.last().map(String::as_str).unwrap_or("");
            let end = chrome.label(3, left..right, title, ACCENT, spacing);
            chrome.text(
                end + 1,
                3,
                &"─".repeat(right.saturating_sub(end + 1)),
                MUTED,
                right.saturating_sub(end + 1),
            );
        }
        let status_row = area.h.saturating_sub(footer_rows(area.w));
        chrome.text(
            1,
            status_row,
            &"─".repeat(area.w.saturating_sub(2)),
            MUTED,
            area.w.saturating_sub(2),
        );
        let edit = self.editing_value().and_then(Result::ok);
        let dirty = |page: usize, row: usize| {
            let form = &self.forms[page];
            let value = if page == self.page && row == self.row {
                edit.as_ref().unwrap_or(&form.draft[row])
            } else {
                &form.draft[row]
            };
            value != &form.applied[row]
        };
        let pending = self
            .forms
            .iter()
            .enumerate()
            .map(|(page, form)| {
                (0..form.draft.len())
                    .filter(|&row| dirty(page, row))
                    .count()
            })
            .sum::<usize>();
        let earliest = layout
            .buttons
            .iter()
            .filter(|(_, r)| r.y == area.y + area.h.saturating_sub(3))
            .map(|(_, r)| r.x - area.x)
            .min()
            .unwrap_or(right);
        if pending > 0 {
            chrome.label(
                area.h.saturating_sub(3),
                2..earliest.saturating_sub(1),
                &i18n::format("settings.pending", &[("count", &pending)]),
                GOLD,
                spacing,
            );
        }
        for (button, rect) in &layout.buttons {
            let mnemonic = Mnemonic::parse(i18n::text(button.key()));
            let mut col = rect.x - area.x;
            for (text, style) in mnemonic.spans() {
                col = chrome.styled_label(
                    rect.y - area.y,
                    col..rect.x - area.x + rect.w,
                    text,
                    if matches!(button, Button::Confirm) {
                        ACCENT
                    } else {
                        TEXT
                    },
                    spacing,
                    style,
                );
            }
        }
        let hint = match &self.editor {
            Some(Editor::List { .. })
                if matches!(self.field().map(|f| &f.kind), Some(Kind::Multi(_))) =>
            {
                "settings.hint_multi"
            }
            Some(Editor::List { .. }) => "settings.hint_select",
            Some(Editor::Input { .. }) => "settings.hint_text",
            None => "settings.hint",
        };
        if self.error.is_empty() || self.accepts_text() {
            chrome.styled_label(
                area.h.saturating_sub(2),
                2..right,
                i18n::text(hint),
                MUTED,
                spacing,
                LabelStyle::Dimmed,
            );
        } else {
            chrome.label(
                area.h.saturating_sub(2),
                2..right,
                &self.error,
                ERROR_TEXT,
                spacing,
            );
        }
        let tree = self.tree_frame(layout.tree, spacing);
        let body = self.form_frame(layout.body, spacing, edit.as_ref());
        for (panel, frame) in self.panels.iter().zip([chrome, tree, body]) {
            panel.update(cx, |panel, cx| panel.set(frame, cx));
        }
        self.refresh_selector(layout.body, spacing, cx);
    }

    fn tree_frame(&self, bounds: Rect, spacing: &TextSpacing) -> Frame {
        let height = bounds.h + 1;
        let mut frame = Frame::new(bounds.w, height);
        frame.hide_theme_background();
        let visible = self.visible_tree();
        for (y, &id) in visible
            .iter()
            .skip(self.tree_scroll.first_cell())
            .take(height)
            .enumerate()
        {
            let n = &self.nodes[id];
            let depth = n.path.len().saturating_sub(1);
            let x = 1 + depth * 2;
            let mut child = id;
            while let Some(parent) = self.nodes[child].parent {
                let depth = self.nodes[parent].path.len() - 1;
                let continues = self.nodes[child + 1..]
                    .iter()
                    .any(|node| node.parent == Some(parent));
                if child == id || continues {
                    frame.tree_guide(1 + depth * 2, y, continues, (child == id).then_some(x));
                }
                child = parent;
            }
            let active = id == self.tree_row;
            if active {
                frame.tint(
                    y,
                    0,
                    bounds.w,
                    if self.pane == Pane::Tree {
                        SELECTION
                    } else {
                        ACTIVE_FILE
                    },
                );
            }
            if n.branch {
                frame.text(
                    x,
                    y,
                    if self.expanded.contains(&id) {
                        "▾"
                    } else {
                        "▸"
                    },
                    MUTED,
                    1,
                );
            }
            frame.label(
                y,
                x + 2..bounds.w,
                n.path.last().map(String::as_str).unwrap_or(""),
                if active { ACCENT } else { TEXT },
                spacing,
            );
        }
        frame
    }

    fn form_frame(&self, bounds: Rect, spacing: &TextSpacing, edit: Option<&Value>) -> Frame {
        let height = bounds.h + 1;
        let mut frame = Frame::new(bounds.w, height);
        frame.hide_theme_background();
        let form = &self.forms[self.page];
        let col = value_column(bounds.w);
        let id_col = form
            .page
            .fields
            .iter()
            .map(|field| spacing.columns(&field.label) + 4)
            .max()
            .unwrap_or(0);
        let body_rows = self.body_rows();
        for (y, row) in body_rows
            .iter()
            .skip(self.form_scroll.first_cell())
            .take(height)
            .enumerate()
        {
            match *row {
                BodyRow::Notice(index, line) => {
                    if let Some(message) = form.page.fields[index]
                        .value
                        .as_str()
                        .and_then(|s| s.lines().nth(line))
                    {
                        frame.label(
                            y,
                            1..bounds.w,
                            message,
                            if line == 0 { ERROR_TEXT } else { MUTED },
                            spacing,
                        );
                    }
                }
                BodyRow::Field(i) => {
                    let f = &form.page.fields[i];
                    let active = i == self.row && self.pane == Pane::Form;
                    if active {
                        frame.tint(
                            y,
                            0,
                            bounds.w,
                            if self.editor.is_none() {
                                SELECTION
                            } else {
                                CURRENT
                            },
                        );
                    }
                    let displayed = if i == self.row {
                        edit.unwrap_or(&form.draft[i])
                    } else {
                        &form.draft[i]
                    };
                    if &form.applied[i] != displayed {
                        frame.text(0, y, "*", GOLD, 1);
                    }
                    frame.label(y, 2..col.saturating_sub(1), &f.label, TEXT, spacing);
                    // Stable technical option names fit beside their translated caption on wide windows.
                    if f.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                        && id_col + spacing.columns(&f.id) < col
                    {
                        frame.styled_label(
                            y,
                            id_col..col,
                            &f.id,
                            MUTED,
                            spacing,
                            LabelStyle::Dimmed,
                        );
                    }
                    if active && let Some(Editor::Input { text, field }) = &self.editor {
                        let text_layout =
                            field.layout(text, &self.composition, input_width(bounds.w), spacing);
                        frame.text(col, y, "[", ACCENT, 1);
                        let l = col + 1;
                        if !text_layout.selection.is_empty() {
                            frame.tint(
                                y,
                                l + text_layout.selection.start / UNITS_PER_COLUMN,
                                l + text_layout.selection.end.div_ceil(UNITS_PER_COLUMN),
                                SELECTION,
                            );
                        }
                        frame.label(
                            y,
                            l..bounds.w.saturating_sub(1),
                            text_layout.text(),
                            TEXT,
                            spacing,
                        );
                        frame.text(bounds.w.saturating_sub(1), y, "]", ACCENT, 1);
                    } else {
                        let list = active
                            .then_some(&self.editor)
                            .and_then(|e| e.as_ref())
                            .and_then(|e| {
                                if let Editor::List { value, .. } = e {
                                    Some(value)
                                } else {
                                    None
                                }
                            });
                        let value = list.unwrap_or(&form.draft[i]);
                        let caption = control_caption(
                            f,
                            value,
                            list.is_some(),
                            bounds.w.saturating_sub(col),
                            spacing,
                        );
                        frame.label(
                            y,
                            col..bounds.w,
                            &caption,
                            if active { ACCENT } else { TEXT },
                            spacing,
                        );
                    }
                }
                BodyRow::Invalid => {
                    frame.label(y, 2..bounds.w, &self.error, ERROR_TEXT, spacing);
                }
                BodyRow::PreviewTitle => {
                    let end = frame.label(
                        y,
                        3..bounds.w,
                        i18n::text("settings.preview"),
                        MUTED,
                        spacing,
                    );
                    let lines = body_rows
                        .iter()
                        .skip(self.form_scroll.first_cell() + y + 1)
                        .take_while(|row| matches!(row, BodyRow::PreviewLine(_)))
                        .count();
                    frame.outline(
                        Rect {
                            x: 0,
                            y,
                            w: bounds.w,
                            h: lines + 2,
                        },
                        MUTED,
                        OutlineStyle::Dashed,
                        std::iter::once(2..end + 1),
                    );
                }
                BodyRow::PreviewLine(line) => {
                    paint_preview(&mut frame, y, bounds.w, self, line, edit)
                }
                BodyRow::ValueInfo => {
                    if let Some(f) = self.field() {
                        let initial = display_value(&f.kind, &f.value);
                        let default = display_value(&f.kind, &f.default);
                        frame.styled_label(
                            y,
                            1..bounds.w,
                            &i18n::format(
                                "settings.values",
                                &[("initial", &initial), ("default", &default)],
                            ),
                            MUTED,
                            spacing,
                            LabelStyle::Dimmed,
                        );
                    }
                }
                BodyRow::Empty => {
                    frame.label(y, 1..bounds.w, i18n::text("settings.empty"), MUTED, spacing);
                }
                BodyRow::Space => {}
            }
        }
        frame
    }

    pub(in crate::grid) fn layers(&self, area: Rect, metrics: Metrics) -> Vec<GridLayer> {
        let (tree, body) = pane_bounds(area);
        if area.w == 0 || area.h == 0 {
            return vec![];
        }
        let chrome = GridLayer {
            panel: self.panels[0].clone(),
            bounds: area.pixels(metrics),
            clip: None,
        };
        if !fits_form(area) {
            return vec![chrome];
        }
        vec![
            chrome,
            GridLayer::scrolled(&self.panels[1], tree, self.tree_scroll.fraction(), metrics),
            GridLayer::scrolled(&self.panels[2], body, self.form_scroll.fraction(), metrics),
        ]
    }
}

impl Button {
    fn key(self) -> &'static str {
        match self {
            Self::Cancel => "settings.cancel",
            Self::Reset => "settings.reset",
            Self::Restore => "settings.restore",
            Self::Apply => "settings.apply",
            Self::Confirm => "settings.confirm",
        }
    }
}

fn display_value(kind: &Kind, value: &Value) -> String {
    match kind {
        Kind::Color { .. } if value.as_str() == Some("") => {
            i18n::text("settings_cursor.inherit").into()
        }
        Kind::OptionalNumber { .. } if value.is_null() => {
            i18n::text("settings_cursor.inherit").into()
        }
        Kind::Toggle => i18n::text(if value.as_bool() == Some(true) {
            "settings.on"
        } else {
            "settings.off"
        })
        .into(),
        Kind::Select(choices) => choices
            .iter()
            .find(|c| value == &c.id)
            .map(|c| c.label.clone())
            .unwrap_or_else(|| value.as_str().unwrap_or("").into()),
        Kind::Multi(choices) => {
            let names: Vec<_> = choices
                .iter()
                .filter(|c| {
                    value
                        .as_array()
                        .is_some_and(|v| v.iter().any(|i| i == &c.id))
                })
                .map(|c| c.label.as_str())
                .collect();
            if names.is_empty() {
                i18n::text("settings.none").into()
            } else {
                names.join(i18n::text("settings.separator"))
            }
        }
        _ => value
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| value.to_string()),
    }
}

fn control_caption(
    field: &Field,
    value: &Value,
    open: bool,
    width: usize,
    spacing: &TextSpacing,
) -> String {
    let mut label = display_value(&field.kind, value);
    match &field.kind {
        Kind::Color {
            inherited: Some(rgb),
        } if value.as_str() == Some("") => format!("[ {label} #{rgb:06x} ]"),
        Kind::Toggle => format!(
            "[ {} ] {label}",
            if value.as_bool() == Some(true) {
                "✓"
            } else {
                "×"
            }
        ),
        Kind::Select(_) | Kind::Multi(_) => {
            if let Some(values) = value.as_array()
                && spacing.columns(&label) + 6 > width
            {
                label = i18n::format("settings.selected", &[("count", &values.len())]);
            }
            format!("[ {label} {} ]", if open { "▴" } else { "▾" })
        }
        _ => {
            let unit = match field.id.as_str() {
                "tabstop" | "shiftwidth" => i18n::text("settings.columns"),
                "zoom" => "%",
                "font_size" => "px",
                "duration_ms" | "insert_ms" | "response_ms" if !value.is_null() => "ms",
                _ => "",
            };
            format!("[ {label} ] {unit}")
        }
    }
}

/// Render a real draft's indent width or combined syntax style without committing it.
fn paint_preview(
    frame: &mut Frame,
    y: usize,
    width: usize,
    settings: &Settings,
    line: usize,
    editing_value: Option<&Value>,
) {
    let form = &settings.forms[settings.page];
    let value = |id: &str| {
        form.page.fields.iter().position(|f| f.id == id).map(|i| {
            if i == settings.row {
                editing_value.unwrap_or(&form.draft[i])
            } else {
                &form.draft[i]
            }
        })
    };
    if form.page.id == "core.vim.indent" {
        let indent = value("shiftwidth")
            .and_then(Value::as_i64)
            .filter(|i| *i > 0)
            .or_else(|| value("tabstop").and_then(Value::as_i64))
            .unwrap_or(4)
            .clamp(0, 80) as usize;
        frame.text(1, y, &(line + 1).to_string(), MUTED, 2);
        let (x, text, color) = match line {
            0 => (4, "fn main() {", theme::syntax_color(SyntaxKind::Keyword)),
            1 => (
                4 + indent,
                "println!(\"Nanobug\");",
                theme::syntax_color(SyntaxKind::Function),
            ),
            _ => (4, "}", TEXT),
        };
        frame.text(x, y, text, color, width.saturating_sub(x));
    } else {
        let mut style = HighlightStyle::default();
        let a = &mut style.attributes;
        a.reverse = value("reverse").and_then(Value::as_bool) == Some(true);
        let underline = value("underline").and_then(Value::as_str);
        a.underline = underline == Some("underline");
        a.undercurl = underline == Some("undercurl");
        a.underdotted = underline == Some("underdotted");
        a.underdashed = underline == Some("underdashed");
        a.underdouble = underline == Some("underdouble");
        let decorations = value("decorations").and_then(Value::as_array);
        let contains = |s: &str| decorations.is_some_and(|v| v.iter().any(|d| d == s));
        a.bold = contains("bold");
        a.italic = contains("italic");
        a.strikethrough = contains("strikethrough");
        a.overline = contains("overline");
        let Some(kind) = SyntaxKind::ALL
            .iter()
            .copied()
            .find(|k| form.page.id.strip_prefix("core.highlight.") == Some(k.group()))
        else {
            return;
        };
        let (_, text) = theme::syntax_example(kind);
        style.foreground = form.page.fields.iter().find_map(|field| match field.kind {
            Kind::Color { inherited } => inherited,
            _ => None,
        });
        if let Some(hex) = value("foreground")
            .and_then(Value::as_str)
            .and_then(|v| v.strip_prefix('#'))
            .filter(|s| s.len() == 6)
        {
            if let Ok(color) = u32::from_str_radix(hex, 16) {
                style.foreground = Some(color);
            }
        }
        frame.highlighted_text(
            2,
            y,
            text,
            theme::syntax_color(kind),
            width.saturating_sub(2),
            Some(style),
        );
    }
}
