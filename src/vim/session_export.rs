//! Ex command parsing for explicitly saved views, Sessions and Vim configuration.

use super::{Action, Vim};

/// Export spellings offered by the Ex command prompt.
pub(super) const NAMES: &[&str] = &["mkview", "loadview", "mksession", "mkexrc", "mkvimrc"];

/// Kind of script written by an export command.
#[derive(Debug, PartialEq, Eq)]
pub enum ExportKind {
    View,
    Session,
    Exrc,
    Vimrc,
}

/// Write or restore an Ex file using the workspace filesystem routes.
#[derive(Debug, PartialEq, Eq)]
pub enum SessionExport {
    Save {
        kind: ExportKind,
        target: String,
        force: bool,
    },
    LoadView(Option<u8>),
}

impl Vim {
    /// Parse the native minimum spellings, optional output path and view slot.
    pub(super) fn session_export_ex_command(&mut self, text: &str) -> Option<Action> {
        let text = text.trim();
        let (word, target) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
        let target = target.trim();
        let (word, force) = word
            .strip_suffix('!')
            .map_or((word, false), |word| (word, true));
        let accepts = |name: &str, minimum| word.len() >= minimum && name.starts_with(word);
        if accepts("loadview", 2) {
            let slot = match target.as_bytes() {
                [] if !force => None,
                [number @ b'1'..=b'9'] if !force => Some(number - b'0'),
                _ => {
                    self.ex_failed = true;
                    self.message =
                        crate::i18n::format("editor.invalid_ex_argument", &[("argument", &text)]);
                    return Some(Action::None);
                }
            };
            return Some(Action::SessionExport(SessionExport::LoadView(slot)));
        }
        let kind = if accepts("mkview", 5) {
            ExportKind::View
        } else if accepts("mksession", 3) {
            ExportKind::Session
        } else if accepts("mkexrc", 2) {
            ExportKind::Exrc
        } else if accepts("mkvimrc", 3) {
            ExportKind::Vimrc
        } else {
            return None;
        };
        Some(Action::SessionExport(SessionExport::Save {
            kind,
            target: target.to_owned(),
            force,
        }))
    }
}
