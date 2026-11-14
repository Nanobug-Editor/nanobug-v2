//! Capture native input as repeatable edits and register macros.

use super::{Action, Mode, Register, Vim};

/// One semantic keystroke or one committed IME/clipboard string.
#[derive(Clone)]
pub(super) enum Stroke {
    /// A grapheme command or a symbolic key.
    Key(String),
    /// Literal text already committed by the platform.
    Text(String),
}

impl Stroke {
    /// Convert the common special keys to their textual register representation.
    pub(super) fn register_text(&self) -> &str {
        match self {
            Self::Key(key) => match key.as_str() {
                "Escape" | "Ctrl+[" => "\u{1b}",
                "Enter" => "\n",
                "Tab" => "\t",
                _ => key,
            },
            Self::Text(text) => text,
        }
    }

    /// Check whether this stroke is the named semantic `key`.
    fn is_key(&self, key: &str) -> bool {
        matches!(self, Self::Key(value) if value == key)
    }

    /// Whether the key can be part of a numeric prefix.
    fn is_digit(&self) -> bool {
        matches!(self, Self::Key(key) if matches!(key.as_bytes(), [b'0'..=b'9']))
    }
}

/// Capture for a pending change and limits shared by nested playback.
#[derive(Default)]
pub(super) struct Replay {
    /// Input since the start of the command or Insert session.
    pub sequence: Vec<Stroke>,
    /// Document revision at the start of the captured sequence.
    revision: u64,
    /// Last sequence that changed text.
    change: Vec<Stroke>,
    /// Whether a dot repeat is being expanded.
    repeating: bool,
    /// Nesting depth and remaining input budget of register playback.
    macro_depth: usize,
    budget: usize,
    /// Revision before the first change of a macro playback, including its count.
    macro_undo: Option<u64>,
}

impl Replay {
    /// Whether this edit at `revision` should create an undo checkpoint.
    pub(super) fn checkpoint(&self, revision: u64, requested: bool) -> bool {
        self.macro_undo.map_or(requested, |start| revision == start)
    }

    /// Start a new macro undo unit after an explicit history command.
    pub(super) fn reset_macro_undo(&mut self, revision: u64) {
        if let Some(start) = &mut self.macro_undo {
            *start = revision;
        }
    }
}

impl Vim {
    /// Apply `stroke` and capture typed input in the active register and last change.
    pub(super) fn dispatch(&mut self, stroke: Stroke, register: &mut Register) -> Action {
        if self.replay.macro_depth == 0
            && !self.replay.repeating
            && !(self.mode == Mode::Normal && !self.awaiting_key() && stroke.is_key("q"))
            && let Some((_, keys)) = &mut register.recording
            && keys.len() < 10_000
        {
            keys.push(stroke.clone());
        }
        if !self.replay.repeating {
            if self.replay.sequence.is_empty() {
                self.replay.revision = self.doc.revision;
            }
            self.replay.sequence.push(stroke.clone());
        }
        let prior = (self.mode, self.anchor, self.doc.cursor, self.doc.revision);
        let history_key = match &stroke {
            Stroke::Key(key) if !self.mode.accepts_text() => match self.pending {
                None => matches!(key.as_str(), "u" | "Ctrl+r"),
                Some(super::Pending::Prefix('g', None, _)) => matches!(key.as_str(), "+" | "-"),
                _ => false,
            },
            _ => false,
        };
        let action = match stroke {
            Stroke::Key(key) => self.handle_key(&key, register),
            Stroke::Text(text) => self.handle_text(&text, register),
        };
        self.settle_fold_cursor(prior.2, prior.3);
        self.track_g_input(prior.0, prior.1, prior.2, prior.3, !history_key);
        if !self.replay.repeating
            && !self.awaiting_key()
            && !self.mode.accepts_text()
            && !self.mode.is_visual()
        {
            let sequence = std::mem::take(&mut self.replay.sequence);
            let mut command = sequence.iter().skip_while(|stroke| stroke.is_digit());
            let first = command.next();
            let first = if first.is_some_and(|stroke| stroke.is_key("\"")) {
                command.next();
                command.find(|stroke| !stroke.is_digit())
            } else {
                first
            };
            let recordable = first.is_some_and(|stroke| {
                !["Ctrl+r", "u", ".", ":"]
                    .iter()
                    .any(|key| stroke.is_key(key))
                    && !(stroke.is_key("g")
                        && command
                            .next()
                            .is_some_and(|key| key.is_key("+") || key.is_key("-")))
            });
            if recordable
                && (self.replay.revision != self.doc.revision
                    || matches!(action, Action::Filter { .. }))
                && !sequence.is_empty()
            {
                self.replay.change = sequence;
            }
        }
        action
    }

