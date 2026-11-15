//! Connect the picker to workspace tabs, native input, and background filesystem work.

mod focused;
mod operations;

use super::{
    Workspace,
    layout::{FOOTER_ROWS, ShellWindow},
};
use crate::{
    grid::{
        PreviewHighlighter,
        render::{GridLayer, MessageLevel, Metrics, Panel, Rect},
        windows::{
            PathChange,
            editor::EditorWindow,
            file_picker::{
                Browse, Delete, FilePicker, Listing, PickerLayout, Preview, Prompt, PromptKind,
                delete_buttons, jump_path,
            },
        },
    },
    ui::keys::normal_character,
};
use focused::FocusedFile;
use gpui::{AppContext, Context, Entity, KeyDownEvent, MouseDownEvent, Task, Window};
use nanobug::filesystem::{FileTransfers, TransferKind};
use nanobug::{animation::scroll::ScrollInput, i18n};
use operations::Operation;
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

/// One floating message, with its panel bounds and a dismissal canceled on replacement.
struct PickerMessage {
    /// Controls the border and text colors for the feedback.
    level: MessageLevel,
    text: String,
    area: Rect,
    _dismiss: Task<()>,
}

/// How the workspace uses the entry activated in one picker session.
enum PickerMode {
    /// Open a project or standalone file, with the workspace's usual starting location.
    Open,
    /// Enter directories and return a matching file path to the caller.
    Choose {
        /// Allowed file extensions without dots. An empty list permits every regular file.
        extensions: &'static [&'static str],
        /// Receives the selected file, or None when the picker closes without a selection.
        finish: SelectionHandler,
    },
}

/// Completion after dropping the picker, with its result and the originating workspace window.
type SelectionHandler =
    Box<dyn FnOnce(&mut Workspace, Option<PathBuf>, &mut Window, &mut Context<Workspace>)>;

/// State and retained panels owned by a single opening of the picker.
pub(super) struct PickerSession {
    /// Activation behavior and optional extension restriction for this opening.
    mode: PickerMode,
    /// Logical browsing state.
    pub(super) state: FilePicker,
    /// Picker border and directory rows, also used by cursor effects.
    pub(super) panel: Entity<Panel>,
    /// Right-column text and syntax colors.
    pub(super) text_panel: Entity<Panel>,
    /// Current mode's bottom shortcut row.
    pub(super) footer_panel: Entity<Panel>,
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
    /// Two-column preview entered from the list, if any.
    focused: Option<FocusedFile>,
}

impl PickerSession {
    /// Retained layers share the workspace grid for painting and pointer hit testing.
    pub(super) fn panels(&self) -> [&Entity<Panel>; 3] {
        [&self.panel, &self.text_panel, &self.message_panel]
    }

    /// Start a session in `mode` with retained panels using grid `metrics` and workspace `cx`.
    fn new(mode: PickerMode, metrics: Metrics, cx: &mut Context<Workspace>) -> Self {
        Self {
            mode,
            state: FilePicker::default(),
            panel: cx.new(|_| Panel::new(metrics)),
            text_panel: cx.new(|_| Panel::new(metrics)),
            footer_panel: cx.new(|_| Panel::new(metrics)),
            message_panel: cx.new(|_| Panel::new(metrics)),
            scan: None,
            preview_path: None,
            preview: None,
            operation: None,
            transfers: FileTransfers::default(),
            message: None,
            focused: None,
        }
    }

    /// Whether Enter chooses a file for a caller.
    pub(super) fn choosing_file(&self) -> bool {
        matches!(self.mode, PickerMode::Choose { .. })
    }

    /// File extensions allowed in the listings for this session.
    fn extensions(&self) -> &'static [&'static str] {
        match self.mode {
            PickerMode::Open => &[],
            PickerMode::Choose { extensions, .. } => extensions,
        }
    }

    /// Install `listing` with `keep_queries`, updating the ordinary browser's `last` location.
    fn install_listing(
        &mut self,
        listing: Listing,
        keep_queries: bool,
        last: &mut Option<PathBuf>,
    ) {
        if !self.choosing_file() {
            last.clone_from(&listing.directory);
        }
        self.state.install(listing, keep_queries);
    }

    /// Whether this session is displaying a file instead of the directory columns.
    pub(super) fn previewing_file(&self) -> bool {
        self.focused.is_some()
    }

    /// Active read-only editor, when the focused file is text.
    pub(super) fn focused_editor(&self) -> Option<&EditorWindow> {
        self.focused.as_ref()?.editor()
    }

    /// Active read-only editor for input.
    pub(super) fn focused_editor_mut(&mut self) -> Option<&mut EditorWindow> {
        self.focused.as_mut()?.editor_mut()
    }
}

