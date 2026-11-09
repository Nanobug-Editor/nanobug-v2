//! Confirm closing the editor, one modified buffer at a time.

use super::{
    Workspace,
    layout::{Focus, ShellWindow},
    projects::Projects,
};
use crate::{
    grid::{
        render::{Frame, LabelStyle, Rect},
        windows::editor::EditorWindow,
    },
    ui::{
        text::TextSpacing,
        theme::{ACCENT, GOLD, TEXT},
    },
};
use gpui::{Context, KeyDownEvent, Modifiers, Pixels, Point, Window};
use nanobug::i18n;
use std::{collections::VecDeque, ops::Range, path::PathBuf};

/// Width of the confirmation and its three buttons in grid columns.
const COLUMNS: usize = 54;
/// Height through the border, filename and button row, before any error.
const ROWS: usize = 6;
/// Button order and translation keys used for drawing and keyboard matching.
const BUTTONS: [(Choice, &str); 3] = [
    (Choice::Save, "editor_close.yes"),
    (Choice::Discard, "editor_close.no"),
    (Choice::Cancel, "editor_close.cancel"),
];

/// The user's response for the file currently shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Choice {
    /// Save this file before continuing.
    Save,
    /// Discard this file's edits once the pane is ready to close.
    Discard,
    /// Keep the editor open and drop any queued discards.
    Cancel,
}

/// A localized caption split around its marked access key.
#[derive(Clone, Copy)]
struct Mnemonic {
    /// Ordinary text before the marked letter.
    prefix: &'static str,
    /// One ASCII letter for both keyboard input and underlining, or empty for a plain caption.
    letter: &'static str,
    /// Ordinary text after the letter.
    suffix: &'static str,
}

impl Mnemonic {
    /// Split a button `label` at its single `&` marker before an ASCII letter.
    fn parse(label: &'static str) -> Self {
        if let Some((prefix, tail)) = label.split_once('&')
            && let Some(letter) = tail
                .get(..1)
                .filter(|s| s.as_bytes()[0].is_ascii_alphabetic())
        {
            Self {
                prefix,
                letter,
                suffix: &tail[1..],
            }
        } else {
            Self {
                prefix: label,
                letter: "",
                suffix: "",
            }
        }
    }

    /// Return the bracketed button in the same spans used for sizing and drawing.
    fn spans(self) -> [(&'static str, LabelStyle); 5] {
        [
            ("[", LabelStyle::Text),
            (self.prefix, LabelStyle::Text),
            (self.letter, LabelStyle::Key),
            (self.suffix, LabelStyle::Text),
            ("]", LabelStyle::Text),
        ]
    }
}

/// Resolve captions for the active language.
fn buttons() -> [(Choice, Mnemonic); BUTTONS.len()] {
    BUTTONS.map(|(choice, label)| (choice, Mnemonic::parse(i18n::text(label))))
}

/// Measure the button hit regions with the same `spacing` as the painted spans.
fn button_slots(spacing: &TextSpacing) -> [(Choice, Mnemonic, Range<usize>); BUTTONS.len()] {
    let mut start = 2;
    buttons().map(|(choice, label)| {
        let width: usize = label
            .spans()
            .map(|(text, _)| spacing.columns(text))
            .into_iter()
            .sum();
        let slot = start..start + width;
        start = slot.end + 2;
        (choice, label, slot)
    })
}

/// Match a fresh `event` against the localized `buttons` or the standard Escape action.
fn key_choice(
    event: &KeyDownEvent,
    buttons: [(Choice, Mnemonic); BUTTONS.len()],
) -> Option<Choice> {
    let key = &event.keystroke;
    let mods = key.modifiers;
    if event.is_held || mods.control || mods.platform || mods.function {
        return None;
    }
    if key.key == "escape" && mods == Modifiers::default() {
        return Some(Choice::Cancel);
    }
    buttons
        .into_iter()
        .find(|(_, label)| !label.letter.is_empty() && label.letter.eq_ignore_ascii_case(&key.key))
        .map(|(choice, _)| choice)
}

/// Pending decisions while the popup owns input and the buffer order is fixed.
pub(super) struct EditorClose {
    /// Dirty buffer indices to ask about, starting with the displayed file.
    pending: VecDeque<usize>,
    /// Buffer indices to reset only after every file is resolved.
    discarded: Vec<usize>,
    /// Last write or file-picker error for the current buffer.
    error: Option<String>,
    /// A native Save As picker owns input while choosing a filename.
    picking_path: bool,
}

impl EditorClose {
    /// Collect the dirty buffers in `editor`, or return None if it can close immediately.
    fn new(editor: &EditorWindow) -> Option<Self> {
        let pending: VecDeque<_> = std::iter::once(editor.active)
            .chain((0..editor.buffers.len()).filter(|&index| index != editor.active))
            .filter(|&index| editor.buffers[index].vim.doc.modified())
            .collect();
        (!pending.is_empty()).then(|| Self {
            pending,
            discarded: Vec::new(),
            error: None,
            picking_path: false,
        })
    }

