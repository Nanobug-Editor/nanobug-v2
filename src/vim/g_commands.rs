//! Buffer-local commands and motions entered after g.

use super::{
    Action, Mode, Operator, Pending, Register, ScreenMotion, ScreenMotionResolver, Vim,
    comments::COMMENT_OPERATOR,
    hint::{Continuation, SequenceHint},
    motions::{Kind, Motion},
};
use crate::{document::display_width, i18n};
use std::{ops::Range, path::Path};
use unicode_segmentation::UnicodeSegmentation;

/// Editable operators whose motion prefix starts with g.
const OPERATORS: &[(char, &str, &str)] = &[
    ('\u{2}', "gu", "vim_g.lower"),
    ('\u{3}', "gU", "vim_g.upper"),
    ('\u{4}', "g~", "vim_g.swap"),
    ('\u{5}', "g?", "vim_g.rot13"),
    ('\u{6}', "gq", "vim_g.format"),
    ('\u{7}', "gw", "vim_g.format_keep"),
    (COMMENT_OPERATOR, "gc", "vim_g.comment"),
    (super::script::USER_OPERATOR, "g@", "vim_g.user_operator"),
];

/// Resolve the accepted key following g to the pending operator identity.
pub(super) fn g_operator(key: &str) -> Option<char> {
    OPERATORS
        .iter()
        .find_map(|&(op, name, _)| (name.get(1..) == Some(key)).then_some(op))
}

/// Return the full visible prefix and translated group key for an internal `operator`.
pub(super) fn g_operator_name(operator: char) -> Option<&'static str> {
    OPERATORS
        .iter()
        .find_map(|&(op, name, _)| (op == operator).then_some(name))
}

/// Return the translated group key for a recognized g `operator`.
pub(super) fn g_operator_group(operator: char) -> Option<(&'static str, &'static str)> {
    OPERATORS
        .iter()
        .find_map(|&(op, name, group)| (op == operator).then_some((&name[1..], group)))
}

/// Motion keys that also work after another operator followed by g.
pub(super) const G_MOTIONS: &[Continuation] = &[
    ("e", "vim_g.word_previous_end", false),
    ("E", "vim_g.bigword_previous_end", false),
    ("0 Home", "vim_g.screen_start", false),
    ("^", "vim_g.screen_nonblank", false),
    ("$ End", "vim_g.screen_end", false),
    ("j ↓", "vim_g.screen_down", false),
    ("k ↑", "vim_g.screen_up", false),
    ("m", "vim_g.screen_middle", false),
    ("M", "vim_g.line_middle", false),
    ("o", "vim_g.goto_byte", false),
    ("' `", "vim_g.local_mark", true),
    ("n", "vim_g.next_match", false),
    ("N", "vim_g.previous_match", false),
    ("D", "vim_g.global_declaration", false),
    ("d", "vim_g.local_declaration", false),
];

/// Additional Normal and Visual g commands.
pub(super) const G_CONTINUATIONS: &[Continuation] = &[
    ("O", "vim_g.outline", false),
    ("u", "vim_g.lower", true),
    ("U", "vim_g.upper", true),
    ("~", "vim_g.swap", true),
    ("?", "vim_g.rot13", true),
    ("q", "vim_g.format", true),
    ("w", "vim_g.format_keep", true),
    ("c", "vim_g.comment", true),
    ("@", "vim_g.user_operator", true),
    ("J", "vim_g.join", false),
    ("I", "vim_g.insert_start", false),
    ("i", "vim_g.last_insert", false),
    ("r", "vim_g.virtual_replace_one", true),
    ("R", "vim_g.virtual_replace", false),
    ("p", "vim_g.put_after", false),
    ("P", "vim_g.put_before", false),
    ("*", "vim_g.partial_forward", false),
    ("#", "vim_g.partial_backward", false),
    ("&", "vim_g.substitute", false),
    ("+", "vim_g.newer", false),
    ("-", "vim_g.older", false),
    (",", "vim_g.change_next", false),
    (";", "vim_g.change_previous", false),
    ("v", "vim_g.reselect", false),
    ("V", "vim_mapping.keep_visual_exit", false),
    ("h", "vim_g.select_character", false),
    ("H", "vim_g.select_line", false),
    ("Ctrl+h Backspace", "vim_g.select_block", false),
    ("Q", "vim_g.ex_mode", false),
    ("a", "vim_g.codepoint", false),
    ("8", "vim_g.utf8", false),
    ("<", "vim_g.previous_message", false),
    ("s", "vim_g.sleep", false),
    ("Ctrl+g", "vim_g.file_stats", false),
    ("f", "vim_g.open_path", false),
    ("F", "vim_g.open_path_line", false),
    ("x", "vim_g.open_external", false),
    ("t", "vim_g.tab_next", false),
    ("T", "vim_g.tab_previous", false),
    ("Tab", "vim_g.tab_recent", false),
];

