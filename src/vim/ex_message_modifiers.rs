//! Native Ex modifiers for message visibility and listing filters.

use super::{Action, Vim, pattern};

/// Command names offered by the Ex prompt.
pub(super) const NAMES: &[&str] = &["filter", "silent", "unsilent"];

/// Scoped output policy for one nested Ex command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MessageModifier {
    /// Suppress normal output and optionally ignore errors.
    Silent { ignore_error: bool },
    /// Enable output within an outer silent command.
    Unsilent,
    /// Retain matching (or nonmatching) lines of ordinary output.
    Filter { pattern: String, invert: bool },
}

impl MessageModifier {
    /// Select the lines to display. Errors are handled by the caller without filtering.
    pub fn filter_text(&self, text: &str) -> String {
        let Self::Filter { pattern, invert } = self else {
            return text.into();
        };
        let Ok(regex) = pattern::compile(pattern, Some(false)) else {
            return text.into();
        };
        text.lines()
            .filter(|line| regex.is_match(line) != *invert)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Vim {
    /// Parse one message modifier with its unexecuted nested command.
    pub(super) fn ex_message_modifier(&mut self, text: &str) -> Option<Action> {
        let (word, tail) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
        let name = word.trim_end_matches('!');
        let bang = name != word;
        let is = |full: &str, minimum| name.len() >= minimum && full.starts_with(name);
        let tail = tail.trim();
        let result = if is("silent", 3) {
            Ok((MessageModifier::Silent { ignore_error: bang }, tail))
        } else if is("unsilent", 3) && !bang {
            Ok((MessageModifier::Unsilent, tail))
        } else if is("filter", 4) {
            filter(tail, bang)
        } else {
            return None;
        };
        Some(match result {
            Ok((modifier, command)) if !command.is_empty() => Action::MessageModified {
                modifier,
                command: command.trim_start_matches(':').trim_start().into(),
            },
            Ok(_) => {
                self.ex_failed = true;
                self.message =
                    crate::i18n::format("editor.invalid_ex_argument", &[("argument", &text)]);
                Action::None
            }
            Err(error) => {
                self.pattern_error(&error);
                Action::None
            }
        })
    }
}

/// Parse a delimited Vim regexp or a bare single-word pattern and its command.
fn filter(text: &str, invert: bool) -> Result<(MessageModifier, &str), String> {
    let first = text.chars().next().ok_or("Expected a filter pattern")?;
    let (pattern, command) = if first.is_alphanumeric() || first == '_' || first == '\\' {
        let (pattern, tail) = text
            .split_once(char::is_whitespace)
            .ok_or("Expected a filter command")?;
        (pattern.to_owned(), tail.trim_start())
    } else {
        let (pattern, tail) = pattern::delimited(&text[first.len_utf8()..], first)
            .ok_or("Missing filter delimiter")?;
        (pattern, tail.trim_start())
    };
    pattern::compile(&pattern, Some(false))?;
    Ok((MessageModifier::Filter { pattern, invert }, command))
}
