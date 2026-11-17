//! Route Neovim's terminal escape keys, review motions and VT input to their owners.

use super::{Events, Terminal};
use crate::ui::keys::normal_character;
use alacritty_terminal::{
    grid::{Dimensions, Scroll},
    index::{Column, Line, Point, Side},
    selection::{Selection, SelectionType},
    term::{Term, TermMode},
    vi_mode::{ViModeCursor, ViMotion},
};
use gpui::{Keystroke, Modifiers};
use nanobug::vim::SequenceHint;

/// Result of handling a native terminal key.
pub(in crate::grid) enum KeyResult {
    /// IME or the native character callback owns this printable key.
    Text,
    /// Key handled by the terminal.
    Consume,
    /// Delegate an Ex prompt to the normal editor's parser.
    Command,
    /// Copy the selected terminal text to the system clipboard.
    Copy(String),
    /// Paste native clipboard text into the terminal.
    Paste,
    /// Remove an exited terminal buffer.
    Close,
}

impl Terminal {
    /// Describe the pending escape or movement family to the existing delayed hint panel.
    pub(in crate::grid) fn sequence_hint(&self) -> Option<SequenceHint> {
        if self.escape && self.accepts_text() {
            Some(SequenceHint {
                prefix: "Ctrl+\\".into(),
                group: "terminal.group",
                continuations: vec![
                    ("Ctrl+n", "terminal.normal_key", false),
                    ("Ctrl+o", "terminal.once_key", false),
                ],
            })
        } else if self.goto {
            Some(SequenceHint {
                prefix: "g".into(),
                group: "vim_hint.goto_group",
                continuations: vec![("g", "vim_hint.first_line", false)],
            })
        } else {
            None
        }
    }

    /// Suppress review prefixes when focus moves to another buffer or native surface.
    pub(in crate::grid) fn clear_prefix(&mut self) {
        self.escape = false;
        self.goto = false;
        self.count = None;
        self.once = false;
    }

    /// Return to Terminal mode after the delegated Normal command completed.
    pub(in crate::grid) fn finish_normal(&mut self) {
        if std::mem::take(&mut self.once) {
            self.set_input(true);
        }
    }

    /// Consume a standalone Normal count for the shared Ctrl+w parser.
    pub(in crate::grid) fn take_window_count(&mut self) -> Option<Option<usize>> {
        (!self.goto && !self.accepts_text()).then(|| self.count.take())
    }

    /// Enter Terminal-Normal mode and stop positioning around the live PTY cursor.
    pub(in crate::grid) fn enter_normal(&mut self) {
        self.escape = false;
        self.set_input(false);
    }

    /// Enter live Terminal input mode and move the viewport to the child.
    pub(in crate::grid) fn enter_input(&mut self) {
        self.clear_prefix();
        self.set_input(true);
    }

    /// Dispatch a `key` either to the active child or through the terminal review mode.
    pub(in crate::grid) fn key(&mut self, key: &Keystroke) -> KeyResult {
        if self.accepts_text() {
            return self.input_key(key);
        }
        self.normal_key(key)
    }

    /// Consume the Ctrl+backslash prefix and send all other terminal keys to the child.
    fn input_key(&mut self, key: &Keystroke) -> KeyResult {
        let ctrl = key.modifiers
            == (Modifiers {
                control: true,
                ..Modifiers::default()
            });
        if std::mem::take(&mut self.escape) {
            if ctrl && matches!(key.key.as_str(), "n" | "o") {
                self.once = key.key == "o";
                self.set_input(false);
                return KeyResult::Consume;
            }
        } else if ctrl && key.key == "\\" {
            self.escape = true;
            return KeyResult::Consume;
        }
        if key.key == "v"
            && key.modifiers
                == (Modifiers {
                    control: true,
                    shift: true,
                    ..Modifiers::default()
                })
        {
            return KeyResult::Paste;
        }
        let mode = *self.session.screen.lock().mode();
        if let Some(bytes) = vt_key(key, mode) {
            self.write(bytes);
            KeyResult::Consume
        } else {
            KeyResult::Text
        }
    }

