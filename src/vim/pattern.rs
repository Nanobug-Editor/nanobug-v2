//! Vim-style search patterns and line-addressed substitutions using Rust regex.

use super::{
    Register, Vim,
    motions::{Kind, Motion},
};
use crate::i18n;
use regex::{Captures, Regex, RegexBuilder};
use std::ops::Range;

/// Last confirmed search, including the direction of its prompt.
pub(super) struct Search {
    /// User-entered pattern used in feedback and empty-pattern substitutions.
    text: String,
    /// Compiled pattern, kept for n and N.
    regex: Regex,
    /// Whether n searches backward.
    backward: bool,
}

/// Last substitution, kept separately from the latest search.
#[derive(Clone)]
pub(super) struct Substitution {
    /// Pattern and replacement as entered in the prompt.
    pattern: String,
    replacement: String,
    /// Replace every match per line, instead of the first.
    global: bool,
    /// Explicit case setting from an Ex flag.
    case: Option<bool>,
    /// Suppress a missing-match error.
    quiet: bool,
    /// Count matches without applying edits.
    count_only: bool,
}

impl Vim {
    /// Most recent successfully compiled search pattern for the slash register.
    pub(super) fn search_pattern(&self) -> Option<&str> {
        self.last_search.as_ref().map(|search| search.text.as_str())
    }

    /// Compile the search prompt `text`, then move or apply the queued operator through `register`.
    pub(super) fn confirm_search(&mut self, text: &str, register: &mut Register) {
        let operator = self.search_operator.take();
        if !text.is_empty() {
            if !self.set_search(text, self.prompt == '?') {
                return;
            }
        } else if let Some(search) = &mut self.last_search {
            search.backward = self.prompt == '?';
        }
        if let Some(operator) = operator {
            if let Some(motion) = self.search_motion(false, self.search_count) {
                self.operate_motion(operator, motion, register);
            }
        } else {
            self.search(false, self.search_count);
        }
    }

    /// Use the word at or after the caret as a whole-word pattern.
    pub(super) fn search_word(&mut self, backward: bool, count: usize) {
        self.search_word_with_boundaries(backward, count, true);
    }

    /// Search from the word at the cursor, using `boundaries` to select whole-word matching.
    pub(super) fn search_word_with_boundaries(
        &mut self,
        backward: bool,
        count: usize,
        boundaries: bool,
    ) {
        let Some(range) = self.keyword() else {
            return;
        };
        let word = &self.doc.text[range.clone()];
        let text = if boundaries {
            format!("\\<{word}\\>")
        } else {
            word.to_owned()
        };
        if self.set_search(&text, backward) {
            self.doc.cursor = range.start;
            self.search(false, count);
        }
    }

    /// Find the identifier-like word under or after the cursor on the same line.
    pub(super) fn keyword(&self) -> Option<Range<usize>> {
        let cursor = self.doc.cursor;
        let row = self.doc.location(cursor).0;
        let range = self.doc.line_range(row);
        let mut start = cursor;
        while start < range.end && self.class(start, false) != 1 {
            start = self.doc.next(start);
        }
        if start == range.end {
            return None;
        }
        while start > range.start && self.class(self.doc.previous(start), false) == 1 {
            start = self.doc.previous(start);
        }
        let mut end = start;
        while end < range.end && self.class(end, false) == 1 {
            end = self.doc.next(end);
        }
        Some(start..end)
    }

    /// Resolve the counted saved match in the absolute `backward` or forward direction.
    pub(super) fn search_span(&self, backward: bool, count: usize) -> Option<Range<usize>> {
        let search = self.last_search.as_ref()?;
        let cursor = self.doc.cursor;
        let hits = search
            .regex
            .find_iter(&self.doc.text)
            .filter(|hit| !hit.is_empty())
            .map(|hit| hit.range())
            .collect::<Vec<_>>();
        let len = hits.len();
        if len == 0 {
            return None;
        }
        let visual = self.mode.is_visual();
        let index = if backward {
            let edge = hits.partition_point(|range| {
                if visual {
                    range.end <= cursor
                } else {
                    range.start <= cursor
                }
            });
            (edge + len - count % len) % len
        } else {
            let edge = hits.partition_point(|range| {
                if visual {
                    self.doc.previous(range.end) <= cursor
                } else {
                    range.end <= cursor
                }
            });
            (edge + (count - 1) % len) % len
        };
        Some(hits[index].clone())
    }

