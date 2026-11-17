//! Vim-style search patterns and line-addressed substitutions using Rust regex.

use super::{
    Action, Register, ScreenMotionResolver, Vim,
    motions::{Kind, Motion},
};
use crate::i18n;
use regex::{Captures, Regex, RegexBuilder};
use std::{collections::VecDeque, ops::Range};

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
    /// Explicit initial magic from :smagic or :snomagic.
    magic: Option<bool>,
    /// Whether each match is confirmed.
    confirm: bool,
    /// Optional output formatting after the last replacement.
    print: Option<char>,
}

/// An occurrence and its already expanded replacement, in the original document.
struct Occurrence {
    range: Range<usize>,
    replacement: String,
}

/// Remaining confirmed-substitution matches and their accumulated byte adjustment.
pub(super) struct Confirmation {
    pending: VecDeque<Occurrence>,
    shift: isize,
    revision: u64,
    initial_revision: u64,
}

impl Confirmation {
    /// Locate an original match in the document after earlier decisions.
    fn range(&self, occurrence: &Occurrence) -> Range<usize> {
        occurrence.range.start.saturating_add_signed(self.shift)
            ..occurrence.range.end.saturating_add_signed(self.shift)
    }
}

impl Vim {
    /// Search matches starting in `range`, for rendering just the visible source.
    pub fn search_highlights(&self, range: Range<usize>) -> Vec<Range<usize>> {
        if self.search_highlights_hidden || !self.highlight_search() {
            return Vec::new();
        }
        self.last_search.as_ref().map_or_else(Vec::new, |search| {
            search
                .regex
                .find_iter(&self.doc.text[range.clone()])
                .map(|hit| range.start + hit.start()..range.start + hit.end())
                .collect()
        })
    }

    /// Most recent successfully compiled search pattern for the slash register.
    pub(super) fn search_pattern(&self) -> Option<&str> {
        self.last_search.as_ref().map(|search| search.text.as_str())
    }

    /// Preview the current prompt, moving `step` matches from its current preview.
    /// A zero step restarts the incremental search at the original caret.
    pub(super) fn preview_search(&mut self, step: isize) {
        if !self.incremental_search() {
            return;
        }
        let Some(origin) = self.command_prompt.search_origin else {
            return;
        };
        let old = self.command_prompt.search_preview.take();
        let position = if self.command.is_empty() {
            None
        } else if let Ok(regex) = compile(&self.command, self.pattern_case(&self.command, None)) {
            let hits: Vec<_> = regex
                .find_iter(&self.doc.text)
                .map(|hit| hit.range())
                .collect();
            if hits.is_empty() {
                None
            } else {
                let first = if self.prompt == '?' {
                    let edge = hits.partition_point(|hit| hit.start < origin);
                    (edge + hits.len() - self.search_count % hits.len()) % hits.len()
                } else {
                    let edge = hits.partition_point(|hit| hit.start <= origin);
                    (edge + self.search_count.saturating_sub(1)) % hits.len()
                };
                let index = if step == 0 {
                    first
                } else {
                    let current = old
                        .and_then(|old| hits.iter().position(|hit| *hit == old))
                        .unwrap_or(first);
                    (current as isize + step).rem_euclid(hits.len() as isize) as usize
                };
                Some(hits[index].clone())
            }
        } else {
            None
        };
        let caret = position.as_ref().map_or(origin, |hit| hit.start);
        self.command_prompt.search_preview = position;
        if self.doc.cursor != caret {
            self.doc
                .set_search_fold_row(Some(self.doc.location(caret).0));
            self.set_cursor(caret);
        }
    }

    /// Restore the search source caret and return the last preview destination, if present.
    pub(super) fn end_prompt_search(&mut self) -> Option<usize> {
        let origin = self.command_prompt.search_origin.take()?;
        let preview = self.command_prompt.search_preview.take()?;
        self.doc
            .set_search_fold_row(Some(self.doc.location(origin).0));
        self.set_cursor(origin);
        Some(preview.start)
    }

