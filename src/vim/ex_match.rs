//! Parse the three window-local Ex match slots using the editor's Vim regex dialect.

use super::{Action, Register, Vim, pattern};
use crate::i18n;
use regex::Regex;

pub(super) const NAMES: &[&str] = &["match"];

/// One window-local match projected onto the active buffer by the host.
pub struct ExMatch {
    /// Highlight group resolved when the host paints a visible match.
    pub group: String,
    /// Compiled expression with case sensitivity independent of the search options.
    pub regex: Regex,
}

/// Effect of a parsed match command before validating its group.
enum MatchOperation {
    Set(ExMatch),
    Clear,
    Pipeline,
}

impl Vim {
    /// Interpret :match and update the slots shared by this window's buffers and scripts.
    pub(super) fn ex_match_command(
        &mut self,
        text: &str,
        register: &mut Register,
    ) -> Option<Action> {
        let (count, name, arguments) = parts(text);
        if !is_match(name) {
            return None;
        }
        let Some(slot) = slot(count).filter(|_| !arguments.starts_with('!')) else {
            self.publish_ex_message(&invalid_argument(text), true);
            return Some(Action::None);
        };
        let arguments = arguments.trim_start();
        if arguments.starts_with('|') {
            return Some(Action::ExPipeline(text.into()));
        }
        let replacement = match operation(arguments) {
            Ok(MatchOperation::Pipeline) => return Some(Action::ExPipeline(text.into())),
            Ok(MatchOperation::Clear) => None,
            Ok(MatchOperation::Set(rule))
                if register.input_maps.highlight_style(&rule.group).is_some() =>
            {
                Some(rule)
            }
            Ok(MatchOperation::Set(rule)) => {
                self.publish_ex_message(&invalid_argument(&rule.group), true);
                None
            }
            Err(error) => {
                self.publish_ex_message(&error, true);
                None
            }
        };
        register.ex_matches[slot - 1] = replacement;
        Some(Action::None)
    }
}

/// Separate the numeric slot, command name and argument while keeping the original slices.
fn parts(text: &str) -> (&str, &str, &str) {
    let text = text.trim_start_matches([' ', '\t', ':']);
    let count_len = text.bytes().take_while(u8::is_ascii_digit).count();
    let (count, tail) = text.split_at(count_len);
    let tail = tail.trim_start();
    let name_len = tail.bytes().take_while(u8::is_ascii_alphabetic).count();
    let (name, arguments) = tail.split_at(name_len);
    (count, name, arguments)
}

/// Use slot one by default. The count is independent of the number of lines in the buffer.
fn slot(count: &str) -> Option<usize> {
    let slot = if count.is_empty() {
        1
    } else {
        count.parse().ok()?
    };
    (1..=3).contains(&slot).then_some(slot)
}

/// Match the documented minimum abbreviation `mat`.
fn is_match(name: &str) -> bool {
    name.len() >= 3 && "match".starts_with(name)
}

/// Report an invalid :match argument using the wording shared with the other Ex commands.
fn invalid_argument(text: &str) -> String {
    i18n::format("editor.invalid_ex_argument", &[("argument", &text)])
}

/// Parse a single operation, leaving a separator for the workspace pipeline runner.
fn operation(arguments: &str) -> Result<MatchOperation, String> {
    if arguments.is_empty() || arguments.starts_with('"') {
        return Ok(MatchOperation::Clear);
    }
    let (group, rest) = arguments
        .split_once(char::is_whitespace)
        .map_or((arguments, ""), |(group, rest)| (group, rest.trim_start()));
    let clear_suffix = arguments.split_at_checked(4).and_then(|(name, rest)| {
        (name.eq_ignore_ascii_case("none")
            && (rest.is_empty()
                || rest.starts_with(char::is_whitespace)
                || rest.starts_with(['|', '"'])))
        .then_some(rest)
    });
    let (result, tail) = if let Some(tail) = clear_suffix {
        (MatchOperation::Clear, tail)
    } else {
        let (source, tail) =
            pattern::regexp_delimited(rest).ok_or_else(|| invalid_argument(arguments))?;
        if source.is_empty() {
            return Err(invalid_argument(arguments));
        }
        if tail.trim_start().starts_with('|') {
            return Ok(MatchOperation::Pipeline);
        }
        let regex = pattern::compile(source, Some(false))
            .map_err(|error| i18n::format("editor.invalid_pattern", &[("error", &error)]))?;
        (
            MatchOperation::Set(ExMatch {
                group: group.into(),
                regex,
            }),
            tail,
        )
    };
    let tail = tail.trim_start();
    if tail.starts_with('|') {
        Ok(MatchOperation::Pipeline)
    } else if tail.is_empty() || tail.starts_with('"') {
        Ok(result)
    } else {
        Err(invalid_argument(tail))
    }
}

/// Skip over pattern punctuation before the generic Ex pipeline scanner examines the rest.
pub(super) fn protected_prefix(command: &str) -> usize {
    let (_, _, body) = super::ex_window_modifiers::command_modifiers(command);
    let (_, name, arguments) = parts(body);
    let arguments = arguments.trim_start();
    if !is_match(name) || arguments.starts_with(['"', '|']) {
        return 0;
    }
    let Some((group, pattern)) = arguments.split_once(char::is_whitespace) else {
        return 0;
    };
    if group.eq_ignore_ascii_case("none") || group.contains('|') {
        return 0;
    }
    pattern::regexp_delimited(pattern.trim_start())
        .map_or(command.len(), |(_, tail)| command.len() - tail.len())
}

