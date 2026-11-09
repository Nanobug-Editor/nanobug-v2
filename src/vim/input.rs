//! Semantic key and committed-text handling, independent of native event types.

use super::{Action, Mode, Register, Vim};
use unicode_segmentation::UnicodeSegmentation;

impl Vim {
    /// Consume native committed `text`, returning the last external action if one was requested.
    pub fn text(&mut self, text: &str, register: &mut Register) -> Action {
        let mut action = Action::None;
        for ch in text.graphemes(true) {
            action = self.key(ch, register);
        }
        action
    }

    /// Process a grapheme or symbolic `key` such as `Escape`, `Enter`, or `Ctrl+r`.
    pub fn key(&mut self, key: &str, register: &mut Register) -> Action {
        if key == "Escape" || key == "Ctrl+[" {
            if self.mode == Mode::Insert {
                let row = self.doc.location(self.doc.cursor).0;
                self.doc.cursor = self.doc.previous(self.doc.cursor).max(self.doc.starts[row]);
            }
            self.mode = Mode::Normal;
            self.cancel_pending();
            self.command.clear();
            self.clamp_normal();
            return Action::None;
        }
        if key == "Ctrl+s" {
            return Action::Save(None);
        }
        if matches!(self.mode, Mode::Command | Mode::Search) {
            return self.command_key(key);
        }
        if self.mode == Mode::Insert {
            self.insert_key(key);
            return Action::None;
        }
        if key == "Ctrl+r" {
            self.doc.undo(true);
            self.clamp_normal();
            return Action::None;
        }
        if key == "u" {
            let n = self.take_count();
            for _ in 0..n {
                self.doc.undo(false);
            }
            self.clamp_normal();
            return Action::None;
        }
        if key.len() == 1
            && key.as_bytes()[0].is_ascii_digit()
            && (key != "0" || !self.count.is_empty())
        {
            if self.count.len() < 4 {
                self.count.push_str(key);
            }
            return Action::None;
        }
        let had_count = !self.count.is_empty();
        let n = self.take_count();
        if !self.pending.is_empty() {
            let operator = std::mem::take(&mut self.pending);
            if operator == "g" {
                if key == "g" {
                    self.doc.cursor = self
                        .doc
                        .offset((n * self.operator_count).saturating_sub(1), 0);
                    self.clamp_normal();
                }
            } else {
                self.operator(
                    &operator,
                    key,
                    (n * self.operator_count).min(10_000),
                    register,
                );
            }
            return Action::None;
        }
        if self.selection().is_some() && matches!(key, "d" | "x" | "c" | "y") {
            let range = self.selection().unwrap();
            register.text = self.doc.text[range.clone()].into();
            register.linewise = self.mode == Mode::VisualLine;
            if register.linewise && !register.text.ends_with('\n') {
                register.text.push('\n');
            }
            self.mode = Mode::Normal;
            if key != "y" {
                self.doc.replace(range.clone(), "", true);
            }
            self.doc.cursor = range.start.min(self.doc.text.len());
            self.clamp_normal();
            if key == "c" {
                self.begin_insert(true);
            }
            return Action::None;
        }
        match key {
            "i" => self.begin_insert(false),
            "a" => {
                let row = self.doc.location(self.doc.cursor).0;
                self.doc.cursor = self
                    .doc
                    .next(self.doc.cursor)
                    .min(self.doc.line_range(row).end);
                self.begin_insert(false);
            }
            "I" => {
                self.first_nonblank();
                self.begin_insert(false);
            }
            "A" => {
                let row = self.doc.location(self.doc.cursor).0;
                self.doc.cursor = self.doc.line_range(row).end;
                self.begin_insert(false);
            }
            "o" | "O" => {
                let row = self.doc.location(self.doc.cursor).0;
                let indent: String = self
                    .doc
                    .line(row)
                    .chars()
                    .take_while(|c| *c == ' ' || *c == '\t')
                    .collect();
                let at = if key == "o" {
                    self.doc.line_range(row).end
                } else {
                    self.doc.starts[row]
                };
                let text = if key == "o" {
                    format!("\n{indent}")
                } else {
                    format!("{indent}\n")
                };
                self.doc.replace(at..at, &text, true);
                if key == "O" {
                    self.doc.cursor = at + indent.len();
                }
                self.begin_insert(true);
            }
            "v" => {
                self.anchor = self.doc.cursor;
                self.mode = if self.mode == Mode::Visual {
                    Mode::Normal
                } else {
                    Mode::Visual
                };
            }
            "V" => {
                self.anchor = self.doc.cursor;
                self.mode = if self.mode == Mode::VisualLine {
                    Mode::Normal
                } else {
                    Mode::VisualLine
                };
            }
            "d" | "y" | "c" | "g" => {
                self.pending = key.into();
                self.operator_count = n;
            }
            "x" | "Delete" | "s" => {
                let row = self.doc.location(self.doc.cursor).0;
                let start = self.doc.cursor;
                for _ in 0..n {
                    self.doc.cursor = self
                        .doc
                        .next(self.doc.cursor)
                        .min(self.doc.line_range(row).end);
                }
                let end = self.doc.cursor;
                self.doc.cursor = start;
                if end > start {
                    register.text = self.doc.text[start..end].into();
                    register.linewise = false;
                    self.doc.replace(start..end, "", true);
                }
                self.clamp_normal();
                if key == "s" {
                    self.begin_insert(end > start);
                }
            }
            "D" | "C" => self.operator(if key == "D" { "d" } else { "c" }, "$", n, register),
            "p" | "P" => self.paste(register, key == "P", n),
            ":" | "/" => {
                self.mode = if key == ":" {
                    Mode::Command
                } else {
                    Mode::Search
                };
                self.command.clear();
            }
            "n" | "N" => self.search(key == "N"),
            "G" => {
                self.doc.cursor = self.doc.offset(
                    if had_count {
                        n - 1
                    } else {
                        self.doc.starts.len() - 1
                    },
                    0,
                )
            }
            "J" => {
                let row = self.doc.location(self.doc.cursor).0;
                if let Some(&next) = self.doc.starts.get(row + 1) {
                    let end = next + self.doc.line(row + 1).len()
                        - self.doc.line(row + 1).trim_start().len();
                    self.doc.replace(next - 1..end, " ", true);
                    self.clamp_normal();
                }
            }
            _ => {
                self.motion(key, n);
            }
        }
        self.clamp_normal();
        Action::None
    }

