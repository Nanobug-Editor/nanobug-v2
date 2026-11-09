//! Translate native editor keys and apply edits through the window's active Vim model.

use super::EditorWindow;
use crate::ui::keys::normal_character;
use gpui::{Keystroke, Modifiers};
use nanobug::vim::{Action, Mode, Register, Vim};
use std::path::PathBuf;

/// Effects returned to the workspace after a document operation.
pub(in crate::grid) struct EditResult {
    /// Filesystem or window action requested by Vim.
    pub action: Action,
    /// Modified file whose manager transfer history must be invalidated.
    pub changed_path: Option<PathBuf>,
}

/// A consumed native key and the status-line behavior of that operation.
pub(in crate::grid) struct NativeKeyResult {
    /// Document edit and external action to dispatch.
    pub edit: EditResult,
    /// Page navigation retains operation feedback; ordinary editing clears it.
    pub clear_status: bool,
}

impl EditorWindow {
    /// Whether `key` is the editor's close shortcut, in any Vim mode.
    pub(in crate::grid) fn matches_close_key(key: &Keystroke) -> bool {
        key.key == "escape"
            && key.modifiers
                == (Modifiers {
                    alt: true,
                    ..Modifiers::default()
                })
    }

    /// Apply `input` to the current buffer and report any changed path without running host actions.
    pub(in crate::grid) fn edit(
        &mut self,
        input: impl FnOnce(&mut Vim, &mut Register) -> Action,
    ) -> EditResult {
        let vim = &mut self.buffers[self.active].vim;
        let before = vim.doc.revision;
        let action = input(vim, &mut self.register);
        let changed_path = (vim.doc.revision != before)
            .then(|| vim.doc.path.clone())
            .flatten();
        EditResult {
            action,
            changed_path,
        }
    }

    /// Handle native `keystroke` in a viewport of `rows`; printable Insert text remains with IME.
    pub(in crate::grid) fn key(
        &mut self,
        keystroke: &Keystroke,
        rows: usize,
    ) -> Option<NativeKeyResult> {
        let raw = keystroke.key.as_str();
        let modifiers = keystroke.modifiers;
        let character = normal_character(keystroke);
        let key = if modifiers.control || modifiers.platform {
            format!("Ctrl+{raw}")
        } else {
            match raw {
                "escape" => "Escape".into(),
                "enter" => "Enter".into(),
                "backspace" => "Backspace".into(),
                "delete" => "Delete".into(),
                "tab" => "Tab".into(),
                "left" => "Left".into(),
                "right" => "Right".into(),
                "up" => "Up".into(),
                "down" => "Down".into(),
                "home" => "Home".into(),
                "end" => "End".into(),
                "pageup" | "pagedown" => {
                    let direction = if raw == "pageup" { "Up" } else { "Down" };
                    let height = rows;
                    let edit = self.edit(|vim, register| {
                        for _ in 0..height {
                            vim.key(direction, register);
                        }
                        Action::None
                    });
                    return Some(NativeKeyResult {
                        edit,
                        clear_status: false,
                    });
                }
                _ if matches!(
                    self.vim().mode,
                    Mode::Normal | Mode::Visual | Mode::VisualLine
                ) =>
                {
                    character?
                }
                _ => return None,
            }
        };
        Some(NativeKeyResult {
            edit: self.edit(|vim, register| vim.key(&key, register)),
            clear_status: true,
        })
    }
}
