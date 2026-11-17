//! Literal native text, insert undo units and Replace-mode restoration.

use super::{Action, Mode, Register, Vim, command_prompt::path_candidates, replay::Stroke};
use std::{
    collections::HashSet,
    fs::File,
    io::{BufRead, BufReader, Read},
    ops::Range,
};
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
    /// Repeatable strokes in the current Insert segment, also used by the dot register.
    events: Vec<Stroke>,
    /// Whether a counted repetition is already replaying its recorded strokes.
    repeating: bool,
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

/// Original insertion point and temporary caret for a single Normal-mode command.
pub(super) struct Resume {
    /// Insert or Replace mode to restore after the command.
    mode: Mode,
    /// Document offset before moving the caret onto a character.
    insertion: usize,
    /// Caret used by commands when the insertion was just after the line's last character.
    caret: usize,
    /// Document version at the start of the Normal-mode command.
    revision: u64,
}

impl Vim {
    /// Return the byte at which the current stretch of Insert typing began.
    pub(super) fn insert_text_start(&self) -> usize {
        self.insert.start
    }

    /// Find completions for the Insert `key` in the current buffer or project directory.
    pub(super) fn insert_candidates(&mut self, key: &str, register: &Register) -> Action {
        let cursor = self.doc.cursor;
        let row = self.doc.location(cursor).0;
        let line = self.doc.line_range(row);
        let before = &self.doc.text[line.start..cursor];
        let word = |ch: char| ch.is_alphanumeric() || ch == '_';
        let word_start = line.start + before.trim_end_matches(word).len();
        let (start, mut values): (usize, Vec<String>) = match key {
            "Ctrl+r" => {
                let prefix = &self.doc.text[word_start..cursor];
                let values = register
                    .completion_texts()
                    .flat_map(|text| text.split(|ch| !word(ch)))
                    .filter(|text| !text.is_empty() && *text != prefix && text.starts_with(prefix))
                    .take(500)
                    .map(str::to_owned)
                    .collect();
                (word_start, values)
            }
            "Ctrl+v" | "Ctrl+q" => {
                let head = before.trim_start();
                let head = head.strip_prefix(':').unwrap_or(head);
                let (start, values) = self.command_candidates(head, register).unwrap_or_default();
                (cursor - head.len() + start, values)
            }
            "Ctrl+d" | "Ctrl+i" => {
                let prefix = &self.doc.text[word_start..cursor];
                (
                    word_start,
                    self.include_completion(prefix, cursor, key == "Ctrl+d"),
                )
            }
            "Ctrl+k" => {
                let prefix = &self.doc.text[word_start..cursor];
                (word_start, super::spelling::dictionary_completion(prefix))
            }
            "Ctrl+t" => {
                let prefix = &self.doc.text[word_start..cursor];
                (word_start, self.thesaurus_completion(prefix))
            }
            "Ctrl+s" | "s" => return self.insert_spelling_candidates(),
            "Ctrl+f" => {
                let head = before.trim_end_matches(|ch: char| {
                    !ch.is_whitespace() && !matches!(ch, '"' | '\'' | '(' | ')' | '[' | ']')
                });
                let prefix = &before[head.len()..];
                (
                    line.start + head.len(),
                    path_candidates(self.completion_directory(), prefix),
                )
            }
            "Ctrl+l" => {
                let start = self.first_nonblank(row).min(cursor);
                let prefix = &self.doc.text[start..cursor];
                let values = self.doc.text[line.end..]
                    .lines()
                    .chain(self.doc.text[..line.start].lines())
                    .map(|line| line.trim_start_matches([' ', '\t']))
                    .filter(|text| !text.is_empty() && text.starts_with(prefix) && *text != prefix)
                    .take(100)
                    .map(str::to_owned)
                    .collect();
                (start, values)
            }
            _ => {
                let start = word_start;
                let prefix = &self.doc.text[start..cursor];
                let word_end = cursor
                    + self.doc.text[cursor..]
                        .find(|ch| !word(ch))
                        .unwrap_or(self.doc.text.len() - cursor);
                let source = |ch| !word(ch);
                let values = self.doc.text[word_end..]
                    .split(source)
                    .chain(self.doc.text[..start].split(source))
                    .filter(|text| !text.is_empty() && text.starts_with(prefix) && *text != prefix)
                    .take(500)
                    .map(str::to_owned)
                    .collect();
                (start, values)
            }
        };
        let mut seen = HashSet::new();
        values.retain(|text| seen.insert(text.clone()));
        values.truncate(100);
        Action::InsertCandidates {
            start,
            values,
            reverse: key == "Ctrl+p",
        }
    }

