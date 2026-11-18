//! Execute line-oriented Ex commands against addresses in the current document.

mod text;

pub(super) use text::LineInput;

use super::{
    Action, Register, ScreenMotionResolver, Vim, expression::Value, options::MAX_COLUMNS, pattern,
    register::Kind,
};
use crate::{
    document::{Document, MarkModifiers, current_mark_modifiers, with_mark_modifiers},
    i18n,
};
use std::{cmp::Ordering, collections::VecDeque, ops::Range, sync::LazyLock};

/// Horizontal whitespace runs that can be recalculated by :retab.
static WHITESPACE: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"[ \t]+").unwrap());

/// Complete the line commands dispatched here before the host Ex dispatcher.
pub(super) fn candidates(prefix: &str) -> impl Iterator<Item = &'static str> + '_ {
    [
        "join",
        "join!",
        "put",
        "put!",
        "retab",
        "retab!",
        "undo",
        "redo",
        "&",
        "<",
        ">",
        "~",
        "append",
        "center",
        "change",
        "goto",
        "insert",
        "iput",
        "left",
        "right",
        "smagic",
        "snomagic",
        "uniq",
        "checkpath",
        "djump",
        "dlist",
        "dsearch",
        "dsplit",
        "ijump",
        "ilist",
        "isearch",
        "isplit",
        "psearch",
        "foldclose",
        "folddoclosed",
        "folddoopen",
        "foldopen",
        "spelldump",
        "spellgood",
        "spellinfo",
        "spellrare",
        "spellrepall",
        "spellundo",
        "spellwrong",
    ]
    .into_iter()
    .filter(move |name| name.starts_with(prefix))
}

/// Original line starts pending for :global or ranged :normal.
pub(super) struct Lines {
    positions: VecDeque<usize>,
    /// Common shift for edits before all remaining lines.
    shift: isize,
    /// Only the first edit in the command creates an undo checkpoint.
    initial_revision: u64,
    /// Detect edits that bypass position tracking, including an explicit undo.
    revision: u64,
    command: String,
    printed: Vec<String>,
    /// Restore the outer command's mark behavior after a confirmation prompt.
    marks: MarkModifiers,
}

impl Lines {
    /// Shift surviving line identities for an edit about to change the buffer.
    pub(super) fn replace(&mut self, doc: &Document, range: &Range<usize>, text: &str) -> bool {
        self.revision = doc.revision + 1;
        let delta = text.len() as isize - range.len() as isize;
        let removed = &doc.text[range.clone()];
        let joins_next_line = doc.line_range(doc.location(range.start).0).start != range.start
            && removed.ends_with('\n')
            && !text.ends_with('\n');
        let first = self.positions.front().map(|offset| self.position(*offset));
        if first.is_some_and(|start| {
            range.end < start || range.end == start && !range.is_empty() && !joins_next_line
        }) {
            self.shift += delta;
        } else if !self.positions.is_empty() {
            let shift = self.shift;
            let contains_line_break = removed.contains('\n');
            let inserted_prefix = text.rfind('\n').map_or(0, |offset| offset + 1);
            self.positions.retain_mut(|position| {
                let start = position.saturating_add_signed(shift);
                *position = match start {
                    start if start < range.start => start,
                    start if start == range.start && range.is_empty() => start + inserted_prefix,
                    start if start == range.end && joins_next_line => return false,
                    start if start >= range.end => start.saturating_add_signed(delta),
                    start if start == range.start && !contains_line_break => start,
                    _ => return false,
                };
                true
            });
            self.shift = 0;
        }
        doc.revision == self.initial_revision
    }

    /// Map a stored line start through the accumulated common shift.
    fn position(&self, offset: usize) -> usize {
        offset.saturating_add_signed(self.shift)
    }

    /// Return the next surviving original line.
    fn next(&mut self) -> Option<usize> {
        self.positions
            .pop_front()
            .map(|offset| self.position(offset))
    }

    /// Move the original line identities along with the rows being reordered or deduplicated.
    fn reorder(
        &mut self,
        doc: &Document,
        bytes: &Range<usize>,
        offsets: &[Option<usize>],
        length: usize,
    ) -> VecDeque<usize> {
        let delta = length as isize - bytes.len() as isize;
        let first_row = doc.location(bytes.start).0;
        let shift = std::mem::take(&mut self.shift);
        let mut pending = std::mem::take(&mut self.positions);
        pending.retain_mut(|position| {
            let start = position.saturating_add_signed(shift);
            *position = if start < bytes.start {
                start
            } else if start >= bytes.end && doc.location(start).0 >= first_row + offsets.len() {
                start.saturating_add_signed(delta)
            } else {
                let row = doc.location(start).0;
                let Some(offset) = offsets.get(row - first_row).copied().flatten() else {
                    return false;
                };
                offset
            };
            true
        });
        pending.make_contiguous().sort_unstable();
        pending
    }
}

