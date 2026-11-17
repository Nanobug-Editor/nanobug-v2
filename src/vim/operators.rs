//! Apply motions and text objects to registers and the document.

use super::{
    Action, Mode, Operator, Register, Vim,
    motions::{Kind as MotionKind, Motion},
    register::{Contents, Kind},
};
use crate::i18n;
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

/// Limit repeated puts to the editor's file size limit.
const PUT_LIMIT: usize = 16 * 1024 * 1024;

impl Vim {
    /// Apply `operator` to its resolved `motion`, starting at the current caret.
    pub(super) fn operate_motion(
        &mut self,
        operator: Operator,
        motion: Motion,
        register: &mut Register,
    ) {
        let at = self.doc.cursor;
        if operator.force == Some(Mode::VisualBlock) {
            self.operate_block_motion(operator.key, motion.end, register);
            return;
        }
        let kind = match operator.force {
            Some(Mode::VisualLine) => MotionKind::Line,
            Some(Mode::Visual) if motion.kind == MotionKind::Exclusive => MotionKind::Inclusive,
            Some(Mode::Visual) => MotionKind::Exclusive,
            _ => motion.kind,
        };
        let (start, end) = (at.min(motion.end), at.max(motion.end));
        let first = self.doc.location(start).0;
        let last = self.doc.location(end).0;
        let mut linewise = kind == MotionKind::Line;
        let range = if linewise {
            self.line_span(first, last)
        } else if kind == MotionKind::Inclusive {
            let end = self.doc.next(end).min(self.doc.line_range(last).end);
            start..end
        } else if operator.force.is_none()
            && first < last
            && self.doc.starts[last] == end
            && at <= motion.end
        {
            if start <= self.first_nonblank(first) {
                linewise = true;
                self.line_span(first, last - 1)
            } else {
                start..self.doc.line_range(last - 1).end
            }
        } else {
            start..end
        };
        self.operate_range(operator.key, range, linewise, register);
    }

    /// Apply an explicit block operator from the current caret to motion endpoint `end`.
    fn operate_block_motion(&mut self, key: char, end: usize, register: &mut Register) {
        self.anchor = self.doc.cursor;
        self.set_cursor(end);
        self.mode = Mode::VisualBlock;
        self.visual_operator(key, register);
    }

    /// Apply a resolved text object to `operator`, respecting a forced range shape.
    pub(super) fn operate_object(
        &mut self,
        operator: Operator,
        range: Range<usize>,
        kind: MotionKind,
        register: &mut Register,
    ) {
        let end = self.doc.previous(range.end).max(range.start);
        match operator.force {
            Some(Mode::VisualBlock) => {
                self.set_cursor(range.start);
                self.operate_block_motion(operator.key, end, register);
            }
            Some(Mode::VisualLine) => {
                let first = self.doc.location(range.start).0;
                let last = self.doc.location(end).0;
                self.operate_range(operator.key, self.line_span(first, last), true, register);
            }
            _ => self.operate_range(
                operator.key,
                range,
                operator.force.is_none() && kind == MotionKind::Line,
                register,
            ),
        }
    }

    /// Resolve counted word motions with the special cw and end-of-line operator rules.
    pub(super) fn operator_motion(
        &mut self,
        operator: Operator,
        key: &str,
        count: Option<usize>,
    ) -> Option<Motion> {
        let combined = operator.count.or(count).map(|_| {
            operator
                .count
                .unwrap_or(1)
                .saturating_mul(count.unwrap_or(1))
                .min(10_000)
        });
        if matches!(key, "w" | "W") {
            let mut at = self.doc.cursor;
            let big = key == "W";
            let n = combined.unwrap_or(1);
            if operator.key == 'c' && self.class(at, big) != 0 {
                let class = self.class(at, big);
                while self.doc.next(at) < self.doc.text.len()
                    && self.class(self.doc.next(at), big) == class
                {
                    at = self.doc.next(at);
                }
                for _ in 1..n {
                    at = self.word_end(at, big);
                }
                return Some(Motion::new(at, MotionKind::Inclusive));
            }
            for i in 0..n {
                let next = self.word_forward(at, big);
                let row = self.doc.location(at).0;
                if i + 1 == n && next > self.doc.line_range(row).end {
                    at = self.doc.line_range(row).end;
                } else {
                    at = next;
                }
            }
            return Some(Motion::new(at, MotionKind::Exclusive));
        }
        self.motion(key, combined)
    }

