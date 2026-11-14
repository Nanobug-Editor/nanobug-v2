//! Square-bracket motions, file lookups and editor navigation requests.

mod includes;

use super::{
    Action, Mode, Register, Vim,
    hint::Continuation,
    motions::{Kind, Motion},
};
use std::ops::Range;

/// Available completions after the opening square bracket.
pub(super) const BRACKET_BACKWARD: &[Continuation] = &[
    ("[", "vim_bracket.previous_section", false),
    ("]", "vim_bracket.previous_section_end", false),
    ("( {", "vim_bracket.open_delimiter", false),
    ("/ *", "vim_bracket.comment_start", false),
    ("#", "vim_bracket.directive_start", false),
    ("'", "vim_bracket.previous_mark_line", false),
    ("`", "vim_bracket.previous_mark", false),
    ("m", "vim_hint.method_prev_start", false),
    ("M", "vim_hint.method_prev_end", false),
    ("z", "vim_hint.fold_boundary_start", false),
    ("s S", "vim_bracket.previous_spelling", false),
    ("f", "vim_bracket.file", false),
    ("i I", "vim_bracket.identifier_list", false),
    ("Ctrl+i", "vim_bracket.identifier_jump", false),
    ("Ctrl+d", "vim_bracket.define_jump", false),
    ("p P", "vim_bracket.put_before", false),
    ("Space", "vim_bracket.blank_above", false),
    ("b", "vim_bracket.previous_buffer", false),
    ("B", "vim_bracket.first_buffer", false),
    ("d", "vim_bracket.previous_diagnostic", false),
    ("D", "vim_bracket.first_diagnostic", false),
];

/// Available completions after the closing square bracket.
pub(super) const BRACKET_FORWARD: &[Continuation] = &[
    ("]", "vim_bracket.next_section", false),
    ("[", "vim_bracket.next_section_end", false),
    (") }", "vim_bracket.close_delimiter", false),
    ("/ *", "vim_bracket.comment_end", false),
    ("#", "vim_bracket.directive_end", false),
    ("'", "vim_bracket.next_mark_line", false),
    ("`", "vim_bracket.next_mark", false),
    ("m", "vim_hint.method_next_start", false),
    ("M", "vim_hint.method_next_end", false),
    ("z", "vim_hint.fold_boundary_end", false),
    ("s S", "vim_bracket.next_spelling", false),
    ("f", "vim_bracket.file", false),
    ("i I", "vim_bracket.identifier_list", false),
    ("Ctrl+i", "vim_bracket.identifier_jump", false),
    ("Ctrl+d", "vim_bracket.define_jump", false),
    ("p", "vim_bracket.put_after", false),
    ("P", "vim_bracket.put_before", false),
    ("Space", "vim_bracket.blank_below", false),
    ("b", "vim_bracket.next_buffer", false),
    ("B", "vim_bracket.last_buffer", false),
    ("d", "vim_bracket.next_diagnostic", false),
    ("D", "vim_bracket.last_diagnostic", false),
];

/// Keys that remain valid bracket motions after an operator.
pub(super) fn is_motion_hint(label: &str) -> bool {
    !matches!(
        label,
        "f" | "i I" | "Ctrl+i" | "Ctrl+d" | "p P" | "p" | "P" | "Space" | "b" | "B" | "d" | "D"
    )
}

impl Vim {
    /// Resolve a square-bracket motion from `command`, with its `count` and operator context.
    pub(super) fn bracket_motion(
        &mut self,
        command: &str,
        count: Option<usize>,
        operator: bool,
    ) -> Option<Motion> {
        let count = count.unwrap_or(1);
        let backward = command.starts_with('[');
        let cursor = self.doc.cursor;
        let row = self.doc.location(cursor).0;
        let (end, kind) = match command {
            "[(" | "[{" | "])" | "]}" => {
                let delimiter = command.chars().last()?;
                (
                    self.unmatched_delimiter(row, delimiter, backward, count)?,
                    Kind::Exclusive,
                )
            }
            "[/" | "[*" | "]/" | "]*" => (
                comment_boundary(&self.doc.text, cursor, backward, count)?,
                Kind::Exclusive,
            ),
            "[#" | "]#" => {
                let target = directive_boundary(self, row, backward, count)?;
                let column = self.doc.line(target).find('#')?;
                (self.doc.starts[target] + column, Kind::Line)
            }
            "[[" | "[]" | "][" | "]]" => {
                let brace = if matches!(command, "[[" | "]]") {
                    '{'
                } else {
                    '}'
                };
                self.section_boundary(row, backward, brace, count, operator)?
            }
            "['" | "]'" | "[`" | "]`" => {
                let linewise = command.ends_with('\'');
                self.ordered_mark(backward, linewise, count)?
            }
            "[s" | "[S" | "]s" | "]S" => (self.spelling_motion(backward, count)?, Kind::Exclusive),
            _ => return None,
        };
        self.desired_column = None;
        Some(Motion {
            end,
            kind,
            jump: true,
        })
    }