impl Vim {
    /// Execute a supported batch command or leave the command for the regular Ex dispatcher.
    pub(super) fn ex_batch(
        &mut self,
        text: &str,
        register: &mut Register,
        resolve_screen: &mut ScreenMotionResolver<'_>,
    ) -> Option<Action> {
        let text = text.trim();
        if self.ex_goto_command(text) {
            return Some(Action::None);
        }
        if let Some(action) = self.ex_spell_command(text) {
            return Some(action);
        }
        let (range, body) = self.ex_range(text)?;
        let (verb, args) = word(body);
        if let Some(action) = self.ex_include_command(verb, args, range.clone()) {
            return Some(action);
        }
        // Hyphenated host commands such as :undo-batch and :undo-plugin have their own dispatch.
        if verb == "undo" && args.starts_with('-') {
            return None;
        }
        let row = self
            .doc
            .location(self.doc.cursor)
            .0
            .min(self.line_count() - 1);
        let current = row..row + 1;
        if let Some(action) = self.ex_fold_command(
            verb,
            args,
            range.clone(),
            current.clone(),
            register,
            resolve_screen,
        ) {
            return Some(action);
        }
        if self.ex_text_command(verb, args, range.clone(), row, text, register) {
            return Some(Action::None);
        }
        let global = abbreviated(verb, "global", 1);
        let inverse = abbreviated(verb, "vglobal", 1);
        if global || inverse {
            let (invert, args) = if global {
                args.strip_prefix('!')
                    .map_or((false, args), |args| (true, args))
            } else {
                (true, args)
            };
            return Some(self.global(range, args.trim_start(), invert, register, resolve_screen));
        }
        if abbreviated(verb, "normal", 4) {
            let (bang, keys) = args
                .strip_prefix('!')
                .map_or(("", args), |keys| ("!", keys));
            let command = format!("normal{bang} {}", keys.trim_start());
            return if let Some(range) = range {
                if self.ex_lines.is_some() {
                    for row in range {
                        if row >= self.line_count() {
                            break;
                        }
                        self.set_cursor(self.doc.starts[row]);
                        self.script_command(&command, register);
                    }
                    Some(Action::None)
                } else {
                    let positions = range.map(|row| self.doc.starts[row]).collect();
                    Some(self.begin_ex_lines(positions, &command, register, resolve_screen))
                }
            } else {
                self.script_command(&command, register)
            };
        }

        let args = args.trim();
        let (bang, argument) = body[verb.len()..]
            .strip_prefix('!')
            .map_or((false, args), |args| (true, args.trim()));
        if abbreviated(verb, "join", 1) {
            let address = &text[..text.len() - body.len()];
            self.ex_join(range.unwrap_or(current), address, argument, bang);
        } else if abbreviated(verb, "put", 2) {
            let addressed_zero = self
                .ex_address(text, row + 1)
                .is_some_and(|(number, rest)| number == 0 && rest.trim_start() == body);
            let destination = if addressed_zero {
                0
            } else {
                range.unwrap_or(current).end - usize::from(bang)
            };
            self.ex_put(destination, argument, None, register);
        } else if abbreviated(verb, "retab", 3) {
            self.ex_retab(
                range.unwrap_or(0..self.line_count()),
                argument,
                bang,
                register,
            );
        } else if abbreviated(verb, "undo", 1) || abbreviated(verb, "redo", 3) {
            if range.is_some() || bang {
                self.ex_argument_error(text);
            } else {
                self.ex_undo(argument, verb.starts_with('r'));
            }
        } else if abbreviated(verb, "delete", 1) || abbreviated(verb, "yank", 1) {
            let mut rows = range.unwrap_or(current);
            if let Some((name, count)) = register_and_count(args) {
                if let Some(count) = count {
                    rows.start = rows.end - 1;
                    rows.end = rows.start.saturating_add(count).min(self.line_count());
                }
                self.ex_register_lines(verb.starts_with('d'), rows, name, register);
            } else {
                self.ex_argument_error(args);
            }
        } else if abbreviated(verb, "copy", 2) || verb == "t" || abbreviated(verb, "move", 1) {
            let rows = range.unwrap_or(current);
            if let Some((destination, rest)) = self.ex_address(args, row + 1)
                && rest.trim().is_empty()
                && destination <= self.line_count()
            {
                self.ex_transfer_lines(rows, destination, verb.starts_with('m'));
            } else {
                self.ex_argument_error(args);
            }
        } else if abbreviated(verb, "sort", 3) {
            let reverse = body[verb.len()..].starts_with('!');
            self.ex_sort(
                range.unwrap_or(0..self.line_count()),
                if reverse { &args[1..] } else { args },
                reverse,
            );
        } else if abbreviated(verb, "print", 1)
            || abbreviated(verb, "list", 1)
            || abbreviated(verb, "number", 2)
            || verb.is_empty() && args == "#"
        {
            let mut rows = range.unwrap_or(current);
            let count = args.parse::<usize>().ok().filter(|count| *count > 0);
            if let Some(count) = count {
                rows.start = rows.end - 1;
                rows.end = rows.start.saturating_add(count).min(self.line_count());
            } else if !args.is_empty() && args != "#" {
                self.ex_argument_error(args);
                return Some(Action::None);
            }
            self.ex_print(
                rows,
                verb.starts_with('l'),
                verb.starts_with('n') || args == "#",
            );
        } else {
            return None;
        }
        Some(Action::None)
    }

    /// Join the selected `rows` with optional count and printing flags.
    fn ex_join(&mut self, mut rows: Range<usize>, address: &str, args: &str, literal: bool) {
        if self.read_only {
            self.reject_edit();
            return;
        }
        let (count, flags) = {
            let split = args.bytes().take_while(u8::is_ascii_digit).count();
            let (digits, flags) = args.split_at(split);
            (digits.parse::<usize>().ok(), flags.trim())
        };
        if count == Some(0) || !flags.chars().all(|flag| "lp#".contains(flag)) {
            self.ex_argument_error(args);
            return;
        }
        if let Some(count) = count {
            rows.end = rows.end.saturating_add(count - 1).min(self.line_count());
        } else if rows.len() == 1 && !address.contains([',', ';', '%']) {
            rows.end = (rows.end + 1).min(self.line_count());
        }
        let row = rows.start;
        self.join_range(rows, literal);
        if !flags.is_empty() {
            self.ex_print(row..row + 1, flags.contains('l'), flags.contains('#'));
        }
    }