    /// Apply a repeated linewise `operator` to `count` lines.
    pub(super) fn line_operator(&mut self, operator: char, count: usize, register: &mut Register) {
        self.repeat_operator(Operator::new(operator, None), count, register);
    }

    /// Complete a repeated `operator` on `count` lines, with its selected shape.
    pub(super) fn repeat_operator(
        &mut self,
        operator: Operator,
        count: usize,
        register: &mut Register,
    ) {
        let row = self.doc.location(self.doc.cursor).0;
        let last = row.saturating_add(count - 1).min(self.line_count() - 1);
        if operator.force.is_some() {
            let end = self.row_target(last);
            self.operate_motion(operator, Motion::new(end, MotionKind::Line), register);
        } else {
            self.operate_range(operator.key, self.line_span(row, last), true, register);
            if operator.key == super::comments::COMMENT_OPERATOR && count == 1 {
                self.set_cursor(self.first_nonblank(row));
            }
        }
    }

    /// Apply `key` to a byte `range` and update the relevant registers.
    pub(super) fn operate_range(
        &mut self,
        key: char,
        range: Range<usize>,
        linewise: bool,
        register: &mut Register,
    ) {
        self.set_operated_range(range.clone());
        if key == super::script::USER_OPERATOR {
            let kind = if linewise { "line" } else { "char" };
            self.invoke_operator(kind, vec![range], register);
            return;
        }
        if key == super::folds::FOLD_OPERATOR {
            self.fold_operator(range);
            return;
        }
        if self.g_operator_range(key, range.clone()) {
            return;
        }
        let key = if key == super::input::TRIM_YANK_OPERATOR {
            'y'
        } else {
            key
        };
        if matches!(key, '<' | '>' | '=') {
            self.indent_range(range, key);
            self.mode = Mode::Normal;
            self.clamp_normal();
            return;
        }
        if key == '!' {
            let first = self.doc.location(range.start).0;
            let last = self
                .doc
                .location(if range.is_empty() {
                    range.end
                } else {
                    self.doc.previous(range.end)
                })
                .0;
            self.filter_range = Some(self.line_span(first, last));
            self.open_prompt('!', 1);
            return;
        }
        let text = &self.doc.text[range.clone()];
        if !range.is_empty() || linewise {
            let value = self.copy_range(range.clone(), linewise);
            register.write(self.selected_register.take(), value, key == 'y');
        }
        let was_visual = self.mode.is_visual();
        self.mode = Mode::Normal;
        let (original_row, original_column) = self.doc.location(self.doc.cursor);
        if key == 'y' {
            let first = self.doc.location(range.start).0;
            if !was_visual && linewise && first < original_row {
                self.set_cursor(self.doc.offset(first, original_column));
            } else if was_visual || !linewise && range.start < self.doc.cursor {
                self.set_cursor(range.start);
            }
            return;
        }

        let start = range.start;
        let mut edit_range = range.clone();
        let mut replacement = String::new();
        if linewise && key == 'c' {
            let row = self.doc.location(start).0;
            replacement.push_str(&self.doc.text[start..self.first_nonblank(row)]);
            if range.end < self.doc.text.len() || self.final_newline && text.ends_with('\n') {
                replacement.push('\n');
            }
        } else if linewise
            && range.end == self.doc.text.len()
            && !self.doc.text.ends_with('\n')
            && start > 0
        {
            edit_range.start -= 1;
        }
        let revision = self.doc.revision;
        let caret = edit_range.start;
        self.replace(edit_range, &replacement, true);
        self.doc.cursor = caret.min(self.doc.text.len());
        self.desired_column = None;
        if key == 'c' {
            let indent = replacement.trim_end_matches('\n').len();
            self.doc.cursor += indent;
            self.begin_insert(Mode::Insert, self.doc.revision != revision, 1, false);
            self.remember_autoindent(indent);
        } else if linewise {
            let row = self
                .doc
                .location(self.doc.cursor)
                .0
                .min(self.line_count() - 1);
            self.set_cursor(self.doc.offset(row, original_column));
        } else {
            self.clamp_normal();
        }
    }

