//! Small, native modal editor. This module intentionally does not emulate complete Vim.
use crate::document::Document;
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

/// Editing mode, independent of native focus and cursor animation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Character motions and editing operators.
    Normal,
    /// Native text entry and IME composition.
    Insert,
    /// Inclusive character selection.
    Visual,
    /// Whole-line selection.
    VisualLine,
    /// A short Ex command entered after `:`.
    Command,
    /// Literal text search entered after `/`.
    Search,
}

impl Mode {
    /// Short mode name displayed in the status line.
    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "NORMAL",
            Self::Insert => "INSERT",
            Self::Visual => "VISUAL",
            Self::VisualLine => "V-LINE",
            Self::Command => "COMMAND",
            Self::Search => "SEARCH",
        }
    }
}

/// Unnamed yank/delete register, shared across the editor's open buffers.
#[derive(Default)]
pub struct Register {
    /// Copied or deleted UTF-8 text.
    pub text: String,
    /// Whether paste inserts complete lines.
    pub linewise: bool,
}

/// Filesystem and window actions handled by the native shell rather than the text model.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// No external action.
    None,
    /// Save, optionally to a new project-relative filename.
    Save(Option<String>),
    /// Open a project-relative file.
    Open(String),
    /// Close the app, optionally discarding unsaved buffers.
    Close(bool),
    /// Save the active file, then close if every buffer is clean.
    SaveClose,
}

/// One buffer's modal state and text; the shell owns the shared register and file tree.
pub struct Vim {
    /// Editable document.
    pub doc: Document,
    /// Current editing mode.
    pub mode: Mode,
    /// Current Ex command or literal search query, without its prefix.
    pub command: String,
    /// Last feedback shown in the status line.
    pub message: String,
    /// Visual selection anchor in UTF-8 bytes.
    pub anchor: usize,
    /// Pending motion/operator, such as `d` or `g`.
    pending: String,
    /// Count typed before the next operation.
    count: String,
    /// Count captured before a pending operator.
    operator_count: usize,
    /// Whether the active insert session has captured an undo checkpoint.
    insert_checkpointed: bool,
    /// Desired display column retained during vertical movement.
    desired_column: Option<usize>,
    /// Last accepted literal search pattern.
    last_search: String,
}

impl Vim {
    /// Wrap `doc` in normal mode at its existing cursor.
    pub fn new(doc: Document) -> Self {
        Self {
            doc,
            mode: Mode::Normal,
            command: String::new(),
            message: String::new(),
            anchor: 0,
            pending: String::new(),
            count: String::new(),
            operator_count: 1,
            insert_checkpointed: false,
            desired_column: None,
            last_search: String::new(),
        }
    }

    /// Return the inclusive visual selection as a half-open UTF-8 interval.
    pub fn selection(&self) -> Option<Range<usize>> {
        let a = self.anchor.min(self.doc.cursor);
        let b = self.anchor.max(self.doc.cursor);
        match self.mode {
            Mode::Visual => Some(a..self.doc.next(b)),
            Mode::VisualLine => {
                let first = self.doc.location(a).0;
                let last = self.doc.location(b).0;
                Some(
                    self.doc.starts[first]
                        ..self
                            .doc
                            .starts
                            .get(last + 1)
                            .copied()
                            .unwrap_or(self.doc.text.len()),
                )
            }
            _ => None,
        }
    }

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
    fn take_count(&mut self) -> usize {
        std::mem::take(&mut self.count)
            .parse::<usize>()
            .unwrap_or(1)
            .clamp(1, 10_000)
    }

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
    fn place_row(&mut self, row: usize, column: usize) {
        let desired = *self.desired_column.get_or_insert(column);
        self.doc.cursor = self.doc.offset(row, desired);
    }

    /// Enter insert mode; `checkpointed` joins a preceding delete/open-line operation's undo unit.
    fn begin_insert(&mut self, checkpointed: bool) {
        self.mode = Mode::Insert;
        self.insert_checkpointed = checkpointed;
        self.desired_column = None;
    }