    /// Insert `argument`'s register or expression as lines after `destination` (zero allowed).
    fn ex_put(
        &mut self,
        destination: usize,
        argument: &str,
        indent: Option<usize>,
        register: &Register,
    ) {
        if self.read_only {
            self.reject_edit();
            return;
        }
        let mut contents = if let Some(expression) = argument.strip_prefix('=') {
            match self.evaluate_expression(expression.trim_start(), register) {
                Ok(Value::List(values)) => values
                    .iter()
                    .map(Value::display)
                    .collect::<Vec<_>>()
                    .join("\n"),
                Ok(value) => value.display(),
                Err(error) => {
                    self.ex_failed = true;
                    self.message = error;
                    return;
                }
            }
        } else {
            let mut letters = argument.chars();
            let name = letters.next();
            if letters.next().is_some() {
                self.ex_argument_error(argument);
                return;
            }
            let mut value = self.text_register(name, register);
            if value.text.is_empty() {
                return;
            }
            if value.kind == Kind::Line && value.text.ends_with('\n') {
                value.text.pop();
            }
            value.text
        };
        if let Some(indent) = indent {
            contents = self.align_put_indentation(&contents, indent);
        }
        self.put_ex_lines(destination, contents);
    }

    /// Insert collected `contents` after a one-based `destination` in one undo step.
    fn put_ex_lines(&mut self, destination: usize, mut contents: String) {
        let lines = contents.bytes().filter(|&byte| byte == b'\n').count() + 1;
        let dest = if self.doc.text.is_empty() {
            0
        } else {
            destination
        };
        let at = self
            .doc
            .starts
            .get(dest)
            .copied()
            .unwrap_or(self.doc.text.len());
        if !self.doc.text.is_empty() {
            if dest < self.line_count() || self.final_newline && self.doc.text.ends_with('\n') {
                contents.push('\n');
            } else {
                contents.insert(0, '\n');
            }
        }
        self.replace(at..at, &contents, true);
        self.set_operated_range(at..at + contents.len());
        self.set_cursor(self.first_nonblank((dest + lines - 1).min(self.line_count() - 1)));
    }

    /// Convert whitespace on `rows` to the requested tab width while keeping display columns.
    fn ex_retab(&mut self, rows: Range<usize>, args: &str, bang: bool, register: &Register) {
        if self.read_only {
            self.reject_edit();
            return;
        }
        let (indent_only, width) = args
            .strip_prefix("-indentonly")
            .map_or((false, args), |width| (true, width.trim_start()));
        self.set_input_options(&register.input_maps);
        let current = self.doc.tabstop();
        let new = if width.is_empty() {
            Some(current)
        } else {
            width
                .parse::<usize>()
                .ok()
                .filter(|&width| width <= MAX_COLUMNS)
                .map(|width| if width == 0 { current } else { width })
        };
        let Some(new) = new else {
            self.ex_argument_error(args);
            return;
        };
        let expand = self.expandtab();
        let mut edits = Vec::new();
        for row in rows {
            let line = self.doc.line(row);
            let base = self.doc.starts[row];
            let mut byte = 0;
            let mut column = 0;
            for span in WHITESPACE.find_iter(line) {
                if indent_only && span.start() != 0 {
                    break;
                }
                column += self.doc.display_width(&line[byte..span.start()], column);
                let end = column + self.doc.display_width(span.as_str(), column);
                if span.as_str().contains('\t') || bang && span.len() > 1 {
                    let text = retab_spaces(column, end, new, expand);
                    if text != span.as_str() {
                        edits.push((base + span.start()..base + span.end(), text));
                    }
                }
                byte = span.end();
                column = end;
            }
        }
        let initial = self.doc.revision;
        let (row, column) = self.doc.location(self.doc.cursor);
        for (span, text) in edits.into_iter().rev() {
            self.replace(span, &text, self.doc.revision == initial);
        }
        self.set_buffer_tabstop(new);
        self.set_cursor(self.doc.offset(row, column));
    }

    /// Step the current buffer's branch or visit the stable change number in `argument`.
    fn ex_undo(&mut self, argument: &str, redo: bool) {
        if self.read_only {
            self.reject_edit();
            return;
        }
        if argument.is_empty() {
            self.doc.undo(redo);
        } else if redo
            || !argument
                .parse()
                .is_ok_and(|number| self.doc.undo_number(number))
        {
            self.ex_argument_error(argument);
            return;
        }
        self.replay.reset_macro_undo(self.doc.revision);
        self.desired_column = None;
        self.clamp_normal();
    }

    /// Select matching line identities before applying the global command to any line.
    fn global(
        &mut self,
        range: Option<Range<usize>>,
        args: &str,
        invert: bool,
        register: &mut Register,
        resolve_screen: &mut ScreenMotionResolver<'_>,
    ) -> Action {
        let Some(delimiter) = args.chars().next() else {
            self.ex_argument_error(args);
            return Action::None;
        };
        if delimiter.is_alphanumeric() || matches!(delimiter, '\\' | '"' | '|' | '!') {
            self.ex_argument_error(args);
            return Action::None;
        }
        let source = &args[delimiter.len_utf8()..];
        let (mut query, command) =
            pattern::delimited(source, delimiter).unwrap_or_else(|| (source.to_owned(), ""));
        if query.is_empty() {
            if let Some(previous) = self.search_pattern() {
                query.push_str(previous);
            } else {
                self.message = i18n::text("editor.no_previous_pattern").into();
                return Action::None;
            }
        }
        let regex = match pattern::compile(&query, self.pattern_case(&query, None)) {
            Ok(regex) => regex,
            Err(error) => {
                self.pattern_error(&error);
                return Action::None;
            }
        };
        let command = command.trim_start_matches([' ', '\t', ':']);
        let command = if command.is_empty() { "print" } else { command };
        let row = self
            .doc
            .location(self.doc.cursor)
            .0
            .min(self.line_count() - 1);
        if self.ex_lines.is_some() {
            if range.is_some() {
                self.message = i18n::text("editor.recursive_global_range").into();
            } else if regex.is_match(self.doc.line(row)) != invert {
                self.remember_global_pattern(query, regex);
                self.ex_on_current_line(command, register, resolve_screen);
            }
            return Action::None;
        }
        let positions: VecDeque<_> = range
            .unwrap_or(0..self.line_count())
            .filter(|&row| regex.is_match(self.doc.line(row)) != invert)
            .map(|row| self.doc.starts[row])
            .collect();
        if positions.is_empty() {
            self.message = i18n::format("editor.search_not_found", &[("query", &query)]);
        } else {
            self.record_jump();
        }
        self.remember_global_pattern(query, regex);
        self.begin_ex_lines(positions, command, register, resolve_screen)
    }