    /// Copy a byte `range` with its linewise register shape when requested.
    pub(super) fn copy_range(&self, range: Range<usize>, linewise: bool) -> Contents {
        let mut text = self.doc.text[range].to_owned();
        let kind = if linewise {
            if !text.ends_with('\n') {
                text.push('\n');
            }
            Kind::Line
        } else {
            Kind::Character
        };
        Contents { text, kind }
    }

    /// Yank, delete, change or shift the current Visual selection.
    pub(super) fn visual_operator(&mut self, key: char, register: &mut Register) {
        if matches!(key, 'c' | 'd') {
            self.skip_select_restore();
        }
        if key == super::script::USER_OPERATOR && self.mode == Mode::VisualBlock {
            let (rows, columns) = self.block_bounds();
            let ranges = rows.map(|row| self.block_range(row, &columns)).collect();
            self.invoke_operator("block", ranges, register);
            return;
        }
        if key == super::script::USER_OPERATOR {
            if let Some(range) = self.selection() {
                self.operate_range(key, range, self.mode == Mode::VisualLine, register);
            }
            return;
        }
        if self.g_visual_operator(key) {
            return;
        }
        let trim = key == super::input::TRIM_YANK_OPERATOR;
        let key = if trim { 'y' } else { key };
        if self.mode == Mode::VisualBlock && matches!(key, 'c' | 'd' | 'y') {
            self.block_operator(key, register, trim);
        } else if let Some(range) = self.selection() {
            self.operate_range(key, range, self.mode == Mode::VisualLine, register);
        } else if self.mode == Mode::VisualBlock {
            let (rows, _) = self.block_bounds();
            self.operate_range(
                key,
                self.line_span(*rows.start(), *rows.end()),
                true,
                register,
            );
        }
    }

