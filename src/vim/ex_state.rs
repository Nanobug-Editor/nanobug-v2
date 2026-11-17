//! Ex commands for registers, marks, character information, and explicit Shell execution.

use super::{Action, Register, Vim, insert_argument::default_digraphs};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    ops::Range,
};
use unicode_segmentation::UnicodeSegmentation;

/// Commands dispatched before the line-edit and workspace command tables.
pub(super) const NAMES: &[&str] = &["=", "@", "ascii", "delmarks", "digraphs", "k", "mark", "!"];

impl Vim {
    /// Clear a special mark without changing its underlying selection or change history.
    fn hide_ex_mark(&mut self, mark: char) {
        self.ex_hidden_marks.insert(mark);
    }

    /// Record the range of a completed text operation and expose its new special marks.
    pub(super) fn set_operated_range(&mut self, range: Range<usize>) {
        self.operated = Some(range);
        self.ex_hidden_marks.remove(&'[');
        self.ex_hidden_marks.remove(&']');
    }

    /// Apply a character or state command, delegating native input and Shell actions to the host.
    pub(super) fn ex_state_command(
        &mut self,
        text: &str,
        register: &mut Register,
    ) -> Option<Action> {
        let (range, body) = self.ex_range(text.trim())?;
        if let Some(args) = body.strip_prefix('=') {
            if !args.trim().is_empty() {
                return Some(self.state_ex_error("ex.number_arguments"));
            }
            let current = self.doc.location(self.doc.cursor).0 + 1;
            let zero = self
                .ex_address(text, current)
                .is_some_and(|(line, rest)| line == 0 && rest.trim() == body);
            let number = if zero {
                0
            } else {
                range.map_or_else(|| self.line_count(), |range| range.end)
            };
            self.message = number.to_string();
            return Some(Action::None);
        }
        if let Some(args) = body.strip_prefix('@') {
            return Some(self.ex_register(args.trim(), range, register));
        }
        if let Some(command) = body.strip_prefix('!') {
            return Some(self.ex_shell(command.trim_start(), range));
        }
        let (name, args) = body.split_once(char::is_whitespace).unwrap_or((body, ""));
        let args = args.trim();
        if name.starts_with('k') && (name == "k" || name.chars().count() == 2) {
            let mark = if name == "k" {
                args
            } else if args.is_empty() {
                &name[1..]
            } else {
                ""
            };
            return Some(self.ex_set_mark(mark, range, register));
        }
        let (name, bang) = name
            .strip_suffix('!')
            .map_or((name, false), |name| (name, true));
        if name.len() >= 2 && "mark".starts_with(name) && !bang {
            return Some(self.ex_set_mark(args, range, register));
        }
        if range.is_some() {
            return None;
        }
        if name.len() >= 2 && "ascii".starts_with(name) && !bang && args.is_empty() {
            self.ex_ascii(register);
            return Some(Action::None);
        }
        if name.len() >= 4 && "delmarks".starts_with(name) {
            return Some(self.ex_delete_marks(args, bang, register));
        }
        if name.len() >= 3 && "digraphs".starts_with(name) {
            return Some(self.ex_digraphs(args, bang, register));
        }
        None
    }

    /// Check whether an Ex register invocation repeats the previous command line.
    pub(super) fn repeats_previous_ex(&self, text: &str, register: &Register) -> bool {
        self.ex_range(text.trim())
            .and_then(|(_, body)| body.strip_prefix('@'))
            .is_some_and(|args| {
                let name = args.trim();
                name == ":" || matches!(name, "" | "@") && register.last_ex_register == Some(':')
            })
    }

    /// Show the code points under the cursor in the same three bases as :ascii.
    fn ex_ascii(&mut self, register: &Register) {
        let Some(text) = self.doc.text[self.doc.cursor..]
            .graphemes(true)
            .next()
            .filter(|t| !t.contains('\n'))
        else {
            self.message = "NUL".into();
            return;
        };
        self.message = text
            .chars()
            .map(|character| {
                let number = u32::from(character);
                let display = match character {
                    '\0'..='\u{1f}' => format!("^{}", char::from_u32(number + 64).unwrap()),
                    '\u{7f}' => "^?".into(),
                    value => value.to_string(),
                };
                let mut result =
                    format!("<{display}>  {number},  Hex {number:02x},  Octal {number:03o}");
                let pair = register
                    .input_maps
                    .digraphs(|table| {
                        table
                            .iter()
                            .find(|(_, value)| **value == character)
                            .map(|(pair, _)| *pair)
                    })
                    .or_else(|| {
                        default_digraphs()
                            .find_map(|(pair, value)| (value == character).then_some(pair))
                    });
                if let Some([a, b]) = pair {
                    let _ = write!(result, ", Digr {a}{b}");
                }
                result
            })
            .collect::<Vec<_>>()
            .join(" ");
    }