    /// Begin an undo unit for the original line starts to which this command applies.
    fn begin_ex_lines(
        &mut self,
        positions: VecDeque<usize>,
        command: &str,
        register: &mut Register,
        resolve_screen: &mut ScreenMotionResolver<'_>,
    ) -> Action {
        self.ex_lines = Some(Lines {
            positions,
            shift: 0,
            command: command.into(),
            printed: Vec::new(),
            initial_revision: self.doc.revision,
            revision: self.doc.revision,
            marks: current_mark_modifiers(),
        });
        self.continue_ex_lines(register, resolve_screen)
    }

    /// Apply the command to still-existing lines, pausing for a confirmation prompt.
    pub(super) fn continue_ex_lines(
        &mut self,
        register: &mut Register,
        resolve_screen: &mut ScreenMotionResolver<'_>,
    ) -> Action {
        let Some(mut lines) = self.ex_lines.take() else {
            return Action::None;
        };
        let command = lines.command.clone();
        loop {
            if lines.revision != self.doc.revision {
                self.message = i18n::text("editor.ex_lines_invalidated").into();
                break;
            }
            let Some(position) = lines.next() else {
                if !lines.printed.is_empty() {
                    self.message = lines.printed.join(" · ");
                }
                break;
            };
            self.set_cursor(position);
            let marks = lines.marks;
            self.ex_lines = Some(lines);
            let supported = with_mark_modifiers(marks, || {
                self.ex_on_current_line(&command, register, resolve_screen)
            });
            let Some(next) = self.ex_lines.take() else {
                break;
            };
            lines = next;
            if self.substitute_confirmation_active() {
                self.ex_lines = Some(lines);
                break;
            }
            if !supported {
                break;
            }
        }
        Action::None
    }

    /// Run a document-local command, so a global loop cannot dispatch a host action.
    fn ex_on_current_line(
        &mut self,
        command: &str,
        register: &mut Register,
        resolve_screen: &mut ScreenMotionResolver<'_>,
    ) -> bool {
        if let Some((modifiers, command)) = super::ex_window_modifiers::parse_modifiers(command)
            && !modifiers.has_native_options()
            && !command.is_empty()
        {
            let mappings = register.input_maps.clone();
            return modifiers.with_ex_state(&mappings, || {
                self.ex_on_current_line(command, register, resolve_screen)
            });
        }
        match self.ex_batch(command, register, resolve_screen) {
            Some(Action::None) => true,
            None if self.substitute_command(command) => true,
            _ => {
                self.ex_failed = true;
                self.message = i18n::format("editor.unsupported_command", &[("command", &command)]);
                false
            }
        }
    }

    /// Apply the line operator with the register explicitly selected by an Ex argument.
    fn ex_register_lines(
        &mut self,
        delete: bool,
        rows: Range<usize>,
        name: Option<char>,
        register: &mut Register,
    ) {
        if delete && self.read_only {
            self.reject_edit();
            return;
        }
        let edited = self.line_span(rows.start, rows.end - 1);
        if delete {
            self.selected_register = name;
            self.operate_range('d', edited, true, register);
        } else {
            register.write(name, self.copy_range(edited.clone(), true), true);
            self.set_operated_range(edited);
        }
    }

    /// Copy or move a line range after a one-based destination, with 0 before line 1.
    fn ex_transfer_lines(&mut self, rows: Range<usize>, dest: usize, moving: bool) {
        if self.read_only {
            self.reject_edit();
            return;
        }
        if !moving {
            let start = self.doc.starts[rows.start];
            let end = self.doc.line_range(rows.end - 1).end;
            let mut text = self.doc.text[start..end].to_owned();
            let destination = self
                .doc
                .starts
                .get(dest)
                .copied()
                .unwrap_or(self.doc.text.len());
            if dest == self.line_count() && !(self.final_newline && self.doc.text.ends_with('\n')) {
                text.insert(0, '\n');
                if self.final_newline {
                    text.push('\n');
                }
            } else {
                text.push('\n');
            }
            self.replace(destination..destination, &text, true);
            let row = dest + rows.len() - 1;
            self.set_cursor(self.first_nonblank(row.min(self.line_count() - 1)));
            return;
        }
        if dest == rows.start || dest == rows.end {
            self.set_cursor(self.first_nonblank(rows.end - 1));
            return;
        }
        if rows.contains(&dest) {
            self.message = i18n::text("editor.move_lines_into_self").into();
            return;
        }
        let affected = dest.min(rows.start)..dest.max(rows.end);
        let mut order: Vec<_> = affected.clone().collect();
        if dest < rows.start {
            order.rotate_right(rows.len());
        } else {
            order.rotate_left(rows.len());
        }
        let row = if dest < rows.start {
            dest + rows.len()
        } else {
            dest
        } - 1;
        self.reorder_ex_lines(affected, order, true);
        self.set_cursor(self.first_nonblank(row));
    }

