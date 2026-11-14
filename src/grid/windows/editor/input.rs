//! Translate native editor keys and apply edits through the window's active Vim model.

use super::EditorWindow;
use crate::{
    grid::render::Rect,
    ui::{
        keys::normal_character,
        text::{TextSpacing, UNITS_PER_COLUMN},
    },
};
use gpui::{Keystroke, Modifiers};
use nanobug::vim::{Action, HorizontalScroll, Mode, Register, Vim};
use std::path::PathBuf;

/// Normal-mode commands that can complete behind the help popup without taking its input.
const HELP_CHORDS: &[&str] = &[
    // Motions, searches for existing terms, and scrolling.
    "B",
    "E",
    "G",
    "H",
    "L",
    "M",
    "N",
    "W",
    "#",
    "$",
    "%",
    "^",
    "*",
    "(",
    ")",
    "{",
    "}",
    "_",
    "+",
    "|",
    "Left",
    "Right",
    "Up",
    "Down",
    "Home",
    "End",
    "PageUp",
    "PageDown",
    "Tab",
    "Backspace",
    "Enter",
    "Ctrl+j",
    "Ctrl+b",
    "Ctrl+d",
    "Ctrl+e",
    "Ctrl+f",
    "Ctrl+i",
    "Ctrl+o",
    "Ctrl+u",
    "Ctrl+y",
    // Complete buffer edits, including repeat substitution and redo.
    "D",
    "J",
    "P",
    "U",
    "X",
    "Y",
    "~",
    "&",
    "Delete",
    "Ctrl+a",
    "Ctrl+r",
    "Ctrl+x",
];

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
    /// Explicit first display row requested by a modal viewport command.
    pub view_top: Option<usize>,
    /// Explicit first measured column requested by a modal viewport command.
    pub view_left: Option<usize>,
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
        self.edit_buffer(self.active, input)
    }

    /// Apply `input` to retained buffer `index` and report the same effects as active input.
    pub(in crate::grid) fn edit_buffer(
        &mut self,
        index: usize,
        input: impl FnOnce(&mut Vim, &mut Register) -> Action,
    ) -> EditResult {
        let vim = &mut self.buffers[index].vim;
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

    /// Handle native `keystroke` in `area` with its measured text `spacing`.
    /// Printable Insert text uses native IME; the result includes explicit viewport placements.
    /// With `help_chord`, accept saving and complete Normal-mode commands.
    pub(in crate::grid) fn key(
        &mut self,
        keystroke: &Keystroke,
        area: Rect,
        spacing: &TextSpacing,
        help_chord: bool,
    ) -> Option<NativeKeyResult> {
        let raw = keystroke.key.as_str();
        let modifiers = keystroke.modifiers;
        let character = normal_character(keystroke);
        if modifiers.alt
            || modifiers.function
            || modifiers.shift && (modifiers.control || modifiers.platform)
        {
            return None;
        }
        let key = if modifiers.control || modifiers.platform {
            format!("Ctrl+{raw}")
        } else {
            match raw {
                "escape" => "Escape".into(),
                "enter" => "Enter".into(),
                "backspace" => "Backspace".into(),
                "insert" if !modifiers.shift => "Insert".into(),
                "delete" => "Delete".into(),
                "tab" => "Tab".into(),
                "left" => "Left".into(),
                "right" => "Right".into(),
                "up" => "Up".into(),
                "down" => "Down".into(),
                "home" => "Home".into(),
                "end" => "End".into(),
                "pageup" => "PageUp".into(),
                "pagedown" => "PageDown".into(),
                _ if !self.vim().accepts_text() => character?,
                _ => return None,
            }
        };
        let vim = self.vim();
        if help_chord
            && key != "Ctrl+s"
            && (vim.mode != Mode::Normal
                || vim.awaiting_key()
                || !HELP_CHORDS.contains(&key.as_str()))
        {
            return None;
        }
        let width = self.text_area(area).w;
        let buffer = &mut self.buffers[self.active];
        buffer
            .vim
            .set_view(buffer.scroll.target(), Self::body(area).h);
        let left = buffer.columns.target();
        buffer.vim.set_view_width(left, width);
        let edit = self.edit(|vim, register| vim.key(&key, register));
        let buffer = &mut self.buffers[self.active];
        let view_left = buffer
            .vim
            .take_horizontal_scroll()
            .map(|horizontal| match horizontal {
                HorizontalScroll::Position(left) => left,
                HorizontalScroll::CursorLeft | HorizontalScroll::CursorRight => {
                    let row = buffer.cursor_row();
                    let (line, _) = buffer.displayed_line(row, "");
                    let span = buffer.caret_span(row, &line, spacing);
                    if horizontal == HorizontalScroll::CursorLeft {
                        span.start / UNITS_PER_COLUMN
                    } else {
                        span.end.div_ceil(UNITS_PER_COLUMN).saturating_sub(width)
                    }
                }
            });
        Some(NativeKeyResult {
            edit,
            clear_status: !matches!(key.as_str(), "PageUp" | "PageDown"),
            view_top: buffer.vim.take_view_top(),
            view_left,
        })
    }
}