/// Retained buffer positions used by the g command family.
#[derive(Default)]
pub(super) struct State {
    /// Last Visual area, including its direction.
    visual: Option<(Mode, usize, usize)>,
    /// Offset where the last insert session finished, before leaving Insert mode.
    pub(super) insertion: Option<usize>,
    /// Most recent edits and the current position in that list.
    changes: Vec<usize>,
    change_index: usize,
    /// Last command feedback available to g<.
    previous_message: String,
    /// Whether a Visual-shaped selection accepts replacement text.
    pub(super) select: bool,
    /// Whether the Ex prompt should remain open after each command.
    pub(super) ex_mode: bool,
}

impl State {
    /// Return the last Visual mode and its original anchor and caret.
    pub(super) fn saved_visual(&self) -> Option<(Mode, usize, usize)> {
        self.visual
    }

    /// The byte position of the most recent recorded change.
    pub(super) fn last_change(&self) -> Option<usize> {
        self.changes.last().copied()
    }

    /// Clear local marks backed by change, insertion and Visual state.
    pub(super) fn clear_mark_history(&mut self) {
        self.changes.clear();
        self.change_index = 0;
        self.insertion = None;
        self.visual = None;
    }

    /// Format this buffer's recorded change locations relative to the current index.
    pub(super) fn ex_changes(&self, doc: &crate::document::Document) -> String {
        use std::fmt::Write as _;
        let mut listing = String::from("change  line  col text\n");
        for (index, offset) in self.changes.iter().enumerate() {
            let (line, col) = doc.location((*offset).min(doc.text.len()));
            let cursor = if index == self.change_index { '>' } else { ' ' };
            let distance = self.change_index.abs_diff(index);
            let _ = writeln!(
                listing,
                "{cursor}{distance:>5} {:>5} {col:>4} {}",
                line + 1,
                doc.line(line).trim()
            );
        }
        if self.change_index == self.changes.len() {
            listing.push('>');
        }
        listing
    }

    /// Return the sorted start or `end` of the last Visual selection.
    pub(super) fn visual_mark(&self, end: bool) -> Option<usize> {
        self.visual.map(|(_, anchor, cursor)| {
            if end {
                anchor.max(cursor)
            } else {
                anchor.min(cursor)
            }
        })
    }

    /// Move one endpoint of the saved Visual area to `cursor`.
    pub(super) fn set_visual_mark(&mut self, end: bool, cursor: usize) {
        let (mode, anchor, caret) = self.visual.unwrap_or((Mode::Visual, cursor, cursor));
        let (mut first, mut last) = (anchor.min(caret), anchor.max(caret));
        if end {
            last = cursor;
        } else {
            first = cursor;
        }
        let (anchor, caret) = if anchor <= caret {
            (first, last)
        } else {
            (last, first)
        };
        self.visual = Some((mode, anchor, caret));
    }

    /// Adjust retained positions around the old byte `range` replaced by `bytes`.
    pub(super) fn shift_positions(&mut self, range: &Range<usize>, bytes: usize) {
        let adjust = |at: &mut usize| {
            if *at >= range.end {
                *at = *at - range.len() + bytes;
            } else if *at >= range.start {
                *at = range.start;
            }
        };
        self.changes.iter_mut().for_each(adjust);
        if let Some(at) = &mut self.insertion {
            adjust(at);
        }
        if let Some((_, anchor, cursor)) = &mut self.visual {
            adjust(anchor);
            adjust(cursor);
        }
    }
}

/// An extra input expected after a two-key g command.
pub(super) enum Argument {
    /// Await the final equals key of the Help-buffer code block command.
    HelpCode,
    /// A mark lookup using either the line or exact position, possibly as an operator motion.
    Mark {
        /// Whether to target the first nonblank on the mark's line.
        linewise: bool,
        /// An operator waiting for the mark motion.
        operator: Option<Operator>,
    },
    /// A character repeated for the supplied number of display-space replacements.
    Replace(usize),
}

impl Argument {
    /// Show what the next argument provides after the completed g prefix.
    pub(super) fn hint(&self) -> SequenceHint {
        match self {
            Self::HelpCode => SequenceHint {
                prefix: "g=".into(),
                group: "vim_g.help_group",
                continuations: vec![("=", "vim_g.help_run", false)],
            },
            Self::Mark { linewise, operator } => {
                let mut prefix = String::new();
                if let Some(operator) = operator {
                    if let Some(name) = g_operator_name(operator.key) {
                        prefix.push_str(name);
                    } else {
                        prefix.push(operator.key);
                    }
                }
                prefix.push_str(if *linewise { "g'" } else { "g`" });
                SequenceHint {
                    prefix,
                    group: "vim_g.mark_group",
                    continuations: vec![(
                        "a–z A–Z 0–9 [ ] < > ( ) { }",
                        "vim_g.mark_argument",
                        false,
                    )],
                }
            }
            Self::Replace(_) => SequenceHint {
                prefix: "gr".to_owned(),
                group: "vim_g.virtual_group",
                continuations: vec![
                    ("…", "vim_g.replace_argument", false),
                    ("Ctrl+k", "vim_hint.digraph_first", true),
                    ("Ctrl+v Ctrl+q", "vim_hint.literal_key", true),
                ],
            },
        }
    }
}

