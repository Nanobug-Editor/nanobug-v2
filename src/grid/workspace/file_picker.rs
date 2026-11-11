//! Connect the picker to workspace tabs, native input, and background filesystem work.

mod operations;

use super::{Workspace, layout::ShellWindow};
use crate::{
    grid::{
        PreviewHighlighter,
        render::{GridLayer, Metrics, Panel, Rect},
        windows::{
            PathChange,
            file_picker::{
                Browse, Delete, FilePicker, Listing, PickerLayout, Preview, Prompt, PromptKind,
                delete_buttons, jump_path,
            },
            manager::{FileTransfers, TransferKind},
        },
    },
    ui::keys::normal_character,
};
use gpui::{AppContext, Context, Entity, KeyDownEvent, MouseDownEvent, Task, Window};
use nanobug::i18n;
use operations::Operation;
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

/// One floating message, with its panel bounds and a dismissal canceled on replacement.
struct PickerMessage {
    text: String,
    area: Rect,
    _dismiss: Task<()>,
}

/// State and retained panels owned by a single opening of the picker.
pub(super) struct PickerSession {
    /// Logical browsing state.
    pub(super) state: FilePicker,
    /// Picker border and directory rows, also used by cursor effects.
    pub(super) panel: Entity<Panel>,
    /// Right-column text and syntax colors.
    text_panel: Entity<Panel>,
    /// Floating picker feedback.
    message_panel: Entity<Panel>,
    /// Pending directory load, canceled when the session closes.
    scan: Option<Task<()>>,
    /// Path of the pending or displayed preview.
    preview_path: Option<PathBuf>,
    /// Preview delivery, replaced when its selected path changes.
    preview: Option<Task<()>>,
    /// Active filesystem operation whose result must be reconciled.
    operation: Option<Task<()>>,
    /// Copy or cut clipboard and transaction staging.
    transfers: FileTransfers,
    /// Most recent feedback, visible for two seconds.
    message: Option<PickerMessage>,
}

impl PickerSession {
    /// Allocate the panels using workspace grid `metrics` through `cx`.
    fn new(metrics: Metrics, cx: &mut Context<Workspace>) -> Self {
        Self {
            state: FilePicker::default(),
            panel: cx.new(|_| Panel::new(metrics)),
            text_panel: cx.new(|_| Panel::new(metrics)),
            message_panel: cx.new(|_| Panel::new(metrics)),
            scan: None,
            preview_path: None,
            preview: None,
            operation: None,
            transfers: FileTransfers::default(),
            message: None,
        }
    }
}