    /// Apply `key` to a rectangular block and `trim` trailing blanks for zy.
    fn block_operator(&mut self, key: char, register: &mut Register, trim: bool) {
        let (rows, columns) = self.block_bounds();
        let first = *rows.start();
        let last = *rows.end();
        let ranges: Vec<_> = rows.map(|row| self.block_range(row, &columns)).collect();
        let text = ranges
            .iter()
            .map(|range| {
                let text = &self.doc.text[range.clone()];
                if trim {
                    text.trim_end_matches([' ', '\t'])
                } else {
                    text
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        register.write(
            self.selected_register.take(),
            Contents {
                text,
                kind: Kind::Block(columns.len()),
            },
            key == 'y',
        );
        let start = ranges[0].start;
        self.mode = Mode::Normal;
        let revision = self.doc.revision;
        if key != 'y' {
            for range in ranges.into_iter().rev() {
                self.replace(range, "", self.doc.revision == revision);
            }
        }
        self.doc.cursor = start;
        self.desired_column = None;
        if key == 'c' {
            self.begin_insert(Mode::Insert, self.doc.revision != revision, 1, false);
            self.set_block_insert(first, last, columns.start);
        } else {
            self.clamp_normal();
        }
    }

    /// Put a `register` `count` times around the caret. `before` chooses the side and
    /// `trim` omits trailing block padding for zp and zP.
    pub(super) fn put(&mut self, register: &mut Register, before: bool, count: usize, trim: bool) {
        self.put_contents(register, before, count, trim, false, false);
    }

    /// Put `register` `count` times with the cursor beyond its final inserted character.
    /// `before` selects insertion before the original caret for gP.
    pub(super) fn put_after_text(&mut self, register: &mut Register, before: bool, count: usize) {
        self.put_contents(register, before, count, false, true, false);
    }

    /// Put `register` `count` times with linewise indentation adjusted to the destination.
    /// `before` selects insertion above the original line.
    pub(super) fn put_indented(&mut self, register: &mut Register, before: bool, count: usize) {
        self.put_contents(register, before, count, false, false, true);
    }

    /// Put the selected `register` `count` times with the requested placement and padding.
    fn put_contents(
        &mut self,
        register: &mut Register,
        before: bool,
        count: usize,
        trim: bool,
        after: bool,
        indent: bool,
    ) {
        let name = self.selected_register.take();
        if self.mode.is_visual() {
            self.skip_select_restore();
        }
        let mut value = self.text_register(name, register);
        if value.text.len().saturating_mul(count) > PUT_LIMIT {
            self.message = i18n::text("editor.paste_too_large").into();
            return;
        }
        if value.text.is_empty() && value.kind == Kind::Character {
            return;
        }
        if indent && value.kind == Kind::Line {
            let row = self.doc.location(self.doc.cursor).0;
            let destination = self.doc.location(self.first_nonblank(row)).1;
            let base = value
                .text
                .lines()
                .find(|line| !line.trim().is_empty())
                .map_or(0, |line| {
                    self.doc.display_width(
                        &line[..line.len() - line.trim_start_matches([' ', '\t']).len()],
                        0,
                    )
                });
            let mut text = String::new();
            for line in value.text.split_inclusive('\n') {
                let content = line.trim_start_matches([' ', '\t']);
                if !content.trim().is_empty() {
                    let width = self
                        .doc
                        .display_width(&line[..line.len() - content.len()], 0);
                    let indentation =
                        width.saturating_add_signed(destination as isize - base as isize);
                    if text
                        .len()
                        .saturating_add(indentation)
                        .saturating_add(content.len())
                        .saturating_mul(count)
                        > PUT_LIMIT
                    {
                        self.message = i18n::text("editor.paste_too_large").into();
                        return;
                    }
                    text.extend(std::iter::repeat_n(' ', indentation));
                }
                text.push_str(content);
            }
            value.text = text;
        }
        if let Some(range) = self.selection()
            && matches!(value.kind, Kind::Character | Kind::Line)
        {
            self.put_over_selection(range, value, before, count, after, register);
            return;
        }
        let visual = self.mode.is_visual();
        let revision = self.doc.revision;
        if visual {
            if before {
                self.selected_register = Some('_');
            }
            self.visual_operator('d', register);
        }
        let checkpoint = self.doc.revision == revision;
        let row = self.doc.location(self.doc.cursor).0;
        let line = self.doc.line_range(row);
        let at = match value.kind {
            Kind::Line => {
                let mut text = value.text.repeat(count);
                let at = if before || visual {
                    line.start
                } else {
                    self.doc
                        .starts
                        .get(row + 1)
                        .copied()
                        .unwrap_or(self.doc.text.len())
                };
                let cursor = if !before
                    && !visual
                    && !self.doc.text.ends_with('\n')
                    && row + 1 == self.line_count()
                    && !self.doc.text.is_empty()
                {
                    text.insert(0, '\n');
                    if text.ends_with('\n') {
                        text.pop();
                    }
                    at + 1
                } else {
                    at
                };
                self.replace(at..at, &text, checkpoint);
                self.set_operated_range(at..at + text.len());
                if after {
                    self.first_nonblank(
                        self.doc
                            .location(at + text.len())
                            .0
                            .min(self.line_count() - 1),
                    )
                } else {
                    self.first_nonblank(self.doc.location(cursor).0)
                }
            }
            Kind::Block(width) => {
                let at = if before || visual {
                    self.doc.cursor
                } else {
                    self.doc.next(self.doc.cursor).min(line.end)
                };
                let column = self.doc.location(at).1;
                self.put_block(&value.text, width, row, column, count, checkpoint, trim);
                let last = row + value.text.bytes().filter(|&byte| byte == b'\n').count();
                self.set_operated_range(
                    at..self
                        .doc
                        .offset(last, column.saturating_add(width.saturating_mul(count)))
                        .max(at),
                );
                if after {
                    self.doc
                        .offset(last, column.saturating_add(width.saturating_mul(count)))
                } else {
                    self.doc.offset(row, column)
                }
            }
            Kind::Character => {
                let at = if before || visual {
                    self.doc.cursor
                } else {
                    self.doc.next(self.doc.cursor).min(line.end)
                };
                let text = value.text.repeat(count);
                self.replace(at..at, &text, checkpoint);
                self.set_operated_range(at..at + text.len());
                if after {
                    at + text.len()
                } else if text.contains('\n') {
                    at
                } else {
                    self.doc.previous(at + text.len())
                }
            }
        };
        self.set_cursor(at);
    }

    /// Replace a Visual `range` with a register `value`, retaining P's unnamed register.
    fn put_over_selection(
        &mut self,
        range: Range<usize>,
        value: Contents,
        before: bool,
        count: usize,
        after: bool,
        register: &mut Register,
    ) {
        let linewise = self.mode == Mode::VisualLine;
        if !before {
            register.write(None, self.copy_range(range.clone(), linewise), false);
        }
        let mut text = value.text.repeat(count);
        let start = range.start;
        let cursor = if linewise {
            let terminated = self.doc.text[range.clone()].ends_with('\n');
            if terminated && !text.ends_with('\n') {
                text.push('\n');
            }
            if !terminated && value.kind == Kind::Line && text.ends_with('\n') {
                text.pop();
            }
            start
        } else if value.kind == Kind::Line {
            text.insert(0, '\n');
            start + 1
        } else if text.contains('\n') {
            start
        } else {
            start + text.len()
        };
        self.mode = Mode::Normal;
        self.replace(range, &text, true);
        self.set_operated_range(start..start + text.len());
        let cursor = if after {
            start + text.len()
        } else if !linewise && value.kind == Kind::Character && cursor > start {
            self.doc.previous(cursor)
        } else {
            cursor
        };
        self.set_cursor(cursor);
    }

    /// Put the rows in `text` starting at `row` and display `column`.
    fn put_block(
        &mut self,
        text: &str,
        width: usize,
        row: usize,
        column: usize,
        count: usize,
        checkpoint: bool,
        trim: bool,
    ) {
        let revision = self.doc.revision;
        for (offset, segment) in text.split('\n').enumerate() {
            let target = row + offset;
            while target >= self.line_count() {
                let at = self.doc.line_range(self.line_count() - 1).end;
                self.replace(at..at, "\n", checkpoint && self.doc.revision == revision);
            }
            let at = self.doc.offset(target, column);
            let mut padding = " ".repeat(column.saturating_sub(self.doc.location(at).1));
            let mut piece = segment.to_owned();
            let right_pad = if trim {
                0
            } else {
                width.saturating_sub(self.doc.display_width(segment, column))
            };
            if count > 1 {
                piece.push_str(&" ".repeat(right_pad));
                padding.push_str(&piece.repeat(count - 1));
            }
            padding.push_str(segment);
            if right_pad != 0 && at < self.doc.line_range(target).end {
                padding.push_str(&" ".repeat(right_pad));
            }
            self.replace(
                at..at,
                &padding,
                checkpoint && self.doc.revision == revision,
            );
        }
    }

    /// Remove `count` graphemes at or before the caret, optionally entering Insert mode.
    pub(super) fn delete_chars(&mut self, key: char, count: usize, register: &mut Register) {
        let at = self.doc.cursor;
        let row = self.doc.location(at).0;
        let line = self.doc.line_range(row);
        let mut edge = at;
        for _ in 0..count {
            edge = if key == 'X' {
                self.doc.previous(edge).max(line.start)
            } else {
                self.doc.next(edge).min(line.end)
            };
        }
        self.operate_range(
            if key == 's' { 'c' } else { 'd' },
            at.min(edge)..at.max(edge),
            false,
            register,
        );
    }

    /// Replace up to `count` graphemes with characters at matching columns in a neighboring line.
    pub(super) fn replace_from_adjacent_row(&mut self, below: bool, count: usize) {
        if self.mode.is_visual() {
            self.replace_chars(if below { "\u{5}" } else { "\u{19}" }, count);
            return;
        }
        let start = self.doc.cursor;
        let row = self.doc.location(start).0;
        let limit = self.doc.line_range(row).end;
        let mut after = start;
        for _ in 0..count {
            if after == limit {
                return;
            }
            after = self.doc.next(after);
        }
        let last = self.doc.previous(after);
        let Some(source) = row
            .checked_add_signed(if below { 1 } else { -1 })
            .filter(|&row| row < self.line_count())
        else {
            self.set_cursor(last);
            return;
        };
        let source_end = self.doc.line_range(source).end;
        let mut end = start;
        let mut replacement = String::new();
        while end < after {
            let at = self.doc.offset(source, self.doc.location(end).1);
            if at == source_end {
                break;
            }
            replacement.push_str(&self.doc.text[at..self.doc.next(at).min(source_end)]);
            end = self.doc.next(end);
        }
        if end != start {
            self.replace(start..end, &replacement, true);
        }
        if last < end {
            self.set_cursor(self.doc.previous(start + replacement.len()));
        } else {
            self.set_cursor(last - (end - start) + replacement.len());
        }
    }

    /// Replace `count` graphemes with `text`, or replace selected graphemes in Visual mode.
    pub(super) fn replace_chars(&mut self, text: &str, count: usize) {
        if text.graphemes(true).count() != 1 {
            return;
        }
        let at = self.doc.cursor;
        if self.mode.is_visual() {
            let ranges = if self.mode == Mode::VisualBlock {
                let (rows, columns) = self.block_bounds();
                rows.map(|row| self.block_range(row, &columns)).collect()
            } else {
                self.selection().into_iter().collect::<Vec<_>>()
            };
            let cursor = ranges.first().map_or(at, |range| range.start);
            let revision = self.doc.revision;
            for range in ranges.into_iter().rev() {
                let replacement: String = self.doc.text[range.clone()]
                    .graphemes(true)
                    .map(|ch| if ch == "\n" { ch } else { text })
                    .collect();
                self.replace(range, &replacement, self.doc.revision == revision);
            }
            self.mode = Mode::Normal;
            self.set_cursor(cursor);
            return;
        }
        let end = self.doc.line_range(self.doc.location(at).0).end;
        let mut last = at;
        for _ in 0..count {
            if last == end {
                return;
            }
            last = self.doc.next(last);
        }
        let replacement = if text == "\n" {
            text.to_owned()
        } else {
            text.repeat(count)
        };
        self.replace(at..last, &replacement, true);
        if text != "\n" {
            self.set_cursor(self.doc.previous(at + replacement.len()));
        }
    }

    /// Join `count` lines, inserting a space at each boundary when needed.
    pub(super) fn join_lines(&mut self, count: usize) {
        let row = self.doc.location(self.doc.cursor).0;
        let end = row.saturating_add(count.max(2)).min(self.line_count());
        self.join_range(row..end, false);
    }

    /// Join the supplied `rows`, preserving spaces when `literal` is true.
    pub(super) fn join_range(&mut self, rows: Range<usize>, literal: bool) {
        let row = rows.start;
        let last = rows.end - 1;
        if row == last {
            return;
        }
        let start = self.doc.line_range(row).end;
        let end = self.doc.line_range(last).end;
        let mut result = String::new();
        let mut joined_at = start;
        let mut space =
            !self.doc.line(row).is_empty() && !self.doc.line(row).ends_with([' ', '\t']);
        for next in row + 1..=last {
            let line = self.doc.line(next);
            let line = if literal {
                line
            } else {
                line.trim_start_matches([' ', '\t'])
            };
            joined_at = start + result.len();
            if !literal && space && !line.is_empty() && !line.starts_with(')') {
                result.push(' ');
            }
            result.push_str(line);
            if !line.is_empty() {
                space = !line.ends_with([' ', '\t']);
            }
        }
        self.replace(start..end, &result, true);
        self.set_cursor(joined_at);
    }

    /// Toggle the next `count` graphemes' case and advance the caret.
    pub(super) fn toggle_case(&mut self, count: usize) {
        let at = self.doc.cursor;
        let end = self.doc.line_range(self.doc.location(at).0).end;
        let input: String = self.doc.text[at..end].graphemes(true).take(count).collect();
        let text: String = input
            .chars()
            .map(|ch| {
                if ch.is_uppercase() {
                    ch.to_lowercase().collect::<String>()
                } else {
                    ch.to_uppercase().collect()
                }
            })
            .collect();
        self.replace(at..at + input.len(), &text, true);
        self.set_cursor(at + text.len());
    }

    /// Shift or recompute indentation for the lines intersecting `range`.
    fn indent_range(&mut self, range: Range<usize>, key: char) {
        let column = self.doc.location(self.doc.cursor).1;
        let first = self.doc.location(range.start).0;
        let last = self
            .doc
            .location(if range.is_empty() {
                range.end
            } else {
                self.doc.previous(range.end)
            })
            .0;
        let mut previous = (0..first)
            .rev()
            .map(|row| self.doc.line(row))
            .find(|line| !line.trim().is_empty());
        let mut edits = Vec::new();
        let shiftwidth = self.shiftwidth();
        for row in first..=last {
            let line = self.doc.line(row);
            let content = line.trim_start_matches([' ', '\t']);
            let old = line.len() - content.len();
            if content.is_empty() {
                continue;
            }
            let column = self.doc.display_width(&line[..old], 0);
            let desired = match key {
                '>' => column.saturating_add(shiftwidth),
                '<' => column.saturating_sub(shiftwidth),
                _ => {
                    let last = previous.unwrap_or("");
                    let base = edits.last().map(|(_, _, col)| *col).unwrap_or_else(|| {
                        self.doc.display_width(
                            &last[..last.len() - last.trim_start_matches([' ', '\t']).len()],
                            0,
                        )
                    });
                    let opens = last.trim_end().ends_with(['{', '(', '[']);
                    let closes = content.starts_with(['}', ')', ']']);
                    (base + if opens { shiftwidth } else { 0 }).saturating_sub(if closes {
                        shiftwidth
                    } else {
                        0
                    })
                }
            };
            edits.push((row, old, desired));
            previous = Some(line);
        }
        let revision = self.doc.revision;
        for (row, old, column) in edits.into_iter().rev() {
            let at = self.doc.starts[row];
            let indent = self.indentation(column);
            self.replace(at..at + old, &indent, self.doc.revision == revision);
        }
        self.set_cursor(self.doc.offset(first, column));
    }

    /// Restore the original text of the last edited line, allowing U to toggle it again.
    pub(super) fn undo_line(&mut self) {
        let Some(previous) = self.line_undo.take() else {
            return;
        };
        let row = previous.row;
        if row >= self.line_count() {
            return;
        }
        let range = self.doc.line_range(row);
        let current = self.doc.text[range.clone()].to_owned();
        let column = if self.doc.location(self.doc.cursor).0 == row {
            self.doc.cursor - range.start
        } else {
            previous.column
        };
        self.replace(range.clone(), &previous.text, true);
        self.line_undo = Some(super::LineUndo {
            row,
            column,
            text: current,
        });
        self.set_cursor(range.start + previous.column.min(previous.text.len()));
    }

    /// Apply a shell filter result to its original byte `range` as one undoable edit.
    pub fn apply_filter(&mut self, range: Range<usize>, output: &str) -> Action {
        if self.read_only {
            return self.reject_edit();
        }
        let start = range.start;
        let trailing_lf = self.doc.text[range.clone()].ends_with('\n');
        if trailing_lf && !output.is_empty() && !output.ends_with('\n') {
            self.replace(range, &format!("{output}\n"), true);
        } else {
            let output = if trailing_lf {
                output
            } else {
                output.strip_suffix('\n').unwrap_or(output)
            };
            self.replace(range, output, true);
        }
        self.set_cursor(start);
        Action::None
    }
}