impl Vim {
    /// Retain the most recent nonempty `message` emitted by a native editor action for g<.
    pub fn remember_host_message(&mut self, message: &str) {
        if !message.is_empty() {
            self.g.previous_message.clear();
            self.g.previous_message.push_str(message);
        }
    }

    /// Apply the `key` after g, routing display-row motion through `resolve_screen`.
    pub(super) fn g_key(
        &mut self,
        key: &str,
        operator: Option<Operator>,
        count: Option<usize>,
        register: &mut Register,
        resolve_screen: &mut ScreenMotionResolver<'_>,
    ) -> Option<Action> {
        let n = count.unwrap_or(1);
        if key == "c"
            && let Some(operator) = operator
        {
            if let Some(range) = self.commented_lines_at_cursor() {
                self.operate_object(operator, range, Kind::Line, register);
            }
            return Some(Action::None);
        }
        if let Some(op) = g_operator(key) {
            if let Some(operator) = operator {
                if operator.key == op {
                    self.line_operator(op, n, register);
                }
            } else if op == super::script::USER_OPERATOR && self.mode.is_visual() {
                self.visual_operator(op, register);
            } else if self.read_only && op != super::script::USER_OPERATOR {
                return Some(self.reject_edit());
            } else if !self.g_visual_operator(op) {
                self.pending = Some(Pending::Operator(Operator::new(op, count)));
            }
            return Some(Action::None);
        }
        match key {
            "=" if self.help_buffer && self.mode == Mode::Normal && operator.is_none() => {
                self.pending = Some(Pending::GArgument(Argument::HelpCode));
                return Some(Action::None);
            }
            "'" | "`" => {
                self.pending = Some(Pending::GArgument(Argument::Mark {
                    linewise: key == "'",
                    operator,
                }));
                return Some(Action::None);
            }
            "n" | "N" => {
                self.select_search_match(key == "N", n, operator, register);
                return Some(Action::None);
            }
            _ => {}
        }
        if let Some(motion) = self.g_motion(key, count, resolve_screen) {
            if let Some(operator) = operator {
                self.operate_motion(operator, motion, register);
                self.screen_column = None;
            } else {
                self.apply_motion(motion);
            }
            return Some(Action::None);
        }
        if operator.is_some() {
            return None;
        }
        if self.read_only
            && matches!(
                key,
                "&" | "+" | "-" | "J" | "I" | "i" | "p" | "P" | "r" | "R"
            )
        {
            return Some(self.reject_edit());
        }
        match key {
            "Ctrl+a" | "Ctrl+x" if self.mode.is_visual() => {
                if self.read_only {
                    return Some(self.reject_edit());
                }
                self.adjust_visual_numbers(key == "Ctrl+a", n, true);
            }
            "*" | "#" => self.search_word_with_boundaries(key == "#", n, false),
            "&" => self.repeat_substitution_all(),
            "+" | "-" => {
                self.doc.undo_chronological(key == "+", n);
                self.replay.reset_macro_undo(self.doc.revision);
                self.clamp_normal();
            }
            ";" | "," => self.change_position(key == ",", n),
            "v" => self.reselect_visual(),
            "V" => self.skip_select_restore(),
            "h" | "H" | "Ctrl+h" | "Backspace" => {
                if !self.mode.is_visual() {
                    self.anchor = self.doc.cursor;
                }
                self.mode = match key {
                    "H" => Mode::VisualLine,
                    "Ctrl+h" | "Backspace" => Mode::VisualBlock,
                    _ => Mode::Visual,
                };
                self.g.select = !self.read_only;
            }
            "Q" => {
                self.open_prompt(':', 1);
                self.g.ex_mode = true;
            }
            "J" => self.join_without_spaces(n),
            "p" | "P" => self.put_after_text(register, key == "P", n),
            "I" | "i" => {
                let row = self.doc.location(self.doc.cursor).0;
                self.doc.cursor = if key == "I" {
                    self.doc.starts[row]
                } else {
                    self.doc.text.floor_char_boundary(
                        self.g
                            .insertion
                            .unwrap_or(self.doc.cursor)
                            .min(self.doc.text.len()),
                    )
                };
                self.begin_insert(Mode::Insert, false, n, false);
            }
            "R" => self.begin_virtual_replace(n),
            "r" => self.pending = Some(Pending::GArgument(Argument::Replace(n))),
            "a" | "8" => self.character_info(key == "8"),
            "Ctrl+g" => self.file_statistics(),
            "s" => return Some(Action::Sleep(n as u64)),
            "<" => self.message.clone_from(&self.g.previous_message),
            "f" => return Some(self.file_at_cursor(count)),
            "F" => {
                return Some(match self.path_under_cursor(true) {
                    Some((path, line)) => Action::OpenFile(path, line.map(|line| (line - 1, 0))),
                    None => Action::None,
                });
            }
            "x" => {
                return Some(
                    self.external_path()
                        .map_or(Action::None, Action::OpenExternal),
                );
            }
            "t" => return Some(Action::TabNavigate(1, count)),
            "T" => return Some(Action::TabNavigate(-1, count)),
            "Tab" | "Ctrl+i" => return Some(Action::TabNavigate(0, count)),
            _ => return None,
        }
        Some(Action::None)
    }

