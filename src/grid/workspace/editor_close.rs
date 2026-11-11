//! Resolve modified buffers before closing an editor pane, project or window.

use super::{Workspace, projects::WorkspaceTabs};
use crate::{
    grid::{
        mnemonic::Mnemonic,
        render::{Frame, Rect},
    },
    ui::{
        text::TextSpacing,
        theme::{ACCENT, GOLD, MUTED, TEXT},
    },
};
use gpui::{Context, KeyDownEvent, Modifiers, MouseDownEvent, Window};
use nanobug::{document::Document, i18n};
use std::{collections::VecDeque, ops::Range, path::PathBuf};

/// Width of the confirmation in grid columns, including the longest button row.
const COLUMNS: usize = 64;
/// Height through the border, filename and button row, before any error.
const ROWS: usize = 6;
/// Button order and translation keys used for drawing and keyboard matching.
const BUTTONS: [(Choice, &str); 5] = [
    (Choice::Save, "editor_close.yes"),
    (Choice::Discard, "editor_close.no"),
    (Choice::SaveAll, "editor_close.save_all"),
    (Choice::DiscardAll, "editor_close.discard_all"),
    (Choice::Cancel, "editor_close.cancel"),
];

/// The user's response for the current file or the remaining close request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Choice {
    /// Save this file before continuing.
    Save,
    /// Discard this file's edits once the request is ready to close.
    Discard,
    /// Save every remaining file, pausing for Save As or a write error.
    SaveAll,
    /// Discard the remaining files' edits and close.
    DiscardAll,
    /// Keep the editor open and drop any queued discards.
    Cancel,
}

/// Resolve captions for the active language, with bulk choices for `multiple` files.
fn buttons(multiple: bool) -> impl Iterator<Item = (Choice, Mnemonic)> {
    BUTTONS
        .into_iter()
        .filter(move |(choice, _)| {
            multiple || !matches!(choice, Choice::SaveAll | Choice::DiscardAll)
        })
        .map(|(choice, label)| (choice, Mnemonic::parse(i18n::text(label))))
}

/// Measure visible button slots with the same `spacing` as drawing, including bulk choices if `multiple`.
fn button_slots(
    spacing: &TextSpacing,
    multiple: bool,
) -> impl Iterator<Item = (Choice, Mnemonic, Range<usize>)> + '_ {
    let mut start = 2;
    buttons(multiple).map(move |(choice, label)| {
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

/// Match a fresh `event` against visible captions, including bulk choices if `multiple`, or Escape.
fn key_choice(event: &KeyDownEvent, multiple: bool) -> Option<Choice> {
    let key = &event.keystroke;
    let mods = key.modifiers;
    if event.is_held || mods.control || mods.platform || mods.function {
        return None;
    }
    if key.key == "escape" && mods == Modifiers::default() {
        return Some(Choice::Cancel);
    }
    buttons(multiple)
        .find(|(_, label)| label.matches(&key.key))
        .map(|(choice, _)| choice)
}

/// Find the button at grid `(x, y)` in `area`, using the drawn `spacing` and `multiple` choices.
/// Ignore `click_count` above one so a double click cannot answer two files.
fn click_choice(
    area: Rect,
    (x, y): (usize, usize),
    click_count: usize,
    spacing: &TextSpacing,
    multiple: bool,
) -> Option<Choice> {
    if click_count > 1 || !area.contains(x, y) || y != area.y + area.h - 2 {
        return None;
    }
    button_slots(spacing, multiple)
        .find_map(|(choice, _, slot)| slot.contains(&(x - area.x)).then_some(choice))
}

/// The scope of the user's close request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CloseTarget {
    /// The editor pane in the selected project.
    Editor,
    /// The selected project tab.
    Project,
    /// The native window and all its projects.
    Quit,
}

/// Identifies a modified buffer while the dialog holds tab and buffer order fixed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingBuffer {
    /// Project tab that owns the buffer.
    tab: usize,
    /// Buffer index within that project.
    index: usize,
}

impl PendingBuffer {
    /// Read the document for this queued buffer in `projects`.
    fn document(self, projects: &WorkspaceTabs) -> &Document {
        &projects.project(self.tab).editor.buffers[self.index]
            .vim
            .doc
    }
}

/// Pending close decisions while the popup owns input.
pub(super) struct EditorClose {
    /// Operation to complete when every file is resolved.
    target: CloseTarget,
    /// Modified buffers, with the active project and its displayed file first.
    pending: VecDeque<PendingBuffer>,
    /// Buffers to reset only once every remaining decision is resolved.
    discarded: Vec<PendingBuffer>,
    /// Last write or file-picker error for the current buffer.
    error: Option<String>,
    /// A native Save As picker owns input while choosing a filename.
    picking_path: bool,
}

impl EditorClose {
    /// Collect the dirty buffers in `projects` covered by `target`, or let a clean request proceed.
    fn new(projects: &WorkspaceTabs, target: CloseTarget) -> Option<Self> {
        let active = projects
            .active_project()
            .map(|project| (projects.selected, project));
        let others = projects
            .indexed()
            .filter(|(tab, _)| target == CloseTarget::Quit && *tab != projects.selected);
        let mut pending = VecDeque::new();
        for (tab, project) in active.into_iter().chain(others) {
            let editor = &project.editor;
            let order = std::iter::once(editor.active)
                .chain((0..editor.buffers.len()).filter(|&index| index != editor.active));
            pending.extend(order.filter_map(|index| {
                editor.buffers[index]
                    .vim
                    .doc
                    .modified()
                    .then_some(PendingBuffer { tab, index })
            }));
        }
        (!pending.is_empty()).then(|| Self {
            target,
            pending,
            discarded: Vec::new(),
            error: None,
            picking_path: false,
        })
    }

