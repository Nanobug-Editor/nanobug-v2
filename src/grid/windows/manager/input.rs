//! File-manager keyboard navigation and commands returned to the workspace.

use super::{ManagerWindow, TransferKind};
use nanobug::{animation::scroll::ScrollInput, i18n};
use std::path::PathBuf;

/// Effects that require cross-window coordination or delayed prefix handling.
pub(in crate::grid) enum ManagerCommand {
    /// The manager changed only its own navigation state.
    None,
    /// Open a file in the editing window.
    Open(PathBuf),
    /// Replace the shared operation-feedback message.
    Status(String),
    /// Paste, undo or redo, reconciled against unsaved editor buffers.
    Transfer(&'static str),
    /// Await the second key; standalone `z` also schedules a delayed undo.
    Prefix(char),
}

impl ManagerWindow {
    /// Whether the unmodified printable `character` requests closing Explorer.
    pub(in crate::grid) fn matches_close_key(character: Option<&str>) -> bool {
        character == Some("q")
    }

    /// Interpret `key` after `prefix`, updating local state and returning scroll and host effects.
    pub(in crate::grid) fn key(
        &mut self,
        key: &str,
        prefix: Option<char>,
    ) -> Option<(ScrollInput, ManagerCommand)> {
        if let [digit @ b'0'..=b'9'] = key.as_bytes() {
            self.count = (self.count * 10 + usize::from(*digit - b'0')).min(9999);
            return Some((ScrollInput::Advance, ManagerCommand::None));
        }
        let count = std::mem::take(&mut self.count).max(1);
        let mut command = ManagerCommand::None;
        match key {
            "k" | "up" => self.tree.selected = self.tree.selected.saturating_sub(count),
            "j" | "down" => {
                self.tree.selected = self
                    .tree
                    .selected
                    .saturating_add(count)
                    .min(self.tree.entries.len().saturating_sub(1))
            }
            "home" => self.tree.selected = 0,
            "g" if prefix == Some('g') => self.tree.selected = 0,
            "g" => command = ManagerCommand::Prefix('g'),
            "G" | "end" => self.tree.selected = self.tree.entries.len().saturating_sub(1),
            "h" | "left" => self.tree.branch(false),
            "right" => self.tree.branch(true),
            "l" | "enter" => {
                if let Some(path) = self.tree.activate() {
                    command = ManagerCommand::Open(path);
                }
            }
            "s" => {
                command = ManagerCommand::Status(i18n::format(
                    "manager.marked",
                    &[("count", &self.tree.toggle_mark())],
                ));
            }
            "escape" => {
                self.tree.marked.clear();
                command = ManagerCommand::Status(String::new());
            }
            "c" | "x" => {
                let kind = if key == "c" {
                    TransferKind::Copy
                } else {
                    TransferKind::Cut
                };
                command =
                    ManagerCommand::Status(match self.transfers.capture(&self.tree.marked, kind) {
                        Ok(count) => i18n::format(
                            match kind {
                                TransferKind::Copy => "manager.copied",
                                TransferKind::Cut => "manager.cut",
                            },
                            &[("count", &count)],
                        ),
                        Err(error) => error.to_string(),
                    });
            }
            "v" => command = ManagerCommand::Transfer("v"),
            "Z" => command = ManagerCommand::Transfer("Z"),
            "z" => command = ManagerCommand::Prefix('z'),
            _ if prefix == Some('z') => {}
            _ => return None,
        }
        Some((ScrollInput::Reveal, command))
    }
}