    /// Review PTY scrollback using the window's Normal-mode cursor.
    fn normal_key(&mut self, key: &Keystroke) -> KeyResult {
        let raw = key.key.as_str();
        let ctrl = key.modifiers
            == (Modifiers {
                control: true,
                ..Modifiers::default()
            });
        let plain = key.modifiers == Modifiers::default();
        let ch = normal_character(key);
        let normal = ch.as_deref().unwrap_or(raw);
        if let [b @ b'0'..=b'9'] = normal.as_bytes()
            && (plain || key.modifiers.shift && ch.is_some())
            && (*b != b'0' || self.count.is_some())
        {
            self.count = Some((self.count.unwrap_or(0) * 10 + usize::from(*b - b'0')).min(9999));
            return KeyResult::Consume;
        }
        let count = self.count.take();
        let n = count.unwrap_or(1);
        let goto = std::mem::take(&mut self.goto);
        if !goto {
            if matches!(ch.as_deref(), Some("i" | "I" | "a" | "A")) {
                if self.exited() {
                    return KeyResult::Close;
                }
                self.enter_input();
                return KeyResult::Consume;
            }
            if ch.as_deref() == Some(":") {
                return KeyResult::Command;
            }
            if ch.as_deref() == Some("g") {
                self.goto = true;
                self.count = count;
                return KeyResult::Consume;
            }
        }
        let mut copied = None;
        {
            let session = self.session.clone();
            let mut term = session.screen.lock();
            self.sync_review(&term);
            if goto && ch.as_deref() == Some("g") || ch.as_deref() == Some("G") {
                let top = term.topmost_line();
                let bottom = term.bottommost_line();
                let line = count.map_or(if goto { top } else { bottom }, |count| {
                    (top + count.saturating_sub(1) as i32).min(bottom)
                });
                self.goto_review(&term, Point::new(line, Column(0)));
                self.motion_review(&mut term, ViMotion::FirstOccupied);
            } else if !goto {
                let motion = match (ctrl, normal) {
                    (false, "h" | "left" | "backspace") | (true, "h") => Some(ViMotion::Left),
                    (false, "j" | "down") | (true, "n") => Some(ViMotion::Down),
                    (false, "k" | "up") | (true, "p") => Some(ViMotion::Up),
                    (false, "l" | "right") => Some(ViMotion::Right),
                    (false, "0" | "home") => Some(ViMotion::First),
                    (false, "$" | "end") => Some(ViMotion::Last),
                    (false, "^") => Some(ViMotion::FirstOccupied),
                    (false, "w") => Some(ViMotion::SemanticRight),
                    (false, "b") => Some(ViMotion::SemanticLeft),
                    (false, "e") => Some(ViMotion::SemanticRightEnd),
                    (false, "W") => Some(ViMotion::WordRight),
                    (false, "B") => Some(ViMotion::WordLeft),
                    (false, "E") => Some(ViMotion::WordRightEnd),
                    (false, "H") => Some(ViMotion::High),
                    (false, "M") => Some(ViMotion::Middle),
                    (false, "L") => Some(ViMotion::Low),
                    (false, "%") => Some(ViMotion::Bracket),
                    (false, "{") => Some(ViMotion::ParagraphUp),
                    (false, "}") => Some(ViMotion::ParagraphDown),
                    _ => None,
                };
                if let Some(motion) =
                    motion.filter(|_| !key.modifiers.modified() || ctrl || ch.is_some())
                {
                    for _ in 0..n {
                        self.motion_review(&mut term, motion);
                    }
                } else {
                    let page = self.view_rows(&term);
                    let scroll = match (ctrl, raw) {
                        (true, "b") | (false, "pageup") => Some(page),
                        (true, "f") | (false, "pagedown") => Some(page),
                        (true, "u" | "d") => Some((page / 2).max(1)),
                        (true, "y" | "e") => Some(1),
                        _ => None,
                    };
                    if let Some(step) = scroll {
                        let up = matches!(raw, "b" | "pageup" | "u" | "y");
                        let delta = step.saturating_mul(n).min(isize::MAX as usize) as isize;
                        self.scroll_review(&term, if up { -delta } else { delta });
                    } else if plain && raw == "escape" {
                        self.selection = None;
                    } else if matches!(ch.as_deref(), Some("v" | "V")) || ctrl && raw == "v" {
                        let ty = match ch.as_deref() {
                            Some("V") => SelectionType::Lines,
                            Some("v") => SelectionType::Simple,
                            _ => SelectionType::Block,
                        };
                        let point = self.review_point(&term);
                        self.selection = match self.selection.take() {
                            Some(previous) if previous.ty == ty => None,
                            _ => {
                                let mut selection = Selection::new(ty, point, Side::Left);
                                selection.include_all();
                                Some(selection)
                            }
                        };
                    } else if ch.as_deref() == Some("y") {
                        term.selection = self.selection.take();
                        copied = term.selection_to_string();
                        term.selection = None;
                    } else if self.exited() && plain && raw == "enter" {
                        return KeyResult::Close;
                    }
                }
            }
        }
        self.finish_normal();
        copied.map_or(KeyResult::Consume, KeyResult::Copy)
    }