    /// Replace a contiguous run of lines in a new order, preserving its final line ending.
    fn reorder_ex_lines(&mut self, rows: Range<usize>, order: Vec<usize>, moving: bool) {
        let bytes = self.line_span(rows.start, rows.end - 1);
        let tail = bytes.end > self.doc.line_range(rows.end - 1).end;
        let mut text = String::with_capacity(bytes.len());
        let mut offsets = vec![None; rows.len()];
        let mut ordered_starts = Vec::with_capacity(order.len());
        for (index, row) in order.into_iter().enumerate() {
            if index > 0 {
                text.push('\n');
            }
            offsets[row - rows.start] = Some(bytes.start + text.len());
            ordered_starts.push((bytes.start + text.len(), self.doc.line(row).len()));
            text.push_str(self.doc.line(row));
        }
        if tail && !ordered_starts.is_empty() {
            text.push('\n');
        }
        let marked = self
            .ex_lines
            .as_mut()
            .map(|lines| lines.reorder(&self.doc, &bytes, &offsets, text.len()));
        let marks: Vec<_> = self
            .marks
            .iter()
            .filter_map(|(&name, &at)| {
                if current_mark_modifiers().lock_marks && !matches!(name, '\'' | '`') {
                    return None;
                }
                let row = self.doc.location(at).0;
                if !rows.contains(&row) {
                    return None;
                }
                let index = row - rows.start;
                let column = at - self.doc.starts[row];
                let position = if moving {
                    offsets[index].map(|start| start + column)
                } else {
                    ordered_starts
                        .get(index)
                        .map(|&(start, len)| start + column.min(len))
                };
                Some((name, position))
            })
            .collect();
        // A :move permutes original lines, so their signs follow even when the text splice
        // would otherwise invalidate a replaced line's decorations.
        let moved_signs: Vec<_> = if moving {
            self.doc
                .signs()
                .into_iter()
                .filter_map(|sign| {
                    rows.contains(&sign.row)
                        .then(|| (offsets[sign.row - rows.start], sign))
                })
                .collect()
        } else {
            Vec::new()
        };
        self.replace(bytes, &text, true);
        for (start, mut sign) in moved_signs.into_iter().rev() {
            if let Some(start) = start {
                sign.row = self.doc.location(start).0;
                self.doc.place_sign(sign);
            }
        }
        if let (Some(lines), Some(marked)) = (&mut self.ex_lines, marked) {
            lines.positions = marked;
        }
        for (name, position) in marks {
            if let Some(position) = position {
                self.marks
                    .insert(name, self.doc.text.floor_char_boundary(position));
            } else {
                self.marks.remove(&name);
            }
        }
    }

    /// Sort addressed lines by text or number, retaining each original line for global execution.
    fn ex_sort(&mut self, rows: Range<usize>, args: &str, reverse: bool) {
        if self.read_only {
            self.reject_edit();
            return;
        }
        let Some((flags, query)) = sort_arguments(args) else {
            self.ex_argument_error(args);
            return;
        };
        let regex = match query
            .as_deref()
            .map(|query| pattern::compile(query, None))
            .transpose()
        {
            Ok(regex) => regex,
            Err(error) => {
                self.pattern_error(&error);
                return;
            }
        };
        let numeric: Vec<_> = flags
            .chars()
            .filter_map(|flag| match flag {
                'n' => Some(10),
                'x' => Some(16),
                'o' => Some(8),
                'b' => Some(2),
                _ => None,
            })
            .collect();
        if numeric.len() > 1 {
            self.ex_argument_error(args);
            return;
        }
        let insensitive = flags.contains('i');
        let mut order: Vec<_> = rows
            .clone()
            .map(|row| {
                let line = self.doc.line(row);
                let key = match &regex {
                    Some(regex) => regex.find(line).map_or("", |matched| {
                        if flags.contains('r') {
                            matched.as_str()
                        } else {
                            &line[matched.end()..]
                        }
                    }),
                    None => line,
                };
                let key = if let Some(&radix) = numeric.first() {
                    SortKey::Number(first_number(key, radix))
                } else if insensitive {
                    SortKey::Text(key.to_lowercase())
                } else {
                    SortKey::Text(key.into())
                };
                (row, key)
            })
            .collect();
        order.sort_by(|(_, a), (_, b)| a.compare(b));
        if reverse {
            order.reverse();
        }
        if flags.contains('u') {
            order.dedup_by(|(row, _), (previous, _)| {
                let a = self.doc.line(*row);
                let b = self.doc.line(*previous);
                a == b || insensitive && a.to_lowercase() == b.to_lowercase()
            });
        }
        let row = rows.start;
        self.reorder_ex_lines(rows, order.into_iter().map(|(row, _)| row).collect(), false);
        self.set_cursor(self.first_nonblank(row));
    }

    /// Display addressed lines in Ex command feedback, including numbering or visible tabs.
    fn ex_print(&mut self, rows: Range<usize>, list: bool, number: bool) {
        let mut rendered = rows
            .clone()
            .map(|row| {
                let line = self.doc.line(row);
                let line = if list {
                    format!("{}$", line.replace('\t', "^I"))
                } else {
                    line.into()
                };
                if number {
                    format!("{} {line}", row + 1)
                } else {
                    line
                }
            })
            .collect::<Vec<_>>();
        self.set_cursor(self.first_nonblank(rows.end - 1));
        if let Some(lines) = &mut self.ex_lines {
            lines.printed.append(&mut rendered);
        } else {
            self.message = rendered.join(" · ");
        }
    }

