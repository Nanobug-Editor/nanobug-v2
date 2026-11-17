//! Range editing and the dot-terminated Ex text input prompt.

use super::{abbreviated, pattern};
use crate::{
    document::display_width_with_tabstop,
    i18n,
    vim::{Action, Mode, Register, ScreenMotionResolver, Vim, options::MAX_COLUMNS},
};
use std::ops::Range;

/// Ex text collected before the final period, together with the original edit location.
pub(in crate::vim) struct LineInput {
    /// Document identity and revision at the start of the prompt.
    origin: (u64, u64),
    /// Insert after this one-based address when no existing rows are replaced.
    after: usize,
    /// Addressed rows removed by :change.
    replace: Option<Range<usize>>,
    /// Literal text supplied by keyboard or a sourced script.
    lines: Vec<String>,
    /// Carry horizontal indentation into each new interactive prompt line.
    autoindent: bool,
}

impl Vim {
    /// Apply an Ex range fold operation or visit the original lines in a fold state.
    pub(super) fn ex_fold_command(
        &mut self,
        name: &str,
        args: &str,
        range: Option<Range<usize>>,
        current: Range<usize>,
        register: &mut Register,
        resolve_screen: &mut ScreenMotionResolver<'_>,
    ) -> Option<Action> {
        if abbreviated(name, "foldopen", 5) || abbreviated(name, "foldclose", 5) {
            let (recursive, args) = args
                .strip_prefix('!')
                .map_or((false, args), |args| (true, args));
            if !args.trim().is_empty() {
                self.ex_argument_error(args);
                return Some(Action::None);
            }
            self.ex_change_fold_rows(
                range.unwrap_or(current),
                name.starts_with("foldc"),
                recursive,
            );
            return Some(Action::None);
        }
        let closed = abbreviated(name, "folddoclosed", 7);
        if !closed && !abbreviated(name, "folddoopen", 5) {
            return None;
        }
        let command = args.trim_start_matches([' ', '\t', ':']);
        if command.is_empty() || self.ex_lines.is_some() {
            self.ex_argument_error(args);
            return Some(Action::None);
        }
        let positions = range
            .unwrap_or(0..self.line_count())
            .filter(|&row| {
                (self.doc.folded_at(row) || self.doc.collapsed_body_at(row).is_some()) == closed
            })
            .map(|row| self.doc.starts[row])
            .collect();
        Some(self.begin_ex_lines(positions, command, register, resolve_screen))
    }

    /// Interpret the address of :goto as a byte index, independent of line ranges.
    pub(super) fn ex_goto_command(&mut self, text: &str) -> bool {
        let current = self.doc.location(self.doc.cursor).0 + 1;
        let (prefix, body) = match self.ex_address(text, current) {
            Some((first, rest)) => {
                let rest = rest.trim_start();
                if let Some(sep @ (';' | ',')) = rest.chars().next() {
                    let base = if sep == ';' { first } else { current };
                    let Some((last, tail)) = self.ex_address(rest[1..].trim_start(), base) else {
                        return false;
                    };
                    (Some(last), tail.trim_start())
                } else {
                    (Some(first), rest)
                }
            }
            None => (None, text),
        };
        let end = body.bytes().take_while(u8::is_ascii_alphabetic).count();
        let (name, argument) = body.split_at(end);
        if !abbreviated(name, "goto", 2) {
            return false;
        }
        let argument = argument.trim();
        let offset = if argument.is_empty() {
            Ok(prefix.unwrap_or(1))
        } else {
            argument.parse()
        };
        match offset {
            Ok(offset) => {
                self.jump_to(self.doc.position_in_file_bytes(offset.saturating_sub(1)));
            }
            Err(_) => self.ex_argument_error(text),
        }
        true
    }