    /// Apply `motion` to this review cursor while borrowing cell traversal from `term`.
    fn motion_review(&mut self, term: &mut Term<Events>, motion: ViMotion) {
        let (point, motion) = match motion {
            ViMotion::High | ViMotion::Middle | ViMotion::Low => {
                let height = self.view_rows(term);
                let first = self.target_top(term);
                let row = match motion {
                    ViMotion::High => first,
                    ViMotion::Middle => first + (height - 1) / 2,
                    _ => first + height - 1,
                };
                (
                    Point::new(term.topmost_line() + row as i32, Column(0)),
                    ViMotion::FirstOccupied,
                )
            }
            motion => (self.review_point(term), motion),
        };
        let point = ViModeCursor::new(point).motion(term, motion).point;
        // Alacritty's motion updates its own grid offset. Nanobug scrolls each view independently.
        term.grid_mut().scroll_display(Scroll::Bottom);
        self.goto_review(term, point);
    }

    /// Move the review caret to a clicked cell in the visible terminal.
    pub(in crate::grid) fn click(&mut self, column: usize, row: usize) {
        self.enter_normal();
        let session = self.session.clone();
        let term = session.screen.lock();
        self.sync_review(&term);
        let first = term.topmost_line() + self.scroll.first_cell() as i32;
        let row = Line((first.0 + row as i32).min(term.bottommost_line().0));
        let column = Column(self.left.saturating_add(column).min(term.last_column().0));
        self.goto_review(&term, Point::new(row, column));
    }
}

/// Translate a native non-text `key` into terminal bytes for the current VT `mode`.
fn vt_key(key: &Keystroke, mode: TermMode) -> Option<Vec<u8>> {
    let raw = key.key.as_str();
    let mods = key.modifiers;
    let app = mode.contains(TermMode::APP_CURSOR);
    let tail = match raw {
        "up" => Some('A'),
        "down" => Some('B'),
        "right" => Some('C'),
        "left" => Some('D'),
        "home" => Some('H'),
        "end" => Some('F'),
        "f1" => Some('P'),
        "f2" => Some('Q'),
        "f3" => Some('R'),
        "f4" => Some('S'),
        _ => None,
    };
    let modifier = 1 + u8::from(mods.shift) + 2 * u8::from(mods.alt) + 4 * u8::from(mods.control);
    if let Some(tail) = tail {
        return Some(if modifier > 1 {
            format!("\x1b[1;{modifier}{tail}").into_bytes()
        } else if app || matches!(tail, 'P'..='S') {
            format!("\x1bO{tail}").into_bytes()
        } else {
            format!("\x1b[{tail}").into_bytes()
        });
    }
    let csi = match raw {
        "insert" => 2,
        "delete" => 3,
        "pageup" => 5,
        "pagedown" => 6,
        "f5" => 15,
        "f6" => 17,
        "f7" => 18,
        "f8" => 19,
        "f9" => 20,
        "f10" => 21,
        "f11" => 23,
        "f12" => 24,
        _ => 0,
    };
    if csi != 0 {
        return Some(if modifier > 1 {
            format!("\x1b[{csi};{modifier}~").into_bytes()
        } else {
            format!("\x1b[{csi}~").into_bytes()
        });
    }
    if raw == "tab" && mods.shift {
        return Some(b"\x1b[Z".to_vec());
    }
    let mut bytes = match raw {
        "enter" => vec![b'\r'],
        "tab" => vec![b'\t'],
        "escape" => vec![0x1b],
        "backspace" => vec![0x7f],
        _ if mods.control => {
            let value = match raw.as_bytes() {
                [b @ (b'a'..=b'z')] => b & 0x1f,
                [b @ (b'['..=b'_')] => b & 0x1f,
                b" " | b"space" | b"2" | b"@" => 0,
                b"6" => 0x1e,
                b"-" => 0x1f,
                _ => return None,
            };
            vec![value]
        }
        _ if mods.alt && !mods.platform => {
            let mut printable = key.clone();
            printable.modifiers.alt = false;
            normal_character(&printable)?.into_bytes()
        }
        _ => return None,
    };
    if mods.alt {
        bytes.insert(0, 0x1b);
    }
    Some(bytes)
}