    /// Apply `choice` in `projects`, saving to `path` only for Save As.
    /// Return whether the dialog can end. Save errors leave the current buffer pending.
    fn resolve(&mut self, choice: Choice, path: Option<PathBuf>, projects: &mut Projects) -> bool {
        let index = self.pending[0];
        match choice {
            Choice::Cancel => return true,
            Choice::Discard => self.discarded.push(index),
            Choice::Save => {
                if let Err(error) = save_buffer(projects, index, path) {
                    self.error = Some(error.root_cause().to_string());
                    return false;
                }
            }
        }
        self.error = None;
        self.pending.pop_front();
        if !self.pending.is_empty() {
            return false;
        }
        for index in self.discarded.drain(..) {
            projects.active.editor.discard(index);
        }
        true
    }
}

/// Save buffer `index` in `projects`, preventing Save As to another open buffer's `path`.
fn save_buffer(projects: &mut Projects, index: usize, path: Option<PathBuf>) -> anyhow::Result<()> {
    if let Some(path) = &path {
        let path = path.canonicalize().unwrap_or_else(|_| path.clone());
        if projects.ordered().any(|project| {
            project
                .editor
                .buffers
                .iter()
                .any(|buffer| buffer.vim.doc.path.as_ref() == Some(&path))
        }) {
            anyhow::bail!(i18n::text("editor_close.path_open"));
        }
    }
    let save_as = path.is_some();
    projects.save_buffer(index, path)?;
    if save_as {
        let _ = projects.active.manager.tree.refresh();
    }
    Ok(())
}

impl Workspace {
    /// Start confirmation for the active editor, returning whether it must stay visible for now.
    pub(super) fn begin_editor_close(&mut self) -> bool {
        let Some(close) = EditorClose::new(&self.projects.active.editor) else {
            return false;
        };
        if self.projects.active.hidden_pane.is_some()
            && let Some(message) = self.projects.inactive_exit_message()
        {
            self.projects.active.status = message;
        } else {
            self.editor_close = Some(close);
            self.resizing = false;
            self.projects.active.editor.dragging = None;
            self.projects.active.editor.composition.clear();
        }
        true
    }

    /// Place the confirmation on the shared popup surface while it owns input.
    pub(super) fn editor_close_area(&self) -> Option<Rect> {
        self.editor_close
            .as_ref()
            .map(|close| self.small_popup_area(COLUMNS, ROWS + usize::from(close.error.is_some())))
    }