impl Workspace {
    /// Open a picker in `window` through `cx`, revealing the active project or standalone file.
    pub(super) fn open_file_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let initial = self.projects.active_project().map(|project| {
            if project.manager.is_some() {
                Browse::reveal_directory(&project.root)
            } else {
                Browse::Path(project.root.clone(), project.editor.vim().doc.path.clone())
            }
        });
        self.start_file_picker(PickerMode::Open, initial, None, window, cx);
    }

    /// Browse in `window` through `cx`, showing files matching `extensions`.
    /// Try `remembered_directory` first, then `fallback_directory` or the chooser's default location.
    /// Replace an idle browser underneath the caller. `finish` receives the path on selection
    /// or None on dismissal. Returns whether the chooser opened.
    pub(super) fn choose_file(
        &mut self,
        fallback_directory: Option<PathBuf>,
        remembered_directory: Option<PathBuf>,
        extensions: &'static [&'static str],
        finish: impl FnOnce(&mut Workspace, Option<PathBuf>, &mut Window, &mut Context<Workspace>)
        + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self
            .file_picker
            .as_ref()
            .is_some_and(|session| session.choosing_file() || session.state.busy)
        {
            return false;
        }
        self.finish_file_picker(None, window, cx);
        self.start_file_picker(
            PickerMode::Choose {
                extensions,
                finish: Box::new(finish),
            },
            fallback_directory.map(|path| Browse::Path(path, None)),
            remembered_directory,
            window,
            cx,
        )
    }

    /// Open `mode` in `window` through `cx`, trying `remembered_directory` before `initial`.
    fn start_file_picker(
        &mut self,
        mode: PickerMode,
        initial: Option<Browse>,
        remembered_directory: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.file_picker.is_some() {
            return false;
        }
        let ordinary_location = match mode {
            PickerMode::Open => self.file_picker_directory.as_ref(),
            PickerMode::Choose { .. } => None,
        };
        let initial = initial.or_else(|| {
            ordinary_location
                .or_else(|| self.recent_projects.roots.first())
                .cloned()
                .or_else(dirs::home_dir)
                .or_else(|| std::env::current_dir().ok())
                .map(|path| Browse::Path(path, None))
        });
        let initial = match (remembered_directory, initial) {
            (Some(directory), Some(fallback)) => {
                Some(Browse::RememberedDirectory(directory, Box::new(fallback)))
            }
            (Some(directory), None) => Some(Browse::Path(directory, None)),
            (None, initial) => initial,
        };
        self.prefix = None;
        self.hovered_controls = None;
        self.resizing = false;
        self.wheel = 0.;
        self.file_picker = Some(PickerSession::new(mode, self.metrics, cx));
        if let Some(initial) = initial {
            self.browse_picker(initial, window, cx);
        } else {
            self.show_picker_message(
                MessageLevel::Warning,
                i18n::text("picker.no_directory").into(),
                window,
                cx,
            );
            self.sync(window, cx);
        }
        true
    }

    /// End the session in `window` through `cx`, delivering `selected` to its caller if any.
    pub(super) fn finish_file_picker(
        &mut self,
        selected: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(PickerSession { mode, .. }) = self.file_picker.take() else {
            return;
        };
        self.wheel = 0.;
        self.prefix = None;
        self.hovered_controls = None;
        if let PickerMode::Choose { finish, .. } = mode {
            finish(self, selected, window, cx);
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
        let area = Rect {
            x: 0,
            y: 1,
            w: self.layout.cols,
            h: self.layout.rows.saturating_sub(1 + FOOTER_ROWS),
        };
        if self
            .file_picker
            .as_ref()
            .is_some_and(PickerSession::previewing_file)
        {
            PickerLayout::for_file(area)
        } else {
            PickerLayout::new(
                area,
                self.file_picker.as_ref().map(|session| &session.state),
            )
        }
    }

    /// Return the active search or filter prompt in workspace grid coordinates.
    pub(super) fn file_picker_prompt_area(&self) -> Option<Rect> {
        self.file_picker
            .as_ref()?
            .state
            .floating_prompt_area(self.file_picker_layout())
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
        let session = self.file_picker.as_mut().expect("active picker");
        session.state.goto = false;
        session.state.composition.clear();
        let text_preview = session.focused_editor().is_some();
        if let Some(editor) = session.focused_editor_mut() {
            editor.composition_mut().clear();
            editor.cancel_pending();
        }
        self.hovered_controls = None;
        self.help.open_file_picker(
            self.projects.manager_available(),
            text_preview,
            session.choosing_file(),
        );
        self.sync(window, cx);
    }

    /// Refresh picker and floating panels through `cx`, clamping resized viewports.
    pub(super) fn refresh_file_picker(&mut self, cx: &mut Context<Self>) {
        let layout = self.file_picker_layout();
        let active = !self.help.is_open() && !self.small_modal_open();
        let Some(session) = &mut self.file_picker else {
            return;
        };
        let controls = self.hovered_controls == Some(ShellWindow::FilePicker);
        let frame = if let Some(file) = &session.focused {
            file.refresh(layout, active, &self.spacing, &session.text_panel, cx);
            session.state.file_frame(
                layout,
                &file.path,
                file.status(),
                active,
                controls,
                &self.spacing,
            )
        } else {
            session.state.reveal(layout.current.h);
            session.state.scroll_preview(0, layout.preview.h);
            if let Some(frame) = session.state.text_frame(layout, &self.spacing) {
                session
                    .text_panel
                    .update(cx, |panel, cx| panel.set(frame, cx));
            }
            session.state.frame(layout, active, controls, &self.spacing)
        };
        session.panel.update(cx, |panel, cx| panel.set(frame, cx));
        let footer = session.footer_frame(self.layout.cols, &self.spacing);
        session
            .footer_panel
            .update(cx, |panel, cx| panel.set(footer, cx));
        if let Some(frame) = session.state.floating_prompt_frame(layout, &self.spacing) {
            self.popup_panel
                .update(cx, |panel, cx| panel.set(frame, cx));
        }
        if let Some(message) = &mut session.message {
            let (area, frame) = layout.message_frame(message.level, &message.text, &self.spacing);
            message.area = area;
            session
                .message_panel
                .update(cx, |panel, cx| panel.set(frame, cx));
        }
    }

    /// Return the picker chrome and text preview in draw order.
    pub(super) fn file_picker_layers(&self) -> Option<Vec<GridLayer>> {
        let session = self.file_picker.as_ref()?;
        let layout = self.file_picker_layout();
        let mut layers = vec![GridLayer {
            panel: session.panel.clone(),
            bounds: layout.area.pixels(self.metrics),
            clip: None,
        }];
        if let Some(file) = &session.focused {
            file.layers(layout, self.metrics, &session.text_panel, &mut layers);
        } else if session.state.delete.is_none()
            && matches!(session.state.preview, Preview::Text(_))
        {
            let bounds = layout.text_area().pixels(self.metrics);
            layers.push(GridLayer {
                panel: session.text_panel.clone(),
                bounds,
                clip: Some(bounds),
            });
        }
        layers.push(GridLayer {
            panel: session.footer_panel.clone(),
            bounds: self.footer_bounds(),
            clip: None,
        });
        Some(layers)
    }

    /// Return the floating message so its bounds can cut out the content behind it.
    pub(super) fn file_picker_message_layer(&self) -> Option<GridLayer> {
        let session = self.file_picker.as_ref()?;
        let message = session.message.as_ref()?;
        Some(GridLayer {
            panel: session.message_panel.clone(),
            bounds: message.area.pixels(self.metrics),
            clip: None,
        })
    }

    /// Scan the new `request`, preserving the current listing until it succeeds.
    fn browse_picker(&mut self, request: Browse, window: &mut Window, cx: &mut Context<Self>) {
        let session = self.file_picker.as_mut().expect("active picker");
        session.state.busy = true;
        session.state.goto = false;
        let extensions = session.extensions();
        let load = cx
            .background_executor()
            .spawn(async move { Listing::read(request, extensions) });
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
                            session.install_listing(
                                listing,
                                false,
                                &mut workspace.file_picker_directory,
                            );
                        }
                        Err(error) => workspace.show_picker_message(
                            MessageLevel::Error,
                            format!("{error:#}"),
                            window,
                            cx,
                        ),
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
        let extensions = session.extensions();
        let reader = cx
            .background_executor()
            .spawn(async move { Preview::read(&requested, directory, extensions) });
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

    /// Show `message` at `level` in `window` and schedule its two-second dismissal through `cx`.
    fn show_picker_message(
        &mut self,
        level: MessageLevel,
        message: String,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
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
            level,
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
        if self
            .file_picker
            .as_ref()
            .is_some_and(PickerSession::previewing_file)
        {
            return self.focused_file_key(event, window, cx);
        }
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
            session.state.update_prompt_matches(preferred.as_deref());
            self.update_picker(window, cx);
            return true;
        }
        if modifiers.alt || modifiers.control || modifiers.platform || modifiers.function {
            return true;
        }
        if command == "escape"
            && !event.is_held
            && session.operation.is_none()
            && session.state.delete.is_none()
            && (session.state.clear_search() || session.state.clear_filter())
        {
            self.update_picker(window, cx);
            return true;
        }
        if session.state.busy {
            if command == "escape" && session.operation.is_none() && !event.is_held {
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
            && (session.state.goto
                || !matches!(command, "j" | "k" | "up" | "down" | "J" | "K" | "n" | "N"))
        {
            return true;
        }
        if command == "escape" {
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
                    self.show_picker_message(
                        MessageLevel::Warning,
                        i18n::text("picker.no_directory").into(),
                        window,
                        cx,
                    );
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
                if let Some(entry) = session.state.entry() {
                    let path = entry.path.clone();
                    if entry.directory {
                        self.browse_picker(Browse::Path(path, None), window, cx);
                    } else {
                        self.enter_file_preview(path, window, cx);
                    }
                    return true;
                }
            }
            "enter" => {
                self.activate_picker_selection(window, cx);
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
            "/" => {
                let search = session.state.search.clone();
                let end = search.len();
                self.start_picker_prompt(PromptKind::Search, search, end);
            }
            "n" | "N" => session.state.repeat_search(command == "N"),
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
                self.show_picker_message(MessageLevel::Info, message, window, cx);
            }
            "y" | "c" | "x" => {
                let cut = command == "x";
                let kind = if cut {
                    TransferKind::Cut
                } else {
                    TransferKind::Copy
                };
                let (level, message) = match session.transfers.capture(&session.state.marked, kind)
                {
                    Ok(count) => (
                        MessageLevel::Info,
                        i18n::format(
                            if cut { "manager.cut" } else { "manager.copied" },
                            &[("count", &count)],
                        ),
                    ),
                    Err(error) => (MessageLevel::Warning, error.to_string()),
                };
                self.show_picker_message(level, message, window, cx);
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

    /// Commit native `text` to the current prompt and update its live matches in `window`.
    pub(super) fn file_picker_commit(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let session = self.file_picker.as_mut().expect("active picker");
        if let Some(editor) = session.focused_editor_mut() {
            editor.composition_mut().clear();
            editor.edit(|vim, register| vim.commit_text(text, register));
            self.sync(window, cx);
            return;
        }
        let state = &mut session.state;
        let preferred = state.entry().map(|entry| entry.path.clone());
        let Some(prompt) = &mut state.prompt else {
            return;
        };
        prompt.insert(text);
        state.composition.clear();
        state.update_prompt_matches(preferred.as_deref());
        self.update_picker(window, cx);
    }

    /// Apply or `cancel` the prompt, dispatching filesystem work only for committed edits.
    fn finish_picker_prompt(&mut self, cancel: bool, window: &mut Window, cx: &mut Context<Self>) {
        let state = &mut self.file_picker.as_mut().expect("active picker").state;
        if cancel && state.clear_filter() {
            self.update_picker(window, cx);
            return;
        }
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
                }
                PromptKind::Search => {
                    if !state.apply_search(prompt.value) && !state.search.is_empty() {
                        let message =
                            i18n::format("editor.search_not_found", &[("query", &state.search)]);
                        self.show_picker_message(MessageLevel::Warning, message, window, cx);
                    }
                }
                PromptKind::Jump => match jump_path(&prompt.value, state.directory()) {
                    Ok(path) => {
                        self.browse_picker(Browse::Path(path, None), window, cx);
                        return;
                    }
                    Err(error) => self.show_picker_message(
                        MessageLevel::Warning,
                        error.to_string(),
                        window,
                        cx,
                    ),
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

    /// Activate the highlighted entry in `window` through `cx` for the current picker mode.
    fn activate_picker_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let session = self.file_picker.as_ref().expect("active picker");
        let state = &session.state;
        let Some(entry) = state.entry() else { return };
        if session.choosing_file() {
            let path = entry.path.clone();
            if entry.directory {
                self.browse_picker(Browse::Path(path, None), window, cx);
            } else {
                self.held_close_key = Some("enter".into());
                self.finish_file_picker(Some(path), window, cx);
                self.sync(window, cx);
            }
            return;
        }
        if !entry.directory
            && (matches!(state.preview, Preview::Unavailable)
                || Preview::requires_system_open(&entry.path))
        {
            self.show_picker_message(
                MessageLevel::Error,
                i18n::text("picker.use_external").into(),
                window,
                cx,
            );
            self.sync(window, cx);
            return;
        }
        let path = entry.path.clone();
        self.open_picker_path(&path, window, cx);
    }

    /// Open `path` or reveal it in selection mode, using `window` and `cx` for feedback.
    fn open_picker_path(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(session) = &mut self.file_picker
            && session.choosing_file()
        {
            session.focused = None;
            self.last_cursor_space = None;
            self.browse_picker(Browse::Path(path.to_path_buf(), None), window, cx);
            return;
        }
        let result = fs::canonicalize(path)
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
                self.show_picker_message(MessageLevel::Error, format!("{error:#}"), window, cx);
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
    pub(super) fn confirm_picker_delete(
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
        let dirty = self.projects.dirty_paths().collect::<Vec<_>>();
        let roots = self
            .projects
            .ordered()
            .filter(|project| project.manager.is_some())
            .map(|project| project.root.clone())
            .collect::<Vec<_>>();
        let session = self.file_picker.as_mut().expect("active picker");
        let directory = session.state.directory().map(Path::to_path_buf);
        let previous = session.state.entry().map(|entry| entry.path.clone());
        let extensions = session.extensions();
        let mut transfers = std::mem::take(&mut session.transfers);
        session.state.busy = true;
        let worker = cx.background_executor().spawn(async move {
            let refresh = !matches!(operation, Operation::External(_));
            let outcome = operation.run(directory.as_deref(), &mut transfers, &dirty, &roots);
            let listing = if refresh {
                directory.as_deref().and_then(|directory| {
                    let target = directory.ancestors().find(|path| path.is_dir())?;
                    Some(Listing::read(
                        Browse::Path(target.to_path_buf(), outcome.select.clone().or(previous)),
                        extensions,
                    ))
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
                    let mut level = outcome.level;
                    if let Some(listing) = listing {
                        session.preview = None;
                        session.preview_path = None;
                        match listing {
                            Ok(listing) => {
                                let same_directory =
                                    session.state.directory() == listing.directory.as_deref();
                                session.install_listing(
                                    listing,
                                    same_directory,
                                    &mut workspace.file_picker_directory,
                                );
                            }
                            Err(error) => {
                                level = MessageLevel::Error;
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
                    workspace.show_picker_message(level, message, window, cx);
                    workspace.update_picker(window, cx);
                });
            });
        }));
        self.show_picker_message(
            MessageLevel::Info,
            i18n::text("picker.working").into(),
            window,
            cx,
        );
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
                manager.invalidate_history(&change.from);
                if let Some(path) = &change.to {
                    manager.invalidate_history(path);
                }
            }
            if changes.iter().any(|change| {
                change.from.starts_with(&project.root)
                    || change
                        .to
                        .as_ref()
                        .is_some_and(|path| path.starts_with(&project.root))
            }) {
                let _ = manager.refresh();
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
        if session.previewing_file() {
            let area = layout.preview_editor_area();
            if let Some(editor) = session.focused_editor_mut()
                && !editor.vim().accepts_text()
                && EditorWindow::body(area).contains(x, y)
            {
                editor.press(event.position, area, self.metrics, &self.spacing);
                self.sync(window, cx);
            }
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
                    self.activate_picker_selection(window, cx);
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
        let session = self.file_picker.as_mut().expect("active picker");
        if session.previewing_file() {
            if let Some(editor) = session.focused_editor()
                && editor.composition().is_empty()
                && EditorWindow::body(layout.preview_editor_area()).contains(x, y)
            {
                self.scroll_frame(
                    [
                        ScrollInput::Advance,
                        ScrollInput::Wheel(rows),
                        ScrollInput::Advance,
                    ],
                    window,
                    cx,
                );
            }
            return;
        }
        let (x, y) = (x - layout.area.x, y - layout.area.y);
        let state = &mut session.state;
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