    /// Set a mark at the first nonblank on the addressed line without moving the caret.
    fn ex_set_mark(
        &mut self,
        mark: &str,
        range: Option<Range<usize>>,
        register: &mut Register,
    ) -> Action {
        let Some(mark) =
            one_char(mark).filter(|ch| ch.is_ascii_alphabetic() || matches!(ch, '\'' | '`'))
        else {
            return self.state_ex_error("ex.mark_invalid");
        };
        let cursor = self.doc.cursor;
        let row = range.map_or_else(
            || self.doc.location(cursor).0,
            |range| range.end.saturating_sub(1),
        );
        self.doc.cursor = self.first_nonblank(row);
        self.set_command_mark(mark, register);
        self.doc.cursor = cursor;
        Action::None
    }

    /// Remove named or local marks, returning numbered marks for the portable history owner.
    fn ex_delete_marks(&mut self, selection: &str, all: bool, register: &mut Register) -> Action {
        if all && selection.is_empty() {
            self.marks.clear();
            self.operated = None;
            self.g.clear_mark_history();
            return Action::None;
        }
        let Some(names) = mark_list(selection).filter(|_| !all) else {
            return self.state_ex_error("ex.mark_invalid");
        };
        let mut numbered = Vec::new();
        for name in names {
            if name.is_ascii_uppercase() {
                register.file_marks.remove(name);
            } else if let Some(number) = name.to_digit(10) {
                numbered.push(number as usize);
            } else {
                self.marks.remove(&name);
                match name {
                    '^' => self.g.insertion = None,
                    '.' | '[' | ']' | '<' | '>' => self.hide_ex_mark(name),
                    _ => {}
                }
            }
        }
        if numbered.is_empty() {
            Action::None
        } else {
            Action::DeleteNumberedMarks(numbered)
        }
    }

    /// List or define digraphs, using the workspace overrides ahead of Nanobug's default table.
    fn ex_digraphs(&mut self, args: &str, headers: bool, register: &Register) -> Action {
        if args.is_empty() {
            let user = register.input_maps.digraphs(Clone::clone);
            let mut defaults = default_digraphs().collect::<BTreeMap<_, _>>();
            for keys in user.keys() {
                defaults.remove(keys);
            }
            let mut output = String::new();
            if headers {
                output.push_str("Default digraphs\n");
            }
            append_digraph_list(&mut output, defaults);
            if headers && !user.is_empty() {
                output.push_str("User digraphs\n");
            }
            append_digraph_list(&mut output, user);
            return Action::ShowListing(output);
        }
        let mut words = args.split_whitespace();
        let mut definitions = Vec::new();
        while let Some(pair) = words.next() {
            let mut chars = pair.chars();
            let Some([a, b]) = chars
                .next()
                .zip(chars.next())
                .filter(|_| chars.next().is_none())
                .map(|(a, b)| [a, b])
            else {
                return self.state_ex_error("ex.digraph_invalid");
            };
            let Some(value) = words
                .next()
                .and_then(|word| word.parse::<u32>().ok())
                .and_then(char::from_u32)
                .filter(|&ch| ch != '\0')
            else {
                return self.state_ex_error("ex.digraph_invalid");
            };
            definitions.push(([a, b], value));
        }
        register.input_maps.define_digraphs(definitions);
        Action::None
    }

    /// Evaluate one register as Ex statements at the specified address.
    fn ex_register(
        &mut self,
        name: &str,
        range: Option<Range<usize>>,
        register: &mut Register,
    ) -> Action {
        let selected = if matches!(name, "" | "@") {
            register.last_ex_register
        } else {
            one_char(name)
        };
        let Some(selected) = selected.filter(|ch| {
            ch.is_ascii_alphanumeric() || matches!(ch, '"' | '.' | '=' | '*' | '+' | ':')
        }) else {
            return self.state_ex_error("ex.register_invalid");
        };
        if let Some(lines) = range {
            let (_, column) = self.doc.location(self.doc.cursor);
            let row = lines.end.saturating_sub(1);
            self.set_cursor(self.doc.offset(row, column));
        }
        let text = match selected {
            '*' | '+' => {
                register.last_ex_register = Some(selected);
                return Action::ClipboardEx;
            }
            '=' if !register.expression_source.is_empty() => {
                match self.evaluate_expression(&register.expression_source, register) {
                    Ok(value) => value.display(),
                    Err(error) => {
                        self.script_error(&error);
                        return Action::None;
                    }
                }
            }
            _ => register
                .macro_keys(selected)
                .iter()
                .map(super::replay::Stroke::register_text)
                .collect(),
        };
        if text.is_empty() {
            return self.state_ex_error("ex.register_empty");
        }
        register.last_ex_register = Some(selected);
        Action::RegisterEx(text)
    }