    /// Append the character immediately after the current incremental match to the pattern.
    pub(super) fn extend_prompt_search(&mut self) {
        let Some(hit) = self.command_prompt.search_preview.as_ref() else {
            return;
        };
        let Some(ch) = self.doc.text[hit.end..].chars().next() else {
            return;
        };
        let origin = hit.start;
        self.command_pos = self.command.len();
        let continuation = if ch == '\n' {
            "\\n".into()
        } else if ".*[]~$^\\".contains(ch) {
            format!("\\{ch}")
        } else {
            ch.to_string()
        };
        self.command_insert(&continuation);
        if let Ok(regex) = compile(&self.command, self.pattern_case(&self.command, None))
            && let Some(hit) = regex.find_at(&self.doc.text, origin)
            && hit.start() == origin
        {
            self.command_prompt.search_preview = Some(hit.range());
            self.doc
                .set_search_fold_row(Some(self.doc.location(origin).0));
            self.set_cursor(origin);
        }
    }

    /// Compile search `text`, then move to `preview` or apply the queued operator through `register`.
    pub(super) fn confirm_search(
        &mut self,
        text: &str,
        preview: Option<usize>,
        register: &mut Register,
    ) {
        let operator = self.search_operator.take();
        if !text.is_empty() {
            if !self.set_search(text, self.prompt == '?') {
                return;
            }
        } else if let Some(search) = &mut self.last_search {
            search.backward = self.prompt == '?';
        }
        if let Some(end) = preview {
            let search = self
                .last_search
                .as_ref()
                .expect("previewed a compiled search");
            self.message = format!("{}{}", self.prompt, search.text);
            self.desired_column = None;
            let motion = Motion {
                end,
                kind: Kind::Exclusive,
                jump: true,
            };
            if let Some(operator) = operator {
                self.operate_motion(operator, motion, register);
            } else {
                self.doc.set_search_fold_row(Some(self.doc.location(end).0));
                self.apply_motion(motion);
            }
        } else if let Some(operator) = operator {
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
        match compile(text, self.pattern_case(text, None)) {
            Ok(regex) => {
                self.search_highlights_hidden = false;
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
        self.search_highlights_hidden = false;
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
            self.substitute(row..row + 1, previous, false);
        }
    }

    /// Repeat the previous substitution across the buffer, preserving its flags.
    pub(super) fn repeat_substitution_all(&mut self) {
        if let Some(mut previous) = self.last_substitution.clone() {
            if let Some(search) = &self.last_search {
                previous.pattern.clone_from(&search.text);
            }
            self.substitute(0..self.line_count(), previous, false);
        }
    }

    /// Parse Ex substitution `text`, returning whether it names a substitution.
    pub(super) fn substitute_command(&mut self, text: &str) -> bool {
        let current = self
            .doc
            .location(self.doc.cursor)
            .0
            .min(self.line_count() - 1);
        let Some((lines, rest)) = self.ex_range(text) else {
            return false;
        };
        let mut lines = lines.unwrap_or(current..current + 1);
        let Some((args, magic, repeat, last_search)) = substitution_head(rest) else {
            return false;
        };
        let args = args.trim_start();
        let delimiter = args
            .chars()
            .next()
            .filter(|ch| !repeat && !ch.is_alphanumeric() && !matches!(ch, '\\' | '"' | '|'));
        let (query, replacement, flags) = if let Some(delimiter) = delimiter {
            let Some((query, tail)) = delimited(&args[delimiter.len_utf8()..], delimiter) else {
                self.pattern_error("Missing substitution delimiter");
                return true;
            };
            let (replacement, flags) =
                delimited(tail, delimiter).unwrap_or_else(|| (tail.to_owned(), ""));
            (Some(query), Some(replacement), flags)
        } else {
            (None, None, args)
        };
        let flags = flags.trim();
        let digits = flags
            .find(|ch: char| ch.is_ascii_digit())
            .unwrap_or(flags.len());
        let (flags, count) = flags.split_at(digits);
        let (inherit, flags) = flags
            .strip_prefix('&')
            .map_or((false, flags), |flags| (true, flags));
        if !flags
            .chars()
            .all(|ch| ch.is_ascii_whitespace() || "giIencpl#r".contains(ch))
        {
            self.pattern_error("Invalid substitution flags");
            return true;
        }
        if !count.is_empty() {
            let Some(count) = count
                .trim()
                .parse::<usize>()
                .ok()
                .filter(|&count| count > 0)
            else {
                self.pattern_error("Invalid substitution line count");
                return true;
            };
            lines.start = lines.end - 1;
            lines.end = lines.start.saturating_add(count).min(self.line_count());
        }
        let previous = self.last_substitution.as_ref();
        let uses_search = last_search || flags.contains('r') || query.as_deref() == Some("");
        let query = if uses_search {
            self.last_search.as_ref().map(|search| search.text.clone())
        } else {
            query.or_else(|| previous.map(|sub| sub.pattern.clone()))
        };
        let Some((pattern, replacement)) =
            query.zip(replacement.or_else(|| previous.map(|sub| sub.replacement.clone())))
        else {
            self.pattern_error("No previous substitution or search pattern");
            return true;
        };
        let prior = previous.filter(|_| inherit);
        let confirm = flags.contains('c') ^ prior.is_some_and(|sub| sub.confirm);
        self.substitute(
            lines,
            Substitution {
                pattern,
                replacement,
                global: flags.contains('g') ^ prior.is_some_and(|sub| sub.global),
                case: flags
                    .chars()
                    .rev()
                    .find_map(|flag| match flag {
                        'i' => Some(true),
                        'I' => Some(false),
                        _ => None,
                    })
                    .or_else(|| prior.and_then(|sub| sub.case)),
                quiet: flags.contains('e') ^ prior.is_some_and(|sub| sub.quiet),
                count_only: flags.contains('n') ^ prior.is_some_and(|sub| sub.count_only),
                magic: magic.or_else(|| {
                    (delimiter.is_none() && !uses_search)
                        .then(|| previous.and_then(|sub| sub.magic))
                        .flatten()
                }),
                confirm,
                print: flags
                    .chars()
                    .rev()
                    .find(|&flag| "pl#".contains(flag))
                    .or_else(|| prior.and_then(|sub| sub.print)),
            },
            confirm,
        );
        true
    }

    /// Replace the first or all matches on each addressed line, as one undo transaction.
    fn substitute(&mut self, rows: Range<usize>, command: Substitution, confirm: bool) {
        if self.read_only && !command.count_only {
            self.reject_edit();
            return;
        }
        let regex = match compile_with_magic(
            &command.pattern,
            self.pattern_case(&command.pattern, command.case),
            command.magic.unwrap_or(true),
        ) {
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
        self.search_highlights_hidden = false;
        self.last_substitution = Some(command.clone());
        if confirm && !command.count_only {
            self.begin_substitute_confirmation(rows, &command, &regex);
            return;
        }
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
            self.ex_failed = true;
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
            if let Some(flag) = command.print {
                let line = self.doc.line(row);
                self.message = match flag {
                    'l' => format!("{}$", line.replace('\t', "^I")),
                    '#' => format!("{} {line}", row + 1),
                    _ => line.to_owned(),
                };
            }
        }
    }

    /// Use the global pattern for both the search register and repeat-substitution patterns.
    pub(super) fn remember_global_pattern(&mut self, pattern: String, regex: Regex) {
        self.search_highlights_hidden = false;
        if let Some(substitute) = &mut self.last_substitution {
            substitute.pattern.clone_from(&pattern);
        }
        self.last_search = Some(Search {
            text: pattern,
            regex,
            backward: false,
        });
    }

    /// Plan replacements against the original text while retaining each full match for highlighting.
    fn begin_substitute_confirmation(
        &mut self,
        rows: Range<usize>,
        command: &Substitution,
        regex: &Regex,
    ) {
        let mut pending = VecDeque::new();
        for row in rows {
            let offset = self.doc.starts[row];
            for captures in regex
                .captures_iter(self.doc.line(row))
                .take(if command.global { usize::MAX } else { 1 })
            {
                let hit = captures.get(0).unwrap();
                let mut replacement = String::new();
                expand_replacement(&command.replacement, &captures, &mut replacement);
                pending.push_back(Occurrence {
                    range: offset + hit.start()..offset + hit.end(),
                    replacement,
                });
            }
        }
        if pending.is_empty() {
            if !command.quiet {
                self.ex_failed = true;
                self.message =
                    i18n::format("editor.search_not_found", &[("query", &command.pattern)]);
            }
            return;
        }
        self.substitute_confirmation = Some(Confirmation {
            pending,
            shift: 0,
            revision: self.doc.revision,
            initial_revision: self.doc.revision,
        });
        self.show_substitution_match();
    }

    /// Whether the current prompt owns keys ahead of normal editor shortcuts and maps.
    pub fn substitute_confirmation_active(&self) -> bool {
        self.substitute_confirmation.is_some()
    }

    /// The current confirmed-substitution match, for drawing its highlight.
    pub fn substitution_match(&self) -> Option<Range<usize>> {
        let prompt = self.substitute_confirmation.as_ref()?;
        if self.doc.revision != prompt.revision {
            return None;
        }
        Some(prompt.range(prompt.pending.front()?))
    }

    /// Reveal the current match and refresh the localized prompt in the editor footer.
    fn show_substitution_match(&mut self) {
        let Some(range) = self.substitution_match() else {
            return;
        };
        self.doc
            .set_search_fold_row(Some(self.doc.location(range.start).0));
        self.set_cursor(range.start);
        self.message = i18n::text("editor.confirm_substitution").into();
    }

    /// Abandon outstanding matches and any enclosing global command, retaining confirmed changes.
    pub(super) fn cancel_substitute_confirmation(&mut self) {
        if self.substitute_confirmation.take().is_some() {
            self.ex_lines = None;
            self.message.clear();
        }
    }

    /// Consume a confirmation response and resume pending global lines when this command ends.
    pub(super) fn substitute_confirmation_key(
        &mut self,
        key: &str,
        register: &mut Register,
        resolve_screen: &mut ScreenMotionResolver<'_>,
    ) -> Action {
        let Some(mut prompt) = self.substitute_confirmation.take() else {
            return Action::None;
        };
        if prompt.revision != self.doc.revision || self.read_only {
            self.ex_lines = None;
            self.message = i18n::text("editor.substitution_invalidated").into();
            self.resume_ex_prompt();
            return Action::None;
        }
        if matches!(key, "Escape" | "Ctrl+[" | "Ctrl+c") {
            self.ex_lines = None;
            self.message.clear();
            self.resume_ex_prompt();
            return Action::None;
        }
        if matches!(key, "Ctrl+e" | "Ctrl+y") {
            self.scroll(key, None);
        }
        let mut last = None;
        if matches!(key, "y" | "n" | "a" | "l") {
            while let Some(occurrence) = prompt.pending.pop_front() {
                let range = prompt.range(&occurrence);
                last = Some(range.start);
                if key != "n" {
                    let shift = occurrence.replacement.len() as isize - range.len() as isize;
                    self.replace(
                        range,
                        &occurrence.replacement,
                        self.doc.revision == prompt.initial_revision,
                    );
                    prompt.shift += shift;
                    prompt.revision = self.doc.revision;
                }
                if key != "a" {
                    break;
                }
            }
        }
        if prompt.pending.is_empty() || matches!(key, "q" | "l") {
            if let Some(last) = last {
                self.set_cursor(last);
            }
            self.message.clear();
            let action = self.continue_ex_lines(register, resolve_screen);
            self.resume_ex_prompt();
            return action;
        } else {
            self.substitute_confirmation = Some(prompt);
            if last.is_some() {
                self.show_substitution_match();
            }
        }
        Action::None
    }

    /// Restore the persistent Ex prompt once it receives input again.
    fn resume_ex_prompt(&mut self) {
        if self.g.ex_mode && !self.substitute_confirmation_active() {
            self.open_prompt(':', 1);
        }
    }

    /// Report a pattern `error` in the status line without changing document text.
    pub(super) fn pattern_error(&mut self, error: &str) {
        self.ex_failed = true;
        self.message = i18n::format("editor.invalid_pattern", &[("error", &error)]);
    }
}

/// Resolve full and abbreviated substitution forms and the two punctuation repeats.
fn substitution_head(rest: &str) -> Option<(&str, Option<bool>, bool, bool)> {
    if let Some(args) = rest.strip_prefix('&') {
        return Some((args, None, true, false));
    }
    if let Some(args) = rest.strip_prefix('~') {
        return Some((args, None, true, true));
    }
    let end = rest.bytes().take_while(u8::is_ascii_alphabetic).count();
    let (name, args) = rest.split_at(end);
    let magic = if !name.is_empty() && "substitute".starts_with(name) {
        None
    } else if name.len() >= 3 && "snomagic".starts_with(name) {
        Some(false)
    } else if name.len() >= 2 && "smagic".starts_with(name) {
        Some(true)
    } else if let Some(flags) = name.strip_prefix('s')
        && !flags.is_empty()
        && flags.chars().all(|flag| "giIencr".contains(flag))
    {
        return Some((&rest[1..], None, true, false));
    } else {
        return None;
    };
    Some((args, magic, false, false))
}

impl Vim {
    /// Read an optional Ex range from `text` and return it with the following command.
    pub(super) fn ex_range<'a>(&self, text: &'a str) -> Option<(Option<Range<usize>>, &'a str)> {
        let current = self.doc.location(self.doc.cursor).0;
        let count = self.line_count();
        if let Some(rest) = text.strip_prefix('%') {
            return Some((Some(0..count), rest.trim_start()));
        }
        let Some((first, rest)) = self.ex_address(text, current + 1) else {
            return Some((None, text));
        };
        let (last, rest) = if let Some(rest) = rest.trim_start().strip_prefix(',') {
            self.ex_address(rest.trim_start(), current + 1)?
        } else if let Some(rest) = rest.trim_start().strip_prefix(';') {
            self.ex_address(rest.trim_start(), first)?
        } else {
            (first, rest)
        };
        (first <= last && last <= count).then_some((
            Some(first.saturating_sub(1)..last.max(1)),
            rest.trim_start(),
        ))
    }

    /// Resolve an Ex line number from `text`, preserving address zero before the first line.
    pub(super) fn ex_address<'a>(&self, text: &'a str, current: usize) -> Option<(usize, &'a str)> {
        let (mut row, mut rest) = match text.as_bytes().first()? {
            b'.' => (current, &text[1..]),
            b'$' => (self.line_count(), &text[1..]),
            b'\'' => {
                let name = text[1..].chars().next()?;
                let mark = self.mark_motion(name, false)?;
                (
                    self.doc.location(mark.end).0 + 1,
                    &text[1 + name.len_utf8()..],
                )
            }
            b'+' | b'-' => (current, text),
            b'0'..=b'9' => {
                let end = text.bytes().take_while(u8::is_ascii_digit).count();
                (text[..end].parse().ok()?, &text[end..])
            }
            _ => return None,
        };
        loop {
            let offset = rest.trim_start();
            let Some(sign @ (b'+' | b'-')) = offset.as_bytes().first() else {
                return Some((row, rest));
            };
            let digits = offset[1..].trim_start();
            let end = digits.bytes().take_while(u8::is_ascii_digit).count();
            let n = if end == 0 {
                1
            } else {
                digits[..end].parse().ok()?
            };
            row = if *sign == b'+' {
                row.checked_add(n)?
            } else {
                row.checked_sub(n)?
            };
            rest = &digits[end..];
        }
    }
}

/// Read one delimiter-terminated field, unescaping only its delimiter.
pub(super) fn delimited(text: &str, delimiter: char) -> Option<(String, &str)> {
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
pub(super) fn compile(input: &str, case: Option<bool>) -> Result<Regex, String> {
    compile_with_magic(input, case, true)
}

/// Translate a Vim pattern with the initial magic forced by an Ex substitution form.
fn compile_with_magic(
    input: &str,
    case: Option<bool>,
    initial_magic: bool,
) -> Result<Regex, String> {
    let mut translated = String::new();
    let mut magic = if initial_magic {
        Syntax::Normal
    } else {
        Syntax::NoMagic
    };
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