    /// Dispatch local text operations in the addressed `range`, or at `current` by default.
    pub(super) fn ex_text_command(
        &mut self,
        name: &str,
        args: &str,
        range: Option<Range<usize>>,
        current: usize,
        original: &str,
        register: &Register,
    ) -> bool {
        let (bang, argument) = args
            .strip_prefix('!')
            .map_or((false, args), |argument| (true, argument));
        let argument = argument.trim_start();
        let rows = range.clone().unwrap_or(current..current + 1);
        if name.is_empty() && args.starts_with(['<', '>']) {
            self.ex_shift(rows, args);
        } else if abbreviated(name, "append", 1)
            || abbreviated(name, "insert", 1)
            || abbreviated(name, "change", 1)
        {
            if self.read_only {
                self.reject_edit();
            } else if self.ex_lines.is_some() || !argument.is_empty() && !argument.starts_with('|')
            {
                self.ex_argument_error(original);
            } else {
                let after = match name.as_bytes()[0] {
                    b'a' if self
                        .ex_address(original, current + 1)
                        .is_some_and(|(n, _)| n == 0) =>
                    {
                        0
                    }
                    b'a' => rows.end,
                    _ => rows.start,
                };
                let indent_row = if name.starts_with('i') {
                    after
                } else {
                    self.line_count().min(rows.end).saturating_sub(1)
                };
                let autoindent = self.autoindent() != bang;
                let initial =
                    autoindent.then(|| leading_indent(self.doc.line(indent_row)).to_owned());
                self.ex_line_input = Some(LineInput {
                    origin: (self.doc.opening, self.doc.revision),
                    after,
                    replace: name.starts_with('c').then_some(rows),
                    lines: Vec::new(),
                    autoindent,
                });
                self.mode = Mode::Command;
                self.prompt = ':';
                self.command = initial.unwrap_or_default();
                self.command_pos = self.command.len();
                if let Some(first) = argument.strip_prefix('|') {
                    self.accept_ex_line(first);
                }
            }
        } else if abbreviated(name, "center", 2)
            || abbreviated(name, "left", 2)
            || abbreviated(name, "right", 2)
        {
            if bang {
                self.ex_argument_error(original);
            } else {
                self.ex_align(rows, name.as_bytes()[0], argument);
            }
        } else if abbreviated(name, "iput", 2) {
            let indent_row = rows.end - 1;
            let indent = self
                .doc
                .display_width(leading_indent(self.doc.line(indent_row)), 0);
            let zero = self
                .ex_address(original, current + 1)
                .is_some_and(|(n, _)| n == 0);
            let after = if zero {
                0
            } else {
                rows.end - usize::from(bang)
            };
            self.ex_put(after, argument.trim_end(), Some(indent), register);
        } else if abbreviated(name, "uniq", 3) {
            self.ex_uniq(range.unwrap_or(0..self.line_count()), argument, bang);
        } else {
            return false;
        }
        true
    }

    /// Whether an Ex input command is waiting for a text line or its final period.
    pub fn collecting_ex_lines(&self) -> bool {
        self.ex_line_input.is_some()
    }

    /// Submit a literal Ex input line. Return false when no input command is active.
    pub fn accept_ex_line(&mut self, line: &str) -> bool {
        let Some(input) = &mut self.ex_line_input else {
            return false;
        };
        if line == "." || input.autoindent && line.trim_start_matches([' ', '\t']) == "." {
            self.finish_ex_input();
        } else {
            input.lines.push(line.replace('\0', ""));
            self.command = if input.autoindent {
                leading_indent(line).to_owned()
            } else {
                String::new()
            };
            self.command_pos = self.command.len();
        }
        true
    }

    /// Apply the entered lines and return to Normal mode, undoing the edit as one step.
    pub(in crate::vim) fn finish_ex_input(&mut self) {
        let Some(input) = self.ex_line_input.take() else {
            return;
        };
        self.mode = Mode::Normal;
        self.command.clear();
        self.command_pos = 0;
        if (self.doc.opening, self.doc.revision) != input.origin {
            self.ex_argument_error("Ex text input: buffer changed");
            return;
        }
        let mut contents = input.lines.join("\n");
        if let Some(rows) = input.replace {
            let mut span = self.line_span(rows.start, rows.end - 1);
            if span.end > self.doc.line_range(rows.end - 1).end && !input.lines.is_empty() {
                contents.push('\n');
            } else if input.lines.is_empty()
                && span.end == self.doc.text.len()
                && span.start > 0
                && !self.doc.text.ends_with('\n')
            {
                span.start -= 1;
            }
            self.set_operated_range(span.start..span.start + contents.len());
            self.replace(span, &contents, true);
            self.set_cursor(self.first_nonblank(
                (rows.start + input.lines.len().saturating_sub(1)).min(self.line_count() - 1),
            ));
        } else if !input.lines.is_empty() {
            self.put_ex_lines(input.after, contents);
        }
    }

