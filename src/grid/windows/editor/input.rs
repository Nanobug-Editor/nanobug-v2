//! Translate native editor keys and apply edits through the window's active Vim model.

use super::{Buffer, EditorWindow, wrap};
use crate::{
    grid::render::Rect,
    ui::{
        keys::normal_character,
        text::{TextSpacing, UNITS_PER_COLUMN},
    },
};
use gpui::{Keystroke, Modifiers};
use nanobug::vim::{Action, HorizontalScroll, Mode, Register, Vim};
use std::{
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
};

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

impl Buffer {
    /// Run a Vim input operation and report the path when it modifies the buffer.
    fn apply_input(&mut self, input: impl FnOnce(&mut Self) -> Action) -> EditResult {
        self.refresh_shared_document();
        let before = self.vim.doc.revision;
        let action = input(self);
        let changed =
            self.vim.doc.revision != before && !self.vim.doc.metadata_revised_since(before);
        if changed {
            self.vim.acknowledge_ex_options_editor_edit();
        }
        let changed_path = changed.then(|| self.vim.doc.path.clone()).flatten();
        EditResult {
            action,
            changed_path,
        }
    }
}

impl EditorWindow {
    /// Supply plugin-provided candidates for the active buffer's native Ex prompt.
    pub(in crate::grid) fn set_language_completion(
        &mut self,
        servers: Vec<String>,
        filetypes: Vec<String>,
    ) {
        self.buffers[self.active]
            .vim
            .set_language_completion(servers, filetypes);
    }

    /// Execute a menu `path` through this pane's input, using its `area` and `spacing` for wrapped motions.
    pub(in crate::grid) fn activate_vim_menu(
        &mut self,
        path: &[String],
        area: Rect,
        spacing: &TextSpacing,
    ) -> EditResult {
        let width = self.text_area(area).w;
        let wrapped = self.visual_rows(width, spacing).is_wrapped();
        let buffer = &mut self.buffers[self.active];
        buffer.apply_input(|buffer| {
            if wrapped {
                let mut resolve =
                    wrap::motion_resolver(&buffer.wraps, width, spacing, &self.composition.text);
                buffer
                    .vim
                    .activate_menu(&mut self.register, path, &mut resolve)
            } else {
                buffer
                    .vim
                    .activate_menu(&mut self.register, path, &mut |_, _, _, _| None)
            }
        })
    }

    /// Resolve a pending user key map after the timeout using `area` and `spacing`.
    pub(in crate::grid) fn finish_mapping_timeout(
        &mut self,
        area: Rect,
        spacing: &TextSpacing,
    ) -> EditResult {
        let width = self.text_area(area).w;
        let wrapped = self.visual_rows(width, spacing).is_wrapped();
        let buffer = &mut self.buffers[self.active];
        buffer.apply_input(|buffer| {
            if wrapped {
                let mut resolve =
                    wrap::motion_resolver(&buffer.wraps, width, spacing, &self.composition.text);
                buffer
                    .vim
                    .finish_mapping_timeout(&mut self.register, &mut resolve)
            } else {
                buffer
                    .vim
                    .finish_mapping_timeout(&mut self.register, &mut |_, _, _, _| None)
            }
        })
    }

    /// Resume Vim playback after a sleep using the measured rows of `area` and `spacing`.
    pub(in crate::grid) fn resume_sleep(
        &mut self,
        area: Rect,
        spacing: &TextSpacing,
    ) -> EditResult {
        let width = self.text_area(area).w;
        let wrapped = self.visual_rows(width, spacing).is_wrapped();
        let buffer = &mut self.buffers[self.active];
        buffer.apply_input(|buffer| {
            if wrapped {
                let mut resolve =
                    wrap::motion_resolver(&buffer.wraps, width, spacing, &self.composition.text);
                buffer.vim.resume_sleep(&mut self.register, &mut resolve)
            } else {
                buffer
                    .vim
                    .resume_sleep(&mut self.register, &mut |_, _, _, _| None)
            }
        })
    }

