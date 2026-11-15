//! Literal native text, insert undo units and Replace-mode restoration.

use super::{Mode, Vim, replay::Stroke};
use crate::document::display_width;
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

/// One rectangular insertion being mirrored to the remaining selected rows.
struct Block {
    /// Row that receives typed text and last row that will receive its copy.
    first: usize,
    last: usize,
    /// Display column of the block and byte at which typed text started.
    column: usize,
    start: usize,
}

/// Data retained until leaving the current Insert or Replace mode.
#[derive(Default)]
pub(super) struct Session {
    /// An undo checkpoint already exists for this insertion.
    checkpointed: bool,
    /// Byte where the current stretch of typing began.
    start: usize,
    /// Count supplied to the command that started this session.
    count: usize,
    /// Whether each counted repetition starts in Replace mode.
    started_in_replace: bool,
    /// Repeats of o/O create additional lines.
    open_line: bool,
    /// Input for a counted insertion.
    events: Vec<Stroke>,
    /// Original graphemes saved in Replace mode, restored in reverse order.
    overwritten: Vec<String>,
    /// Display-width replacement, set by gR rather than character Replace.
    virtual_replace: bool,
    /// Old text and its replacement range for each virtual replacement.
    virtual_history: Vec<(Range<usize>, String)>,
    /// A Visual block's remaining insertion targets.
    block: Option<Block>,
    /// Indentation added by opening or splitting a line.
    autoindent: Option<Range<usize>>,
    /// Keep the current undo unit through the next horizontal cursor movement.
    keep_undo_on_move: bool,
}

impl Vim {
    /// Apply a validated completion edit in the current undo unit when inserting text.
    pub fn apply_completion(&mut self, range: Range<usize>, text: &str) {
        if self.mode == Mode::Insert {
            self.replace_insert(range, text);
        } else {
            self.replace(range, text, true);
        }
    }

    /// Enter `mode` with an optional existing undo checkpoint and repeat `count`.
    pub(super) fn begin_insert(
        &mut self,
        mode: Mode,
        checkpointed: bool,
        count: usize,
        open_line: bool,
    ) {
        self.mode = mode;
        self.insert = Session {
            checkpointed,
            start: self.doc.cursor,
            count,
            started_in_replace: mode == Mode::Replace,
            open_line,
            ..Session::default()
        };
        self.desired_column = None;
    }

    /// Start Virtual Replace with the same undo unit and repeat `count` as Replace.
    pub(super) fn begin_virtual_replace(&mut self, count: usize) {
        self.begin_insert(Mode::Replace, false, count, false);
        self.insert.virtual_replace = true;
    }

    /// Replace `count` screen cells with `key` through one Virtual Replace undo unit.
    pub(super) fn replace_virtual_characters(&mut self, key: &str, count: usize) {
        let text = match key {
            "Enter" => "\r",
            "Tab" => "\t",
            key if key.graphemes(true).count() == 1 => key,
            _ => return,
        };
        if self.mode.is_visual() {
            self.replace_chars(text, count);
            return;
        }
        self.begin_virtual_replace(count);
        self.insert_text(text);
        self.finish_insert();
    }

    /// Open a line `above` or below the caret, retaining its current indentation.
    pub(super) fn open_line(&mut self, above: bool, count: usize) {
        let row = self.doc.location(self.doc.cursor).0;
        let indent = self.doc.text[self.doc.starts[row]..self.first_nonblank(row)].to_owned();
        let at = if above {
            self.doc.starts[row]
        } else {
            self.doc.line_range(row).end
        };
        let text = if above {
            format!("{indent}\n")
        } else {
            format!("\n{indent}")
        };
        self.replace(at..at, &text, true);
        if above {
            self.doc.cursor = at + indent.len();
        }
        self.begin_insert(Mode::Insert, true, count, true);
        self.remember_autoindent(indent.len());
    }