    /// Complete the extra `key` expected by `argument`, with `register` for mark motions.
    pub(super) fn g_argument(
        &mut self,
        argument: Argument,
        key: &str,
        register: &mut Register,
    ) -> Action {
        match argument {
            Argument::HelpCode => {
                if key == "=" {
                    self.run_help_code(register);
                }
            }
            Argument::Mark { linewise, operator } => {
                let mut chars = key.chars();
                if let Some(name) = chars.next()
                    && chars.next().is_none()
                {
                    return self.command_mark(name, linewise, false, operator, register);
                }
            }
            Argument::Replace(count) if matches!(key, "Ctrl+k" | "Ctrl+v" | "Ctrl+q") => {
                self.begin_character_argument(
                    key,
                    super::insert_argument::Target::Replace(count, true),
                );
            }
            Argument::Replace(count) => self.replace_virtual_characters(key, count, register),
        }
        Action::None
    }

    /// Resolve the motion for `key` and `count`, using `resolve_screen` for measured rows.
    fn g_motion(
        &mut self,
        key: &str,
        count: Option<usize>,
        resolve_screen: &mut ScreenMotionResolver<'_>,
    ) -> Option<Motion> {
        let n = count.unwrap_or(1);
        let cursor = self.doc.cursor;
        let row = self.doc.location(cursor).0;
        let screen = match key {
            "j" | "Down" => Some(ScreenMotion::Down),
            "k" | "Up" => Some(ScreenMotion::Up),
            "0" | "Home" => Some(ScreenMotion::Start),
            "^" => Some(ScreenMotion::FirstNonblank),
            "m" => Some(ScreenMotion::Middle),
            "$" => Some(ScreenMotion::End),
            "End" => Some(ScreenMotion::LastNonblank),
            _ => None,
        };
        if let Some(screen) = screen
            && let Some((end, column)) = resolve_screen(self, screen, n, self.screen_column)
        {
            let vertical = matches!(screen, ScreenMotion::Up | ScreenMotion::Down);
            self.screen_column = vertical.then_some(column);
            self.desired_column = None;
            let kind = if matches!(screen, ScreenMotion::End | ScreenMotion::LastNonblank) {
                Kind::Inclusive
            } else {
                Kind::Exclusive
            };
            return (!vertical || end != cursor).then_some(Motion::new(end, kind));
        }
        let end = match key {
            "j" | "Down" | "k" | "Up" => {
                let down = matches!(key, "j" | "Down");
                return self.cursor_motion(if down { "j" } else { "k" }, count);
            }
            "e" | "E" => {
                let big = key == "E";
                let mut at = cursor;
                for _ in 0..n {
                    let class = self.class(at, big);
                    while at > 0 && class != 0 && self.class(self.doc.previous(at), big) == class {
                        at = self.doc.previous(at);
                    }
                    at = self.doc.previous(at);
                    while at > 0 && self.class(at, big) == 0 {
                        if self.doc.starts.binary_search(&at).is_ok() {
                            break;
                        }
                        at = self.doc.previous(at);
                    }
                }
                self.desired_column = None;
                return Some(Motion::new(at, Kind::Inclusive));
            }
            "o" => {
                let byte = self
                    .doc
                    .text
                    .floor_char_boundary(n.saturating_sub(1).min(self.doc.text.len()));
                self.desired_column = None;
                return Some(Motion {
                    end: byte,
                    kind: Kind::Exclusive,
                    jump: true,
                });
            }
            "d" | "D" => {
                let at = self.text_declaration(key == "d", count == Some(1))?;
                return Some(Motion {
                    end: at,
                    kind: Kind::Exclusive,
                    jump: true,
                });
            }
            "0" | "Home" => self.doc.offset(row, self.view.left),
            "^" => self
                .doc
                .offset(row, self.view.left)
                .max(self.first_nonblank(row)),
            "m" => self
                .doc
                .offset(row, self.view.left.saturating_add(self.view.columns / 2)),
            "M" => {
                let columns = display_width(self.doc.line(row), 0);
                self.doc.offset(
                    row,
                    columns.saturating_mul(count.unwrap_or(50).min(100)) / 100,
                )
            }
            "$" | "End" => {
                let row = row.saturating_add(n - 1).min(self.line_count() - 1);
                let range = self.doc.line_range(row);
                let visible = self.doc.offset(
                    row,
                    self.view
                        .left
                        .saturating_add(self.view.columns.saturating_sub(1)),
                );
                let mut at = visible.min(self.doc.previous(range.end).max(range.start));
                if key == "End" {
                    while at > range.start && matches!(self.doc.text.as_bytes()[at], b' ' | b'\t') {
                        at = self.doc.previous(at);
                    }
                }
                self.desired_column = None;
                return Some(Motion::new(at, Kind::Inclusive));
            }
            _ => return None,
        };
        self.desired_column = None;
        let end = self
            .doc
            .previous(self.doc.line_range(row).end)
            .max(self.doc.starts[row])
            .min(end);
        Some(Motion::new(end, Kind::Exclusive))
    }