    /// Execute a non-motion bracket `command` using its `count` and active `register`.
    pub(super) fn bracket_action(
        &mut self,
        command: &str,
        count: Option<usize>,
        register: &mut Register,
    ) -> Option<Action> {
        let steps = count.unwrap_or(1);
        let forward = command.starts_with(']');
        match command {
            "[f" | "]f" => Some(self.file_at_cursor(count)),
            "[p" | "[P" | "]P" | "]p" => {
                if self.read_only {
                    return Some(self.reject_edit());
                }
                self.put_indented(register, command != "]p", steps);
                Some(Action::None)
            }
            "[ " | "] " if self.mode == Mode::Normal => {
                if self.read_only {
                    return Some(self.reject_edit());
                }
                self.add_blank_lines(forward, steps);
                Some(Action::None)
            }
            "[i" | "[I" | "]i" | "]I" | "[Tab" | "]Tab" | "[Ctrl+i" | "]Ctrl+i" | "[Ctrl+d"
            | "]Ctrl+d" => Some(self.include_lookup(command, count)),
            "[b" | "]b" | "[B" | "]B" if self.mode == Mode::Normal && !self.read_only => {
                Some(Action::BracketBuffer {
                    forward,
                    edge: command.ends_with('B'),
                    count,
                })
            }
            "[d" | "]d" | "[D" | "]D" if self.mode == Mode::Normal && !self.read_only => {
                Some(Action::BracketDiagnostic {
                    forward,
                    edge: command.ends_with('D'),
                    count: steps,
                })
            }
            "[d" | "]d" | "[D" | "]D" if !self.mode.is_visual() => {
                Some(self.include_lookup(command, count))
            }
            _ => None,
        }
    }

    /// Insert `count` blank lines below the cursor when `below` is true.
    fn add_blank_lines(&mut self, below: bool, count: usize) {
        let cursor = self.doc.cursor;
        let row = self.doc.location(cursor).0;
        let range = self.doc.line_range(row);
        let at = if below { range.end } else { range.start };
        self.replace(at..at, &"\n".repeat(count), true);
        self.set_cursor(if below { cursor } else { cursor + count });
    }

    /// Find the `count`th lowercase mark in the requested direction.
    /// `linewise` uses only marked lines and returns the first nonblank on the target.
    fn ordered_mark(&self, backward: bool, linewise: bool, count: usize) -> Option<(usize, Kind)> {
        let cursor = if linewise {
            self.doc.location(self.doc.cursor).0
        } else {
            self.doc.cursor
        };
        let mut marks = self
            .marks
            .iter()
            .filter(|(name, _)| name.is_ascii_lowercase())
            .map(|(_, &offset)| {
                let offset = self
                    .doc
                    .text
                    .floor_char_boundary(offset.min(self.doc.text.len()));
                if linewise {
                    self.doc.location(offset).0
                } else {
                    offset
                }
            })
            .filter(|&position| {
                if backward {
                    position < cursor
                } else {
                    position > cursor
                }
            })
            .collect::<Vec<_>>();
        marks.sort_unstable();
        marks.dedup();
        let position = if backward {
            marks.into_iter().rev().take(count).last()?
        } else {
            marks.into_iter().take(count).last()?
        };
        Some(if linewise {
            (self.first_nonblank(position), Kind::Line)
        } else {
            (position, Kind::Exclusive)
        })
    }