    /// Apply parser `nodes` for `version` and resume playback using this pane's measured rows.
    pub(in crate::grid) fn apply_syntax_selection(
        &mut self,
        version: (u64, u64),
        nodes: Arc<[Range<usize>]>,
        area: Rect,
        spacing: &TextSpacing,
    ) -> EditResult {
        let width = self.text_area(area).w;
        let wrapped = self.visual_rows(width, spacing).is_wrapped();
        let buffer = &mut self.buffers[self.active];
        buffer.apply_input(|buffer| {
            if wrapped {
                let mut resolve =
                    wrap::motion_resolver(&buffer.wraps, width, spacing, &self.composition.text);
                buffer.vim.apply_syntax_selection_with_screen_rows(
                    version,
                    nodes,
                    &mut self.register,
                    &mut resolve,
                )
            } else {
                buffer
                    .vim
                    .apply_syntax_selection(version, nodes, &mut self.register)
            }
        })
    }

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
        self.buffers[index].apply_input(|buffer| input(&mut buffer.vim, &mut self.register))
    }

    /// Whether the editor has a mapping for this native key before shell shortcuts run.
    pub(in crate::grid) fn wants_native_mapping(&self, keystroke: &Keystroke) -> bool {
        self.native_mapping_key(keystroke).is_some()
    }

    /// Release held keys that belong to the workspace's default Leader or window bindings.
    pub(in crate::grid) fn take_unmapped_shell_prefix(&mut self) -> Option<Vec<String>> {
        self.buffers[self.active]
            .vim
            .take_unmapped_shell_prefix(&self.register)
    }

    /// Match a physical chord while leaving printable text in input modes to native IME.
    fn native_mapping_key(&self, keystroke: &Keystroke) -> Option<String> {
        let key = Self::mapping_key_token(keystroke, self.vim().accepts_text())?;
        self.vim()
            .accepts_input_mapping(&key, &self.register)
            .then_some(key)
    }

    /// Convert a native key to Vim's mapping notation without intercepting printable IME input.
    pub(in crate::grid) fn mapping_key_token(
        keystroke: &Keystroke,
        accepts_text: bool,
    ) -> Option<String> {
        let raw = keystroke.key.as_str();
        let modifiers = keystroke.modifiers;
        if modifiers.platform || modifiers.function {
            return None;
        }
        let named = match raw {
            "escape" => Some("Escape"),
            "enter" => Some("Enter"),
            "backspace" => Some("Backspace"),
            "insert" => Some("Insert"),
            "delete" => Some("Delete"),
            "tab" => Some("Tab"),
            "left" => Some("Left"),
            "right" => Some("Right"),
            "up" => Some("Up"),
            "down" => Some("Down"),
            "home" => Some("Home"),
            "end" => Some("End"),
            "pageup" => Some("PageUp"),
            "pagedown" => Some("PageDown"),
            _ => None,
        };
        let mut prefix = String::new();
        if modifiers.control {
            prefix.push_str("Ctrl+");
        }
        if modifiers.alt {
            prefix.push_str("Alt+");
        }
        let symbolic = named.map(str::to_owned).or_else(|| {
            raw.strip_prefix('f')
                .and_then(|n| n.parse::<u8>().ok())
                .filter(|n| (1..=24).contains(n))
                .map(|n| format!("F{n}"))
        });
        let key = if let Some(symbolic) = symbolic {
            if modifiers.shift {
                prefix.push_str("Shift+");
            }
            format!("{prefix}{symbolic}")
        } else if modifiers.control && !modifiers.alt && matches!(raw, "6" | "^") {
            "Ctrl+6".into()
        } else if modifiers.control && !modifiers.alt && !modifiers.shift {
            match raw {
                "i" => "Tab".into(),
                "m" => "Enter".into(),
                "[" => "Escape".into(),
                "space" => "Ctrl+space".into(),
                _ if raw.len() == 1 => format!("Ctrl+{raw}"),
                _ => return None,
            }
        } else {
            if prefix.is_empty() && accepts_text {
                return None;
            }
            if modifiers.control && !modifiers.alt && modifiers.shift {
                prefix.push_str("Shift+");
            }
            let mut character_key = keystroke.clone();
            character_key.modifiers.control = false;
            character_key.modifiers.alt = false;
            let character = normal_character(&character_key)?;
            format!("{prefix}{character}")
        };
        Some(key)
    }

    /// Handle native `keystroke` in `area` with its measured text `spacing`.
    /// Printable text for editing and literal arguments uses native IME.
    /// Resolve file completion relative to `directory`.
    /// The result includes explicit viewport placements.
    /// With `help_chord`, accept saving and complete Normal-mode commands.
    pub(in crate::grid) fn key(
        &mut self,
        keystroke: &Keystroke,
        area: Rect,
        spacing: &TextSpacing,
        directory: &Path,
        help_chord: bool,
    ) -> Option<NativeKeyResult> {
        let raw = keystroke.key.as_str();
        let modifiers = keystroke.modifiers;
        let character = normal_character(keystroke);
        let mapped_chord = self.native_mapping_key(keystroke);
        let mapped = mapped_chord.is_some();
        if help_chord && mapped {
            return None;
        }
        let prompt = matches!(self.vim().mode, Mode::Command | Mode::Search);
        let shifted_literal = self.vim().mode.accepts_text()
            && modifiers.shift
            && modifiers.control
            && matches!(raw, "v" | "q" | "2" | "@");
        if mapped_chord.is_none()
            && (modifiers.alt
                || modifiers.function
                || modifiers.shift
                    && (modifiers.control || modifiers.platform)
                    && !(modifiers.control && matches!(raw, "6" | "^"))
                    && !shifted_literal)
        {
            return None;
        }
        let key = if let Some(key) = mapped_chord {
            key
        } else if modifiers.control || modifiers.platform {
            match raw {
                "v" if shifted_literal => "Ctrl+Shift+v".into(),
                "q" if shifted_literal => "Ctrl+Shift+q".into(),
                "2" | "@" if shifted_literal => "Ctrl+@".into(),
                "left" if prompt => "Ctrl+Left".into(),
                "right" if prompt => "Ctrl+Right".into(),
                _ => format!("Ctrl+{raw}"),
            }
        } else {
            match raw {
                "escape" => "Escape".into(),
                "enter" if modifiers.shift && !self.vim().accepts_text() => "PageDown".into(),
                "enter" => "Enter".into(),
                "backspace" => "Backspace".into(),
                "insert" if !modifiers.shift => "Insert".into(),
                "delete" => "Delete".into(),
                "tab" if modifiers.shift && self.vim().mode == Mode::Command => "Shift+Tab".into(),
                "tab" => "Tab".into(),
                "left" if modifiers.shift && prompt => "Shift+Left".into(),
                "right" if modifiers.shift && prompt => "Shift+Right".into(),
                "left" if modifiers.shift && !self.vim().accepts_text() => "Shift+Left".into(),
                "right" if modifiers.shift && !self.vim().accepts_text() => "Shift+Right".into(),
                "left" => "Left".into(),
                "right" => "Right".into(),
                "up" if modifiers.shift && !self.vim().mode.is_insert() => "PageUp".into(),
                "down" if modifiers.shift && !self.vim().mode.is_insert() => "PageDown".into(),
                "up" => "Up".into(),
                "down" => "Down".into(),
                "home" => "Home".into(),
                "end" => "End".into(),
                "pageup" => "PageUp".into(),
                "pagedown" => "PageDown".into(),
                key if key
                    .strip_prefix('f')
                    .and_then(|n| n.parse::<u8>().ok())
                    .is_some_and(|n| (1..=24).contains(&n)) =>
                {
                    key.to_ascii_uppercase()
                }
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
        let body = self.text_area(area);
        let width = body.w;
        let (wrapped, has_continuations, physical_top) = {
            let rows = self.visual_rows(width, spacing);
            let buffer = self.buffer();
            let top = buffer.scroll.target();
            let physical_top = if rows.is_wrapped() {
                let row = rows.row(top.min(rows.len().saturating_sub(1)));
                row.map_or(0, |row| buffer.vim.doc.display_row(row.line))
            } else {
                top
            };
            let has_continuations = rows.is_wrapped() && rows.len() > buffer.vim.doc.display_rows();
            (rows.is_wrapped(), has_continuations, physical_top)
        };
        // Recorded keys replay in Vim's physical viewport, so use that same path while recording.
        if has_continuations
            && !mapped
            && !self.register.is_recording_macro()
            && matches!(key.as_str(), "Ctrl+f" | "Ctrl+b" | "PageUp" | "PageDown")
            && let Some(input) = self.page_wrapped(&key, body, spacing)
        {
            return Some(input);
        }
        let alternate_path = self.alternate_path().map(std::path::Path::to_owned);
        let buffer = &mut self.buffers[self.active];
        buffer.vim.set_view(physical_top, Self::body(area).h);
        let left = buffer.columns.target();
        buffer.vim.set_view_width(left, width);
        buffer.vim.set_completion_directory(directory);
        buffer.vim.set_alternate_file(alternate_path);
        let edit = buffer.apply_input(|buffer| {
            if wrapped {
                let mut resolve =
                    wrap::motion_resolver(&buffer.wraps, width, spacing, &self.composition.text);
                buffer
                    .vim
                    .key_with_screen_rows(&key, &mut self.register, &mut resolve)
            } else {
                buffer.vim.key(&key, &mut self.register)
            }
        });
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
        let view_top = buffer.vim.take_view_top();
        let view_top = if wrapped {
            view_top.map(|top| {
                let rows = self.visual_rows(width, spacing);
                rows.first_row(self.vim().doc.document_row(top))
            })
        } else {
            view_top
        };
        Some(NativeKeyResult {
            edit,
            clear_status: !matches!(key.as_str(), "PageUp" | "PageDown"),
            view_top,
            view_left,
        })
    }

    /// Page in the wrapped `body` using Vim's `key`, moving the caret with measured `spacing`.
    /// Leave pending commands and input methods to the regular key handler.
    fn page_wrapped(
        &mut self,
        key: &str,
        body: Rect,
        spacing: &TextSpacing,
    ) -> Option<NativeKeyResult> {
        let page_key = matches!(key, "PageUp" | "PageDown");
        let buffer = &mut self.buffers[self.active];
        let count = if buffer.vim.mode.is_insert() {
            if !page_key || buffer.vim.awaiting_key() || !self.composition.is_empty() {
                return None;
            }
            1
        } else {
            buffer.vim.take_window_count()?.unwrap_or(1)
        };
        let (top, offset) = {
            let rows = buffer.measure_visual_rows(body.w, spacing, &self.composition.text);
            let last = rows.len().saturating_sub(1);
            let (row, span) = buffer.visual_caret(&rows, spacing, &self.composition.text);
            let (top, next) = Vim::page_scroll(
                buffer.scroll.target().min(last),
                body.h,
                last,
                row,
                count,
                matches!(key, "Ctrl+f" | "PageDown"),
            );
            let offset = rows.row(next).filter(|_| next != row).map(|row| {
                let x = span.start as f64 / UNITS_PER_COLUMN as f64;
                buffer.offset_in_visual_row(row, x, spacing, &self.composition.text)
            });
            (top, offset)
        };
        if let Some(offset) = offset {
            buffer.vim.set_cursor(offset);
        }
        Some(NativeKeyResult {
            edit: EditResult {
                action: Action::None,
                changed_path: None,
            },
            clear_status: !page_key,
            view_top: Some(top),
            view_left: None,
        })
    }
}