    /// Jump to the inferred local declaration when no language provider serves gd.
    pub fn local_declaration(&mut self) -> bool {
        let Some(at) = self.text_declaration(true, false) else {
            return false;
        };
        self.jump_to(at);
        true
    }

    /// Find a preceding textual declaration in the file or current C-like function.
    /// `exclude_closed_scope` ignores candidates inside a brace scope that already ended.
    fn text_declaration(&self, local: bool, exclude_closed_scope: bool) -> Option<usize> {
        let keyword = self.keyword()?;
        let word = &self.doc.text[keyword.clone()];
        let row = self.doc.location(keyword.start).0;
        let scope = if local {
            (0..=row)
                .rev()
                .find(|&row| {
                    let line = self.doc.line(row);
                    line.starts_with('{')
                        || (!line.starts_with(char::is_whitespace)
                            && line.trim_end().ends_with('{')
                            && line.contains('('))
                })
                .map_or(0, |opening| {
                    (0..opening)
                        .rev()
                        .find(|&row| self.doc.line(row).trim().is_empty())
                        .map_or(0, |blank| blank + 1)
                })
        } else {
            0
        };
        let is_keyword = |ch: char| ch.is_alphanumeric() || ch == '_';
        let mut depth = 0usize;
        let mut found = None;
        for row in scope..=row {
            let range = self.doc.line_range(row);
            let line = &self.doc.text[range.start..range.end.min(keyword.end)];
            let line = line.trim_end();
            let trimmed = line.trim_start();
            if ["//", "*", "/*", "#"]
                .into_iter()
                .any(|prefix| trimmed.starts_with(prefix))
            {
                continue;
            }
            let mut index = 0;
            for (at, _) in line.match_indices(word) {
                advance_depth(
                    &line[index..at],
                    &mut depth,
                    &mut found,
                    exclude_closed_scope,
                );
                index = at + word.len();
                if !line[..at].chars().next_back().is_some_and(is_keyword)
                    && !line[index..].chars().next().is_some_and(is_keyword)
                {
                    found.get_or_insert((range.start + at, depth));
                    if !exclude_closed_scope {
                        return found.map(|(at, _)| at);
                    }
                }
            }
            advance_depth(&line[index..], &mut depth, &mut found, exclude_closed_scope);
        }
        found.map(|(at, _)| at)
    }

    /// Apply a counted match selection, or use the saved match as an operator range.
    fn select_search_match(
        &mut self,
        backward: bool,
        count: usize,
        operator: Option<Operator>,
        register: &mut Register,
    ) {
        let Some(range) = self.search_span(backward, count) else {
            return;
        };
        if let Some(operator) = operator {
            self.doc.cursor = range.start;
            self.operate_object(operator, range, Kind::Exclusive, register);
        } else {
            if !self.mode.is_visual() {
                self.anchor = if backward {
                    self.doc.previous(range.end)
                } else {
                    range.start
                };
                self.mode = Mode::Visual;
            }
            self.set_cursor(if backward {
                range.start
            } else {
                self.doc.previous(range.end)
            });
        }
    }

    /// Apply `operator` to `range` through the same replacement and undo path as other operators.
    pub(super) fn g_operator_range(&mut self, operator: char, range: Range<usize>) -> bool {
        if g_operator_name(operator).is_none() {
            return false;
        }
        if operator == COMMENT_OPERATOR {
            self.toggle_comment_lines(range);
            return true;
        }
        if matches!(operator, '\u{6}' | '\u{7}') {
            self.reflow_text(range, operator == '\u{7}');
            return true;
        }
        let start = range.start;
        self.g_transform(operator, range, true);
        self.mode = Mode::Normal;
        self.set_cursor(start);
        true
    }

    /// Apply a g `operator` to the active Visual selection, including separate block rows.
    pub(super) fn g_visual_operator(&mut self, operator: char) -> bool {
        if !self.mode.is_visual() || g_operator_name(operator).is_none() {
            return false;
        }
        if self.mode != Mode::VisualBlock {
            if let Some(range) = self.selection() {
                self.g_operator_range(operator, range);
            }
            return true;
        }
        let (rows, columns) = self.block_bounds();
        if matches!(operator, COMMENT_OPERATOR | '\u{6}' | '\u{7}') {
            self.g_operator_range(operator, self.line_span(*rows.start(), *rows.end()));
        } else {
            let start = self.block_range(*rows.start(), &columns).start;
            let revision = self.doc.revision;
            for row in rows.rev() {
                self.g_transform(
                    operator,
                    self.block_range(row, &columns),
                    self.doc.revision == revision,
                );
            }
            self.mode = Mode::Normal;
            self.set_cursor(start);
        }
        true
    }