    /// Resolve an explicit Shell command or ranged filter, expanding unescaped previous commands.
    fn ex_shell(&mut self, input: &str, range: Option<Range<usize>>) -> Action {
        let mut command = String::new();
        let mut chars = input.chars().peekable();
        while let Some(ch) = chars.next() {
            match ch {
                '\\' if chars.peek() == Some(&'!') => {
                    chars.next();
                    command.push('!');
                }
                '!' if self.last_filter.is_empty() => {
                    return self.state_ex_error("ex.no_previous_shell");
                }
                '!' => command.push_str(&self.last_filter),
                _ => command.push(ch),
            }
        }
        if command.trim().is_empty() {
            return self.state_ex_error("ex.empty_shell");
        }
        if range.is_some() && self.is_read_only() {
            return self.reject_edit();
        }
        self.last_filter.clone_from(&command);
        if let Some(lines) = range {
            return Action::Filter {
                range: self.line_span(lines.start, lines.end.saturating_sub(1)),
                command,
            };
        }
        Action::Shell(command)
    }

    /// Report a localized diagnostic without requiring a workspace action.
    fn state_ex_error(&mut self, key: &'static str) -> Action {
        self.ex_failed = true;
        self.message = crate::i18n::text(key).into();
        Action::None
    }
}

/// Read exactly one character from an Ex argument.
fn one_char(text: &str) -> Option<char> {
    let mut chars = text.chars();
    chars.next().filter(|_| chars.next().is_none())
}

/// Expand mark letters, digits, punctuation and like-kind ascending ranges.
fn mark_list(text: &str) -> Option<BTreeSet<char>> {
    let mut marks = BTreeSet::new();
    let mut chars = text.chars().filter(|ch| !ch.is_whitespace()).peekable();
    while let Some(start) = chars.next() {
        if chars.peek() == Some(&'-') {
            chars.next();
            let end = chars.next()?;
            if start > end
                || !((start.is_ascii_lowercase() && end.is_ascii_lowercase())
                    || (start.is_ascii_uppercase() && end.is_ascii_uppercase())
                    || (start.is_ascii_digit() && end.is_ascii_digit()))
            {
                return None;
            }
            marks.extend((start as u8..=end as u8).map(char::from));
        } else if start.is_ascii_alphanumeric()
            || matches!(start, '^' | '.' | '[' | ']' | '<' | '>' | '"')
        {
            marks.insert(start);
        } else {
            return None;
        }
    }
    (!marks.is_empty()).then_some(marks)
}