/// Resolve :match group names before ordinary line-address or filename completion.
pub(super) fn completion(
    head: &str,
    groups: impl FnOnce(&str) -> Vec<String>,
) -> Option<(usize, Vec<String>)> {
    let (_, _, body) = super::ex_window_modifiers::command_modifiers(head);
    let (count, name, arguments) = parts(body);
    let arguments = arguments.trim_start();
    slot(count)?;
    if arguments.is_empty() && !body.ends_with(char::is_whitespace) {
        return (!count.is_empty() && !name.is_empty() && "match".starts_with(name))
            .then(|| (head.len() - name.len(), vec!["match".into()]));
    }
    if !is_match(name) || arguments.contains(char::is_whitespace) {
        return None;
    }
    let mut candidates = groups(arguments);
    if "none".starts_with(arguments) {
        candidates.push("none".into());
    }
    Some((head.len() - arguments.len(), candidates))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;

    #[test]
    fn match_parses_independent_slots_and_the_existing_vim_regex_dialect() {
        let mut vim = Vim::new(Document::new("one"));
        let mut register = Register::default();
        vim.execute_ex_line("set ignorecase", &mut register);
        vim.execute_ex_line(r"3mat Search /TODO[/]\|FIXME/", &mut register);
        assert!(!vim.ex_failed());
        let rule = register.ex_matches()[2].as_ref().unwrap();
        assert_eq!(rule.group, "Search");
        assert!(rule.regex.is_match("TODO/"));
        assert!(rule.regex.is_match("FIXME"));
        assert!(!rule.regex.is_match("todo/"));
        vim.execute_ex_line(r"match Search #\cTodo#", &mut register);
        assert!(
            register.ex_matches()[0]
                .as_ref()
                .unwrap()
                .regex
                .is_match("TODO")
        );
        vim.execute_ex_line("2match Normal /second/", &mut register);
        vim.execute_ex_line("2match NoNe", &mut register);
        assert!(register.ex_matches()[1].is_none());
        vim.execute_ex_line("match", &mut register);
        assert!(register.ex_matches()[0].is_none());
        assert!(register.ex_matches()[2].is_some());
    }

    #[test]
    fn match_invalid_pattern_clears_its_slot_and_invalid_slot_preserves_others() {
        let mut vim = Vim::new(Document::new(""));
        let mut register = Register::default();
        vim.execute_ex_line("2match Search /valid/", &mut register);
        vim.execute_ex_line("3match Search /elsewhere/", &mut register);
        assert!(register.ex_matches()[1].is_some());
        assert!(register.ex_matches()[2].is_some());
        vim.execute_ex_line(r"2mat Search /\(/", &mut register);
        assert!(vim.ex_failed());
        assert!(register.ex_matches()[1].is_none());
        assert_eq!(
            vim.execute_ex_line("4match none", &mut register),
            Action::None
        );
        assert!(vim.ex_failed());
        assert!(register.ex_matches()[2].is_some());
        vim.execute_ex_line("3match Search /unterminated", &mut register);
        assert!(vim.ex_failed());
        assert!(register.ex_matches()[2].is_none());
        assert_eq!(
            vim.execute_ex_line("match! none", &mut register),
            Action::None
        );
        assert!(vim.ex_failed());
    }

    #[test]
    fn match_pipeline_keeps_regex_bars_and_preserves_following_statements() {
        let mut vim = Vim::new(Document::new(""));
        let mut register = Register::default();
        let line = r"highlight Demo gui=bold | 2match Demo /\vfoo|bar/ | echo 'end'";
        assert_eq!(
            vim.source_statements(line),
            [
                "highlight Demo gui=bold",
                r"2match Demo /\vfoo|bar/",
                "echo 'end'"
            ]
        );
        let direct = "match Demo |foo/bar| | echo 'end'";
        assert_eq!(
            vim.execute_ex_line(direct, &mut register),
            Action::ExPipeline(direct.into())
        );
    }

    #[test]
    fn match_completion_treats_slot_numbers_as_independent_of_the_document_range() {
        let mut vim = Vim::new(Document::new("one"));
        let mut register = Register::default();
        vim.execute_ex_line("highlight Demo guifg=#ff0000", &mut register);
        assert_eq!(
            vim.command_candidates("3mat", &register),
            Some((1, vec!["match".into()]))
        );
        assert_eq!(
            vim.command_candidates("3match de", &register),
            Some((7, vec!["Demo".into()]))
        );
        assert_eq!(
            vim.command_candidates("silent 3match non", &register),
            Some((14, vec!["none".into()]))
        );
    }

    /// A function can define a match on the same window bank as directly typed Ex commands.
    #[test]
    fn match_in_function_changes_the_current_windows_slots() {
        let mut vim = Vim::new(Document::new("one"));
        let mut register = Register::default();
        vim.run_script("function! MarkLine()\nhi FromFunction guifg=#00ffaa\n3match FromFunction /one/\nendfunction", &mut register).unwrap();
        assert_eq!(
            vim.execute_ex_line("call MarkLine()", &mut register),
            Action::None
        );
        assert!(!vim.ex_failed(), "{}", vim.message);
        assert_eq!(
            register.ex_matches()[2].as_ref().unwrap().group,
            "FromFunction"
        );
    }
}