    /// Compile and remember `text` with its `backward` direction, leaving the prior search on error.
    fn set_search(&mut self, text: &str, backward: bool) -> bool {
        match compile(text, None) {
            Ok(regex) => {
                self.last_search = Some(Search {
                    text: text.into(),
                    regex,
                    backward,
                });
                true
            }
            Err(error) => {
                self.pattern_error(&error);
                false
            }
        }
    }

    /// Move to a match of the saved search, reversing its direction for N and wrapping at EOF.
    pub(super) fn search(&mut self, reverse: bool, count: usize) {
        if let Some(motion) = self.search_motion(reverse, count) {
            self.doc
                .set_search_fold_row(Some(self.doc.location(motion.end).0));
            self.apply_motion(motion);
        }
    }

    /// Find the saved pattern's counted match with optional `reverse` direction.
    pub(super) fn search_motion(&mut self, reverse: bool, count: usize) -> Option<Motion> {
        let search = self.last_search.as_ref()?;
        let backward = search.backward ^ reverse;
        let cursor = self.doc.cursor;
        let matches: Vec<_> = search
            .regex
            .find_iter(&self.doc.text)
            .map(|hit| hit.start())
            .collect();
        if matches.is_empty() {
            self.message = i18n::format("editor.search_not_found", &[("query", &search.text)]);
            return None;
        }
        let len = matches.len();
        let index = if backward {
            (matches.partition_point(|&at| at < cursor) + len - count % len) % len
        } else {
            (matches.partition_point(|&at| at <= cursor) + (count - 1) % len) % len
        };
        self.message = format!("{}{}", if backward { '?' } else { '/' }, search.text);
        self.desired_column = None;
        Some(Motion {
            end: matches[index],
            kind: Kind::Exclusive,
            jump: true,
        })
    }

    /// Repeat the previous substitution on the current line, retaining its flags.
    pub(super) fn repeat_substitution(&mut self) {
        let row = self.doc.location(self.doc.cursor).0;
        if let Some(previous) = self.last_substitution.clone() {
            self.substitute(row..row + 1, previous);
        }
    }

    /// Repeat the previous substitution across the buffer, preserving its flags.
    pub(super) fn repeat_substitution_all(&mut self) {
        if let Some(mut previous) = self.last_substitution.clone() {
            if let Some(search) = &self.last_search {
                previous.pattern.clone_from(&search.text);
            }
            self.substitute(0..self.line_count(), previous);
        }
    }

    /// Parse Ex substitution `text`, returning whether it names a substitution.
    pub(super) fn substitute_command(&mut self, text: &str) -> bool {
        let current = self.doc.location(self.doc.cursor).0;
        let Some((lines, rest)) = parse_range(text, current, self.line_count()) else {
            return false;
        };
        let Some(args) = rest
            .strip_prefix("substitute")
            .or_else(|| rest.strip_prefix('s'))
        else {
            return false;
        };
        if args.starts_with(char::is_alphabetic) {
            return false;
        }
        let args = args.trim_start();
        if args.is_empty() {
            if let Some(previous) = self.last_substitution.clone() {
                self.substitute(
                    lines,
                    Substitution {
                        global: false,
                        quiet: false,
                        count_only: false,
                        ..previous
                    },
                );
            }
            return true;
        }
        let delimiter = args.chars().next().unwrap();
        if delimiter.is_alphanumeric() || matches!(delimiter, '\\' | '"' | '|') {
            return false;
        }
        let Some((pattern, tail)) = delimited(&args[delimiter.len_utf8()..], delimiter) else {
            self.pattern_error("Missing substitution delimiter");
            return true;
        };
        let (replacement, flags) =
            delimited(tail, delimiter).unwrap_or_else(|| (tail.to_owned(), ""));
        let flags = flags.trim();
        if flags
            .chars()
            .any(|ch| !matches!(ch, 'g' | 'i' | 'I' | 'e' | 'n'))
        {
            self.message = i18n::format("editor.unsupported_command", &[("command", &text)]);
            return true;
        }
        let pattern = if pattern.is_empty() {
            self.last_search
                .as_ref()
                .map_or_else(String::new, |search| search.text.clone())
        } else {
            pattern
        };
        if pattern.is_empty() {
            return true;
        }
        self.substitute(
            lines,
            Substitution {
                pattern,
                replacement,
                global: flags.contains('g'),
                case: if flags.contains('i') {
                    Some(true)
                } else {
                    flags.contains('I').then_some(false)
                },
                quiet: flags.contains('e'),
                count_only: flags.contains('n'),
            },
        );
        true
    }