    /// Apply one insert-mode `key`, retaining a single undo checkpoint for the entire session.
    fn insert_key(&mut self, key: &str) {
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

    /// Move according to `key` and repeat count `n`; report whether the key is a supported motion.
    fn motion(&mut self, key: &str, n: usize) -> bool {
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
    fn first_nonblank(&mut self) {
        let row = self.doc.location(self.doc.cursor).0;
        self.doc.cursor =
            self.doc.starts[row] + self.doc.line(row).len() - self.doc.line(row).trim_start().len();
    }

    /// Classify a grapheme at byte `at` as whitespace, word text, or punctuation.
    fn class(&self, at: usize) -> u8 {
        match self.doc.text[at..].chars().next() {
            None => 0,
            Some(c) if c.is_whitespace() => 0,
            Some(c) if c.is_alphanumeric() || c == '_' => 1,
            _ => 2,
        }
    }

    /// Start of the next word after byte `at`, skipping whitespace.
    fn word_forward(&self, mut at: usize) -> usize {
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
    fn word_backward(&self, at: usize) -> usize {
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
    fn word_end(&self, at: usize) -> usize {
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

    /// Apply pending `operator` to `motion`, with combined `count`, updating the shared register.
    fn operator(&mut self, operator: &str, motion: &str, count: usize, register: &mut Register) {
        let start = self.doc.cursor;
        let row = self.doc.location(start).0;
        let linewise = motion == operator || matches!(motion, "j" | "k");
        let range = if linewise {
            let (first, last) = if motion == "k" {
                (row.saturating_sub(count), row)
            } else {
                (
                    row,
                    (row + count - usize::from(motion == operator)).min(self.doc.starts.len() - 1),
                )
            };
            self.doc.starts[first]
                ..self
                    .doc
                    .starts
                    .get(last + 1)
                    .copied()
                    .unwrap_or(self.doc.text.len())
        } else if motion == "$" {
            start
                ..self
                    .doc
                    .line_range((row + count - 1).min(self.doc.starts.len() - 1))
                    .end
        } else {
            let change_word = operator == "c" && motion == "w" && self.class(start) != 0;
            if !self.motion(if change_word { "e" } else { motion }, count) {
                return;
            }
            let end = self.doc.cursor;
            self.doc.cursor = start;
            let inclusive = motion == "e" || change_word;
            start.min(end)..if inclusive {
                self.doc.next(start.max(end))
            } else {
                start.max(end)
            }
        };
        register.text = self.doc.text[range.clone()].into();
        register.linewise = linewise;
        if linewise && !register.text.ends_with('\n') {
            register.text.push('\n');
        }
        if operator != "y" {
            let at = range.start;
            let changed = !range.is_empty();
            let replacement = if operator == "c" && linewise && range.end < self.doc.text.len() {
                "\n"
            } else {
                ""
            };
            self.doc.replace(range, replacement, true);
            self.doc.cursor = at.min(self.doc.text.len());
            self.clamp_normal();
            if operator == "c" {
                self.begin_insert(changed);
            }
        }
    }

    /// Paste `register` before/after the cursor as chosen by `before`, repeated `count` times.
    fn paste(&mut self, register: &Register, before: bool, count: usize) {
        if register.text.is_empty() {
            return;
        }
        let row = self.doc.location(self.doc.cursor).0;
        if register.text.len().saturating_mul(count) > 16 * 1024 * 1024 {
            self.message = "单次粘贴暂限 16 MiB".into();
            return;
        }
        let mut text = register.text.repeat(count);
        let at = if register.linewise {
            if before {
                self.doc.starts[row]
            } else if let Some(&next) = self.doc.starts.get(row + 1) {
                next
            } else {
                text = format!("\n{}", text.trim_end_matches('\n'));
                self.doc.text.len()
            }
        } else if before {
            self.doc.cursor
        } else {
            self.doc
                .next(self.doc.cursor)
                .min(self.doc.line_range(row).end)
        };
        self.doc.replace(at..at, &text, true);
        self.doc.cursor = if register.linewise {
            at
        } else {
            self.doc.previous(at + text.len())
        };
        if register.linewise && text.starts_with('\n') {
            self.doc.cursor += 1;
        }
        self.clamp_normal();
    }

    /// Edit/execute command-line `key`; return requested save/open/close actions to the shell.
    fn command_key(&mut self, key: &str) -> Action {
        match key {
            "Backspace" => {
                let byte = self
                    .command
                    .grapheme_indices(true)
                    .next_back()
                    .map_or(0, |(i, _)| i);
                self.command.truncate(byte);
            }
            "Enter" => {
                let search = self.mode == Mode::Search;
                self.mode = Mode::Normal;
                let text = std::mem::take(&mut self.command);
                if search {
                    if !text.is_empty() {
                        self.last_search = text;
                    }
                    self.search(false);
                    return Action::None;
                }
                let (name, args) = text.trim().split_once(' ').unwrap_or((text.trim(), ""));
                let args = args.trim();
                return match name {
                    "w" | "write" => Action::Save((!args.is_empty()).then(|| args.into())),
                    "q" | "quit" => Action::Close(false),
                    "q!" | "quit!" => Action::Close(true),
                    "wq" | "x" => Action::SaveClose,
                    "e" | "edit" if !args.is_empty() => Action::Open(args.into()),
                    _ => {
                        self.message = format!("暂不支持命令：{text}");
                        Action::None
                    }
                };
            }
            _ if !key.starts_with("Ctrl+")
                && !matches!(key, "Left" | "Right" | "Up" | "Down" | "Tab" | "Delete") =>
            {
                self.command.push_str(key)
            }
            _ => {}
        }
        Action::None
    }

    /// Repeat the last literal search, wrapping around the file; `backward` selects reverse order.
    fn search(&mut self, backward: bool) {
        if self.last_search.is_empty() {
            return;
        }
        let cursor = self.doc.cursor;
        let found = if backward {
            self.doc.text[..cursor]
                .rfind(&self.last_search)
                .or_else(|| {
                    self.doc.text[cursor..]
                        .rfind(&self.last_search)
                        .map(|i| cursor + i)
                })
        } else {
            let start = self.doc.next(cursor);
            self.doc.text[start..]
                .find(&self.last_search)
                .map(|i| start + i)
                .or_else(|| self.doc.text[..start].find(&self.last_search))
        };
        if let Some(at) = found {
            self.doc.cursor = at;
            self.message = format!("/{}", self.last_search);
        } else {
            self.message = format!("未找到：{}", self.last_search);
        }
    }
}