    /// Shift all addressed rows by the repeated sign and optionally print the final row.
    fn ex_shift(&mut self, mut rows: Range<usize>, args: &str) {
        if self.read_only {
            self.reject_edit();
            return;
        }
        let sign = args.as_bytes()[0];
        let repeat = args.bytes().take_while(|&ch| ch == sign).count();
        let (count, flags) = print_flags(&args[repeat..]);
        if count == Some(0) || !flags.chars().all(|ch| "lp#".contains(ch)) {
            self.ex_argument_error(args);
            return;
        }
        if let Some(count) = count {
            rows.start = rows.end - 1;
            rows.end = rows.start.saturating_add(count).min(self.line_count());
        }
        let shift = self.shiftwidth().saturating_mul(repeat);
        self.reindent_ex_rows(rows.clone(), |columns, _| {
            if sign == b'>' {
                columns.saturating_add(shift)
            } else {
                columns.saturating_sub(shift)
            }
        });
        if !flags.is_empty() {
            self.ex_print(
                rows.end - 1..rows.end,
                flags.contains('l'),
                flags.contains('#'),
            );
        }
    }

    /// Align the content of each row using its display width, omitting leading whitespace.
    fn ex_align(&mut self, rows: Range<usize>, alignment: u8, argument: &str) {
        if self.read_only {
            self.reject_edit();
            return;
        }
        let fallback = match (alignment, self.textwidth()) {
            (b'l', _) => 0,
            (_, 0) => 80,
            (_, width) => width,
        };
        let argument = argument.trim_end();
        let width = if argument.is_empty() {
            Some(fallback)
        } else {
            argument
                .parse::<usize>()
                .ok()
                .filter(|&width| width <= MAX_COLUMNS)
        };
        let Some(width) = width else {
            self.ex_argument_error(argument);
            return;
        };
        let tabstop = self.doc.tabstop();
        let candidates = (0..tabstop.min(width.saturating_add(1))).collect::<Vec<_>>();
        self.reindent_ex_rows(rows, |_, content| {
            if alignment == b'l' {
                return width;
            }
            let content = content.trim_end_matches([' ', '\t']);
            let mut best = 0usize;
            let mut score = usize::MAX;
            for &rest in &candidates {
                let end = rest + display_width_with_tabstop(content, rest, tabstop);
                if end > width {
                    continue;
                }
                let leftover = width - end;
                let block = if alignment == b'c' {
                    leftover.saturating_sub(rest) / (2 * tabstop)
                } else {
                    leftover / tabstop
                };
                let indent = rest + block * tabstop;
                let remaining = leftover - block * tabstop;
                let distance = if alignment == b'c' {
                    indent.abs_diff(remaining)
                } else {
                    remaining
                };
                if distance < score || distance == score && indent < best {
                    best = indent;
                    score = distance;
                }
            }
            best
        });
    }

    /// Replace leading indentation using a single undo checkpoint for all rows.
    fn reindent_ex_rows(&mut self, rows: Range<usize>, columns: impl Fn(usize, &str) -> usize) {
        let revision = self.doc.revision;
        for row in rows.clone().rev() {
            let line = self.doc.line(row);
            let indent = leading_indent(line);
            if indent.len() == line.len() {
                continue;
            }
            let next = self.indentation(columns(
                self.doc.display_width(indent, 0),
                &line[indent.len()..],
            ));
            let start = self.doc.starts[row];
            self.replace(
                start..start + indent.len(),
                &next,
                self.doc.revision == revision,
            );
        }
        self.set_cursor(self.first_nonblank(rows.end - 1));
    }

