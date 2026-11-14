//! Keyboard inspection state for the editor help page.

mod paint;

use self::paint::Layout;
use super::HelpCommand;
use crate::{
    grid::render::{CursorCell, Frame, Metrics, Rect},
    ui::{
        keys::normal_character,
        text::{TextSpacing, UNITS_PER_COLUMN},
        theme::{ACCENT, GOLD, MUTED, TEXT},
    },
};
use gpui::{KeyDownEvent, Modifiers, Pixels, Point};
use nanobug::{
    animation::scroll::ScrollInput,
    i18n,
    vim::shortcuts::{Example, continuation, example as vim_example},
};
use std::borrow::Cow;

/// Preferred popup width in grid columns, including the keyboard's navigation column.
pub(super) const KEYBOARD_COLUMNS: usize = 84;

/// Preferred body height, including the function row and the example.
pub(super) const KEYBOARD_BODY_ROWS: usize = 26;

/// Help and workspace commands available from the editor keyboard.
const WORKSPACE_EXAMPLES: &[(&str, &str, &str)] = &[
    ("Escape", "keyboard.escape", "Esc"),
    ("F5", "keyboard.refresh", "F5"),
    ("F6", "keyboard.cycle_cursor", "F6"),
    ("Shift+F6", "keyboard.cycle_cursor_back", "Shift+F6"),
    ("F7", "keyboard.reload_cursor", "F7"),
    ("Ctrl+Left", "keyboard.area_shortcuts", "Ctrl+←"),
    ("Ctrl+Right", "keyboard.global_shortcuts", "Ctrl+→"),
    ("Alt+h", "keyboard.previous_tab", "Alt+h"),
    ("Alt+k", "keyboard.previous_tab", "Alt+k"),
    ("Alt+j", "keyboard.next_tab", "Alt+j"),
    ("Alt+l", "keyboard.next_tab", "Alt+l"),
    ("Alt+Escape", "keyboard.close_editor", "Alt+Esc"),
    ("Ctrl+Alt+h", "keyboard.move_tab_left", "Ctrl+Alt+h"),
    ("Ctrl+Alt+k", "keyboard.move_tab_left", "Ctrl+Alt+k"),
    ("Ctrl+Alt+j", "keyboard.move_tab_right", "Ctrl+Alt+j"),
    ("Ctrl+Alt+l", "keyboard.move_tab_right", "Ctrl+Alt+l"),
    ("Ctrl+Alt+Home", "keyboard.tab_to_first", "Ctrl+Alt+Home"),
    ("Ctrl+Alt+End", "keyboard.tab_to_last", "Ctrl+Alt+End"),
];

/// Combination view selected by a standalone modifier tap or a keycap click.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Modifier {
    /// Show Control combinations.
    Ctrl,
    /// Show Alt combinations.
    Alt,
    /// Show combinations with both Control and Alt.
    CtrlAlt,
    /// Show uppercase letters and shifted punctuation.
    Shift,
}

impl Modifier {
    /// Display name used on the key and in the description.
    fn name(self) -> &'static str {
        match self {
            Self::Ctrl => "Ctrl",
            Self::Alt => "Alt",
            Self::CtrlAlt => "Ctrl+Alt",
            Self::Shift => "Shift",
        }
    }

    /// Whether the current view includes the modifier represented by `cap`.
    fn contains(self, cap: Self) -> bool {
        self == cap || self == Self::CtrlAlt && matches!(cap, Self::Ctrl | Self::Alt)
    }
}

/// One visible key, independent of its position in the popup.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Cap {
    /// Printable key with its normal and shifted characters.
    Character { base: char, shifted: char },
    /// Non-character key with a native command name and a shorter visible legend.
    Named {
        name: &'static str,
        label: &'static str,
    },
    /// A modifier toggle on either side of the keyboard.
    Modifier(Modifier),
}

impl Cap {
    /// Visible glyph for the current `modifier` view.
    fn label(self, modifier: Option<Modifier>) -> String {
        match self {
            Self::Modifier(modifier) => modifier.name().to_owned(),
            Self::Named { label, .. } => label.to_owned(),
            Self::Character { base, shifted } => if modifier == Some(Modifier::Shift) {
                shifted
            } else {
                base
            }
            .to_string(),
        }
    }