    /// Re-execute the last change, optionally replacing its leading `count`.
    pub(super) fn repeat_change(
        &mut self,
        count: Option<usize>,
        register: &mut Register,
    ) -> Action {
        if self.replay.repeating || self.replay.change.is_empty() {
            return Action::None;
        }
        let mut keys = self.replay.change.clone();
        if let Some(count) = count {
            let digits = keys.iter().take_while(|stroke| stroke.is_digit()).count();
            let mut command = digits;
            if keys.get(command).is_some_and(|stroke| stroke.is_key("\"")) {
                command += 2;
                let after = command
                    + keys[command..]
                        .iter()
                        .take_while(|stroke| stroke.is_digit())
                        .count();
                keys.drain(command..after);
            }
            if keys.get(command).is_some_and(|stroke| {
                ["d", "c", "y", "<", ">", "=", "!"]
                    .iter()
                    .any(|key| stroke.is_key(key))
            }) {
                let motion = command + 1;
                let after = motion
                    + keys[motion..]
                        .iter()
                        .take_while(|stroke| stroke.is_digit())
                        .count();
                // A standalone zero is the line-start motion rather than a count.
                if !keys.get(motion).is_some_and(|stroke| stroke.is_key("0")) {
                    keys.drain(motion..after);
                }
            }
            keys.splice(
                ..digits,
                count
                    .to_string()
                    .chars()
                    .map(|ch| Stroke::Key(ch.to_string())),
            );
        }
        self.replay.sequence.clear();
        self.replay.repeating = true;
        let action = self.run_strokes(&keys, register);
        self.replay.repeating = false;
        action
    }

    /// Execute register `name` `count` times, sharing the budget with nested macros.
    pub(super) fn play_macro(
        &mut self,
        name: char,
        count: usize,
        register: &mut Register,
    ) -> Action {
        if self.replay.macro_depth == 16 {
            return Action::None;
        }
        let keys = register.macro_keys(name);
        if keys.is_empty() {
            return Action::None;
        }
        if self.replay.macro_depth == 0 {
            self.replay.budget = 10_000;
            self.replay.macro_undo = Some(self.doc.revision);
        }
        register.last_played = Some(name);
        self.replay.sequence.clear();
        self.replay.macro_depth += 1;
        let mut action = Action::None;
        for _ in 0..count {
            action = self.run_strokes(&keys, register);
            if action != Action::None || self.replay.budget == 0 {
                break;
            }
        }
        self.replay.macro_depth -= 1;
        if self.replay.macro_depth == 0 {
            self.replay.macro_undo = None;
        }
        action
    }

    /// Dispatch recorded `keys` until exhausted or a host operation is requested.
    fn run_strokes(&mut self, keys: &[Stroke], register: &mut Register) -> Action {
        for stroke in keys {
            if self.replay.macro_depth > 0 {
                if self.replay.budget == 0 {
                    break;
                }
                self.replay.budget -= 1;
            }
            let action = self.dispatch(stroke.clone(), register);
            if action != Action::None {
                return action;
            }
        }
        Action::None
    }
}