    /// Rebase line indentation of a put on the indentation of the destination line.
    pub(super) fn align_put_indentation(&self, contents: &str, target: usize) -> String {
        let reference = contents
            .split('\n')
            .find(|line| !line.trim().is_empty())
            .unwrap_or("");
        let reference = self.doc.display_width(leading_indent(reference), 0);
        contents
            .split('\n')
            .map(|line| {
                let indent = leading_indent(line);
                if indent.len() == line.len() {
                    String::new()
                } else {
                    let old = self.doc.display_width(indent, 0);
                    let mut shifted =
                        self.indentation(old.saturating_add(target).saturating_sub(reference));
                    shifted.push_str(&line[indent.len()..]);
                    shifted
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Deduplicate runs of adjacent rows using the selected pattern portion and case option.
    fn ex_uniq(&mut self, rows: Range<usize>, argument: &str, repeats_only: bool) {
        if self.read_only {
            self.reject_edit();
            return;
        }
        let Some((flags, pattern)) = uniq_arguments(argument) else {
            self.ex_argument_error(argument);
            return;
        };
        let regex = match pattern
            .as_deref()
            .map(|source| {
                let source = if source.is_empty() {
                    self.search_pattern().unwrap_or("")
                } else {
                    source
                };
                if source.is_empty() {
                    return Err(i18n::text("editor.no_previous_pattern").into());
                }
                pattern::compile(source, self.pattern_case("", None))
            })
            .transpose()
        {
            Ok(regex) => regex,
            Err(error) => {
                self.pattern_error(&error);
                return;
            }
        };
        let key = |row| {
            let line = self.doc.line(row);
            let value = if let Some(regex) = &regex {
                regex.find(line).map_or("", |matched| {
                    if flags.contains('r') {
                        matched.as_str()
                    } else {
                        &line[matched.end()..]
                    }
                })
            } else {
                line
            };
            if flags.contains('i') {
                value.to_lowercase()
            } else {
                value.to_owned()
            }
        };
        let mut kept = Vec::new();
        let mut start = rows.start;
        while start < rows.end {
            let value = key(start);
            let mut end = start + 1;
            while end < rows.end {
                let next = key(end);
                let equal = if flags.contains('l') {
                    locale_equal(&value, &next)
                } else {
                    value == next
                };
                if !equal {
                    break;
                }
                end += 1;
            }
            let duplicate = end - start > 1;
            if repeats_only && duplicate || !repeats_only && (!flags.contains('u') || !duplicate) {
                kept.push(start);
            }
            start = end;
        }
        if kept.len() < rows.len() {
            self.reorder_ex_lines(rows.clone(), kept, true);
        }
        self.set_cursor(self.first_nonblank(rows.start.min(self.line_count() - 1)));
    }
}

/// Return a line's indentation without consuming its first text character.
fn leading_indent(line: &str) -> &str {
    &line[..line.len() - line.trim_start_matches([' ', '\t']).len()]
}

/// Split an optional positive count from trailing Ex printing flags.
fn print_flags(args: &str) -> (Option<usize>, &str) {
    let args = args.trim_start();
    let end = args.bytes().take_while(u8::is_ascii_digit).count();
    (args[..end].parse().ok(), args[end..].trim())
}

/// Parse :uniq flags and a single optional delimiter-paired pattern.
fn uniq_arguments(mut args: &str) -> Option<(String, Option<String>)> {
    let mut flags = String::new();
    let mut regex = None;
    loop {
        args = args.trim_start();
        let Some(first) = args.chars().next() else {
            return Some((flags, regex));
        };
        if "ilru".contains(first) {
            flags.push(first);
            args = &args[1..];
        } else if regex.is_none() && !first.is_alphanumeric() {
            let (pattern, tail) = pattern::delimited(&args[first.len_utf8()..], first)?;
            regex = Some(pattern);
            args = tail;
        } else {
            return None;
        }
    }
}

/// Compare with the process collation on Unix. Embedded NUL falls back to exact comparison.
#[cfg(unix)]
fn locale_equal(a: &str, b: &str) -> bool {
    unsafe extern "C" {
        fn strcoll(
            left: *const std::ffi::c_char,
            right: *const std::ffi::c_char,
        ) -> std::ffi::c_int;
    }
    let (Ok(left), Ok(right)) = (std::ffi::CString::new(a), std::ffi::CString::new(b)) else {
        return a == b;
    };
    // Both C strings are valid for the duration of the comparison.
    unsafe { strcoll(left.as_ptr(), right.as_ptr()) == 0 }
}

/// Compare text with the system user collation on Windows.
#[cfg(windows)]
fn locale_equal(a: &str, b: &str) -> bool {
    use windows_sys::Win32::Globalization::{CSTR_EQUAL, CompareStringEx};
    let left = a.encode_utf16().collect::<Vec<_>>();
    let right = b.encode_utf16().collect::<Vec<_>>();
    let (Ok(l), Ok(r)) = (i32::try_from(left.len()), i32::try_from(right.len())) else {
        return a == b;
    };
    if l == 0 || r == 0 {
        return a == b;
    }
    // Null locale selects the user default. Both lengths match the UTF-16 buffers.
    unsafe {
        CompareStringEx(
            std::ptr::null(),
            0,
            left.as_ptr(),
            l,
            right.as_ptr(),
            r,
            std::ptr::null(),
            std::ptr::null_mut(),
            0,
        ) == CSTR_EQUAL
    }
}

/// Use exact comparison on targets without a platform collation API.
#[cfg(not(any(unix, windows)))]
fn locale_equal(a: &str, b: &str) -> bool {
    a == b
}