    /// Resolve the example name for this character in the selected `modifier` view.
    fn command(self, modifier: Option<Modifier>) -> String {
        match (self, modifier) {
            (Self::Character { base, .. }, Some(modifier)) if modifier != Modifier::Shift => {
                format!("{}+{base}", modifier.name())
            }
            (Self::Named { name: "Escape", .. }, Some(Modifier::Ctrl | Modifier::Shift)) => {
                "Escape".to_owned()
            }
            (Self::Named { name, .. }, Some(modifier)) => {
                format!("{}+{name}", modifier.name())
            }
            (Self::Named { name, .. }, _) => name.to_owned(),
            _ => self.label(modifier),
        }
    }
}

/// Last inspected key or sequence and the catalog entry it resolves to.
struct Selection {
    /// Visible key or sequence.
    typed: String,
    /// Catalog lookup key, which can be the completion of a partial sequence.
    command: String,
    /// Final key used by the sequence.
    cap: Cap,
    /// Combination view in which it was selected.
    modifier: Option<Modifier>,
}

/// Inspect example keys and distinguish standalone modifier taps from live chords.
pub(super) struct Keyboard {
    /// Modifier selected by tapping its key or clicking its keycap.
    picked: Option<Modifier>,
    /// Physical modifier state, independent of the displayed keycaps.
    held: Modifiers,
    /// A standalone modifier eligible to toggle the view on release.
    modifier_tap: Option<Modifier>,
    /// The latest example in any combination view.
    selection: Selection,
    /// First key or incomplete text object for the next sequence.
    prefix: String,
}

impl Keyboard {
    /// Start with the Normal-mode `j` example selected.
    pub fn new() -> Self {
        Self {
            picked: None,
            held: Modifiers::default(),
            modifier_tap: None,
            selection: Selection {
                typed: "j".into(),
                command: "j".into(),
                cap: Cap::Character {
                    base: 'j',
                    shifted: 'J',
                },
                modifier: None,
            },
            prefix: String::new(),
        }
    }

    /// Track physical `modifiers`. Return whether a standalone tap changed the view.
    /// When `allow_tap` is false, synchronize the baseline without arming a tap.
    pub fn update_modifiers(&mut self, modifiers: Modifiers, allow_tap: bool) -> bool {
        let previous = std::mem::replace(&mut self.held, modifiers);
        if !allow_tap {
            self.cancel_modifier_tap();
            return false;
        }
        if previous == modifiers {
            return false;
        }
        let tap = self.modifier_tap.take();
        if !modifiers.modified()
            && let Some(tapped) = tap
        {
            self.toggle_modifier(tapped);
            return true;
        }
        if !previous.modified() && modifiers.number_of_modifiers() == 1 {
            self.modifier_tap = match (modifiers.control, modifiers.shift, modifiers.alt) {
                (true, false, false) => Some(Modifier::Ctrl),
                (false, true, false) => Some(Modifier::Shift),
                (false, false, true) => Some(Modifier::Alt),
                _ => None,
            };
        }
        false
    }

    /// A chord, tab switch or pointer action consumes the current modifier tap.
    pub fn cancel_modifier_tap(&mut self) {
        self.modifier_tap = None;
    }

    /// Inspect or forward `event`, reserving closure for a fresh, unmodified Escape press.
    /// Space stays disabled in every modifier view.
    pub fn key(&mut self, event: &KeyDownEvent) -> HelpCommand {
        self.cancel_modifier_tap();
        let key = &event.keystroke;
        if key.modifiers.platform || key.modifiers.function {
            return HelpCommand::Ignore;
        }
        if key.key == "escape" {
            return if event.is_held || key.modifiers.modified() {
                HelpCommand::Ignore
            } else {
                HelpCommand::Close
            };
        }
        if matches!(key.key.as_str(), "space" | " ") {
            return HelpCommand::Ignore;
        }
        // Windows removes Shift from the keystroke when it produces a shifted symbol.
        if key.modifiers.modified() || self.held.shift {
            return HelpCommand::DispatchChord;
        }
        let character = normal_character(key).and_then(|character| character.chars().next());
        let cap = paint::caps().find(|cap| match *cap {
            Cap::Character { base, shifted } => {
                character == Some(base) || character == Some(shifted)
            }
            Cap::Named { name, .. } => key.key.eq_ignore_ascii_case(name),
            Cap::Modifier(_) => false,
        });
        if cap.is_some_and(|cap| self.inspect(cap, self.modifier())) {
            HelpCommand::Update(ScrollInput::Advance)
        } else {
            HelpCommand::Ignore
        }
    }