    /// Find a section from `row` using column-one `brace` markers or default nroff sections.
    fn section_boundary(
        &self,
        row: usize,
        backward: bool,
        brace: char,
        count: usize,
        operator: bool,
    ) -> Option<(usize, Kind)> {
        let last = self.line_count() - 1;
        let stop_at_close = operator && !backward && brace == '{';
        let matches = |line: &str| {
            line.starts_with(brace)
                || stop_at_close && line.starts_with('}')
                || line.starts_with('\x0c')
                || line.strip_prefix('.').is_some_and(|name| {
                    matches!(
                        name.split_whitespace().next(),
                        Some("SH" | "NH" | "H" | "HU" | "nh" | "sh")
                    )
                })
        };
        let mut remaining = count;
        let edge = if backward { 0 } else { last };
        let mut target = row;
        if row == edge && !operator && count == 1 {
            return Some((self.first_nonblank(row), Kind::Exclusive));
        }
        while target != edge {
            target = if backward { target - 1 } else { target + 1 };
            if target != edge && !matches(self.doc.line(target)) {
                continue;
            }
            remaining -= 1;
            if remaining == 0 {
                let at = if operator {
                    self.doc.starts[target]
                } else {
                    self.first_nonblank(target)
                };
                if stop_at_close && self.doc.line(target).starts_with('}') {
                    let end = self
                        .doc
                        .starts
                        .get(target + 1)
                        .copied()
                        .unwrap_or(self.doc.text.len());
                    return Some((end, Kind::Exclusive));
                }
                let at =
                    if !backward && operator && target == last && !matches(self.doc.line(target)) {
                        self.doc.line_range(target).end
                    } else {
                        at
                    };
                return Some((at, Kind::Exclusive));
            }
        }
        None
    }

    /// Find up to count unmatched delimiters from the given row in either direction.
    fn unmatched_delimiter(
        &self,
        row: usize,
        delimiter: char,
        backward: bool,
        count: usize,
    ) -> Option<usize> {
        let opposite = match delimiter {
            '(' => ')',
            ')' => '(',
            '{' => '}',
            '}' => '{',
            _ => return None,
        };
        let cursor = self.doc.cursor;
        let column = cursor - self.doc.starts[row];
        let mut depth = 0usize;
        let mut remaining = count;
        let mut target = None;
        let mut search_line = |line_number: usize| {
            let line = self.doc.line(line_number);
            let quotes = quote_spans(line);
            let cursor_quote = (line_number == row)
                .then(|| quotes.iter().position(|range| range.contains(&column)))
                .flatten();
            let mut visit = |(at, ch): (usize, char)| {
                if ch != opposite && ch != delimiter
                    || line_number == row && if backward { at >= column } else { at <= column }
                    || escaped(line, at)
                    || quotes
                        .iter()
                        .position(|range| range.contains(&at))
                        .is_some_and(|quote| Some(quote) != cursor_quote)
                {
                    return false;
                }
                if ch == opposite {
                    depth += 1;
                } else if depth != 0 {
                    depth -= 1;
                } else {
                    target = Some(self.doc.starts[line_number] + at);
                    remaining -= 1;
                }
                remaining == 0
            };
            if backward {
                line.char_indices().rev().any(&mut visit)
            } else {
                line.char_indices().any(&mut visit)
            }
        };
        if backward {
            (0..=row).rev().any(&mut search_line);
        } else {
            (row..self.line_count()).any(&mut search_line);
        }
        target
    }
}

/// Check the backslash parity immediately before the given byte in a line.
fn escaped(line: &str, at: usize) -> bool {
    line.as_bytes()[..at]
        .iter()
        .rev()
        .take_while(|&&byte| byte == b'\\')
        .count()
        % 2
        != 0
}

/// Recognize paired double-quoted strings and ordinary character literals in a line.
fn quote_spans(line: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut double_quote = None;
    let mut chars = line.char_indices();
    while let Some((at, ch)) = chars.next() {
        if !matches!(ch, '"' | '\'') || escaped(line, at) {
            continue;
        }
        if ch == '"' {
            if let Some(start) = double_quote.take() {
                ranges.push(start..at + 1);
            } else {
                double_quote = Some(at);
            }
        } else if double_quote.is_none() {
            let mut lookahead = chars.clone();
            if lookahead.next().is_some_and(|(_, ch)| ch == '\\') {
                lookahead.next();
            }
            if let Some((end, '\'')) = lookahead.next() {
                ranges.push(at..end + 1);
                chars = lookahead;
            }
        }
    }
    ranges
}
/// Find the `count`th block comment start before `cursor` or end after it.
fn comment_boundary(text: &str, cursor: usize, backward: bool, count: usize) -> Option<usize> {
    if backward {
        text.rmatch_indices("/*")
            .filter(|&(at, _)| at + 1 < cursor)
            .take(count)
            .last()
            .map(|(at, _)| at)
    } else {
        text.match_indices("*/")
            .filter(|&(at, _)| at > cursor)
            .take(count)
            .last()
            .map(|(at, _)| at + 1)
    }
}