/// Add one line per digraph and Unicode scalar to the command listing.
fn append_digraph_list(output: &mut String, entries: BTreeMap<[char; 2], char>) {
    for ([a, b], value) in entries {
        let _ = writeln!(output, "{a}{b}  {value}  {}", u32::from(value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;

    /// Check the agreed inventory at the actual completion and Ex dispatch boundaries.
    #[test]
    fn all_97_ex_entries_are_offered_and_dispatched() {
        let inventory = include_str!("../../docs/research/2026-11-17-neovim-ex-compatibility.md");
        let words: Vec<_> = inventory
            .lines()
            .filter(|line| line.starts_with("- **可直接接入（"))
            .flat_map(|line| line.split('`').skip(1).step_by(2))
            .filter_map(|entry| entry.strip_prefix(':'))
            .collect();
        assert_eq!(words.len(), 97);
        assert_eq!(words.iter().copied().collect::<BTreeSet<_>>().len(), 97);
        let vim = Vim::new(Document::new("example\nsecond\n"));
        let register = Register::default();
        let (_, names) = vim
            .command_candidates("", &register)
            .expect("Ex completion");
        for word in words {
            assert!(
                names.iter().any(|name| name == word),
                "missing completion for :{word}"
            );
            let mut vim = Vim::new(Document::new("sample\nsecond\n"));
            let mut register = Register::default();
            let line = match word {
                "!" => "!echo test".into(),
                "@" => "@a".into(),
                "k" | "mark" | "delmarks" => format!("{word} a"),
                "delfunction" => "delfunction! Test".into(),
                "eval" | "echoerr" | "echon" => format!("{word} 'example'"),
                "find" | "sfind" | "tabfind" => format!("{word} example.txt"),
                "ijump" | "ilist" | "isearch" | "isplit" | "psearch" | "djump" | "dlist"
                | "dsearch" | "dsplit" => format!("{word} token"),
                "sleep" => "sleep 1m".into(),
                _ => word.to_owned(),
            };
            let unsupported =
                crate::i18n::format("editor.unsupported_command", &[("command", &line)]);
            vim.execute_ex_line(&line, &mut register);
            assert_ne!(vim.message, unsupported, "unrouted :{line}");
        }
    }

    /// A ranged Ex mark stores the first nonblank without moving the current cursor.
    #[test]
    fn ex_marks_and_line_numbers() {
        let mut vim = Vim::new(Document::new("first\n  second\n"));
        let mut register = Register::default();
        vim.execute_ex_line("2ka", &mut register);
        assert_eq!(vim.doc.cursor, 0);
        vim.text("`a", &mut register);
        assert_eq!(vim.doc.location(vim.doc.cursor), (1, 2));
        vim.execute_ex_line("delmarks a", &mut register);
        assert!(vim.mark_motion('a', false).is_none());
        vim.execute_ex_line("1,2=", &mut register);
        assert_eq!(vim.message, "2");
        vim.execute_ex_line("ascii", &mut register);
        assert!(vim.message.contains("<s>  115,  Hex 73,  Octal 163"));
    }

    /// A defined digraph affects the standard Ctrl+k insertion route in another buffer.
    #[test]
    fn ex_digraphs_override_default_insert_lookup() {
        let mut vim = Vim::new(Document::new(""));
        let mut register = Register::default();
        vim.execute_ex_line("digraphs ab 9731", &mut register);
        let mut other = Vim::new(Document::new(""));
        other.key("i", &mut register);
        other.key("Ctrl+k", &mut register);
        other.text("ab", &mut register);
        assert_eq!(other.doc.text, "☃");
    }

    /// Detect standalone and ranged Shell forms without starting a process in the unit test.
    #[test]
    fn ex_shell_resolves_filters_and_repeats() {
        let mut vim = Vim::new(Document::new("one\ntwo\n"));
        let mut register = Register::default();
        assert_eq!(
            vim.execute_ex_line("!echo hello", &mut register),
            Action::Shell("echo hello".into())
        );
        assert!(
            matches!(vim.execute_ex_line("1!!", &mut register), Action::Filter { command, .. } if command == "echo hello")
        );
    }

    /// Evaluate or delete local script content without an external scripting host.
    #[test]
    fn script_helpers_share_the_rust_evaluator() {
        let mut vim = Vim::new(Document::new(""));
        let mut register = Register::default();
        vim.execute_ex_line("function! Test() | echo 'old' | endfunction", &mut register);
        vim.execute_ex_line("call Test()", &mut register);
        assert_eq!(vim.message, "old");
        vim.execute_ex_line("delfunction Test", &mut register);
        vim.execute_ex_line("call Test()", &mut register);
        assert!(vim.ex_failed());
        vim.execute_ex_line("echon 'one' | echon 'two'", &mut register);
        assert_eq!(vim.message, "onetwo");
        vim.execute_ex_line("eval 3 + 4", &mut register);
        assert!(vim.message.is_empty() && !vim.ex_failed());
        vim.execute_ex_line("echoerr 'error'", &mut register);
        assert!(vim.message.contains("error") && vim.ex_failed());
    }

    /// Repeating command-line history prepares the preceding Ex line instead of recursing.
    #[test]
    fn ex_register_repeats_previous_prompt_and_preserves_addresses() {
        let mut vim = Vim::new(Document::new("first\nsecond\n"));
        let mut register = Register::default();
        vim.text(":echo 7", &mut register);
        vim.key("Enter", &mut register);
        vim.text(":2@:", &mut register);
        assert_eq!(
            vim.key("Enter", &mut register),
            Action::RegisterEx("echo 7".into())
        );
        assert_eq!(vim.doc.location(vim.doc.cursor).0, 1);
        assert_eq!(
            vim.execute_ex_line("@@", &mut register),
            Action::RegisterEx("echo 7".into())
        );
    }
}