    /// Paint the current filename, any error and the marked captions through `cx`.
    pub(super) fn refresh_editor_close(&self, cx: &mut Context<Self>) {
        let (Some(close), Some(area)) = (&self.editor_close, self.editor_close_area()) else {
            return;
        };
        let project = &self.projects.active;
        let doc = &project.editor.buffers[close.pending[0]].vim.doc;
        let filename = doc.path.as_deref().map_or_else(
            || i18n::text("editor.untitled").into(),
            |path| {
                path.strip_prefix(&project.root)
                    .unwrap_or(path)
                    .to_string_lossy()
            },
        );
        let spacing = &self.spacing;
        let slot = 2..area.w - 2;
        let mut frame = Frame::new(area.w, area.h);
        frame.border(i18n::text("editor_close.title"), true, false, spacing);
        frame.label(
            2,
            slot.clone(),
            &spacing.ellipsize(&filename, slot.len()),
            TEXT,
            spacing,
        );
        if let Some(error) = &close.error {
            frame.label(
                3,
                slot.clone(),
                &spacing.ellipsize(error, slot.len()),
                GOLD,
                spacing,
            );
        }
        for (_, label, slot) in button_slots(spacing) {
            let mut start = slot.start;
            for (text, style) in label.spans() {
                start =
                    frame.styled_label(area.h - 2, start..slot.end, text, ACCENT, spacing, style);
            }
        }
        self.popup_panel
            .update(cx, |panel, cx| panel.set(frame, cx));
    }

    /// Use `event` to choose a localized button in `window` through `cx`.
    pub(super) fn editor_close_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(choice) = key_choice(event, buttons()) {
            self.held_close_key = Some(event.keystroke.key.clone());
            self.choose_editor_close(choice, window, cx);
        }
    }

    /// Handle a press at `position` in `window` through `cx`, consuming clicks behind the dialog.
    pub(super) fn press_editor_close(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(area) = self.editor_close_area() else {
            return false;
        };
        let (x, y) = self.metrics.cell(position);
        if area.contains(x, y)
            && y == area.y + area.h - 2
            && let Some((choice, _, _)) = button_slots(&self.spacing)
                .into_iter()
                .find(|(_, _, slot)| slot.contains(&(x - area.x)))
        {
            self.choose_editor_close(choice, window, cx);
        }
        true
    }

    /// Dispatch `choice` in `window` through `cx`, asking for a path for unnamed buffers.
    fn choose_editor_close(&mut self, choice: Choice, window: &mut Window, cx: &mut Context<Self>) {
        let Some(close) = self
            .editor_close
            .as_mut()
            .filter(|close| !close.picking_path)
        else {
            return;
        };
        if choice == Choice::Save
            && self.projects.active.editor.buffers[close.pending[0]]
                .vim
                .doc
                .path
                .is_none()
        {
            close.picking_path = true;
            self.pick_editor_save_path(window, cx);
        } else {
            self.resolve_editor_close(choice, None, window, cx);
        }
    }

    /// Select a Save As path from the active project directory in `window` through `cx`.
    fn pick_editor_save_path(&self, window: &Window, cx: &mut Context<Self>) {
        let picker = cx.prompt_for_new_path(&self.projects.active.root, None);
        cx.spawn_in(window, async move |view, cx| {
            let result = match picker.await {
                Ok(result) => result,
                Err(error) => Err(error.into()),
            };
            let _ = cx.update(|window, cx| {
                let _ = view.update(cx, |workspace, cx| {
                    let Some(close) = workspace
                        .editor_close
                        .as_mut()
                        .filter(|close| close.picking_path)
                    else {
                        return;
                    };
                    close.picking_path = false;
                    match result {
                        Ok(Some(path)) => {
                            workspace.resolve_editor_close(Choice::Save, Some(path), window, cx)
                        }
                        Ok(None) => {}
                        Err(error) => {
                            close.error = Some(error.root_cause().to_string());
                            workspace.sync(window, cx);
                        }
                    }
                });
            });
        })
        .detach();
    }

    /// Apply `choice` with an optional Save As `path`, then update `window` through `cx`.
    fn resolve_editor_close(
        &mut self,
        choice: Choice,
        path: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(close) = &mut self.editor_close
            && close.resolve(choice, path, &mut self.projects)
        {
            self.editor_close = None;
            if choice != Choice::Cancel {
                self.finish_grid_close(ShellWindow::Pane(Focus::Editor), window);
            }
        }
        self.sync(window, cx);
    }
}

#[cfg(test)]
mod tests;