    /// Transform one `range` and create an undo step when `checkpoint` is requested.
    fn g_transform(&mut self, operator: char, range: Range<usize>, checkpoint: bool) {
        let mut text = String::new();
        for c in self.doc.text[range.clone()].chars() {
            match operator {
                '\u{2}' => text.extend(c.to_lowercase()),
                '\u{3}' => text.extend(c.to_uppercase()),
                '\u{4}' if c.is_lowercase() => text.extend(c.to_uppercase()),
                '\u{4}' => text.extend(c.to_lowercase()),
                '\u{5}' if c.is_ascii_lowercase() => {
                    text.push(char::from(b'a' + (c as u8 - b'a' + 13) % 26))
                }
                '\u{5}' if c.is_ascii_uppercase() => {
                    text.push(char::from(b'A' + (c as u8 - b'A' + 13) % 26))
                }
                _ => text.push(c),
            }
        }
        self.replace(range, &text, checkpoint);
    }

    /// Wrap the lines touched by `range` as paragraphs, keeping cursor position for gw.
    fn reflow_text(&mut self, range: Range<usize>, keep_cursor: bool) {
        let first = self.doc.location(range.start).0;
        let last = self
            .doc
            .location(self.doc.previous(range.end).max(range.start))
            .0;
        let width = self.view.columns.saturating_sub(1).clamp(1, 79);
        let cursor = self.doc.location(self.doc.cursor);
        let mut lines = Vec::new();
        let mut paragraph: Vec<&str> = Vec::new();
        for row in first..=last {
            let line = self.doc.line(row);
            if line.trim().is_empty() {
                append_paragraph(&mut lines, &mut paragraph, width);
                lines.push(line.to_owned());
            } else {
                paragraph.push(line);
            }
        }
        append_paragraph(&mut lines, &mut paragraph, width);
        let span = self.doc.starts[first]..self.doc.line_range(last).end;
        self.replace(span, &lines.join("\n"), true);
        self.mode = Mode::Normal;
        let at = if keep_cursor {
            self.doc.offset(cursor.0, cursor.1)
        } else {
            self.first_nonblank(first + lines.len().saturating_sub(1))
        };
        self.set_cursor(at);
    }

    /// Join the selected rows or `count` lines, deleting only the intervening line breaks.
    fn join_without_spaces(&mut self, count: usize) {
        let row = self.doc.location(self.doc.cursor).0;
        let (first, last) = if self.mode.is_visual() {
            let anchor = self.doc.location(self.anchor).0;
            (
                row.min(anchor),
                row.max(anchor).max(row.min(anchor).saturating_add(1)),
            )
        } else {
            (row, row.saturating_add(count.max(2) - 1))
        };
        let last = last.min(self.line_count() - 1);
        if first >= last {
            return;
        }
        self.mode = Mode::Normal;
        self.join_range(first..last + 1, true);
    }

    /// Display Unicode values or the UTF-8 bytes of the grapheme under the caret.
    fn character_info(&mut self, raw_bytes: bool) {
        let at = self.doc.cursor;
        let text = &self.doc.text[at..self.doc.next(at)];
        if raw_bytes {
            self.message = text
                .bytes()
                .map(|byte| format!("{byte:02x}"))
                .collect::<Vec<_>>()
                .join(" ");
        } else {
            self.message = text
                .chars()
                .map(|c| format!("<{}> {} (U+{:04X})", c.escape_debug(), c as u32, c as u32))
                .collect::<Vec<_>>()
                .join(" ");
        }
    }

    /// Report text counts for the caret position or the current selection.
    fn file_statistics(&mut self) {
        if self.mode.is_visual() {
            self.selection_statistics();
            return;
        }
        let at = self.doc.cursor;
        let (row, col) = self.doc.location(at);
        let index = |n: usize| n.to_string();
        self.message = i18n::format(
            "vim_g.stats",
            &[
                ("line", &index(row + 1)),
                ("lines", &index(self.line_count())),
                ("column", &index(col + 1)),
                (
                    "word",
                    &index(self.doc.text[..self.doc.next(at)].unicode_words().count()),
                ),
                ("words", &index(self.doc.text.unicode_words().count())),
                (
                    "character",
                    &index(self.doc.text[..self.doc.next(at)].chars().count()),
                ),
                ("characters", &index(self.doc.text.chars().count())),
                ("byte", &index(at + 1)),
                ("bytes", &index(self.doc.text.len())),
            ],
        );
    }

    /// Report selected rows and text per row, with width for rectangular selections.
    fn selection_statistics(&mut self) {
        let first = self.doc.location(self.anchor.min(self.doc.cursor)).0;
        let last = self.doc.location(self.anchor.max(self.doc.cursor)).0;
        let mut words = 0;
        let mut characters = 0;
        let mut bytes = 0;
        for row in first..=last {
            let selected = self.selection_on_line(row).expect("selected row");
            let start = selected.start.max(self.doc.starts[row]);
            let end = selected.end.min(
                self.doc
                    .starts
                    .get(row + 1)
                    .copied()
                    .unwrap_or(self.doc.text.len()),
            );
            let text = &self.doc.text[start..end];
            words += text.split_whitespace().count();
            characters += text.chars().count();
            bytes += text.len();
        }
        let columns = if self.mode == Mode::VisualBlock {
            i18n::format(
                "vim_g.stats_selection_columns",
                &[("columns", &self.block_bounds().1.len().to_string())],
            )
        } else {
            String::new()
        };
        self.message = i18n::format(
            "vim_g.stats_selection",
            &[
                ("columns", &columns),
                ("line", &(last - first + 1).to_string()),
                ("lines", &self.line_count().to_string()),
                ("word", &words.to_string()),
                (
                    "words",
                    &self.doc.text.split_whitespace().count().to_string(),
                ),
                ("character", &characters.to_string()),
                ("characters", &self.doc.text.chars().count().to_string()),
                ("byte", &bytes.to_string()),
                ("bytes", &self.doc.text.len().to_string()),
            ],
        );
    }

