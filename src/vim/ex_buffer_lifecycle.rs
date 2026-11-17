//! Parse Session buffer lifetime and argument-list scope commands.

use super::{Action, Vim};

/// Canonical command names offered in Ex completion.
pub(super) const NAMES: &[&str] = &[
    "all",
    "argglobal",
    "arglocal",
    "badd",
    "ball",
    "balt",
    "bunload",
    "bwipeout",
    "hide",
    "sall",
    "sball",
    "sunhide",
    "unhide",
];

/// Entries to lay out in one editor per file or buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BufferSet {
    /// Files in the current window's argument list.
    Arguments,
    /// All listed buffers, including unloaded entries.
    Listed,
    /// Listed buffers whose contents are already loaded.
    Loaded,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{document::Document, vim::Register};

    /// Distinguish buffer addresses, pane addresses and argument-list operands.
    #[test]
    fn parses_buffer_lifecycle_commands_with_their_own_address_scope() {
        let mut vim = Vim::new(Document::new("only one text row"));
        let mut register = Register::default();
        assert_eq!(
            vim.execute_ex_line("30,48bwipeout!", &mut register),
            Action::BufferLifecycle(BufferCommand::Remove {
                wipe: true,
                force: true,
                address: "30,48".into(),
                selectors: String::new(),
            })
        );
        assert_eq!(
            vim.execute_ex_line("%bunload", &mut register),
            Action::BufferLifecycle(BufferCommand::Remove {
                wipe: false,
                force: false,
                address: "%".into(),
                selectors: String::new(),
            })
        );
        assert_eq!(
            vim.execute_ex_line("2hide", &mut register),
            Action::BufferLifecycle(BufferCommand::Hide {
                window: Some(2),
                command: String::new(),
            })
        );
        for (text, set) in [
            ("all 4", BufferSet::Arguments),
            ("4sball", BufferSet::Listed),
            ("4unhide", BufferSet::Loaded),
        ] {
            assert_eq!(
                vim.execute_ex_line(text, &mut register),
                Action::BufferLifecycle(BufferCommand::Show {
                    set,
                    maximum: Some(4),
                    force: false,
                })
            );
        }
    }
}

/// A buffer or argument-list operation that needs the owning Session.
#[derive(Debug, PartialEq, Eq)]
pub enum BufferCommand {
    /// Use the Session list or a local copy in the focused window.
    ArgumentScope {
        local: bool,
        arguments: String,
        force: bool,
    },
    /// List a file without loading it, optionally setting the window's alternate file.
    Add { alternate: bool, arguments: String },
    /// Unload the selected buffers, or remove their identities and references.
    Remove {
        wipe: bool,
        force: bool,
        address: String,
        selectors: String,
    },
    /// Close a window while retaining its buffers, or execute a command with the same retention.
    Hide {
        window: Option<usize>,
        command: String,
    },
    /// Arrange the selected files or buffers into editor windows on the current Tabpage.
    Show {
        set: BufferSet,
        maximum: Option<usize>,
        force: bool,
    },
}

impl Vim {
    /// Recognize this family before treating buffer and window addresses as document lines.
    pub(super) fn ex_buffer_lifecycle(&mut self, text: &str) -> Option<Action> {
        let text = text.trim_start();
        let prefix = text
            .bytes()
            .take_while(|byte| byte.is_ascii_digit() || b",;.$%+- ".contains(byte))
            .count();
        let address = text[..prefix].trim();
        let body = &text[prefix..];
        let end = body.bytes().take_while(u8::is_ascii_alphabetic).count();
        let (name, arguments) = body.split_at(end);
        let (force, arguments) = arguments
            .strip_prefix('!')
            .map_or((false, arguments), |tail| (true, tail));
        let arguments = arguments.trim();
        let word = |full: &str, minimum| name.len() >= minimum && full.starts_with(name);
        let count = (!address.is_empty())
            .then(|| address.parse::<usize>())
            .transpose();
        let request = if word("argglobal", 4) || word("arglocal", 4) {
            if !address.is_empty() {
                return Some(self.invalid_buffer_lifecycle(text));
            }
            BufferCommand::ArgumentScope {
                local: word("arglocal", 4),
                arguments: arguments.into(),
                force,
            }
        } else if word("badd", 3) || name == "balt" {
            if !address.is_empty() || force || arguments.is_empty() {
                return Some(self.invalid_buffer_lifecycle(text));
            }
            BufferCommand::Add {
                alternate: name == "balt",
                arguments: arguments.into(),
            }
        } else if word("bunload", 3) || word("bwipeout", 2) {
            BufferCommand::Remove {
                wipe: word("bwipeout", 2),
                force,
                address: address.into(),
                selectors: arguments.into(),
            }
        } else if word("hide", 3) {
            let Ok(window) = count else {
                return Some(self.invalid_buffer_lifecycle(text));
            };
            if force || window == Some(0) || window.is_some() && !arguments.is_empty() {
                return Some(self.invalid_buffer_lifecycle(text));
            }
            BufferCommand::Hide {
                window,
                command: arguments.into(),
            }
        } else {
            let set = if word("all", 2) || word("sall", 3) {
                BufferSet::Arguments
            } else if word("ball", 2) || word("sball", 3) {
                BufferSet::Listed
            } else if word("unhide", 3) || word("sunhide", 3) {
                BufferSet::Loaded
            } else {
                return None;
            };
            let Ok(count) = count else {
                return Some(self.invalid_buffer_lifecycle(text));
            };
            let maximum = if arguments.is_empty() {
                count
            } else if let Ok(value) = arguments.parse::<usize>() {
                Some(value)
            } else {
                return Some(self.invalid_buffer_lifecycle(text));
            };
            if maximum == Some(0) || force && set != BufferSet::Arguments {
                return Some(self.invalid_buffer_lifecycle(text));
            }
            BufferCommand::Show {
                set,
                maximum,
                force,
            }
        };
        Some(Action::BufferLifecycle(request))
    }

    /// Report an invalid operand through the common Ex status path.
    fn invalid_buffer_lifecycle(&mut self, text: &str) -> Action {
        self.ex_failed = true;
        self.message = crate::i18n::format("editor.invalid_ex_argument", &[("argument", &text)]);
        Action::None
    }
}
