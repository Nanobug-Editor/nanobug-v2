//! Grapheme, word and line motions with a retained preferred column.

use super::{Mode, Vim};

impl Vim {
    /// Keep a normal-mode cursor on the final grapheme rather than the end-of-line insertion slot.
    pub fn clamp_normal(&mut self) {
        if self.mode == Mode::Insert {
            return;
        }
        let row = self.doc.location(self.doc.cursor).0;
        let range = self.doc.line_range(row);
        if self.doc.cursor >= range.end && range.end > range.start {
            self.doc.cursor = self.doc.previous(range.end);
        }
    }

    /// Place the caret at grapheme boundary `offset` and start a new preferred column on the next vertical move.
    pub fn set_cursor(&mut self, offset: usize) {
        self.doc.cursor = offset;
        self.desired_column = None;
        self.clamp_normal();
    }

    /// Move the viewport-following caret to `row` while retaining its preferred display column.
    pub fn move_to_row(&mut self, row: usize) {
        let (_, column) = self.doc.location(self.doc.cursor);
        self.place_row(row, column);
        self.clamp_normal();
    }

    /// Place the caret on `row`, taking its goal column from `column` on the first vertical move.
    pub(super) fn place_row(&mut self, row: usize, column: usize) {
        let desired = *self.desired_column.get_or_insert(column);
        self.doc.cursor = self.doc.offset(row, desired);
    }

    /// Move according to `key` and repeat count `n`; report whether the key is a supported motion.
    pub(super) fn motion(&mut self, key: &str, n: usize) -> bool {
        let (row, col) = self.doc.location(self.doc.cursor);
        let range = self.doc.line_range(row);
        match key {
            "h" | "Left" => {
                for _ in 0..n {
                    self.doc.cursor = self.doc.previous(self.doc.cursor).max(range.start);
                }
            }
            "l" | "Right" => {
                for _ in 0..n {
                    self.doc.cursor = self.doc.next(self.doc.cursor).min(range.end);
                }
            }
            "j" | "Down" | "k" | "Up" => {
                let target = if matches!(key, "j" | "Down") {
                    row.saturating_add(n)
                } else {
                    row.saturating_sub(n)
                };
                self.place_row(target, col);
                return true;
            }
            "0" | "Home" => self.doc.cursor = range.start,
            "^" => self.first_nonblank(),
            "$" | "End" => self.doc.cursor = range.end,
            "G" => self.doc.cursor = self.doc.offset(self.doc.starts.len() - 1, 0),
            "w" => {
                for _ in 0..n {
                    self.doc.cursor = self.word_forward(self.doc.cursor);
                }
            }
            "b" => {
                for _ in 0..n {
                    self.doc.cursor = self.word_backward(self.doc.cursor);
                }
            }
            "e" => {
                for _ in 0..n {
                    self.doc.cursor = self.word_end(self.doc.cursor);
                }
            }
            _ => return false,
        }
        self.desired_column = None;
        true
    }

    /// Move to the first non-whitespace byte on the current line.
    pub(super) fn first_nonblank(&mut self) {
        let row = self.doc.location(self.doc.cursor).0;
        self.doc.cursor =
            self.doc.starts[row] + self.doc.line(row).len() - self.doc.line(row).trim_start().len();
    }

    /// Classify a grapheme at byte `at` as whitespace, word text, or punctuation.
    pub(super) fn class(&self, at: usize) -> u8 {
        match self.doc.text[at..].chars().next() {
            None => 0,
            Some(c) if c.is_whitespace() => 0,
            Some(c) if c.is_alphanumeric() || c == '_' => 1,
            _ => 2,
        }
    }

    /// Start of the next word after byte `at`, skipping whitespace.
    pub(super) fn word_forward(&self, mut at: usize) -> usize {
        let class = self.class(at);
        while at < self.doc.text.len() && self.class(at) == class {
            at = self.doc.next(at);
        }
        while at < self.doc.text.len() && self.class(at) == 0 {
            at = self.doc.next(at);
        }
        at
    }

    /// Start of the preceding word before byte `at`.
    pub(super) fn word_backward(&self, at: usize) -> usize {
        let mut at = self.doc.previous(at);
        while at > 0 && self.class(at) == 0 {
            at = self.doc.previous(at);
        }
        let class = self.class(at);
        while at > 0 && self.class(self.doc.previous(at)) == class {
            at = self.doc.previous(at);
        }
        at
    }

    /// Final grapheme of the next/current word after byte `at`.
    pub(super) fn word_end(&self, at: usize) -> usize {
        let mut at = self.doc.next(at);
        while at < self.doc.text.len() && self.class(at) == 0 {
            at = self.doc.next(at);
        }
        let class = self.class(at);
        while self.doc.next(at) < self.doc.text.len() && self.class(self.doc.next(at)) == class {
            at = self.doc.next(at);
        }
        at
    }
}