    /// Report an invalid command argument without changing the document.
    fn ex_argument_error(&mut self, argument: &str) {
        self.ex_failed = true;
        self.message = i18n::format("editor.invalid_ex_argument", &[("argument", &argument)]);
    }
}

/// Split off an Ex verb, even when its arguments immediately follow it.
fn word(text: &str) -> (&str, &str) {
    text.split_at(text.bytes().take_while(u8::is_ascii_alphabetic).count())
}

/// Whether the verb is a permitted abbreviation of the canonical command.
fn abbreviated(verb: &str, name: &str, minimum: usize) -> bool {
    verb.len() >= minimum && name.starts_with(verb)
}

/// Build whitespace ending at `end` from `start` using the new spacing.
fn retab_spaces(start: usize, end: usize, tabstop: usize, expand: bool) -> String {
    let tabs = end / tabstop - start / tabstop;
    if expand || tabs == 0 || end - start == 1 {
        " ".repeat(end - start)
    } else {
        "\t".repeat(tabs) + &" ".repeat(end % tabstop)
    }
}

/// Parse an optional register and count for :delete and :yank.
fn register_and_count(args: &str) -> Option<(Option<char>, Option<usize>)> {
    let mut words = args.split_whitespace();
    let Some(first) = words.next() else {
        return Some((None, None));
    };
    let (register, count) = if let Ok(number) = first.parse::<usize>() {
        (None, Some(number))
    } else {
        let name = first.strip_prefix('"').unwrap_or(first);
        let mut name = name.chars();
        let register = name.next()?;
        if name.next().is_some()
            || !(register.is_ascii_alphanumeric() || "\"_*-+".contains(register))
        {
            return None;
        }
        (
            Some(register),
            words.next().map(str::parse).transpose().ok()?,
        )
    };
    (words.next().is_none() && count != Some(0)).then_some((register, count))
}

/// Sort options may appear on either side of an optional delimited key pattern.
fn sort_arguments(mut args: &str) -> Option<(String, Option<String>)> {
    let mut flags = String::new();
    let mut pattern = None;
    loop {
        args = args.trim_start();
        let Some(first) = args.chars().next() else {
            return Some((flags, pattern));
        };
        if "inobxru".contains(first) {
            flags.push(first);
            args = &args[1..];
        } else if pattern.is_none() && !first.is_alphanumeric() {
            let (query, tail) = pattern::delimited(&args[first.len_utf8()..], first)?;
            pattern = Some(query);
            args = tail;
        } else {
            return None;
        }
    }
}

/// Compare only keys of the same kind within one sort command.
enum SortKey {
    Text(String),
    Number(Option<i128>),
}

impl SortKey {
    fn compare(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Text(a), Self::Text(b)) => a.cmp(b),
            (Self::Number(a), Self::Number(b)) => a.cmp(b),
            _ => Ordering::Equal,
        }
    }
}

