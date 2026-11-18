//! Ex navigation of saved undo states and their edit boundaries.

use super::{Action, Register, ScreenMotionResolver, Vim};
use crate::i18n;
use std::{collections::HashSet, fmt::Write as _};

/// Undo commands available through the command line.
pub(super) const NAMES: &[&str] = &[
    "earlier", "later", "messages", "oldfiles", "rundo", "rviminfo", "undojoin", "wundo",
    "wviminfo",
];

impl Vim {
    /// Run an undo command from `text`, reusing `register` and `resolve_screen` for a chained edit.
    pub(super) fn ex_history_command(
        &mut self,
        text: &str,
        register: &mut Register,
        resolve_screen: &mut ScreenMotionResolver<'_>,
    ) -> Option<Action> {
        let text = text.trim();
        let count_end = text.bytes().take_while(u8::is_ascii_digit).count();
        let (count, message_command) = text.split_at(count_end);
        let (verb, args) = message_command
            .trim_start()
            .split_once(char::is_whitespace)
            .unwrap_or((message_command.trim_start(), ""));
        if verb.len() >= 3 && "messages".starts_with(verb) && matches!(args.trim(), "" | "clear") {
            let listing = register
                .input_maps
                .ex_messages(count.parse().ok(), !args.trim().is_empty());
            return Some(if listing.text.is_empty() {
                Action::None
            } else if listing.spans.is_empty() {
                Action::ShowListing(listing.text)
            } else {
                Action::ShowStyledListing(listing)
            });
        }
        if verb.len() >= 1
            && "oldfiles".starts_with(verb)
            && count.is_empty()
            && args.trim().is_empty()
        {
            let mut visited = HashSet::new();
            let mut listing = String::new();
            for mark in register
                .numbered_marks
                .iter()
                .filter(|mark| !mark.path.as_os_str().is_empty())
            {
                if visited.insert(&mark.path) {
                    let _ = writeln!(listing, "{}: {}", visited.len(), mark.path.display());
                }
            }
            return Some(if listing.is_empty() {
                Action::None
            } else {
                Action::ShowListing(listing)
            });
        }
        let end = text.bytes().take_while(u8::is_ascii_alphabetic).count();
        let (name, tail) = text.split_at(end);
        let tail = tail.trim_start();
        let rvim = name.len() >= 2 && "rviminfo".starts_with(name);
        let wvim = name.len() >= 2 && "wviminfo".starts_with(name);
        if rvim || wvim {
            let (force, path) = tail
                .strip_prefix('!')
                .map_or((false, tail), |path| (true, path.trim_start()));
            return Some(Action::VimInfo {
                write: wvim,
                force,
                path: (!path.is_empty()).then(|| path.into()),
            });
        }
        let write = name.len() >= 2 && "wundo".starts_with(name);
        if write || name.len() >= 4 && "rundo".starts_with(name) {
            let (force, path) = tail
                .strip_prefix('!')
                .map_or((false, tail), |path| (true, path.trim_start()));
            if path.is_empty() || force && !write {
                return Some(self.undo_command_error("ex.undo_file_argument"));
            }
            return Some(Action::UndoFile {
                path: path.into(),
                force,
                write,
            });
        }
        if name.len() >= 5 && "undojoin".starts_with(name) {
            let chained = tail.strip_prefix('|');
            if !tail.is_empty() && chained.is_none() {
                return Some(self.undo_command_error("ex.undo_argument"));
            }
            if !self.doc.join_undo_block() {
                return Some(self.undo_command_error("ex.undo_join_after_undo"));
            }
            return Some(chained.map_or(Action::None, |command| {
                self.execute_ex_command(command.trim_start(), None, None, register, resolve_screen)
            }));
        }
        let forward = if name.len() >= 2 && "earlier".starts_with(name) {
            false
        } else if name.len() >= 3 && "later".starts_with(name) {
            true
        } else {
            return None;
        };
        let digits = tail.bytes().take_while(u8::is_ascii_digit).count();
        let (amount, unit) = tail.split_at(digits);
        let count = if tail.is_empty() {
            Some(1)
        } else {
            amount.parse::<usize>().ok()
        };
        let Some(count) = count else {
            return Some(self.undo_command_error("ex.undo_argument"));
        };
        match unit {
            "" => self.doc.undo_chronological(forward, count),
            "f" => self.doc.undo_by_write(forward, count),
            "s" | "m" | "h" | "d" => {
                let seconds = match unit {
                    "m" => 60,
                    "h" => 3_600,
                    "d" => 86_400,
                    _ => 1,
                };
                self.doc
                    .undo_by_time(forward, (count as u64).saturating_mul(seconds));
            }
            _ => return Some(self.undo_command_error("ex.undo_argument")),
        }
        Some(Action::None)
    }

    /// Show a localized Ex undo `key` and stop the containing command sequence.
    fn undo_command_error(&mut self, key: &'static str) -> Action {
        self.ex_failed = true;
        self.message = i18n::text(key).into();
        Action::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;

    /// Traverse a branch, return to the newest state, then join a subsequent deletion.
    #[test]
    fn ex_undo_navigation_and_join_use_the_buffer_history() {
        let mut vim = Vim::new(Document::new("first\nsecond\nthird\n"));
        let mut register = Register::default();
        vim.execute_ex_line("1delete", &mut register);
        vim.execute_ex_line("1delete", &mut register);
        vim.execute_ex_line("earlier", &mut register);
        assert_eq!(vim.doc.text, "second\nthird\n");
        vim.execute_ex_line("undojoin | 1delete", &mut register);
        assert!(vim.ex_failed());
        assert_eq!(vim.doc.text, "second\nthird\n");
        vim.execute_ex_line("later 1h", &mut register);
        assert_eq!(vim.doc.text, "third\n");
        vim.execute_ex_line("earlier 1h", &mut register);
        vim.execute_ex_line("1delete", &mut register);
        vim.execute_ex_line("undojoin | 1delete", &mut register);
        assert!(!vim.ex_failed());
        vim.execute_ex_line("undo", &mut register);
        assert_eq!(vim.doc.text, "first\nsecond\nthird\n");
    }

    /// Use write-count navigation without deleting an unsaved state from redo history.
    #[test]
    fn ex_undo_navigation_visits_successful_writes() -> anyhow::Result<()> {
        let temporary = tempfile::tempdir()?;
        let path = temporary.path().join("buffer.txt");
        let mut doc = Document::new("initial\n");
        doc.replace(0..7, "written", true);
        doc.save(Some(&path))?;
        doc.replace(0..7, "changed", true);
        let mut vim = Vim::new(doc);
        let mut register = Register::default();
        vim.execute_ex_line("earlier 1f", &mut register);
        assert_eq!(vim.doc.text, "written\n");
        assert!(!vim.doc.modified());
        vim.execute_ex_line("later 1f", &mut register);
        assert_eq!(vim.doc.text, "changed\n");
        assert!(vim.doc.modified());
        Ok(())
    }
}
