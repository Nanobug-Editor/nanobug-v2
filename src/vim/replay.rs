//! Capture native input as repeatable edits and register macros.

use super::{Action, Mode, Register, ScreenMotionResolver, Vim};

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

/// Position of a dot repeat or counted macro waiting for a host action.
struct Frame {
    /// Strokes replayed once for dot, or repeatedly for a macro.
    keys: Vec<Stroke>,
    /// Next stroke in the current iteration.
    next: usize,
    /// Iterations remaining, including the current one.
    remaining: usize,
    /// Next physical line for a linewise Visual macro; ordinary playback keeps its current caret.
    row: Option<usize>,
    /// Dot repeat disables recapture of the change being replayed.
    dot: bool,
    /// Ordinary registers are typed through current mappings; @: and @= are literal.
    remap: bool,
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
    /// Suspended frames, innermost first, waiting for the current host action.
    frames: Vec<Frame>,
    /// Whether a sleep timer still owns this buffer's replay.
    sleeping: bool,
}

/// Whether `action` will later resume replay on the current buffer.
pub(super) fn defers_replay(action: &Action) -> bool {
    matches!(action, Action::SyntaxSelection) || is_sleep(action)
}

/// Match both Normal-mode `gs` and Ex's timed input pause.
fn is_sleep(action: &Action) -> bool {
    matches!(
        action,
        Action::Sleep(_) | Action::WindowExtras(super::WindowCommand::Sleep { .. })
    )
}

impl Replay {
    /// Whether Normal input is coming from a register rather than the active keyboard.
    pub(super) fn playing_register(&self) -> bool {
        self.macro_depth > 0
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;

    /// Counted Ex replay re-evaluates the substitution on the updated line each time.
    #[test]
    fn repeat_last_ex_with_count_and_previous_register() {
        let mut vim = Vim::new(Document::new("foo foo foo foo foo\n"));
        let mut register = Register::default();
        vim.text(":s/foo/bar/", &mut register);
        vim.key("Enter", &mut register);
        vim.text("2@:", &mut register);
        assert_eq!(vim.doc.text, "bar bar bar foo foo\n");
        vim.text("@@", &mut register);
        assert_eq!(vim.doc.text, "bar bar bar bar foo\n");
    }

    /// Record to numbered and unnamed registers and replay at column one on selected lines.
    #[test]
    fn visual_line_replays_named_and_unnamed_macros() {
        let mut vim = Vim::new(Document::new("abc\ndef\nghi\n"));
        let mut register = Register::default();
        vim.text("q1xq", &mut register);
        vim.text("Vj@1", &mut register);
        assert_eq!(vim.doc.text, "c\nef\nghi\n");
        assert_eq!(vim.mode, Mode::Normal);
        vim.text("q\"xqj@\"", &mut register);
        assert_eq!(vim.doc.text, "c\nf\nhi\n");
        assert_eq!(register.text, "g");
    }
}

impl Vim {
    /// Discard a superseded syntax request along with any suspended dot or macro playback.
    pub(super) fn cancel_syntax_request(&mut self) {
        if self.syntax_selection.cancel_request() {
            self.stop_deferred_replay();
        }
    }

    /// End playback and undo grouping after abandoning a deferred host action.
    pub(super) fn stop_deferred_replay(&mut self) {
        self.replay.sequence.clear();
        self.replay.frames.clear();
        self.replay.repeating = false;
        self.replay.macro_depth = 0;
        self.replay.macro_undo = None;
        self.replay.sleeping = false;
    }

    /// Cancel an interrupted sleep and discard its remaining playback.
    pub fn interrupt_sleep(&mut self) {
        if self.replay.sleeping {
            self.stop_deferred_replay();
            if let Some(resume) = self.insert_resume.take() {
                self.resume_insert(resume);
            }
        }
    }

    /// Resume sleep playback with `register` and the pane's measured `resolve_screen`.
    pub fn resume_sleep(
        &mut self,
        register: &mut Register,
        resolve_screen: &mut ScreenMotionResolver<'_>,
    ) -> Action {
        if !std::mem::take(&mut self.replay.sleeping) {
            return Action::None;
        }
        let insert_resume = self.insert_resume.take();
        if insert_resume.is_none() {
            self.finish_replay_change(&Action::None);
        }
        let action = self.resume_deferred_replay(register, resolve_screen);
        if defers_replay(&action) {
            self.insert_resume = insert_resume.or(self.insert_resume.take());
        } else if let Some(resume) = insert_resume {
            self.resume_insert(resume);
            self.finish_replay_change(&action);
        }
        action
    }