    /// Apply `choice` in `projects`, using `path` for the first buffer when resuming Save As.
    /// Return whether the dialog can end. A picker or write error leaves that buffer pending.
    fn resolve(
        &mut self,
        choice: Choice,
        path: Option<PathBuf>,
        projects: &mut WorkspaceTabs,
    ) -> bool {
        self.error = None;
        match choice {
            Choice::Cancel => return true,
            Choice::Discard => self.discarded.extend(self.pending.drain(..1)),
            Choice::DiscardAll => self.discarded.extend(self.pending.drain(..)),
            Choice::Save | Choice::SaveAll => {
                let mut path = path;
                let count = if choice == Choice::SaveAll {
                    self.pending.len()
                } else {
                    1
                };
                for _ in 0..count {
                    let buffer = self.pending[0];
                    let filename = buffer.document(projects).path.as_ref();
                    if filename.is_none() && path.is_none() {
                        self.picking_path = true;
                        return false;
                    }
                    // Overlapping project roots can retain different drafts for one path.
                    if choice == Choice::SaveAll
                        && let Some(filename) = filename
                        && self
                            .pending
                            .iter()
                            .skip(1)
                            .any(|other| other.document(projects).path.as_ref() == Some(filename))
                    {
                        self.error = Some(i18n::text("editor_close.duplicate_draft").into());
                        return false;
                    }
                    if let Err(error) = save_buffer(projects, buffer, path.take()) {
                        self.error = Some(error.root_cause().to_string());
                        return false;
                    }
                    self.pending.pop_front();
                }
            }
        }
        if !self.pending.is_empty() {
            return false;
        }
        for buffer in self.discarded.drain(..) {
            projects
                .project_mut(buffer.tab)
                .editor
                .discard(buffer.index);
        }
        true
    }
}

/// Save `buffer` in `projects`, preventing Save As to another open buffer's `path`.
fn save_buffer(
    projects: &mut WorkspaceTabs,
    buffer: PendingBuffer,
    path: Option<PathBuf>,
) -> anyhow::Result<()> {
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
    projects.save_buffer(buffer.tab, buffer.index, path)?;
    if save_as && let Some(manager) = &mut projects.project_mut(buffer.tab).manager {
        let _ = manager.tree.refresh();
    }
    Ok(())
}

impl Workspace {
    /// Resolve dirty buffers before closing `target` in `window` through `cx`.
    pub(super) fn request_close(
        &mut self,
        target: CloseTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.begin_close(target) {
            self.finish_close(target, window, cx);
        }
    }

    /// Start confirmation for `target`, returning whether closure must wait.
    pub(super) fn begin_close(&mut self, target: CloseTarget) -> bool {
        let Some(close) = EditorClose::new(&self.projects, target) else {
            return false;
        };
        self.editor_close = Some(close);
        self.help.close();
        self.language_selection = None;
        self.prefix = None;
        self.resizing = false;
        self.hovered_controls = None;
        if let Some(project) = self.projects.active_project_mut() {
            if let Some(manager) = &mut project.manager {
                manager.paste_timer = None;
            }
            project.editor.dragging = None;
            project.editor.composition.clear();
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
        let buffer = close.pending[0];
        let project = self.projects.project(buffer.tab);
        let doc = buffer.document(&self.projects);
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
        if close.target != CloseTarget::Editor {
            frame.label(
                1,
                slot.clone(),
                &spacing.ellipsize(&project.name(), slot.len()),
                MUTED,
                spacing,
            );
        }
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
        for (_, label, slot) in button_slots(spacing, close.pending.len() > 1) {
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
        if let Some(close) = &self.editor_close
            && let Some(choice) = key_choice(event, close.pending.len() > 1)
        {
            self.held_close_key = Some(event.keystroke.key.clone());
            self.choose_editor_close(choice, window, cx);
        }
    }

    /// Handle `event` in `window` through `cx`, consuming clicks behind the dialog.
    pub(super) fn press_editor_close(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let (Some(close), Some(area)) = (&self.editor_close, self.editor_close_area()) else {
            return false;
        };
        let cell = self.metrics.cell(event.position);
        if let Some(choice) = click_choice(
            area,
            cell,
            event.click_count,
            &self.spacing,
            close.pending.len() > 1,
        ) {
            self.choose_editor_close(choice, window, cx);
        }
        true
    }

    /// Dispatch a fresh `choice` in `window` through `cx` while no native picker owns input.
    fn choose_editor_close(&mut self, choice: Choice, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .editor_close
            .as_ref()
            .is_some_and(|close| !close.picking_path)
        {
            self.resolve_editor_close(choice, None, window, cx);
        }
    }

    /// Select a path in `window` through `cx` and resume `choice` only if a path is supplied.
    fn pick_editor_save_path(&self, choice: Choice, window: &Window, cx: &mut Context<Self>) {
        let buffer = self
            .editor_close
            .as_ref()
            .expect("save picker needs a close request")
            .pending[0];
        let picker = cx.prompt_for_new_path(&self.projects.project(buffer.tab).root, None);
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
                            workspace.resolve_editor_close(choice, Some(path), window, cx)
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

    /// Apply `choice` with an optional Save As `path`, advancing the close flow through `window` and `cx`.
    fn resolve_editor_close(
        &mut self,
        choice: Choice,
        path: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(close) = &mut self.editor_close else {
            return;
        };
        if close.resolve(choice, path, &mut self.projects) {
            let target = close.target;
            self.editor_close = None;
            if choice != Choice::Cancel {
                self.finish_close(target, window, cx);
            }
        } else if close.picking_path {
            self.pick_editor_save_path(choice, window, cx);
        }
        self.sync(window, cx);
    }
}

#[cfg(test)]
mod tests;