    /// Inspect or toggle the painted key under the window-local pointer `position`.
    pub fn press(&mut self, body: Rect, metrics: Metrics, position: Point<Pixels>) -> bool {
        self.cancel_modifier_tap();
        Layout::new(body, metrics)
            .caps
            .into_iter()
            .find(|(_, bounds)| bounds.contains(&position))
            .is_some_and(|(cap, _)| match cap {
                Cap::Modifier(modifier) => {
                    self.toggle_modifier(modifier);
                    true
                }
                _ => self.inspect(cap, self.modifier()),
            })
    }

    /// Draw the example text in the visible `body` using measured grid `spacing`.
    pub fn frame(&self, body: Rect, spacing: &TextSpacing) -> Frame {
        let mut frame = Frame::new(body.w, body.h);
        if let Some(info) = description_area(body) {
            self.description(&mut frame, info, spacing);
        }
        frame
    }

    /// Locate the first cell of the binding in `body` and project it with `metrics`.
    pub(super) fn caret(body: Rect, metrics: Metrics) -> Option<CursorCell> {
        let (column, row) = description_heading(description_area(body)?);
        Some(CursorCell {
            row,
            column: column as f32,
            origin: metrics.at((body.x + column) as f32, (body.y + row) as f32),
        })
    }

    /// Draw the selected operation and its example inside the dashed `area`.
    fn description(&self, frame: &mut Frame, area: Rect, spacing: &TextSpacing) {
        let entry = self
            .shows_selection()
            .then(|| example(&self.selection.command))
            .flatten();
        let (name, label, keys) = if let Some(entry) = entry {
            (
                self.selection.typed.as_str(),
                entry.description,
                Some(entry.keys),
            )
        } else {
            let modifier = self.modifier();
            let label = match modifier {
                Some(Modifier::Ctrl) => "keyboard.ctrl",
                Some(Modifier::Alt) => "keyboard.alt",
                Some(Modifier::CtrlAlt) => "keyboard.ctrl_alt",
                Some(Modifier::Shift) => "keyboard.shift",
                None => "keyboard.choose",
            };
            (modifier.map_or("", Modifier::name), label, None)
        };
        let mut message = i18n::text(label).splitn(3, '\n');
        let heading = message.next().unwrap_or_default();
        let detail = message.next().unwrap_or_default();
        let result = message.next().unwrap_or_default();
        let (column, row) = description_heading(area);
        let slot = column..area.x + area.w - 2;
        // Binding names use grid cells so the shared cursor can recolor their first glyph.
        let start = frame.text(column, row, name, ACCENT, slot.len());
        frame.label(row, start + 2..slot.end, heading, TEXT, spacing);

        // Keep the concrete example together when a short window leaves fewer rows for details.
        let rows = area.h - 3 - usize::from(keys.is_some());
        let result_lines = detail_lines(result, slot.len(), rows.min(2), spacing);
        let mut next = row + 1;
        for text in detail_lines(
            detail,
            slot.len(),
            (rows - result_lines.len()).min(2),
            spacing,
        ) {
            frame.label(next, slot.clone(), &text, TEXT, spacing);
            next += 1;
        }
        if let Some(keys) = keys {
            let start = frame.label(
                next,
                slot.clone(),
                i18n::text("keyboard.example"),
                MUTED,
                spacing,
            );
            frame.label(next, start + 2..slot.end, keys, GOLD, spacing);
            for (line, text) in result_lines.into_iter().enumerate() {
                frame.label(next + 1 + line, slot.clone(), &text, TEXT, spacing);
            }
        }
    }