/// Locate the first integer in a line's sorting key, retaining an immediately preceding minus.
fn first_number(key: &str, radix: u32) -> Option<i128> {
    let start = key.find(|ch: char| ch.is_digit(radix))?;
    let sign = if key[..start].ends_with('-') { -1 } else { 1 };
    let digits = &key[start..];
    let digits = if radix == 16 {
        digits
            .strip_prefix("0x")
            .or_else(|| digits.strip_prefix("0X"))
            .unwrap_or(digits)
    } else {
        digits
    };
    let value = digits
        .chars()
        .map_while(|ch| ch.to_digit(radix))
        .fold(0i128, |value, digit| {
            value
                .saturating_mul(i128::from(radix))
                .saturating_add(i128::from(digit))
        });
    Some(sign * value)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Submit an Ex command through the same prompt as a native keyboard.
    fn ex(vim: &mut Vim, register: &mut Register, command: &str) {
        assert_eq!(vim.text(&format!(":{command}"), register), Action::None);
        assert_eq!(vim.key("Enter", register), Action::None);
    }

    #[test]
    fn append_insert_and_change_collect_literal_lines_until_period() {
        let mut vim = Vim::new(Document::new("first\nlast\n"));
        let mut register = Register::default();
        ex(&mut vim, &mut register, "1append");
        assert!(vim.collecting_ex_lines());
        vim.text("middle", &mut register);
        vim.key("Enter", &mut register);
        vim.text(".", &mut register);
        vim.key("Enter", &mut register);
        assert_eq!(vim.doc.text, "first\nmiddle\nlast\n");
        assert_eq!(vim.mode, crate::vim::Mode::Normal);

        ex(&mut vim, &mut register, "1insert|head");
        assert!(vim.accept_ex_line("."));
        assert_eq!(vim.doc.text, "head\nfirst\nmiddle\nlast\n");
        ex(&mut vim, &mut register, "2,3change!");
        assert!(vim.accept_ex_line("replacement"));
        assert!(vim.accept_ex_line("."));
        assert_eq!(vim.doc.text, "head\nreplacement\nlast\n");
        vim.key("u", &mut register);
        assert_eq!(vim.doc.text, "head\nfirst\nmiddle\nlast\n");
    }

    #[test]
    fn range_shift_alignment_put_and_byte_goto_reuse_buffer_options() {
        let mut vim = Vim::new(Document::new("xy\n  猫\nend"));
        let mut register = Register::default();
        ex(
            &mut vim,
            &mut register,
            "set expandtab shiftwidth=2 textwidth=10",
        );
        ex(&mut vim, &mut register, "1,2>>");
        assert_eq!(vim.doc.text, "    xy\n      猫\nend");
        ex(&mut vim, &mut register, "1,2<");
        assert_eq!(vim.doc.text, "  xy\n    猫\nend");
        ex(&mut vim, &mut register, "1center");
        ex(&mut vim, &mut register, "2right 8");
        ex(&mut vim, &mut register, "3left 1");
        assert_eq!(vim.doc.text, "    xy\n      猫\n end");
        ex(&mut vim, &mut register, "1yank");
        ex(&mut vim, &mut register, "2iput");
        assert_eq!(vim.doc.text, "    xy\n      猫\n      xy\n end");
        ex(&mut vim, &mut register, "goto 15");
        assert_eq!(vim.doc.location(vim.doc.cursor), (1, 6));
    }

    #[test]
    fn goto_counts_crlf_and_uses_the_final_address_of_a_range() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("dos.txt");
        std::fs::write(&file, b"ab\r\ncd\r\nef").unwrap();
        let mut vim = Vim::new(Document::open(&file).unwrap());
        let mut register = Register::default();
        ex(&mut vim, &mut register, "2,5goto");
        assert_eq!(vim.doc.location(vim.doc.cursor), (1, 0));
        ex(&mut vim, &mut register, "goto 10");
        assert_eq!(vim.doc.location(vim.doc.cursor), (2, 1));
    }

    #[test]
    fn uniq_compares_adjacent_rows_with_pattern_and_keep_modes() {
        let mut vim = Vim::new(Document::new("a,CAT\nb,cat\none\none\nend\na,CAT\n"));
        let mut register = Register::default();
        ex(&mut vim, &mut register, "1,2uniq i /[^,]*,/");
        assert_eq!(vim.doc.text, "a,CAT\none\none\nend\na,CAT\n");
        ex(&mut vim, &mut register, "uniq u");
        assert_eq!(vim.doc.text, "a,CAT\nend\na,CAT\n");
        vim.key("u", &mut register);
        ex(&mut vim, &mut register, "uniq!");
        assert_eq!(vim.doc.text, "one\n");
    }

    #[test]
    fn folding_ex_operations_visit_header_and_body_before_edits_shift_rows() {
        let mut vim = Vim::new(Document::new("inside\ninside\ninside\noutside\n"));
        let mut register = Register::default();
        vim.doc.set_provider_folds(vec![0..3], false);
        ex(&mut vim, &mut register, "1foldclose");
        assert!(vim.doc.folded_at(0));
        ex(&mut vim, &mut register, "folddoclosed s/inside/folded/");
        assert_eq!(vim.doc.text, "folded\nfolded\nfolded\noutside\n");
        ex(&mut vim, &mut register, "folddoopen s/outside/open/");
        assert!(vim.doc.text.ends_with("open\n"));
        ex(&mut vim, &mut register, "1foldopen!");
        assert!(!vim.doc.folded_at(0));
    }

    #[test]
    fn ranged_ex_fold_open_visits_nested_siblings_without_reopening_new_visibility() {
        let mut vim = Vim::new(Document::new(&"line\n".repeat(12)));
        let mut register = Register::default();
        vim.doc.set_provider_folds(vec![0..10, 2..5, 6..9], false);
        ex(&mut vim, &mut register, "3foldclose");
        ex(&mut vim, &mut register, "7foldclose");
        assert!(!vim.doc.folded_at(0));
        assert!(vim.doc.folded_at(2) && vim.doc.folded_at(6));

        ex(&mut vim, &mut register, "1foldclose");
        ex(&mut vim, &mut register, "%foldopen");
        assert!(!vim.doc.folded_at(0));
        assert!(vim.doc.folded_at(2) && vim.doc.folded_at(6));

        ex(&mut vim, &mut register, "%foldopen");
        assert!(vim.doc.hidden_rows().is_empty());

        ex(&mut vim, &mut register, "%foldclose!");
        assert!(vim.doc.folded_at(0));
        ex(&mut vim, &mut register, "1foldopen!");
        assert!(vim.doc.hidden_rows().is_empty());
    }

    #[test]
    fn substitute_magic_forms_and_symbol_repeats_use_distinct_patterns() {
        let mut vim = Vim::new(Document::new("a.b axb\na.b axb\nword word\n"));
        let mut register = Register::default();
        ex(&mut vim, &mut register, "1snomagic/a.b/lit/g");
        assert_eq!(vim.doc.line(0), "lit axb");
        ex(&mut vim, &mut register, "2&");
        assert_eq!(vim.doc.line(1), "lit axb");
        ex(&mut vim, &mut register, "1smagic/a.b/wild/");
        assert_eq!(vim.doc.line(0), "lit wild");
        vim.text("/word", &mut register);
        vim.key("Enter", &mut register);
        ex(&mut vim, &mut register, "3~g");
        assert_eq!(vim.doc.line(2), "wild wild");
    }

    #[test]
    fn global_skips_original_lines_deleted_by_a_prior_iteration() {
        let mut vim = Vim::new(Document::new("x\nx\nkeep\nx\nend\n"));
        let mut register = Register::default();
        ex(&mut vim, &mut register, "g/x/delete 2");
        assert_eq!(vim.doc.text, "keep\n");
        vim.key("u", &mut register);
        assert_eq!(vim.doc.text, "x\nx\nkeep\nx\nend\n");
        ex(&mut vim, &mut register, "g/x/normal! dd");
        assert_eq!(vim.doc.text, "keep\nend\n");
    }

    #[test]
    fn global_inversion_patterns_and_mutations_follow_original_lines() {
        let mut vim = Vim::new(Document::new("x1\nx2\nkeep\nx3\nend"));
        let mut register = Register::default();
        ex(&mut vim, &mut register, "v/^x/");
        assert_eq!(vim.message, "keep · end");
        ex(&mut vim, &mut register, "g//m0");
        assert_eq!(vim.doc.text, "x3\nx2\nx1\nkeep\nend");
        vim.key("u", &mut register);
        ex(&mut vim, &mut register, "g/^x/t0");
        assert_eq!(vim.doc.text, "x3\nx2\nx1\nx1\nx2\nkeep\nx3\nend");
        vim.key("u", &mut register);
        ex(&mut vim, &mut register, "2,4g!/^x/s/keep/stay/");
        assert_eq!(vim.doc.text, "x1\nx2\nstay\nx3\nend");
    }

    #[test]
    fn global_remembers_pattern_for_empty_and_repeated_substitutions() {
        let mut vim = Vim::new(Document::new("x\nx\ny\nx"));
        let mut register = Register::default();
        ex(&mut vim, &mut register, "1s/x/Z/");
        ex(&mut vim, &mut register, "g/y/p");
        ex(&mut vim, &mut register, "3s");
        assert_eq!(vim.doc.text, "Z\nx\nZ\nx");
        ex(&mut vim, &mut register, "g/^x/s//fill/");
        assert_eq!(vim.doc.text, "Z\nfill\nZ\nfill");
        vim.key("u", &mut register);
        assert_eq!(vim.doc.text, "Z\nx\nZ\nx");
    }

    #[test]
    fn line_commands_respect_destination_zero_and_file_termination() {
        let mut vim = Vim::new(Document::new("d\n  B\nc\nb\nA"));
        let mut register = Register::default();
        ex(&mut vim, &mut register, "2,3ya a");
        assert_eq!(register.read(Some('a')).text, "  B\nc\n");
        ex(&mut vim, &mut register, "2,3copy 0");
        assert_eq!(vim.doc.text, "  B\nc\nd\n  B\nc\nb\nA");
        ex(&mut vim, &mut register, "1,2move $");
        assert_eq!(vim.doc.text, "d\n  B\nc\nb\nA\n  B\nc");
        ex(&mut vim, &mut register, "2,3d _");
        assert_eq!(vim.doc.text, "d\nb\nA\n  B\nc");
        ex(&mut vim, &mut register, "%sort iu");
        assert_eq!(vim.doc.text, "  B\nA\nb\nc\nd");
    }

    #[test]
    fn ranged_normal_and_pattern_sort_use_normal_ex_addresses() {
        let mut vim = Vim::new(Document::new("x,10\ny,-2\nbad\nz,3\n"));
        let mut register = Register::default();
        ex(&mut vim, &mut register, "sort n /,/");
        assert_eq!(vim.doc.text, "bad\ny,-2\nz,3\nx,10\n");
        ex(&mut vim, &mut register, "2,3normal! A!");
        assert_eq!(vim.doc.text, "bad\ny,-2!\nz,3!\nx,10\n");
        vim.key("u", &mut register);
        assert_eq!(vim.doc.text, "bad\ny,-2\nz,3\nx,10\n");
    }

    #[test]
    fn join_put_retab_and_ex_undo_preserve_file_boundaries() {
        let mut vim = Vim::new(Document::new("one\n two\nthree"));
        let mut register = Register::default();
        ex(&mut vim, &mut register, "1,2join!");
        assert_eq!(vim.doc.text, "one two\nthree");
        ex(&mut vim, &mut register, "undo");
        assert_eq!(vim.doc.text, "one\n two\nthree");
        ex(&mut vim, &mut register, "redo");
        ex(&mut vim, &mut register, "0put =['a', 'b']");
        assert_eq!(vim.doc.text, "a\nb\none two\nthree");
        ex(&mut vim, &mut register, "undo 1");
        assert_eq!(vim.doc.text, "one two\nthree");

        let mut vim = Vim::new(Document::new("\talpha \t beta\n    end"));
        ex(&mut vim, &mut register, "retab 8");
        assert_eq!(vim.doc.text, "    alpha    beta\n    end");
        assert_eq!(vim.doc.tabstop(), 8);
        ex(&mut vim, &mut register, "set expandtab");
        ex(&mut vim, &mut register, "1retab -indentonly 4");
        assert_eq!(vim.doc.text, "    alpha    beta\n    end");
    }

    #[test]
    fn confirmed_substitution_tracks_shift_and_ignores_normal_mappings() {
        let mut vim = Vim::new(Document::new("abc abc\nabc abc\n"));
        let mut register = Register::default();
        ex(&mut vim, &mut register, "nnoremap y x");
        ex(&mut vim, &mut register, "%s/abc/你好/gc");
        assert_eq!(vim.substitution_match(), Some(0..3));
        assert!(!vim.accepts_text());
        vim.key("y", &mut register);
        assert_eq!(vim.substitution_match(), Some(7..10));
        vim.key("n", &mut register);
        assert_eq!(vim.substitution_match(), Some(11..14));
        vim.key("l", &mut register);
        assert_eq!(vim.doc.text, "你好 abc\n你好 abc\n");
        assert_eq!(vim.substitution_match(), None);
        vim.key("u", &mut register);
        assert_eq!(vim.doc.text, "abc abc\nabc abc\n");
    }

    #[test]
    fn confirmation_can_resume_global_lines_but_escape_stops_remaining_matches() {
        let mut vim = Vim::new(Document::new("x x\nkeep\nx x\nx\n"));
        let mut register = Register::default();
        ex(&mut vim, &mut register, "g/x/s/x/long/gc");
        vim.key("a", &mut register);
        assert_eq!(vim.doc.text, "long long\nkeep\nx x\nx\n");
        assert_eq!(vim.substitution_match(), Some(15..16));
        vim.key("y", &mut register);
        vim.key("Ctrl+c", &mut register);
        assert_eq!(vim.doc.text, "long long\nkeep\nlong x\nx\n");
        assert!(!vim.substitute_confirmation_active());
        vim.key("u", &mut register);
        assert_eq!(vim.doc.text, "x x\nkeep\nx x\nx\n");
    }
}