    /// Remember `bytes` of indentation immediately before the caret.
    pub(super) fn remember_autoindent(&mut self, bytes: usize) {
        self.insert.autoindent = (bytes > 0).then(|| self.doc.cursor - bytes..self.doc.cursor);
    }

    /// Apply `key` after Insert-mode Ctrl+g.
    pub(super) fn insert_control_g(&mut self, key: &str) {
        match key {
            "u" => {
                self.insert.checkpointed = false;
                self.insert.start = self.doc.cursor;
            }
            "U" => self.insert.keep_undo_on_move = true,
            "j" | "k" | "Ctrl+j" | "Ctrl+k" | "Down" | "Up" => {
                let (row, _) = self.doc.location(self.doc.cursor);
                let column = self
                    .doc
                    .location(self.insert.start.min(self.doc.text.len()))
                    .1;
                let next = if matches!(key, "j" | "Ctrl+j" | "Down") {
                    row.saturating_add(1).min(self.line_count() - 1)
                } else {
                    row.saturating_sub(1)
                };
                if next != row {
                    self.doc.cursor = self.doc.offset(next, column);
                    self.insert.checkpointed = false;
                }
            }
            _ => {}
        }
    }

    /// Insert register `text` literally, above the current line when `linewise`.
    /// `indent` aligns its leading line indentation with the current line.
    pub(super) fn insert_literal_register(&mut self, text: &str, linewise: bool, indent: bool) {
        let row = self.doc.location(self.doc.cursor).0;
        let cursor = self.doc.cursor;
        let inserted = if indent && linewise {
            let indentation = &self.doc.text[self.doc.starts[row]..self.first_nonblank(row)];
            let first = text
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("");
            let base = first.len() - first.trim_start_matches([' ', '\t']).len();
            let mut result = String::new();
            for line in text.split_inclusive('\n') {
                if !line.trim().is_empty() {
                    let existing = line.len() - line.trim_start_matches([' ', '\t']).len();
                    result.push_str(indentation);
                    result.push_str(&line[existing.min(base)..]);
                } else {
                    result.push_str(line.trim_start_matches([' ', '\t']));
                }
            }
            result
        } else {
            text.to_owned()
        };
        if linewise {
            let at = self.doc.starts[row];
            self.replace_insert(at..at, &inserted);
            self.doc.cursor = cursor + inserted.len();
        } else {
            self.replace_insert(cursor..cursor, &inserted);
        }
    }

    /// Remove untouched autoindent when its new line was left empty.
    fn trim_autoindent(&mut self) {
        let Some(range) = self.insert.autoindent.take() else {
            return;
        };
        if range.end <= self.doc.text.len()
            && self.doc.text.is_char_boundary(range.start)
            && self.doc.text.is_char_boundary(range.end)
            && self.doc.line_range(self.doc.location(range.start).0) == range
            && self.doc.text[range.clone()]
                .bytes()
                .all(|b| matches!(b, b' ' | b'\t'))
        {
            let cursor = self.doc.cursor;
            let length = range.len();
            let end = range.end;
            self.replace_insert(range, "");
            if cursor >= end {
                self.doc.cursor = cursor - length;
            }
        }
    }

    /// Set up Visual block insertion from `first` through `last` at display `column`.
    pub(super) fn set_block_insert(&mut self, first: usize, last: usize, column: usize) {
        let at = self.doc.offset(first, column);
        let padding = " ".repeat(column.saturating_sub(self.doc.location(at).1));
        self.replace_insert(at..at, &padding);
        self.doc.cursor = at + padding.len();
        self.insert.block = Some(Block {
            first,
            last,
            column,
            start: self.doc.cursor,
        });
    }