    /// Replace the first or all matches on each addressed line, as one undo transaction.
    fn substitute(&mut self, rows: Range<usize>, command: Substitution) {
        if self.read_only && !command.count_only {
            self.reject_edit();
            return;
        }
        let regex = match compile(&command.pattern, command.case) {
            Ok(regex) => regex,
            Err(error) => {
                self.pattern_error(&error);
                return;
            }
        };
        self.last_search = Some(Search {
            text: command.pattern.clone(),
            regex: regex.clone(),
            backward: false,
        });
        self.last_substitution = Some(command.clone());
        let mut count = 0;
        let mut edits = Vec::new();
        for row in rows {
            let line = self.doc.line(row);
            let mut text = String::new();
            let mut at = 0;
            for captures in
                regex
                    .captures_iter(line)
                    .take(if command.global { usize::MAX } else { 1 })
            {
                count += 1;
                if !command.count_only {
                    let hit = captures.get(0).unwrap();
                    text.push_str(&line[at..hit.start()]);
                    expand_replacement(&command.replacement, &captures, &mut text);
                    at = hit.end();
                }
            }
            if at != 0 || !text.is_empty() {
                text.push_str(&line[at..]);
                edits.push((row, self.doc.line_range(row), text));
            }
        }
        if count == 0 && !command.quiet {
            self.message = i18n::format("editor.search_not_found", &[("query", &command.pattern)]);
        }
        if command.count_only {
            self.message = i18n::format("editor.matches", &[("count", &count)]);
        }
        let cursor = edits.last().map(|(row, _, _)| *row);
        let revision = self.doc.revision;
        for (_, range, text) in edits.into_iter().rev() {
            self.replace(range, &text, self.doc.revision == revision);
        }
        if let Some(row) = cursor {
            self.set_cursor(self.first_nonblank(row.min(self.line_count() - 1)));
        }
    }

    /// Report a pattern `error` in the status line without changing document text.
    fn pattern_error(&mut self, error: &str) {
        self.message = i18n::format("editor.invalid_pattern", &[("error", &error)]);
    }
}

/// Parse percent, current line, last line or numeric line addresses before an Ex command.
fn parse_range(text: &str, current: usize, count: usize) -> Option<(Range<usize>, &str)> {
    if let Some(rest) = text.strip_prefix('%') {
        return Some((0..count, rest.trim_start()));
    }
    let Some((first, rest)) = address(text, current, count) else {
        return Some((current..current + 1, text));
    };
    let (last, rest) = if let Some(rest) = rest.trim_start().strip_prefix([',', ';']) {
        address(rest.trim_start(), current, count)?
    } else {
        (first, rest)
    };
    if first > last || last >= count {
        return None;
    }
    Some((first..last + 1, rest.trim_start()))
}

/// Parse one one-based Ex line address and return its zero-based row.
fn address(text: &str, current: usize, count: usize) -> Option<(usize, &str)> {
    if let Some(rest) = text.strip_prefix('.') {
        return Some((current, rest));
    }
    if let Some(rest) = text.strip_prefix('$') {
        return Some((count - 1, rest));
    }
    let end = text.bytes().take_while(u8::is_ascii_digit).count();
    let row = text[..end].parse::<usize>().ok()?.saturating_sub(1);
    Some((row, &text[end..]))
}