    /// Toggle a modifier's view, allowing Control and Alt to be picked together.
    fn toggle_modifier(&mut self, modifier: Modifier) {
        use Modifier::{Alt, Ctrl, CtrlAlt};
        self.picked = match (self.picked, modifier) {
            (Some(Ctrl), Alt) | (Some(Alt), Ctrl) => Some(CtrlAlt),
            (Some(CtrlAlt), Ctrl) => Some(Alt),
            (Some(CtrlAlt), Alt) => Some(Ctrl),
            (Some(picked), modifier) if picked == modifier => None,
            (_, modifier) => Some(modifier),
        };
    }

    /// Return the selected combination view.
    fn modifier(&self) -> Option<Modifier> {
        self.picked
    }

    /// Whether the selected example is visible in the current modifier view.
    fn shows_selection(&self) -> bool {
        let modifier = self.modifier();
        modifier.is_none() || self.selection.modifier == modifier
    }

    /// Inspect `cap` under `modifier`, carrying an incomplete command sequence.
    fn inspect(&mut self, cap: Cap, modifier: Option<Modifier>) -> bool {
        if !available(cap, modifier) {
            return false;
        }
        let key = cap.command(modifier);
        let prefix = std::mem::take(&mut self.prefix);
        let candidate = format!("{prefix}{key}");
        let typed = if example(&candidate).is_some() || continuation(&candidate).is_some() {
            candidate
        } else {
            key
        };
        let next = continuation(&typed);
        if next.is_some() {
            self.prefix.clone_from(&typed);
        }
        let command = if example(&typed).is_some() {
            typed.clone()
        } else {
            next.unwrap_or(&typed).to_owned()
        };
        self.selection = Selection {
            typed,
            command,
            cap,
            modifier,
        };
        true
    }
}

/// Look up `command` in the workspace and Vim catalogs.
fn example(command: &str) -> Option<Example> {
    WORKSPACE_EXAMPLES
        .iter()
        .find_map(|&(key, description, keys)| {
            (key == command).then_some(Example { description, keys })
        })
        .or_else(|| vim_example(command))
}

/// Whether `cap` has an operation in the current combination view.
fn available(cap: Cap, modifier: Option<Modifier>) -> bool {
    matches!(cap, Cap::Modifier(_)) || example(&cap.command(modifier)).is_some()
}

/// Reserve the heading, detail and example near the bottom of `body`, above the footer inset.
fn description_area(body: Rect) -> Option<Rect> {
    let height = body.h.saturating_sub(6).clamp(4, 8);
    let bottom = body.h.saturating_sub(usize::from(body.h >= 18));
    (body.w >= 11 && bottom >= height).then(|| Rect {
        x: 1,
        y: bottom - height,
        w: body.w - 2,
        h: height,
    })
}

/// Return the binding's column and row inside the description frame.
fn description_heading(area: Rect) -> (usize, usize) {
    (area.x + 2, area.y + 1)
}

