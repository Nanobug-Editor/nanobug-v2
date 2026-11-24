//! Parse the bundled Neovim cfilter commands using the editor's Vim regexp dialect.

use super::{Action, Register, Vim, pattern};
use regex::Regex;

pub(super) const NAMES: &[&str] = &["Cfilter", "Lfilter"];

/// One validated list filter. Its source is retained for the new list's history title.
#[derive(Debug)]
pub struct ListFilter {
    source: String,
    inverted: bool,
    regex: Regex,
}

// The compiled form is determined entirely by the source, including Vim case flags.
impl PartialEq for ListFilter {
    fn eq(&self, other: &Self) -> bool {
        self.inverted == other.inverted && self.source == other.source
    }
}
impl Eq for ListFilter {}

impl ListFilter {
    /// Format the command and effective pattern shown in the list history.
    pub fn title(&self, location: bool) -> String {
        let command = if location { "Lfilter" } else { "Cfilter" };
        let bang = if self.inverted { "!" } else { "" };
        format!(":{command}{bang} /{}/", self.source)
    }

    /// Keep an entry when its message or file name satisfies the requested filter.
    pub fn keeps(&self, message: &str, filenames: &[&str]) -> bool {
        let matched =
            self.regex.is_match(message) || filenames.iter().any(|name| self.regex.is_match(name));
        matched != self.inverted
    }
}

impl Vim {
    /// Resolve an optional pattern delimiter and compile the expression before changing any list.
    pub(super) fn ex_list_filter(&mut self, text: &str, register: &Register) -> Option<Action> {
        let text = text.trim();
        let (verb, args) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
        let name = verb.strip_suffix('!').unwrap_or(verb);
        let command = NAMES
            .iter()
            .find(|candidate| !name.is_empty() && candidate.starts_with(name))?;
        let args = args.trim();
        if args.is_empty() {
            self.publish_ex_message("E471: Argument required", true);
            return Some(Action::None);
        }
        let first = args.chars().next()?;
        let source = if matches!(first, '/' | '\'' | '"') && args.ends_with(first) {
            args.strip_prefix(first)
                .and_then(|rest| rest.strip_suffix(first))
                .unwrap_or("")
        } else {
            args
        };
        let source = if source.is_empty() {
            self.search_pattern()
                .or_else(|| register.command_history.latest('/'))
                .unwrap_or("")
        } else {
            source
        };
        if source.is_empty() {
            return Some(Action::None);
        }
        Some(match pattern::compile(source, None) {
            Ok(regex) => Action::FilterList {
                location: *command == "Lfilter",
                filter: ListFilter {
                    source: source.into(),
                    inverted: verb != name,
                    regex,
                },
            },
            Err(error) => {
                self.pattern_error(&error);
                Action::None
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;

    #[test]
    fn filters_match_filenames_or_messages_and_bang_inverts_the_pair() {
        let mut vim = Vim::new(Document::new(""));
        let mut register = Register::default();
        for (line, local, inverted) in [
            (r"Cfilter /\cfoo\|bar/", false, false),
            (r#"Lfilter! "\cfoo\|bar""#, true, true),
            (r"Cf! '\cfoo\|bar'", false, true),
            (r"Lf \cfoo\|bar", true, false),
        ] {
            let Action::FilterList { location, filter } = vim.execute_ex_line(line, &mut register)
            else {
                panic!("failed to parse {line}: {}", vim.message);
            };
            assert_eq!(location, local);
            assert_eq!(filter.keeps("FOO error", &["lib.rs"]), !inverted);
            assert_eq!(filter.keeps("style warning", &["src/BAR.rs"]), !inverted);
            assert_eq!(filter.keeps("style warning", &["lib.rs"]), inverted);
            assert!(filter.title(local).ends_with(r" /\cfoo\|bar/"));
        }
    }

    #[test]
    fn empty_filter_uses_the_search_register_without_overwriting_it() {
        let mut vim = Vim::new(Document::new("before\nCase\n"));
        let mut register = Register::default();
        assert_eq!(
            vim.execute_ex_line("Cfilter //", &mut register),
            Action::None
        );
        vim.key("/", &mut register);
        vim.text("Case", &mut register);
        vim.key("Enter", &mut register);
        vim.execute_ex_line("set ignorecase", &mut register);
        vim.execute_ex_line("Lfilter /before/", &mut register);
        let Action::FilterList { filter, .. } = vim.execute_ex_line("Cfilter //", &mut register)
        else {
            panic!("missing previous search");
        };
        assert_eq!(filter.title(false), ":Cfilter /Case/");
        assert!(filter.keeps("Case", &[]));
        assert!(!filter.keeps("case", &[]));
        assert_eq!(
            vim.execute_ex_line("Cfilter /[/", &mut register),
            Action::None
        );
        assert!(vim.ex_failed());
        assert_eq!(vim.search_pattern(), Some("Case"));
    }
}