/// Read one delimiter-terminated field, unescaping only its delimiter.
fn delimited(text: &str, delimiter: char) -> Option<(String, &str)> {
    let mut field = String::new();
    let mut chars = text.char_indices().peekable();
    while let Some((at, ch)) = chars.next() {
        if ch == delimiter {
            return Some((field, &text[at + ch.len_utf8()..]));
        }
        if ch == '\\'
            && let Some((_, next)) = chars.next()
        {
            if next != delimiter {
                field.push(ch);
            }
            field.push(next);
        } else {
            field.push(ch);
        }
    }
    None
}

/// Expand &, numbered groups and escaped replacement characters for one match.
fn expand_replacement(replacement: &str, captures: &Captures<'_>, out: &mut String) {
    let mut chars = replacement.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '&' => out.push_str(&captures[0]),
            '\\' => match chars.next() {
                Some(ch @ '0'..='9') => {
                    if let Some(group) = captures.get((ch as u8 - b'0') as usize) {
                        out.push_str(group.as_str());
                    }
                }
                Some('r' | 'n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(ch) => out.push(ch),
                None => out.push('\\'),
            },
            _ => out.push(ch),
        }
    }
}

/// How unescaped metacharacters are interpreted in the supported Vim pattern syntax.
#[derive(Clone, Copy)]
enum Syntax {
    /// Vim's default magic mode.
    Normal,
    /// Only anchors are special without a backslash.
    NoMagic,
    /// Common regex punctuation is special without a backslash.
    VeryMagic,
    /// Unescaped characters are all literal.
    Literal,
}

/// Translate Vim grouping, quantifiers, word boundaries and magic/case switches into regex.
fn compile(input: &str, case: Option<bool>) -> Result<Regex, String> {
    let mut translated = String::new();
    let mut magic = Syntax::Normal;
    let mut ignore_case = case.unwrap_or(false);
    let mut class = false;
    let mut counted = false;
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            let next = chars
                .next()
                .ok_or_else(|| "Trailing backslash".to_owned())?;
            if class {
                translated.push('\\');
                translated.push(next);
                continue;
            }
            match next {
                'v' => magic = Syntax::VeryMagic,
                'm' => magic = Syntax::Normal,
                'M' => magic = Syntax::NoMagic,
                'V' => magic = Syntax::Literal,
                'c' => ignore_case = true,
                'C' => ignore_case = false,
                '<' => translated.push_str(r"\b{start}"),
                '>' => translated.push_str(r"\b{end}"),
                '=' => translated.push('?'),
                '(' | ')' | '|' | '+' | '?'
                    if !matches!(magic, Syntax::VeryMagic | Syntax::Literal) =>
                {
                    translated.push(next)
                }
                '{' if !matches!(magic, Syntax::VeryMagic | Syntax::Literal) => {
                    translated.push('{');
                    counted = true;
                }
                '.' | '[' | '*' if matches!(magic, Syntax::NoMagic) => {
                    translated.push(next);
                    class = next == '[';
                }
                'd' | 'D' | 's' | 'S' | 'w' | 'W' | 'n' | 't' | 'r' => {
                    translated.push('\\');
                    translated.push(next);
                }
                'k' => translated.push_str(r"\w"),
                '0'..='9' | '@' | 'z' | '%' => {
                    return Err(format!("Unsupported pattern token \\{next}"));
                }
                _ => translated.push_str(&regex::escape(&next.to_string())),
            }
            continue;
        }
        let special = if class {
            if ch == ']' {
                class = false;
            }
            true
        } else if counted && ch == '}' {
            counted = false;
            true
        } else {
            match magic {
                Syntax::Normal => ".^$*[".contains(ch),
                Syntax::NoMagic => "^$".contains(ch),
                Syntax::VeryMagic => ".^$*[]()+?{}|".contains(ch),
                Syntax::Literal => false,
            }
        };
        if special {
            translated.push(ch);
            if ch == '[' {
                class = true;
            }
        } else {
            translated.push_str(&regex::escape(&ch.to_string()));
        }
    }
    RegexBuilder::new(&translated)
        .case_insensitive(ignore_case)
        .multi_line(true)
        .build()
        .map_err(|error| error.to_string())
}