/// Fit `detail` to the available rows, breaking at spaces or grapheme boundaries.
fn detail_lines<'a>(
    mut detail: &'a str,
    columns: usize,
    rows: usize,
    spacing: &TextSpacing,
) -> Vec<Cow<'a, str>> {
    let mut lines = Vec::with_capacity(rows);
    for row in 0..rows {
        if detail.is_empty() {
            break;
        }
        if row + 1 == rows {
            lines.push(spacing.ellipsize(detail, columns));
            break;
        }
        let end = spacing
            .fitting_cells(detail, columns * UNITS_PER_COLUMN)
            .last()
            .map_or(0, |cell| cell.byte + cell.text.len());
        if end == 0 {
            break;
        }
        let end = if end == detail.len() || detail[end..].starts_with(' ') {
            end
        } else {
            detail[..end]
                .rfind(' ')
                .filter(|&split| split > end / 2)
                .unwrap_or(end)
        };
        lines.push(Cow::Borrowed(detail[..end].trim_end()));
        detail = detail[end..].trim_start();
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{FontId, Keystroke, NoopTextSystem, point, px};
    use std::sync::Arc;

    /// Construct a fresh key-down event for the shortcut inspector.
    fn key_event(binding: &'static str) -> KeyDownEvent {
        KeyDownEvent {
            keystroke: Keystroke::parse(binding).unwrap(),
            is_held: false,
            prefer_character_input: false,
        }
    }

    /// The inspector changes its view only on the release of an unused modifier.
    #[test]
    fn keyboard_modifier_taps_toggle_the_selected_view() {
        let mut keyboard = Keyboard::new();
        let released = Modifiers::default();
        let ctrl = Modifiers {
            control: true,
            ..released
        };
        let shift = Modifiers {
            shift: true,
            ..released
        };

        assert!(!keyboard.update_modifiers(ctrl, true));
        assert!(!keyboard.update_modifiers(ctrl, true));
        assert!(keyboard.modifier().is_none());
        assert_eq!(keyboard.selection.command, "j");
        assert!(keyboard.update_modifiers(released, true));
        assert!(keyboard.modifier() == Some(Modifier::Ctrl));
        keyboard.key(&key_event("s"));
        assert_eq!(keyboard.selection.command, "Ctrl+s");

        for (before, expected) in [
            (Some(Modifier::Ctrl), Some(Modifier::Shift)),
            (Some(Modifier::Shift), None),
        ] {
            assert!(!keyboard.update_modifiers(shift, true));
            assert!(keyboard.modifier() == before);
            assert!(keyboard.update_modifiers(released, true));
            assert!(keyboard.modifier() == expected);
        }
    }

    /// A chord preserves the inspector selection, even if Windows already normalized its symbol.
    #[test]
    fn keyboard_routes_physical_chords_without_inspecting_them() {
        let mut keyboard = Keyboard::new();
        keyboard.toggle_modifier(Modifier::Shift);
        keyboard.key(&key_event("j"));
        assert_eq!(keyboard.selection.command, "J");
        let released = Modifiers::default();
        let ctrl = Modifiers {
            control: true,
            ..released
        };
        let shift = Modifiers {
            shift: true,
            ..released
        };
        let alt = Modifiers {
            alt: true,
            ..released
        };
        let ctrl_alt = Modifiers {
            control: true,
            ..alt
        };
        for (held, key) in [
            (ctrl, "ctrl-s"),
            (shift, "shift-n"),
            (shift, "?"),
            (alt, "alt-l"),
            (ctrl_alt, "ctrl-alt-home"),
        ] {
            assert!(!keyboard.update_modifiers(held, true));
            assert!(matches!(
                keyboard.key(&key_event(key)),
                HelpCommand::DispatchChord
            ));
            assert!(!keyboard.update_modifiers(released, true));
            assert!(keyboard.modifier() == Some(Modifier::Shift));
            assert_eq!(keyboard.selection.command, "J");
        }
    }

    /// Sequential taps expose Alt commands and their Ctrl+Alt variants.
    #[test]
    fn keyboard_combines_separate_alt_and_ctrl_taps() {
        let mut keyboard = Keyboard::new();
        let released = Modifiers::default();
        let alt = Modifiers {
            alt: true,
            ..released
        };
        let ctrl = Modifiers {
            control: true,
            ..released
        };

        assert!(!keyboard.update_modifiers(alt, true));
        assert!(keyboard.modifier().is_none());
        assert!(keyboard.update_modifiers(released, true));
        assert!(keyboard.modifier() == Some(Modifier::Alt));
        assert!(matches!(
            keyboard.key(&key_event("l")),
            HelpCommand::Update(_)
        ));
        assert_eq!(keyboard.selection.command, "Alt+l");

        assert!(!keyboard.update_modifiers(ctrl, true));
        assert!(keyboard.modifier() == Some(Modifier::Alt));
        assert!(keyboard.update_modifiers(released, true));
        assert!(keyboard.modifier() == Some(Modifier::CtrlAlt));
        assert!(matches!(
            keyboard.key(&key_event("home")),
            HelpCommand::Update(_)
        ));
        assert_eq!(keyboard.selection.command, "Ctrl+Alt+Home");

        assert!(!keyboard.update_modifiers(alt, true));
        assert!(keyboard.update_modifiers(released, true));
        assert!(keyboard.modifier() == Some(Modifier::Ctrl));
    }

    /// Partially released chords and modifiers held before activation cannot toggle the inspector.
    #[test]
    fn keyboard_modifier_taps_require_a_fresh_press_from_rest() {
        let mut keyboard = Keyboard::new();
        let released = Modifiers::default();
        let ctrl = Modifiers {
            control: true,
            ..released
        };
        let shift = Modifiers {
            shift: true,
            ..released
        };
        let both = Modifiers {
            control: true,
            ..shift
        };
        for held in [ctrl, both, shift, released] {
            assert!(!keyboard.update_modifiers(held, true));
        }

        assert!(!keyboard.update_modifiers(ctrl, false));
        assert!(!keyboard.update_modifiers(ctrl, true));
        assert!(!keyboard.update_modifiers(released, true));
        assert!(!keyboard.update_modifiers(ctrl, true));
        assert!(!keyboard.update_modifiers(ctrl, false));
        assert!(!keyboard.update_modifiers(released, true));
        assert!(keyboard.modifier().is_none());
    }

    /// A bare Escape closes with a selected modifier or an incomplete example.
    #[test]
    fn keyboard_escape_closes_with_pending_state() {
        let escape = key_event("escape");
        let mut keyboard = Keyboard::new();
        keyboard.toggle_modifier(Modifier::Shift);
        assert!(matches!(keyboard.key(&escape), HelpCommand::Close));
        for chord in ["shift-escape", "ctrl-escape", "alt-escape"] {
            assert!(matches!(
                keyboard.key(&key_event(chord)),
                HelpCommand::Ignore
            ));
        }
        let mut keyboard = Keyboard::new();
        keyboard.key(&key_event("g"));
        assert!(matches!(keyboard.key(&escape), HelpCommand::Close));
        assert!(matches!(
            keyboard.key(&KeyDownEvent {
                is_held: true,
                ..escape
            }),
            HelpCommand::Ignore
        ));
    }

    /// Both bundled languages explain every example within the compact keyboard's text width.
    #[test]
    fn keyboard_details_fit_in_the_example_box() {
        let spacing = TextSpacing::new(Arc::new(NoopTextSystem::new()), FontId(0), 16., 10.);
        let area = description_area(Rect {
            x: 0,
            y: 0,
            w: 68,
            h: 14,
        })
        .unwrap();
        for text in [
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/i18n/en.toml")),
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/i18n/zh-CN.toml"
            )),
        ] {
            let catalog: toml::Table = toml::from_str(text).unwrap();
            for (name, message) in catalog["keyboard"].as_table().unwrap() {
                if name == "example" {
                    continue;
                }
                let mut parts = message.as_str().unwrap().splitn(3, '\n');
                assert!(!parts.next().unwrap().is_empty(), "{name}");
                let detail = parts.next().unwrap();
                let result = parts.next();
                if !matches!(
                    name.as_str(),
                    "choose" | "ctrl" | "shift" | "alt" | "ctrl_alt"
                ) {
                    assert!(result.is_some_and(|text| !text.is_empty()), "{name}");
                }
                for text in std::iter::once(detail).chain(result) {
                    let lines = detail_lines(text, area.w - 4, 2, &spacing);
                    assert!(!lines.is_empty(), "{name}");
                    assert!(
                        lines.iter().all(|line| !line.ends_with('…')),
                        "{name}: {text}"
                    );
                }
            }
        }
    }

    /// Inspect supported commands through either input path, leaving count digits inactive.
    #[test]
    fn keyboard_inspects_commands_and_modifier_combinations() {
        let mut keyboard = Keyboard::new();
        let event = key_event;
        for key in ["1", "2", "3", "4", "5", "6", "7", "8", "9"] {
            assert!(matches!(keyboard.key(&event(key)), HelpCommand::Ignore));
        }
        assert_eq!(keyboard.selection.typed, "j");
        for key in ["g", "8", "g"] {
            keyboard.key(&event(key));
        }
        assert_eq!(keyboard.selection.typed, "gg");
        keyboard.key(&event("0"));
        assert_eq!(keyboard.selection.typed, "0");
        assert_eq!(keyboard.selection.command, "0");
        keyboard.key(&event("j"));
        assert_eq!(keyboard.selection.typed, "j");
        keyboard.toggle_modifier(Modifier::Shift);
        keyboard.key(&event("/"));
        assert_eq!(keyboard.selection.command, "?");
        for key in ["z", "z"] {
            keyboard.key(&event(key));
        }
        assert_eq!(keyboard.selection.command, "ZZ");
        keyboard.toggle_modifier(Modifier::Shift);
        for key in ["=", "i", "p"] {
            keyboard.key(&event(key));
        }
        assert_eq!(keyboard.selection.command, "=ip");

        keyboard.toggle_modifier(Modifier::Ctrl);
        keyboard.key(&event("s"));
        assert_eq!(keyboard.selection.command, "Ctrl+s");
        assert!(matches!(keyboard.key(&event("7")), HelpCommand::Ignore));
        keyboard.toggle_modifier(Modifier::Ctrl);

        let body = Rect {
            x: 0,
            y: 0,
            w: 72,
            h: 20,
        };
        let metrics = Metrics {
            cw: 10.,
            lh: 22.,
            font_size: 16.,
            origin: point(px(14.), px(42.)),
        };
        let layout = Layout::new(body, metrics);
        let find = |target| {
            layout
                .caps
                .iter()
                .find(|(cap, _)| *cap == target)
                .unwrap()
                .1
        };
        for (base, shifted) in "123456789".chars().zip("!@#$%^&*(".chars()) {
            let cap = Cap::Character { base, shifted };
            assert!(!available(cap, None));
            assert!(available(cap, Some(Modifier::Shift)));
            assert!(!keyboard.press(body, metrics, find(cap).center()));
        }
        keyboard.key(&event("j"));
        assert_eq!(keyboard.selection.typed, "j");
        let shift = find(Cap::Modifier(Modifier::Shift));
        assert!(keyboard.press(body, metrics, shift.center()));
        let star = find(Cap::Character {
            base: '8',
            shifted: '*',
        });
        assert!(keyboard.press(body, metrics, star.center()));
        assert_eq!(keyboard.selection.command, "*");
        let key = find(Cap::Character {
            base: 'j',
            shifted: 'J',
        });
        assert!(keyboard.press(body, metrics, key.center()));
        assert_eq!(keyboard.selection.command, "J");
    }

    /// Inspect named keys from either input path and keep unavailable keys dim.
    #[test]
    fn keyboard_inspects_function_and_navigation_keys() {
        let mut keyboard = Keyboard::new();
        let body = Rect {
            x: 0,
            y: 0,
            w: 82,
            h: 26,
        };
        let metrics = Metrics {
            cw: 10.,
            lh: 22.,
            font_size: 16.,
            origin: point(px(14.), px(42.)),
        };
        let layout = Layout::new(body, metrics);
        let center = |key| {
            layout
                .caps
                .iter()
                .find_map(|(cap, bounds)| match cap {
                    Cap::Named { name, .. } if *name == key => Some(bounds.center()),
                    _ => None,
                })
                .unwrap()
        };
        for (raw, name) in [
            ("f5", "F5"),
            ("pageup", "PageUp"),
            ("right", "Right"),
            ("insert", "Insert"),
        ] {
            assert!(matches!(
                keyboard.key(&key_event(raw)),
                HelpCommand::Update(_)
            ));
            assert_eq!(keyboard.selection.typed, name);
            assert!(keyboard.press(body, metrics, center(name)));
            assert_eq!(keyboard.selection.command, name);
        }
        assert!(matches!(
            keyboard.key(&key_event("f1")),
            HelpCommand::Ignore
        ));
        assert!(!keyboard.press(body, metrics, center("F1")));
        assert_eq!(keyboard.selection.command, "Insert");
        for binding in ["space", "shift-space", "ctrl-space", "alt-space"] {
            assert!(matches!(
                keyboard.key(&key_event(binding)),
                HelpCommand::Ignore
            ));
        }
        assert!(!keyboard.press(body, metrics, center("Space")));
        assert_eq!(keyboard.selection.command, "Insert");
        assert!(keyboard.press(body, metrics, center("Escape")));
        assert_eq!(keyboard.selection.command, "Escape");
        keyboard.toggle_modifier(Modifier::Ctrl);
        assert!(keyboard.press(body, metrics, center("Right")));
        assert_eq!(keyboard.selection.command, "Ctrl+Right");
        keyboard.toggle_modifier(Modifier::Shift);
        assert!(matches!(
            keyboard.key(&key_event("f6")),
            HelpCommand::Update(_)
        ));
        assert_eq!(keyboard.selection.command, "Shift+F6");
    }
}