/// Find a conditional preprocessor boundary from `row`, skipping complete nested blocks.
fn directive_boundary(vim: &Vim, row: usize, backward: bool, count: usize) -> Option<usize> {
    let mut row = row;
    let mut target = None;
    let mut depth = 0usize;
    let mut remaining = count;
    let edge = if backward { 0 } else { vim.line_count() - 1 };
    while row != edge {
        row = if backward { row - 1 } else { row + 1 };
        let Some(directive) = vim.doc.line(row).trim_start().strip_prefix('#') else {
            continue;
        };
        let keyword = directive
            .trim_start()
            .split(|ch: char| !ch.is_alphanumeric() && ch != '_')
            .next()?;
        let start = matches!(keyword, "if" | "ifdef" | "ifndef");
        let end = keyword == "endif";
        if if backward { end } else { start } {
            depth += 1;
        } else if if backward { start } else { end } {
            if depth > 0 {
                depth -= 1;
            } else {
                target = Some(row);
                remaining -= 1;
            }
        } else if depth == 0 && matches!(keyword, "else" | "elif" | "elifdef" | "elifndef") {
            target = Some(row);
            remaining -= 1;
        }
        if remaining == 0 {
            break;
        }
    }
    target
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;

    /// Enter a command and verify the resulting document position.
    fn jump(vim: &mut Vim, register: &mut Register, keys: &str, row: usize, col: usize) {
        vim.text(keys, register);
        assert_eq!(vim.doc.location(vim.doc.cursor), (row, col), "{keys}");
    }

    #[test]
    fn bracket_delimiters_and_comment_boundaries_follow_neovim() {
        let mut register = Register::default();
        let mut vim = Vim::new(Document::new("text\n{\n  (one (item)\n    tail)\n}\nlast"));
        vim.set_cursor(vim.doc.offset(2, 9));
        jump(&mut vim, &mut register, "2[(", 2, 2);
        jump(&mut vim, &mut register, "])", 3, 8);
        jump(&mut vim, &mut register, "[{", 1, 0);
        jump(&mut vim, &mut register, "]}", 4, 0);
        let mut vim = Vim::new(Document::new(
            "f(a\n \"literal (\"\n  body)\n/* first */\ntext\n/* last */",
        ));
        vim.set_cursor(vim.doc.offset(2, 3));
        jump(&mut vim, &mut register, "[(", 0, 1);
        jump(&mut vim, &mut register, "]/", 3, 10);
        jump(&mut vim, &mut register, "]*", 5, 9);
        jump(&mut vim, &mut register, "2[/", 3, 0);
    }

    #[test]
    fn section_and_directive_brackets_preserve_counts_and_operator_shapes() {
        let mut register = Register::default();
        let mut vim = Vim::new(Document::new(
            "#if OUTER\nfirst\n#if INNER\nother\n#else\nbody\n#endif\n #elif LAST\nouter\n #endif",
        ));
        vim.set_cursor(vim.doc.offset(3, 1));
        jump(&mut vim, &mut register, "2[#", 0, 0);
        jump(&mut vim, &mut register, "]#", 7, 1);
        jump(&mut vim, &mut register, "]#", 9, 1);
        let mut vim = Vim::new(Document::new(
            "func\n{\n  prior\n   next\n} trailing\nafter",
        ));
        vim.set_cursor(vim.doc.offset(3, 4));
        jump(&mut vim, &mut register, "d]]", 3, 3);
        assert_eq!(vim.doc.text, "func\n{\n  prior\n   n\nafter");
        jump(&mut vim, &mut register, "[[", 1, 0);
        jump(&mut vim, &mut register, "]]", 4, 0);
    }

    #[test]
    fn bracket_marks_blank_lines_and_spelling_use_the_active_buffer() {
        let mut register = Register::default();
        let mut vim = Vim::new(Document::new("alpha\n  beta\ngamma"));
        vim.set_cursor(vim.doc.offset(1, 4));
        vim.text("ma", &mut register);
        vim.set_cursor(vim.doc.offset(2, 2));
        jump(&mut vim, &mut register, "[`", 1, 4);
        jump(&mut vim, &mut register, "3[ ", 4, 4);
        assert_eq!(vim.doc.text, "alpha\n\n\n\n  beta\ngamma");
        jump(&mut vim, &mut register, "] ", 4, 4);
        assert_eq!(vim.doc.text, "alpha\n\n\n\n  beta\n\ngamma");
        let mut vim = Vim::new(Document::new("only teh words\nanother erorr"));
        jump(&mut vim, &mut register, "]s", 0, 5);
        jump(&mut vim, &mut register, "]S", 1, 8);
        jump(&mut vim, &mut register, "[s", 0, 5);
    }
}