    /// Apply `stroke` and record it in `register` or the last change.
    /// Resolve vertical screen motions through `resolve_screen`.
    pub(super) fn dispatch(
        &mut self,
        stroke: Stroke,
        register: &mut Register,
        resolve_screen: &mut ScreenMotionResolver<'_>,
    ) -> Action {
        self.cancel_syntax_request();
        if stroke.is_key("Ctrl+s")
            && !matches!(
                self.pending,
                Some(super::Pending::InsertArgument(_) | super::Pending::InsertCompletion)
            )
        {
            if let Some(resume) = self.insert_resume.take() {
                self.resume_insert(resume);
            }
            return if self.read_only {
                self.reject_edit()
            } else {
                Action::Save(None)
            };
        }
        self.interrupt_sleep();
        if self.screen_column.is_some() {
            let continuing_screen_motion = match self.pending {
                Some(super::Pending::Prefix('g', ..)) => ["j", "k", "Up", "Down"]
                    .iter()
                    .any(|key| stroke.is_key(key)),
                None if !self.mode.accepts_text() => {
                    stroke.is_key("g")
                        || stroke.is_digit() && (!stroke.is_key("0") || self.count.is_some())
                }
                _ => false,
            };
            if !continuing_screen_motion {
                self.screen_column = None;
            }
        }
        if !(self.mode == Mode::Normal && !self.awaiting_key() && stroke.is_key("q")) {
            self.capture_macro_input(std::iter::once_with(|| stroke.clone()), register);
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
        // A macro invoked by Ctrl+o must finish its own strokes before Insert resumes.
        let insert_resume = self.insert_resume.take();
        let action = match stroke {
            Stroke::Key(key) => self.handle_key(&key, register, resolve_screen),
            Stroke::Text(text) => self.handle_text(&text, register),
        };
        self.settle_fold_cursor(prior.2, prior.3);
        if is_sleep(&action) {
            self.replay.sleeping = true;
        }
        if defers_replay(&action) {
            self.insert_resume = insert_resume.or(self.insert_resume.take());
        } else if let Some(resume) = insert_resume {
            self.resume_insert(resume);
        }
        self.track_g_input(prior.0, prior.1, prior.2, prior.3, !history_key);
        if !defers_replay(&action) {
            self.finish_replay_change(&action);
        }
        action
    }

    /// Record original modal-map left sides and other keystrokes, up to the macro limit.
    pub(super) fn capture_macro_input(
        &self,
        strokes: impl Iterator<Item = Stroke>,
        register: &mut Register,
    ) {
        if self.replay.macro_depth == 0
            && !self.replay.repeating
            && !self.input_maps.suppress_macro_input
            && let Some((_, keys)) = &mut register.recording
        {
            keys.extend(strokes.take(10_000usize.saturating_sub(keys.len())));
        }
    }

    /// Finalize a completed command and record it as the dot-repeat target if it changed text.
    pub(super) fn finish_replay_change(&mut self, action: &Action) {
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
    }

    /// Re-execute the last change through `register` and `resolve_screen`, overriding its `count`.
    pub(super) fn repeat_change(
        &mut self,
        count: Option<usize>,
        register: &mut Register,
        resolve_screen: &mut ScreenMotionResolver<'_>,
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
        self.run_frame(
            Frame {
                keys,
                next: 0,
                remaining: 1,
                row: None,
                dot: true,
                remap: false,
            },
            register,
            resolve_screen,
        )
    }

    /// Execute macro `name` from `register` `count` times, resolving motions through `resolve_screen`.
    pub(super) fn play_macro(
        &mut self,
        name: char,
        count: usize,
        register: &mut Register,
        resolve_screen: &mut ScreenMotionResolver<'_>,
    ) -> Action {
        let keys = if name == '=' {
            return self.repeat_expression_macro(count, register, resolve_screen);
        } else if name == ':' {
            let Some(command) = register.command_history.latest(':') else {
                self.message = crate::i18n::text("vim_hint.no_ex_history").into();
                return Action::None;
            };
            vec![
                Stroke::Key(":".into()),
                Stroke::Text(command.into()),
                Stroke::Key("Enter".into()),
            ]
        } else {
            register.macro_keys(name)
        };
        self.play_keys(name, keys, count, register, resolve_screen)
    }

    /// Run semantic keys under name, retaining the same count and recursion limits as macros.
    pub(super) fn play_keys(
        &mut self,
        name: char,
        keys: Vec<Stroke>,
        count: usize,
        register: &mut Register,
        resolve_screen: &mut ScreenMotionResolver<'_>,
    ) -> Action {
        if keys.is_empty() || count == 0 || self.replay.macro_depth == 16 {
            return Action::None;
        }
        let (row, remaining) = if self.mode == Mode::VisualLine {
            let start = self.doc.location(self.anchor.min(self.doc.cursor)).0;
            let last = self.doc.location(self.anchor.max(self.doc.cursor)).0;
            self.mode = Mode::Normal;
            self.track_g_input(
                Mode::VisualLine,
                self.anchor,
                self.doc.cursor,
                self.doc.revision,
                false,
            );
            (Some(start), last - start + 1)
        } else {
            (None, count)
        };
        if self.replay.macro_depth == 0 {
            self.replay.budget = 10_000;
            self.replay.macro_undo = Some(self.doc.revision);
        }
        register.last_played = Some(name);
        self.replay.sequence.clear();
        self.replay.macro_depth += 1;
        self.run_frame(
            Frame {
                keys,
                next: 0,
                remaining,
                row,
                dot: false,
                remap: !matches!(name, ':' | '='),
            },
            register,
            resolve_screen,
        )
    }

    /// Continue any dot or macro frames with `register` and `resolve_screen` after a host reply.
    pub(super) fn resume_deferred_replay(
        &mut self,
        register: &mut Register,
        resolve_screen: &mut ScreenMotionResolver<'_>,
    ) -> Action {
        let mut frames = std::mem::take(&mut self.replay.frames).into_iter();
        while let Some(frame) = frames.next() {
            let action = self.run_frame(frame, register, resolve_screen);
            if action != Action::None {
                if defers_replay(&action) {
                    self.replay.frames.extend(frames);
                } else {
                    self.stop_deferred_replay();
                }
                return action;
            }
        }
        Action::None
    }

    /// Replay `frame` until it finishes or a host action suspends or completes it.
    fn run_frame(
        &mut self,
        mut frame: Frame,
        register: &mut Register,
        resolve_screen: &mut ScreenMotionResolver<'_>,
    ) -> Action {
        let mut action = Action::None;
        while frame.remaining > 0 && (self.replay.macro_depth == 0 || self.replay.budget > 0) {
            if frame.next == 0
                && let Some(row) = frame.row
            {
                if row >= self.line_count() {
                    break;
                }
                self.set_cursor(self.doc.starts[row]);
            }
            let Some(stroke) = frame.keys.get(frame.next) else {
                frame.remaining -= 1;
                frame.next = 0;
                if let Some(row) = &mut frame.row {
                    *row += 1;
                }
                continue;
            };
            frame.next += 1;
            if self.replay.macro_depth > 0 {
                self.replay.budget -= 1;
            }
            action = match stroke {
                Stroke::Key(key) if frame.remap => {
                    self.dispatch_macro_key(key, register, resolve_screen)
                }
                _ => self.dispatch(stroke.clone(), register, resolve_screen),
            };
            if defers_replay(&action) {
                self.replay.frames.push(frame);
                return action;
            }
            if action != Action::None {
                break;
            }
        }
        if frame.remap && action == Action::None && self.input_maps.has_prefix() {
            action = self.flush_input_prefix(register, resolve_screen);
            if defers_replay(&action) {
                self.replay.frames.push(frame);
                return action;
            }
        }
        if frame.dot {
            self.replay.repeating = false;
        } else {
            self.replay.macro_depth -= 1;
            if self.replay.macro_depth == 0 {
                self.replay.macro_undo = None;
            }
        }
        action
    }
}