    /// Finish the insertion and replicate any count or rectangular insert.
    pub(super) fn finish_insert(&mut self) {
        let count = self.insert.count;
        self.insert.count = 1;
        let events = std::mem::take(&mut self.insert.events);
        let open_line = self.insert.open_line;
        if open_line || !events.is_empty() {
            for _ in 1..count {
                // A recorded Insert key can switch modes again within each repetition.
                self.mode = if self.insert.started_in_replace {
                    Mode::Replace
                } else {
                    Mode::Insert
                };
                if open_line {
                    self.insert_key("Enter");
                }
                for stroke in &events {
                    match stroke {
                        Stroke::Key(key) => self.insert_key(key),
                        Stroke::Text(text) => self.insert_text(text),
                    }
                }
            }
        }
        if let Some(block) = self.insert.block.take()
            && self.doc.cursor >= block.start
            && self.doc.location(self.doc.cursor).0 == block.first
        {
            let text = self.doc.text[block.start..self.doc.cursor].to_owned();
            if !text.is_empty() {
                let caret = self.doc.cursor;
                for row in (block.first + 1..=block.last).rev() {
                    let at = self.doc.offset(row, block.column);
                    let mut insertion =
                        " ".repeat(block.column.saturating_sub(self.doc.location(at).1));
                    insertion.push_str(&text);
                    self.replace_insert(at..at, &insertion);
                }
                self.doc.cursor = caret;
            }
        }
        self.trim_autoindent();
        self.g.insertion = Some(self.doc.cursor);
        let row = self.doc.location(self.doc.cursor).0;
        self.doc.cursor = self.doc.previous(self.doc.cursor).max(self.doc.starts[row]);
        self.insert = Session::default();
        self.mode = Mode::Normal;
        self.clamp_normal();
    }

