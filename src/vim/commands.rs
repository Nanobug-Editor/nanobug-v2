//! Command-line actions and literal search; filesystem effects belong to the host.

use super::{Mode, Vim};
use crate::i18n;
use unicode_segmentation::UnicodeSegmentation;

/// Filesystem and window actions handled by the native shell rather than the text model.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// No external action.
    None,
    /// Save, optionally to a new project-relative filename.
    Save(Option<String>),
    /// Open a file or project directory, relative to the current project when needed.
    Open(String),
    /// Close the app, optionally discarding unsaved buffers.
    Close(bool),
    /// Save the active file, then request closing the app.
    SaveClose,
}

impl Vim {
    /// Edit/execute command-line `key`; return requested save/open/close actions to the shell.
    pub(super) fn command_key(&mut self, key: &str) -> Action {
        match key {
            "Backspace" => {
                let byte = self
                    .command
                    .grapheme_indices(true)
                    .next_back()
                    .map_or(0, |(i, _)| i);
                self.command.truncate(byte);
            }
            "Enter" => {
                let search = self.mode == Mode::Search;
                self.mode = Mode::Normal;
                let text = std::mem::take(&mut self.command);
                if search {
                    if !text.is_empty() {
                        self.last_search = text;
                    }
                    self.search(false);
                    return Action::None;
                }
                let (name, args) = text.trim().split_once(' ').unwrap_or((text.trim(), ""));
                let args = args.trim();
                return match name {
                    "w" | "write" => Action::Save((!args.is_empty()).then(|| args.into())),
                    "q" | "quit" | "qa" | "qall" => Action::Close(false),
                    "q!" | "quit!" => Action::Close(true),
                    "wq" | "x" => Action::SaveClose,
                    "e" | "edit" if !args.is_empty() => Action::Open(args.into()),
                    _ => {
                        self.message =
                            i18n::format("editor.unsupported_command", &[("command", &text)]);
                        Action::None
                    }
                };
            }
            _ if !key.starts_with("Ctrl+")
                && !matches!(key, "Left" | "Right" | "Up" | "Down" | "Tab" | "Delete") =>
            {
                self.command.push_str(key)
            }
            _ => {}
        }
        Action::None
    }

    /// Repeat the last literal search, wrapping around the file; `backward` selects reverse order.
    pub(super) fn search(&mut self, backward: bool) {
        if self.last_search.is_empty() {
            return;
        }
        let cursor = self.doc.cursor;
        let found = if backward {
            self.doc.text[..cursor]
                .rfind(&self.last_search)
                .or_else(|| {
                    self.doc.text[cursor..]
                        .rfind(&self.last_search)
                        .map(|i| cursor + i)
                })
        } else {
            let start = self.doc.next(cursor);
            self.doc.text[start..]
                .find(&self.last_search)
                .map(|i| start + i)
                .or_else(|| self.doc.text[..start].find(&self.last_search))
        };
        if let Some(at) = found {
            self.doc.cursor = at;
            self.message = format!("/{}", self.last_search);
        } else {
            self.message = i18n::format("editor.search_not_found", &[("query", &self.last_search)]);
        }
    }
}