impl Workspace {
    /// Open a picker in `window` from the project root or last browsed directory.
    pub(super) fn open_file_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.file_picker.is_some() {
            return;
        }
        let initial = self
            .projects
            .active_project()
            .map(|project| project.root.clone())
            .or_else(|| self.file_picker_directory.clone())
            .or_else(|| self.recent_projects.roots.first().cloned())
            .or_else(dirs::home_dir)
            .or_else(|| std::env::current_dir().ok());
        self.prefix = None;
        self.hovered_controls = None;
        self.resizing = false;
        self.wheel = 0.;
        self.file_picker = Some(PickerSession::new(self.metrics, cx));
        if let Some(initial) = initial {
            self.browse_picker(Browse::Path(initial, None), window, cx);
        } else {
            self.show_picker_message(i18n::text("picker.no_directory").into(), window, cx);
            self.sync(window, cx);
        }
    }

    /// Whether a picker operation still needs to apply its result before the window can close.
    pub(super) fn picker_operation_pending(&self) -> bool {
        self.file_picker
            .as_ref()
            .is_some_and(|session| session.operation.is_some())
    }

    /// Position the picker over the work area below the project tabs.
    pub(super) fn file_picker_layout(&self) -> PickerLayout {
        PickerLayout::new(
            Rect {
                x: 0,
                y: 1,
                w: self.layout.cols,
                h: self.layout.rows.saturating_sub(1),
            },
            self.file_picker.as_ref().map(|session| &session.state),
        )
    }

    /// Return the active filter's floating rectangle in workspace grid coordinates.
    pub(super) fn file_picker_filter_area(&self) -> Option<Rect> {
        self.file_picker
            .as_ref()?
            .state
            .filter_area(self.file_picker_layout())
    }

    /// Whether the picker can expose its border controls without interrupting an operation.
    pub(super) fn file_picker_controls_available(&self) -> bool {
        self.file_picker
            .as_ref()
            .is_some_and(|session| !session.state.busy && session.state.delete.is_none())
    }

    /// Open the picker shortcuts from its keyboard binding or border button.
    pub(super) fn open_file_picker_help(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.small_modal_open() || self.help.is_open() || !self.file_picker_controls_available()
        {
            return;
        }
        self.dismiss_leader_hint(cx);
        let state = &mut self.file_picker.as_mut().expect("active picker").state;
        state.goto = false;
        state.composition.clear();
        self.hovered_controls = None;
        self.help
            .open_file_picker(self.projects.manager_available());
        self.sync(window, cx);
    }

    /// Refresh picker and floating panels through `cx`, clamping resized viewports.
    pub(super) fn refresh_file_picker(&mut self, cx: &mut Context<Self>) {
        let layout = self.file_picker_layout();
        let active = !self.help.is_open() && !self.small_modal_open();
        let Some(session) = &mut self.file_picker else {
            return;
        };
        session.state.reveal(layout.current.h);
        session.state.scroll_preview(0, layout.preview.h);
        let controls = self.hovered_controls == Some(ShellWindow::FilePicker);
        let frame = session.state.frame(layout, active, controls, &self.spacing);
        session.panel.update(cx, |panel, cx| panel.set(frame, cx));
        if let Some(frame) = session.state.text_frame(layout, &self.spacing) {
            session
                .text_panel
                .update(cx, |panel, cx| panel.set(frame, cx));
        }
        if let Some(frame) = session.state.filter_frame(layout, &self.spacing) {
            self.popup_panel
                .update(cx, |panel, cx| panel.set(frame, cx));
        }
        if let Some(message) = &mut session.message {
            let (area, frame) = layout.message_frame(&message.text, &self.spacing);
            message.area = area;
            session
                .message_panel
                .update(cx, |panel, cx| panel.set(frame, cx));
        }
    }

    /// Return the picker chrome, text preview and floating feedback in draw order.
    pub(super) fn file_picker_layers(&self) -> Option<Vec<GridLayer>> {
        let session = self.file_picker.as_ref()?;
        let layout = self.file_picker_layout();
        let mut layers = vec![GridLayer {
            panel: session.panel.clone(),
            bounds: layout.area.pixels(self.metrics),
            clip: None,
        }];
        if session.state.delete.is_none() && matches!(session.state.preview, Preview::Text(_)) {
            let bounds = layout.text_area().pixels(self.metrics);
            layers.push(GridLayer {
                panel: session.text_panel.clone(),
                bounds,
                clip: Some(bounds),
            });
        }
        if !self.help.is_open()
            && let Some(message) = &session.message
        {
            layers.push(GridLayer {
                panel: session.message_panel.clone(),
                bounds: message.area.pixels(self.metrics),
                clip: None,
            });
        }
        Some(layers)
    }

    /// Scan the new `request`, preserving the current listing until it succeeds.
    fn browse_picker(&mut self, request: Browse, window: &mut Window, cx: &mut Context<Self>) {
        let session = self.file_picker.as_mut().expect("active picker");
        session.state.busy = true;
        session.state.goto = false;
        let load = cx
            .background_executor()
            .spawn(async move { Listing::read(request) });
        session.scan = Some(cx.spawn_in(window, async move |view, cx| {
            let listing = load.await;
            let _ = cx.update(|window, cx| {
                let _ = view.update(cx, |workspace, cx| {
                    let Some(session) = &mut workspace.file_picker else {
                        return;
                    };
                    session.state.busy = false;
                    match listing {
                        Ok(listing) => {
                            workspace
                                .file_picker_directory
                                .clone_from(&listing.directory);
                            session.state.install(listing, false);
                        }
                        Err(error) => {
                            workspace.show_picker_message(format!("{error:#}"), window, cx)
                        }
                    }
                    workspace.update_picker(window, cx);
                });
            });
        }));
        self.sync(window, cx);
    }

    /// Read the newly selected preview and then apply any installed language service colors.
    fn sync_picker_preview(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(session) = &mut self.file_picker else {
            return;
        };
        let target = session
            .state
            .entry()
            .map(|entry| (entry.path.clone(), entry.directory));
        if session.preview_path.as_ref() == target.as_ref().map(|(path, _)| path) {
            return;
        }
        session.preview = None;
        session.preview_path = target.as_ref().map(|(path, _)| path.clone());
        session.state.preview_first = 0;
        session.state.preview = Preview::Empty;
        let Some((path, directory)) = target else {
            return;
        };
        session.state.preview = Preview::Loading;
        let highlighter = cx
            .try_global::<PreviewHighlighter>()
            .map(|provider| provider.0.clone());
        let requested = path.clone();
        let reader = cx
            .background_executor()
            .spawn(async move { Preview::read(&requested, directory) });
        session.preview = Some(cx.spawn_in(window, async move |view, cx| {
            let preview = reader.await;
            let text = match &preview {
                Preview::Text(text) => Some(text.text.clone()),
                _ => None,
            };
            let _ = cx.update(|_, cx| {
                let _ = view.update(cx, |workspace, cx| {
                    if let Some(session) = workspace
                        .file_picker
                        .as_mut()
                        .filter(|session| session.preview_path.as_ref() == Some(&path))
                    {
                        session.state.preview = preview;
                        workspace.refresh_file_picker(cx);
                        cx.notify();
                    }
                });
            });
            if let (Some(text), Some(highlighter)) = (text, highlighter) {
                let requested = path.clone();
                let colors = cx
                    .background_executor()
                    .spawn(async move { highlighter(&requested, &text) })
                    .await;
                let _ = cx.update(|_, cx| {
                    let _ = view.update(cx, |workspace, cx| {
                        if let Some(session) = workspace
                            .file_picker
                            .as_mut()
                            .filter(|session| session.preview_path.as_ref() == Some(&path))
                            && let Preview::Text(preview) = &mut session.state.preview
                        {
                            preview.set_colors(colors);
                            workspace.refresh_file_picker(cx);
                            cx.notify();
                        }
                    });
                });
            }
        }));
    }

    /// Refresh selection, preview and native input in `window`.
    fn update_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_picker_preview(window, cx);
        self.sync(window, cx);
    }

    /// Replace the picker's message and dismiss it after two seconds without changing selection.
    fn show_picker_message(&mut self, message: String, window: &Window, cx: &mut Context<Self>) {
        let session = self.file_picker.as_mut().expect("active picker");
        let text = message.replace(['\r', '\n', '\t'], " ");
        if text.trim().is_empty() {
            session.message = None;
            return;
        }
        let delay = cx.background_executor().timer(Duration::from_secs(2));
        let dismiss = cx.spawn_in(window, async move |view, cx| {
            delay.await;
            let _ = cx.update(|_, cx| {
                let _ = view.update(cx, |workspace, cx| {
                    if let Some(session) = &mut workspace.file_picker {
                        session.message = None;
                        cx.notify();
                    }
                });
            });
        });
        session.message = Some(PickerMessage {
            text,
            area: Rect::default(),
            _dismiss: dismiss,
        });
    }

    /// Route an active picker `event`. False leaves text and IME keys to native input.
    pub(super) fn file_picker_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let key = &event.keystroke;
        let raw = key.key.as_str();
        let character = normal_character(key);
        let command = character.as_deref().unwrap_or(raw);
        let modifiers = key.modifiers;
        let preview_height = self.file_picker_layout().preview.h;
        let session = self.file_picker.as_mut().expect("active picker");
        if session.state.prompt.is_some() {
            if (modifiers.control || modifiers.platform) && !modifiers.alt && raw == "v" {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    self.file_picker_commit(&text, window, cx);
                }
                return true;
            }
            if !session.state.composition.is_empty() || modifiers.alt || modifiers.function {
                return false;
            }
            if (modifiers.control || modifiers.platform) && raw == "a" {
                session
                    .state
                    .prompt
                    .as_mut()
                    .expect("prompt")
                    .edit("select-all");
                self.sync(window, cx);
                return true;
            }
            if modifiers.control || modifiers.platform || modifiers.shift {
                return false;
            }
            if matches!(raw, "enter" | "escape") {
                if !event.is_held {
                    self.finish_picker_prompt(raw == "escape", window, cx);
                }
                return true;
            }
            let preferred = session.state.entry().map(|entry| entry.path.clone());
            let prompt = session.state.prompt.as_mut().expect("prompt");
            if !prompt.edit(raw) {
                return false;
            }
            if matches!(prompt.kind, PromptKind::Filter) {
                session.state.refilter(preferred.as_deref());
            }
            self.update_picker(window, cx);
            return true;
        }
        if modifiers.alt || modifiers.control || modifiers.platform || modifiers.function {
            return true;
        }
        if session.state.busy {
            if matches!(command, "q" | "escape") && session.operation.is_none() && !event.is_held {
                self.dismiss_file_picker(raw, window, cx);
            }
            return true;
        }
        if session.state.delete.is_some() {
            if matches!(command, "J" | "K") {
                session
                    .state
                    .scroll_preview(if command == "J" { 5 } else { -5 }, preview_height);
                self.sync(window, cx);
                return true;
            }
            if !event.is_held {
                let confirm = match raw {
                    "enter" => Some(true),
                    "escape" => Some(false),
                    _ => delete_buttons()
                        .into_iter()
                        .find_map(|(confirm, label)| label.matches(raw).then_some(confirm)),
                };
                if let Some(confirm) = confirm {
                    self.confirm_picker_delete(confirm, window, cx);
                }
            }
            return true;
        }
        if event.is_held
            && (session.state.goto || !matches!(command, "j" | "k" | "up" | "down" | "J" | "K"))
        {
            return true;
        }
        if matches!(command, "q" | "escape") {
            self.dismiss_file_picker(raw, window, cx);
            return true;
        }
        if session.state.goto {
            session.state.goto = false;
            match command {
                "g" => session.state.selected = 0,
                "h" | "d" => {
                    let path = if command == "h" {
                        dirs::home_dir()
                    } else {
                        dirs::download_dir()
                    };
                    if let Some(path) = path {
                        self.browse_picker(Browse::Path(path, None), window, cx);
                        return true;
                    }
                    self.show_picker_message(i18n::text("picker.no_directory").into(), window, cx);
                }
                " " => self.start_picker_prompt(PromptKind::Jump, String::new(), 0),
                _ => {}
            }
            self.update_picker(window, cx);
            return true;
        }
        let session = self.file_picker.as_mut().expect("active picker");
        match command {
            "j" | "down" => {
                session.state.selected =
                    (session.state.selected + 1).min(session.state.visible.len().saturating_sub(1))
            }
            "k" | "up" => session.state.selected = session.state.selected.saturating_sub(1),
            "G" | "end" => session.state.selected = session.state.visible.len().saturating_sub(1),
            "home" => session.state.selected = 0,
            "J" => session.state.scroll_preview(5, preview_height),
            "K" => session.state.scroll_preview(-5, preview_height),
            "g" => session.state.goto = true,
            "h" | "left" => {
                if let Some(directory) = session.state.directory() {
                    if let Some(parent) = directory.parent() {
                        let request =
                            Browse::Path(parent.to_path_buf(), Some(directory.to_path_buf()));
                        self.browse_picker(request, window, cx);
                        return true;
                    }
                    #[cfg(windows)]
                    {
                        let request = Browse::Drives(Some(directory.to_path_buf()));
                        self.browse_picker(request, window, cx);
                        return true;
                    }
                }
            }
            "l" | "right" => {
                if let Some(entry) = session.state.entry().filter(|entry| entry.directory) {
                    let request = Browse::Path(entry.path.clone(), None);
                    self.browse_picker(request, window, cx);
                    return true;
                }
            }
            "enter" => {
                self.open_picker_selection(window, cx);
                return true;
            }
            "O" => {
                if let Some(entry) = session.state.entry().filter(|entry| !entry.directory)
                    && !matches!(session.state.preview, Preview::Text(_))
                {
                    let operation = Operation::External(entry.path.clone());
                    self.run_picker_operation(operation, window, cx);
                    return true;
                }
            }
            "f" => {
                let filter = session.state.filter.clone();
                let end = filter.len();
                self.start_picker_prompt(PromptKind::Filter, filter, end);
            }
            "a" if session.state.directory().is_some() => {
                self.start_picker_prompt(PromptKind::Create, String::new(), 0)
            }
            "r" if session.state.directory().is_some() => {
                if let Some(entry) = session.state.entry()
                    && let Some(name) = entry.path.file_name().and_then(|name| name.to_str())
                {
                    let end = if entry.directory {
                        name.len()
                    } else {
                        entry
                            .path
                            .file_stem()
                            .and_then(|stem| stem.to_str())
                            .map_or(name.len(), str::len)
                    };
                    let kind = PromptKind::Rename(entry.path.clone());
                    let name = name.to_owned();
                    self.start_picker_prompt(kind, name, end);
                }
            }
            "s" if session.state.directory().is_some() => {
                if let Some(path) = session.state.entry().map(|entry| entry.path.clone())
                    && !session.state.marked.remove(&path)
                {
                    session.state.marked.insert(path);
                }
                let message =
                    i18n::format("manager.marked", &[("count", &session.state.marked.len())]);
                self.show_picker_message(message, window, cx);
            }
            "y" | "c" | "x" => {
                let cut = command == "x";
                let kind = if cut {
                    TransferKind::Cut
                } else {
                    TransferKind::Copy
                };
                let message = match session.transfers.capture(&session.state.marked, kind) {
                    Ok(count) => i18n::format(
                        if cut { "manager.cut" } else { "manager.copied" },
                        &[("count", &count)],
                    ),
                    Err(error) => error.to_string(),
                };
                self.show_picker_message(message, window, cx);
            }
            "v" | "p" if session.state.directory().is_some() => {
                self.run_picker_operation(Operation::Paste, window, cx);
                return true;
            }
            "d" | "D" if session.state.directory().is_some() => {
                let mut paths: Vec<_> = session
                    .state
                    .marked
                    .iter()
                    .filter(|path| {
                        !path
                            .ancestors()
                            .skip(1)
                            .any(|parent| session.state.marked.contains(parent))
                    })
                    .cloned()
                    .collect();
                if paths.is_empty()
                    && let Some(entry) = session.state.entry()
                {
                    paths.push(entry.path.clone());
                }
                if !paths.is_empty() {
                    paths.sort();
                    session.state.preview_first = 0;
                    session.state.delete = Some(Delete {
                        paths,
                        permanent: command == "D",
                    });
                }
            }
            "?" => {
                self.open_file_picker_help(window, cx);
                return true;
            }
            _ => return true,
        }
        self.update_picker(window, cx);
        true
    }

    /// Start editing `kind` with its initial `value` selected through byte `end`.
    fn start_picker_prompt(&mut self, kind: PromptKind, value: String, end: usize) {
        let state = &mut self.file_picker.as_mut().expect("active picker").state;
        state.prompt = Some(Prompt::new(kind, value, end));
        state.composition.clear();
    }

    /// Commit native `text` to the current prompt and refresh a live filter.
    pub(super) fn file_picker_commit(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = &mut self.file_picker.as_mut().expect("active picker").state;
        let preferred = state.entry().map(|entry| entry.path.clone());
        let Some(prompt) = &mut state.prompt else {
            return;
        };
        prompt.insert(text);
        state.composition.clear();
        if matches!(prompt.kind, PromptKind::Filter) {
            state.refilter(preferred.as_deref());
        }
        self.update_picker(window, cx);
    }

    /// Apply or `cancel` the prompt, dispatching filesystem work only for committed edits.
    fn finish_picker_prompt(&mut self, cancel: bool, window: &mut Window, cx: &mut Context<Self>) {
        let state = &mut self.file_picker.as_mut().expect("active picker").state;
        let preferred = state.entry().map(|entry| entry.path.clone());
        let prompt = state.prompt.take().expect("active prompt");
        state.composition.clear();
        if cancel {
            state.refilter(preferred.as_deref());
        } else {
            match prompt.kind {
                PromptKind::Filter => {
                    state.filter = prompt.value;
                    state.refilter(preferred.as_deref());
                    if !state.filter.is_empty() {
                        let message = format!("{} · {}", i18n::text("picker.filter"), state.filter);
                        self.show_picker_message(message, window, cx);
                    }
                }
                PromptKind::Jump => match jump_path(&prompt.value, state.directory()) {
                    Ok(path) => {
                        self.browse_picker(Browse::Path(path, None), window, cx);
                        return;
                    }
                    Err(error) => self.show_picker_message(error.to_string(), window, cx),
                },
                PromptKind::Create => {
                    self.run_picker_operation(Operation::Create(prompt.value), window, cx);
                    return;
                }
                PromptKind::Rename(path) => {
                    self.run_picker_operation(Operation::Rename(path, prompt.value), window, cx);
                    return;
                }
            }
        }
        self.update_picker(window, cx);
    }

    /// Open the highlighted directory as a project or the highlighted file in its own tab.
    fn open_picker_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = &self.file_picker.as_ref().expect("active picker").state;
        let Some(entry) = state.entry() else { return };
        if !entry.directory
            && (matches!(state.preview, Preview::Unavailable)
                || Preview::requires_system_open(&entry.path))
        {
            self.show_picker_message(i18n::text("picker.use_external").into(), window, cx);
            self.sync(window, cx);
            return;
        }
        let result = fs::canonicalize(&entry.path)
            .map_err(anyhow::Error::from)
            .and_then(|path| {
                if path.is_dir() {
                    // Keep the picker open when the directory cannot be read as a project.
                    fs::read_dir(&path)?;
                    self.open_project_path(path, None, window, cx);
                    Ok(())
                } else {
                    self.open_standalone_file(path, window, cx)
                }
            });
        match result {
            Ok(()) => self.dismiss_file_picker("enter", window, cx),
            Err(error) => {
                self.show_picker_message(format!("{error:#}"), window, cx);
                self.sync(window, cx);
            }
        }
    }

    /// Dismiss the picker, ignoring repeats of the `key` that closed it.
    fn dismiss_file_picker(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.close_grid_window(ShellWindow::FilePicker, window, cx);
        self.held_close_key = Some(key.into());
        self.sync(window, cx);
    }

    /// Confirm or cancel the selected deletion through the same keyboard and pointer action.
    fn confirm_picker_delete(
        &mut self,
        confirm: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = &mut self.file_picker.as_mut().expect("active picker").state;
        let delete = state.delete.take();
        state.preview_first = 0;
        if let Some(delete) = delete.filter(|_| confirm) {
            self.run_picker_operation(Operation::Delete(delete), window, cx);
        } else {
            self.sync(window, cx);
        }
    }

    /// Apply a background `operation`, then reconcile open buffers and refresh the listing.
    fn run_picker_operation(
        &mut self,
        operation: Operation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dirty = self
            .projects
            .ordered()
            .flat_map(|project| project.editor.dirty_paths())
            .collect::<Vec<_>>();
        let roots = self
            .projects
            .ordered()
            .filter(|project| project.manager.is_some())
            .map(|project| project.root.clone())
            .collect::<Vec<_>>();
        let session = self.file_picker.as_mut().expect("active picker");
        let directory = session.state.directory().map(Path::to_path_buf);
        let previous = session.state.entry().map(|entry| entry.path.clone());
        let mut transfers = std::mem::take(&mut session.transfers);
        session.state.busy = true;
        let worker = cx.background_executor().spawn(async move {
            let refresh = !matches!(operation, Operation::External(_));
            let outcome = operation.run(directory.as_deref(), &mut transfers, &dirty, &roots);
            let listing = if refresh {
                directory.as_deref().and_then(|directory| {
                    let target = directory.ancestors().find(|path| path.is_dir())?;
                    Some(Listing::read(Browse::Path(
                        target.to_path_buf(),
                        outcome.select.clone().or(previous),
                    )))
                })
            } else {
                None
            };
            (outcome, transfers, listing)
        });
        session.operation = Some(cx.spawn_in(window, async move |view, cx| {
            let (outcome, transfers, listing) = worker.await;
            let _ = cx.update(|window, cx| {
                let _ = view.update(cx, |workspace, cx| {
                    workspace.apply_picker_path_changes(&outcome.changes);
                    let Some(session) = &mut workspace.file_picker else {
                        return;
                    };
                    session.operation = None;
                    session.transfers = transfers;
                    session.state.busy = false;
                    let mut message = outcome.status;
                    if let Some(listing) = listing {
                        session.preview = None;
                        session.preview_path = None;
                        match listing {
                            Ok(listing) => {
                                let keep_filter =
                                    session.state.directory() == listing.directory.as_deref();
                                workspace
                                    .file_picker_directory
                                    .clone_from(&listing.directory);
                                session.state.install(listing, keep_filter);
                            }
                            Err(error) => {
                                message = if message.is_empty() {
                                    format!("{error:#}")
                                } else {
                                    format!("{message} · {error:#}")
                                };
                            }
                        }
                    }
                    session.state.marked.retain(|path| {
                        !outcome
                            .changes
                            .iter()
                            .any(|change| path.starts_with(&change.from))
                    });
                    workspace.show_picker_message(message, window, cx);
                    workspace.update_picker(window, cx);
                });
            });
        }));
        self.show_picker_message(i18n::text("picker.working").into(), window, cx);
        self.sync(window, cx);
    }

    /// Reconcile editor and Explorer state for visible path `changes`.
    fn apply_picker_path_changes(&mut self, changes: &[PathChange]) {
        if changes.is_empty() {
            return;
        }
        self.projects.apply_path_changes(changes);
        for project in self.projects.iter_mut() {
            let Some(manager) = &mut project.manager else {
                continue;
            };
            for change in changes {
                manager.transfers.invalidate(&change.from);
                if let Some(path) = &change.to {
                    manager.transfers.invalidate(path);
                }
            }
            if changes.iter().any(|change| {
                change.from.starts_with(&project.root)
                    || change
                        .to
                        .as_ref()
                        .is_some_and(|path| path.starts_with(&project.root))
            }) {
                let _ = manager.tree.refresh();
            }
        }
    }

    /// Select or activate a visible row or delete button from a pointer `event`.
    pub(super) fn press_file_picker(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (x, y) = self.metrics.cell(event.position);
        let layout = self.file_picker_layout();
        let session = self.file_picker.as_mut().expect("active picker");
        if session.state.busy || !layout.area.contains(x, y) {
            return;
        }
        let (x, y) = (x - layout.area.x, y - layout.area.y);
        if session.state.delete.is_some() {
            if event.click_count == 1
                && let Some(confirm) = layout.delete_hit(x, y, &self.spacing)
            {
                self.confirm_picker_delete(confirm, window, cx);
            }
            return;
        }
        if session.state.prompt.is_some() {
            return;
        }
        session.state.goto = false;
        if layout.current.contains(x, y) {
            let row = session.state.first + y - layout.current.y;
            if row < session.state.visible.len() {
                let activate = row == session.state.selected && event.click_count > 1;
                session.state.selected = row;
                if activate {
                    self.open_picker_selection(window, cx);
                    return;
                }
            }
        } else if layout.parent.contains(x, y) {
            let row = session.state.parent_first(layout) + y - layout.parent.y;
            if let Some(entry) = session
                .state
                .listing
                .as_ref()
                .and_then(|listing| listing.parent.get(row))
                && entry.directory
            {
                let request = Browse::Path(entry.path.clone(), None);
                self.browse_picker(request, window, cx);
                return;
            }
        }
        self.update_picker(window, cx);
    }

    /// Scroll the middle selection or right preview under pointer cell `x`, `y`.
    pub(super) fn scroll_file_picker(
        &mut self,
        x: usize,
        y: usize,
        rows: isize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let layout = self.file_picker_layout();
        if !layout.area.contains(x, y) {
            return;
        }
        let (x, y) = (x - layout.area.x, y - layout.area.y);
        let state = &mut self.file_picker.as_mut().expect("active picker").state;
        if state.busy || state.prompt.is_some() {
            return;
        }
        if layout.preview.contains(x, y) {
            state.scroll_preview(rows, layout.preview.h);
        } else if state.delete.is_none() && layout.current.contains(x, y) {
            state.selected = state
                .selected
                .saturating_add_signed(rows)
                .min(state.visible.len().saturating_sub(1));
        }
        self.update_picker(window, cx);
    }
}