    /// Commit literal UTF-8 `text` to Insert or Replace mode.
    pub(super) fn insert_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if self.insert.count > 1 {
            self.insert.events.push(Stroke::Text(text.to_owned()));
        }
        if self.mode == Mode::Replace {
            for grapheme in text.graphemes(true) {
                if self.insert.virtual_replace {
                    self.virtual_replace_one(grapheme);
                    continue;
                }
                let at = self.doc.cursor;
                let line = self.doc.line_range(self.doc.location(at).0);
                let end = if grapheme == "\n" {
                    at
                } else {
                    self.doc.next(at).min(line.end)
                };
                self.insert
                    .overwritten
                    .push(self.doc.text[at..end].to_owned());
                self.replace_insert(at..end, grapheme);
                // Replacing a grapheme with the same text still advances the caret.
                self.doc.cursor = at + grapheme.len();
            }
        } else {
            let at = self.doc.cursor;
            self.replace_insert(at..at, text);
        }
    }

    /// Apply an insert-mode semantic `key`, keeping literal IME text on its own path.
    pub(super) fn insert_key(&mut self, key: &str) {
        let cursor = self.doc.cursor;
        let row = self.doc.location(cursor).0;
        let range = match key {
            "Insert" => {
                if self.insert.count > 1 {
                    self.insert.events.push(Stroke::Key(key.to_owned()));
                }
                self.mode = if self.mode == Mode::Replace {
                    Mode::Insert
                } else {
                    Mode::Replace
                };
                return;
            }
            "Backspace" | "Ctrl+h" => {
                if self.mode == Mode::Replace {
                    self.replace_backspace();
                    return;
                }
                self.doc.previous(cursor)..cursor
            }
            "Delete" => cursor..self.doc.next(cursor),
            "Ctrl+w" | "Ctrl+u" => {
                let line = self.doc.starts[row];
                let start = if cursor == line {
                    self.doc.previous(cursor)
                } else if key == "Ctrl+w" {
                    self.word_backward(cursor, false).max(line)
                } else if self.insert.start < cursor {
                    self.insert.start.max(line)
                } else {
                    line
                };
                // Neovim's default mappings start an undo unit before either deletion.
                self.insert.checkpointed = false;
                start..cursor
            }
            "Enter" | "Ctrl+j" if self.insert.virtual_replace && self.mode == Mode::Replace => {
                self.insert_text("\n");
                return;
            }
            "Enter" | "Ctrl+j" => {
                let indent = self.doc.text
                    [self.doc.starts[row]..self.first_nonblank(row).min(cursor)]
                    .to_owned();
                self.trim_autoindent();
                self.insert_text(&format!("\n{indent}"));
                self.remember_autoindent(indent.len());
                return;
            }
            "Tab" | "Ctrl+i" => {
                self.insert_text("\t");
                return;
            }
            "Left" | "Right" | "Up" | "Down" | "Home" | "End" => {
                let keep = std::mem::take(&mut self.insert.keep_undo_on_move)
                    && matches!(key, "Left" | "Right");
                self.trim_autoindent();
                if let Some(motion) = self.cursor_motion(key, None) {
                    self.apply_motion(motion);
                }
                if !keep || self.doc.location(self.doc.cursor).0 != row {
                    self.insert.checkpointed = false;
                    self.insert.start = self.doc.cursor;
                }
                self.insert.overwritten.clear();
                self.insert.virtual_history.clear();
                self.insert.count = 1;
                return;
            }
            _ if key.graphemes(true).count() == 1 => {
                self.insert_text(key);
                return;
            }
            _ => return,
        };
        if self.insert.count > 1 {
            self.insert.events.push(Stroke::Key(key.to_owned()));
        }
        if self.mode == Mode::Replace && matches!(key, "Ctrl+w" | "Ctrl+u") {
            let count = self.doc.text[range].graphemes(true).count();
            for _ in 0..count {
                self.replace_backspace();
            }
        } else {
            self.replace_insert(range, "");
        }
    }

    /// Back up by one grapheme and restore any original text it replaced.
    fn replace_backspace(&mut self) {
        if self.insert.virtual_replace
            && let Some((range, original)) = self.insert.virtual_history.pop()
        {
            let at = range.start;
            self.replace_insert(range, &original);
            self.doc.cursor = at;
            return;
        }
        let cursor = self.doc.cursor;
        let at = self.doc.previous(cursor);
        if at < cursor
            && let Some(original) = self.insert.overwritten.pop()
        {
            self.replace_insert(at..cursor, &original);
        }
        self.doc.cursor = at;
        self.insert.start = self.insert.start.min(at);
    }

    /// Replace enough graphemes to fit `text`, padding any partially overwritten cell.
    fn virtual_replace_one(&mut self, text: &str) {
        let at = self.doc.cursor;
        let (row, column) = self.doc.location(at);
        let line_end = self.doc.line_range(row).end;
        let (end, padding) = if text == "\n" {
            ((line_end + 1).min(self.doc.text.len()), String::new())
        } else {
            // Normal-mode gr<Enter> inserts a displayed ^M and replaces its two cells.
            let control = text == "\r";
            let target = column
                + if control {
                    2
                } else {
                    display_width(text, column)
                };
            let mut end = at;
            let mut width = column;
            let mut padding = String::new();
            while end < line_end && width < target {
                let next = self.doc.next(end).min(line_end);
                let old = &self.doc.text[end..next];
                width += display_width(old, width);
                end = next;
                if width > target && !control {
                    if old == "\t" {
                        padding.push('\t');
                    } else {
                        padding.push_str(&" ".repeat(width - target));
                    }
                }
            }
            (end, padding)
        };
        let old = self.doc.text[at..end].to_owned();
        let replacement = format!("{text}{padding}");
        self.replace_insert(at..end, &replacement);
        self.doc.cursor = at + text.len();
        self.insert
            .virtual_history
            .push((at..at + replacement.len(), old));
    }

    /// Replace the selected insert `range` with `text` within this undo unit.
    fn replace_insert(&mut self, range: Range<usize>, text: &str) {
        let before = self.doc.revision;
        if self
            .insert
            .autoindent
            .as_ref()
            .is_some_and(|indent| range.start < indent.end && range.end > indent.start)
        {
            self.insert.autoindent = None;
        }
        self.replace(range, text, !self.insert.checkpointed);
        self.insert.start = self.insert.start.min(self.doc.cursor);
        self.insert.checkpointed |= self.doc.revision != before;
    }
}