    /// Cancel a partially typed motion, operator, or repeat count before switching command layers.
    pub fn cancel_pending(&mut self) {
        self.pending.clear();
        self.count.clear();
    }

    /// Consume the accumulated count, defaulting to one and limiting work per keystroke.
    pub(super) fn take_count(&mut self) -> usize {
        std::mem::take(&mut self.count)
            .parse::<usize>()
            .unwrap_or(1)
            .clamp(1, 10_000)
    }

    /// Enter insert mode; `checkpointed` joins a preceding delete/open-line operation's undo unit.
    pub(super) fn begin_insert(&mut self, checkpointed: bool) {
        self.mode = Mode::Insert;
        self.insert_checkpointed = checkpointed;
        self.desired_column = None;
    }

    /// Apply one insert-mode `key`, retaining a single undo checkpoint for the entire session.
    pub(super) fn insert_key(&mut self, key: &str) {
        let cursor = self.doc.cursor;
        let mut range = cursor..cursor;
        let mut text = key.to_string();
        match key {
            "Backspace" => {
                range = self.doc.previous(cursor)..cursor;
                text.clear();
            }
            "Delete" => {
                range = cursor..self.doc.next(cursor);
                text.clear();
            }
            "Ctrl+w" => {
                range = self.word_backward(cursor)..cursor;
                text.clear();
            }
            "Ctrl+u" => {
                range = self.doc.starts[self.doc.location(cursor).0]..cursor;
                text.clear();
            }
            "Enter" => {
                let row = self.doc.location(cursor).0;
                let indent: String = self
                    .doc
                    .line(row)
                    .chars()
                    .take_while(|c| *c == ' ' || *c == '\t')
                    .collect();
                text = format!("\n{indent}");
            }
            "Tab" => text = "\t".into(),
            "Left" | "Right" | "Up" | "Down" | "Home" | "End" => {
                self.motion(key, 1);
                return;
            }
            _ if key.starts_with("Ctrl+")
                || key.starts_with("Alt+")
                || key.starts_with('F') && key.len() > 1 =>
            {
                return;
            }
            _ => {}
        }
        if range.is_empty() && text.is_empty() {
            return;
        }
        self.doc.replace(range, &text, !self.insert_checkpointed);
        self.insert_checkpointed = true;
    }
}
