//! Parse Ex requests for native crash recovery.

use super::{Action, Vim};

/// Recovery commands offered in Ex completion.
pub(super) const NAMES: &[&str] = &["preserve", "recover", "swapname"];

/// A request for the workspace to checkpoint, locate or load a draft.
#[derive(Debug, PartialEq, Eq)]
pub enum RecoveryCommand {
    /// Write a checkpoint for the current buffer immediately.
    Preserve,
    /// Show the current buffer's recovery file path.
    SwapName,
    /// Restore a draft into a buffer for later review and saving.
    Recover {
        /// Original file or explicit recovery file. Empty selects the current buffer.
        file: String,
        /// Allow replacing a modified buffer.
        force: bool,
    },
}

/// Recognize `recover` and its native abbreviation, with an optional attached bang.
pub(super) fn recover_verb(name: &str) -> Option<bool> {
    let (verb, force) = name
        .strip_suffix('!')
        .map_or((name, false), |verb| (verb, true));
    (verb.len() >= 3 && "recover".starts_with(verb)).then_some(force)
}

impl Vim {
    /// Parse one recovery command from `text`, keeping its file operand for workspace resolution.
    pub(super) fn ex_recovery_command(&mut self, text: &str) -> Option<Action> {
        let text = text.trim();
        let (name, arguments) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
        let verb = name.strip_suffix('!').unwrap_or(name);
        let command = if let Some(force) = recover_verb(name) {
            RecoveryCommand::Recover {
                file: arguments.trim_start().to_owned(),
                force,
            }
        } else {
            let command = if verb.len() >= 3 && "preserve".starts_with(verb) {
                RecoveryCommand::Preserve
            } else if verb.len() >= 2 && "swapname".starts_with(verb) {
                RecoveryCommand::SwapName
            } else {
                return None;
            };
            if name != verb || !arguments.is_empty() {
                self.ex_failed = true;
                self.message =
                    crate::i18n::format("editor.invalid_ex_argument", &[("argument", &text)]);
                return Some(Action::None);
            }
            command
        };
        Some(Action::Recovery(command))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{document::Document, vim::Register};

    /// Recovery parsing retains path escapes and the bang without catching unrelated commands.
    #[test]
    fn recovery_ex_commands_keep_file_operands_and_validate_arguments() {
        let mut vim = Vim::new(Document::new("test"));
        let mut register = Register::default();
        assert_eq!(
            vim.execute_ex_line("pre", &mut register),
            Action::Recovery(RecoveryCommand::Preserve),
        );
        assert_eq!(
            vim.execute_ex_line("sw", &mut register),
            Action::Recovery(RecoveryCommand::SwapName),
        );
        assert_eq!(
            vim.execute_ex_line("recover", &mut register),
            Action::Recovery(RecoveryCommand::Recover {
                file: String::new(),
                force: false
            }),
        );
        assert_eq!(
            vim.execute_ex_line(r"rec! a\ b.rs", &mut register),
            Action::Recovery(RecoveryCommand::Recover {
                file: r"a\ b.rs".into(),
                force: true
            }),
        );
        assert_eq!(
            vim.execute_ex_line("preserve file", &mut register),
            Action::None
        );
        assert!(vim.ex_failed());
        assert_eq!(
            vim.execute_ex_line("swapname!", &mut register),
            Action::None
        );
        assert!(vim.ex_failed());
        assert_eq!(
            vim.execute_ex_line("redr", &mut register),
            Action::WindowExtras(super::super::WindowCommand::Redraw),
        );
    }
}