    /// Retain visual boundaries and cursor positions when `stroke` changes the model.
    pub(super) fn track_g_input(
        &mut self,
        prior_mode: Mode,
        anchor: usize,
        cursor: usize,
        revision: u64,
        record_change: bool,
    ) {
        if prior_mode.is_visual() && !self.mode.is_visual() {
            self.ex_hidden_marks.remove(&'<');
            self.ex_hidden_marks.remove(&'>');
            self.g.visual = Some((
                prior_mode,
                anchor.min(self.doc.text.len()),
                cursor.min(self.doc.text.len()),
            ));
        }
        self.g.select &= self.mode.is_visual();
        self.g.ex_mode &= self.mode == Mode::Command;
        if record_change && revision != self.doc.revision {
            self.ex_hidden_marks.remove(&'.');
            let changes = &mut self.g.changes;
            let boundary = |byte: usize| {
                self.doc
                    .text
                    .floor_char_boundary(byte.min(self.doc.text.len()))
            };
            let cursor = boundary(cursor);
            let last_row = changes.last().map(|&at| self.doc.location(boundary(at)).0);
            if last_row == Some(self.doc.location(cursor).0) {
                changes.pop();
            }
            changes.push(cursor);
            if changes.len() > 100 {
                changes.remove(0);
            }
            self.g.change_index = changes.len();
        }
        if !self.message.is_empty() {
            self.g.previous_message.clone_from(&self.message);
        }
    }

    /// Replace the current Select area with literal `text` and enter Insert mode.
    pub(super) fn replace_select(&mut self, text: &str, register: &mut Register) {
        self.g.select = false;
        self.visual_operator('c', register);
        self.insert_text(text);
    }

    /// Restore the last Visual area or exchange it with the current one.
    fn reselect_visual(&mut self) {
        let Some((mode, anchor, cursor)) = self.g.visual else {
            return;
        };
        if self.mode.is_visual() {
            self.g.visual = Some((self.mode, self.anchor, self.doc.cursor));
        }
        self.mode = mode;
        self.anchor = self
            .doc
            .text
            .floor_char_boundary(anchor.min(self.doc.text.len()));
        self.set_cursor(cursor);
    }

    /// Visit `count` newer or older edits without changing the text or jumplist.
    fn change_position(&mut self, newer: bool, count: usize) {
        let Some(last) = self.g.changes.len().checked_sub(1) else {
            return;
        };
        let index = if newer {
            self.g.change_index.saturating_add(count).min(last)
        } else {
            self.g.change_index.saturating_sub(count)
        };
        if index != self.g.change_index {
            self.g.change_index = index;
            self.set_cursor(self.g.changes[index]);
        }
    }

    /// Resolve and open the counted file name under the cursor.
    pub(super) fn file_at_cursor(&self, _count: Option<usize>) -> Action {
        self.path_under_cursor(false)
            .map_or(Action::None, |(path, _)| Action::OpenFile(path, None))
    }

    /// Resolve the current file token and its one-based line when `with_line` is enabled.
    pub fn path_under_cursor(&self, with_line: bool) -> Option<(String, Option<usize>)> {
        let (raw, line) = self.path_reference_under_cursor(with_line)?;
        let path = self
            .doc
            .path
            .as_deref()
            .and_then(Path::parent)
            .map(|parent| parent.join(raw));
        let path = path.filter(|path| path.is_file()).map_or_else(
            || raw.to_owned(),
            |path| path.to_string_lossy().into_owned(),
        );
        Some((path, line))
    }

    /// Read the file token without resolving directories and, for `with_line`, its one-based line.
    pub fn path_reference_under_cursor(&self, with_line: bool) -> Option<(&str, Option<usize>)> {
        let (raw, rest) = self.path_token(false)?;
        let (raw, embedded) = raw.rsplit_once(':').map_or((raw, None), |(path, line)| {
            if let Some(line) = line_number(line) {
                (path, Some(line))
            } else {
                (raw, None)
            }
        });
        if raw.is_empty() || raw.contains("://") {
            return None;
        }
        let line = with_line
            .then(|| {
                embedded.or_else(|| line_number(rest.trim_start_matches([' ', '\t', ':', '('])))
            })
            .flatten();
        Some((raw, line))
    }

    /// Resolve a URL or file path for the native system handler.
    fn external_path(&self) -> Option<String> {
        let (path, _) = self.path_token(true)?;
        if path.contains("://") {
            Some(path.to_owned())
        } else {
            self.path_under_cursor(false).map(|(path, _)| path)
        }
    }