    /// Accept an Ex `args` assignment or query for local thesaurus paths.
    pub(super) fn set_thesaurus(&mut self, args: &str) -> bool {
        let paths = if let Some(paths) = args
            .strip_prefix("thesaurus=")
            .or_else(|| args.strip_prefix("tsr="))
        {
            paths
        } else if matches!(args, "thesaurus?" | "tsr?") {
            self.message = format!(
                "thesaurus={}",
                self.command_prompt
                    .thesaurus
                    .iter()
                    .map(|path| path.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(",")
            );
            return true;
        } else {
            return false;
        };
        self.command_prompt.thesaurus = paths
            .split(',')
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .map(|path| self.completion_directory().join(path))
            .collect();
        true
    }

    /// Complete `prefix` from matching thesaurus rows, followed by their synonyms.
    fn thesaurus_completion(&self, prefix: &str) -> Vec<String> {
        let mut values = Vec::new();
        let mut seen = HashSet::new();
        let sources = self
            .command_prompt
            .thesaurus
            .iter()
            .filter_map(|path| File::open(path).ok());
        for file in sources {
            // Stop scanning after 4 MiB per file, keeping synchronous completion bounded.
            for row in BufReader::new(file.take(4 * 1024 * 1024))
                .lines()
                .map_while(Result::ok)
            {
                let words: Vec<_> = row
                    .split(|ch: char| !ch.is_alphanumeric() && ch != '_')
                    .filter(|word| !word.is_empty())
                    .collect();
                let Some(first) = words.iter().position(|word| word.starts_with(prefix)) else {
                    continue;
                };
                for word in words[first..].iter().chain(&words[..first]) {
                    if *word != prefix && seen.insert((*word).to_owned()) {
                        values.push((*word).to_owned());
                        if values.len() == 100 {
                            return values;
                        }
                    }
                }
            }
        }
        values
    }

    /// Apply recorded Insert `strokes` through the current Insert or Replace mode.
    pub(super) fn replay_insert(&mut self, strokes: &[Stroke]) {
        for stroke in strokes {
            match stroke {
                Stroke::Key(key) => self.insert_key(key),
                Stroke::Text(text) => self.insert_text(text),
            }
        }
    }

    /// Retain `input` once for counted insertion and the last Insert register.
    fn record_insert(&mut self, input: Stroke) {
        if !self.insert.repeating {
            self.insert.events.push(input);
        }
    }

    /// Pause the current insertion so the next complete command runs in Normal mode.
    pub(super) fn suspend_insert(&mut self) -> Resume {
        let mode = self.mode;
        let insertion = self.doc.cursor;
        self.mode = Mode::Normal;
        self.clamp_normal();
        Resume {
            mode,
            insertion,
            caret: self.doc.cursor,
            revision: self.doc.revision,
        }
    }

    /// Resume a suspended `resume` after its command has completed.
    pub(super) fn resume_insert(&mut self, resume: Resume) {
        if self.mode.is_insert() {
            return;
        }
        if self.mode != Mode::Normal || self.awaiting_key() {
            self.insert_resume = Some(resume);
            return;
        }
        if self.doc.cursor == resume.caret && self.doc.revision == resume.revision {
            self.doc.cursor = resume.insertion;
        }
        self.mode = resume.mode;
        self.insert.overwritten.clear();
        self.insert.virtual_history.clear();
    }

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

    /// Replace `count` screen cells with `key` and retain the recorded text in `register`.
    pub(super) fn replace_virtual_characters(
        &mut self,
        key: &str,
        count: usize,
        register: &mut Register,
    ) {
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
        register.last_insert = self.finish_insert();
    }

    /// Open a line `above` or below the caret, retaining its current indentation.
    pub(super) fn open_line(&mut self, above: bool, count: usize) {
        let row = self.doc.location(self.doc.cursor).0;
        let indent = if self.autoindent() {
            self.doc.text[self.doc.starts[row]..self.first_nonblank(row)].to_owned()
        } else {
            String::new()
        };
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

    /// Finish the insertion, replicate its count or block, and return the segment strokes.
    pub(super) fn finish_insert(&mut self) -> Vec<Stroke> {
        let count = self.insert.count;
        self.insert.count = 1;
        let events = std::mem::take(&mut self.insert.events);
        let open_line = self.insert.open_line;
        if open_line || !events.is_empty() {
            self.insert.repeating = true;
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
                self.replay_insert(&events);
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
        events
    }

    /// Commit literal UTF-8 `text` to Insert or Replace mode.
    pub(super) fn insert_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.record_insert(Stroke::Text(text.to_owned()));
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
                self.record_insert(Stroke::Key(key.to_owned()));
                self.mode = if self.mode == Mode::Replace {
                    Mode::Insert
                } else {
                    Mode::Replace
                };
                return;
            }
            "Backspace" | "Ctrl+h" => {
                if self.mode == Mode::Replace {
                    self.record_insert(Stroke::Key(key.to_owned()));
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
            "Enter" | "Ctrl+j" | "Ctrl+m"
                if self.insert.virtual_replace && self.mode == Mode::Replace =>
            {
                self.insert_text("\n");
                return;
            }
            "Enter" | "Ctrl+j" | "Ctrl+m" => {
                let indent = if self.autoindent() {
                    self.doc.text[self.doc.starts[row]..self.first_nonblank(row).min(cursor)]
                        .to_owned()
                } else {
                    String::new()
                };
                self.trim_autoindent();
                self.insert_text(&format!("\n{indent}"));
                self.remember_autoindent(indent.len());
                return;
            }
            "Tab" | "Ctrl+i" => {
                if self.expandtab() {
                    let tabstop = self.doc.tabstop();
                    let column = self.doc.location(cursor).1;
                    self.insert_text(&" ".repeat(tabstop - column % tabstop));
                } else {
                    self.insert_text("\t");
                }
                return;
            }
            "Ctrl+d" | "Ctrl+t" => {
                self.shift_insert_indent(row, key == "Ctrl+t");
                return;
            }
            "Ctrl+e" | "Ctrl+y" => {
                let direction = if key == "Ctrl+e" { 1 } else { -1 };
                if let Some(target) = row.checked_add_signed(direction)
                    && target < self.line_count()
                {
                    let at = self.doc.offset(target, self.doc.location(cursor).1);
                    let end = self.doc.next(at).min(self.doc.line_range(target).end);
                    if at < end {
                        let text = self.doc.text[at..end].to_owned();
                        self.insert_text(&text);
                    }
                }
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
                    self.insert.events.clear();
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
        self.record_insert(Stroke::Key(key.to_owned()));
        if self.mode == Mode::Replace && matches!(key, "Ctrl+w" | "Ctrl+u") {
            let count = self.doc.text[range].graphemes(true).count();
            for _ in 0..count {
                self.replace_backspace();
            }
        } else {
            self.replace_insert(range, "");
        }
    }

    /// Shift indentation on `row` to the next or previous stop, retaining its text-relative caret.
    fn shift_insert_indent(&mut self, row: usize, right: bool) {
        let start = self.doc.starts[row];
        let old_end = self.first_nonblank(row);
        let cursor = self.doc.cursor;
        let width = self.doc.display_width(&self.doc.text[start..old_end], 0);
        let shiftwidth = self.shiftwidth();
        let truncate = !right
            && cursor == old_end + 1
            && matches!(self.doc.text.as_bytes().get(old_end), Some(b'0' | b'^'));
        let columns = if right {
            (width / shiftwidth + 1) * shiftwidth
        } else if truncate {
            0
        } else {
            width.saturating_sub(1) / shiftwidth * shiftwidth
        };
        let new_indent = self.indentation(columns);
        let end = if truncate { cursor } else { old_end };
        let key = if right { "Ctrl+t" } else { "Ctrl+d" };
        self.record_insert(Stroke::Key(key.into()));
        self.replace_insert(start..end, &new_indent);
        self.doc.cursor = start + new_indent.len() + cursor.saturating_sub(end);
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
                    self.doc.display_width(text, column)
                };
            let mut end = at;
            let mut width = column;
            let mut padding = String::new();
            while end < line_end && width < target {
                let next = self.doc.next(end).min(line_end);
                let old = &self.doc.text[end..next];
                width += self.doc.display_width(old, width);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;

    /// Complete macro and identifier prefixes through local headers and preserve the typed prefix.
    #[test]
    fn insert_completion_resolves_local_includes_and_ex_commands() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("main.c");
        std::fs::write(&source, "#include \"local.h\"\nlo").unwrap();
        std::fs::write(
            temp.path().join("local.h"),
            "#define local_size 3\nint local_item;\n",
        )
        .unwrap();
        let mut vim = Vim::new(Document::open(&source).unwrap());
        let mut register = Register::default();
        vim.text("GA", &mut register);
        for (key, values) in [
            ("Ctrl+d", vec!["local_size"]),
            ("Ctrl+i", vec!["local_size", "local_item"]),
        ] {
            vim.key("Ctrl+x", &mut register);
            assert_eq!(
                vim.key(key, &mut register),
                Action::InsertCandidates {
                    start: vim.doc.starts[1],
                    values: values.into_iter().map(String::from).collect(),
                    reverse: false,
                }
            );
        }
        let mut vim = Vim::new(Document::new(":tabn"));
        vim.key("A", &mut register);
        vim.key("Ctrl+x", &mut register);
        assert_eq!(
            vim.key("Ctrl+v", &mut register),
            Action::InsertCandidates {
                start: 1,
                values: vec!["tabnew".into(), "tabnext".into()],
                reverse: false
            }
        );
    }

    /// Replay literal insertion and Backspace through Ctrl+a and the last Insert register.
    #[test]
    fn last_insert_preserves_backspace_for_replay() {
        let mut vim = Vim::new(Document::new(""));
        let mut register = Register::default();
        vim.text("iabc", &mut register);
        vim.key("Backspace", &mut register);
        vim.text("X", &mut register);
        vim.key("Escape", &mut register);
        assert_eq!(vim.doc.text, "abX");
        vim.key("a", &mut register);
        vim.key("Ctrl+a", &mut register);
        assert_eq!(vim.doc.text, "abXabX");
        vim.key("Ctrl+@", &mut register);
        assert_eq!(vim.doc.text, "abXabXabX");
        assert_eq!(vim.mode, Mode::Normal);
    }
}