    /// Find the path-like token at or following the cursor, extending URL punctuation when needed.
    fn path_token(&self, url: bool) -> Option<(&str, &str)> {
        let at = self.doc.cursor;
        let range = self.doc.line_range(self.doc.location(at).0);
        let before = &self.doc.text[range.start..at];
        let after = &self.doc.text[at..range.end];
        let token_char = |c: char| {
            c.is_alphanumeric() || "./\\-+_~$%#@:".contains(c) || (url && "?&=;".contains(c))
        };
        let begin = before.rfind(|c: char| !token_char(c)).map_or(0, |byte| {
            byte + before[byte..].chars().next().unwrap().len_utf8()
        });
        let ending = after.find(|c: char| !token_char(c)).unwrap_or(after.len());
        let path = &self.doc.text[range.start + begin..at + ending];
        if path.is_empty() {
            return None;
        }
        Some((path, &after[ending..]))
    }
}

/// Maintain brace depth and discard a declaration when its containing scope closes.
fn advance_depth(
    text: &str,
    depth: &mut usize,
    found: &mut Option<(usize, usize)>,
    exclude_closed_scope: bool,
) {
    if !exclude_closed_scope {
        return;
    }
    for byte in text.bytes() {
        if byte == b'{' {
            *depth += 1;
        } else if byte == b'}' {
            *depth = depth.saturating_sub(1);
            if found.is_some_and(|(_, scope)| scope > *depth) {
                *found = None;
            }
        }
    }
}

/// Parse a one-based line at the beginning of a path suffix.
fn line_number(text: &str) -> Option<usize> {
    let end = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    text[..end].parse::<usize>().ok().filter(|&line| line != 0)
}

/// Append a paragraph's wrapped lines at `width`, preserving initial indentation.
fn append_paragraph(lines: &mut Vec<String>, paragraph: &mut Vec<&str>, width: usize) {
    let Some(&first) = paragraph.first() else {
        return;
    };
    let indent = &first[..first.len() - first.trim_start_matches([' ', '\t']).len()];
    let mut line = indent.to_owned();
    for word in paragraph.drain(..).flat_map(str::split_whitespace) {
        let padded = line.len() > indent.len();
        let columns = display_width(&line, 0) + usize::from(padded);
        if padded && columns + display_width(word, columns) > width {
            lines.push(line);
            line = indent.to_owned();
        } else if padded {
            line.push(' ');
        }
        line.push_str(word);
    }
    lines.push(line);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;

    /// Select mode forwards composed text through the replacement and insert undo path.
    #[test]
    fn select_mode_replaces_with_committed_text() {
        let mut vim = Vim::new(Document::new("abc"));
        let mut register = Register::default();
        vim.text("gh", &mut register);
        assert!(vim.accepts_text());
        assert_eq!(vim.selection(), Some(0..1));
        vim.commit_text("中", &mut register);
        vim.key("Escape", &mut register);
        assert_eq!(vim.doc.text, "中bc");
        vim.key("u", &mut register);
        assert_eq!(vim.doc.text, "abc");
        assert!(!vim.accepts_text());
    }

    /// Retain the Ex prompt across submitted commands, then leave it through :vi.
    #[test]
    fn gq_uppercase_keeps_the_ex_prompt_until_exit() {
        let mut vim = Vim::new(Document::new("one\ntwo"));
        let mut register = Register::default();
        vim.text("gQ", &mut register);
        assert_eq!(vim.mode, Mode::Command);
        vim.commit_text("%s/o/O/g", &mut register);
        vim.key("Enter", &mut register);
        assert_eq!(vim.mode, Mode::Command);
        assert_eq!(vim.doc.text, "One\ntwO");
        vim.commit_text("vi", &mut register);
        vim.key("Enter", &mut register);
        assert_eq!(vim.mode, Mode::Normal);
        assert!(!vim.g.ex_mode);
    }

    /// A path token can include a one-based line while gx retains URL punctuation.
    #[test]
    fn g_file_commands_preserve_the_target_and_line() {
        let mut vim = Vim::new(Document::new(
            "see src/main.rs:20\nhttps://example.com/?x=1&y=2",
        ));
        let mut register = Register::default();
        vim.set_cursor(5);
        assert_eq!(
            vim.text("gf", &mut register),
            Action::OpenFile("src/main.rs".into(), None)
        );
        assert_eq!(
            vim.text("gF", &mut register),
            Action::OpenFile("src/main.rs".into(), Some((19, 0)))
        );
        vim.set_cursor(vim.doc.starts[1]);
        assert_eq!(
            vim.text("gx", &mut register),
            Action::OpenExternal("https://example.com/?x=1&y=2".into())
        );
    }

    /// Replacing the first cell of a tab retains alignment, and Backspace restores the tab.
    #[test]
    fn virtual_replace_restores_display_cells() {
        let mut vim = Vim::new(Document::new("\tabc"));
        let mut register = Register::default();
        vim.text("gRx", &mut register);
        assert_eq!(vim.doc.text, "x\tabc");
        vim.key("Backspace", &mut register);
        assert_eq!(vim.doc.text, "\tabc");
        vim.key("Escape", &mut register);
        vim.key("u", &mut register);
        assert_eq!(vim.doc.text, "\tabc");
    }
}
