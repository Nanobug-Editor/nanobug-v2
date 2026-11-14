//! Global shortcuts, modal keyboard routing and committed native text.

use crate::grid::windows::{editor::EditorWindow, help::HelpCommand};
use crate::ui::keys::normal_character;

use super::{
    Workspace,
    editor_close::CloseTarget,
    layout::{Focus, ShellWindow},
    tabs::ProjectShortcut,
    window_keys::{WindowSequence, direction_target},
};
use gpui::{Context, KeyDownEvent, Keystroke, Window};
use nanobug::{
    animation::{cursor::CursorConfig, scroll::ScrollInput},
    i18n,
    vim::{Action, Mode},
};

/// Default printable key for the workspace's `<leader>` mappings.
const LEADER_KEY: &str = " ";

/// A sequence awaiting its next key in the workspace or Explorer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PendingPrefix {
    /// Workspace `<leader>` sequence.
    Leader(LeaderPrefix),
    /// Explorer's own sequence, beginning with this character.
    Manager(char),
    /// Window command after Ctrl+w.
    Window(WindowSequence),
}

/// The complete suffix entered after the leader, identifying the current group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct LeaderPrefix(&'static str);

/// Targets available to the leader hint and keyboard dispatcher.
#[derive(Clone, Copy)]
pub(super) struct LeaderAvailability {
    /// The selected tab has an Explorer.
    manager: bool,
    /// The active project pane or file picker has shortcut help.
    help: bool,
    /// Restrict the current surface to close bindings.
    close_only: bool,
    /// File selection keeps shell actions scoped to the picker.
    choosing_file: bool,
    /// A grid window is available to close.
    grid_window: bool,
    /// Top-level tab actions are available on this surface.
    tabs: bool,
}

/// One continuation shared by keyboard dispatch and the leader hint.
pub(super) struct LeaderBinding {
    /// Entire sequence after the leader, using a tab character for the Tab key.
    sequence: &'static str,
    /// Translation key for the action or group shown beside the binding.
    pub(super) description_key: &'static str,
    /// Action executed at this sequence. A group has no immediate action.
    shortcut: Option<ShellShortcut>,
}

/// Workspace leader mappings, including group names in display order.
const LEADER_BINDINGS: &[LeaderBinding] = &[
    LeaderBinding {
        sequence: "e",
        description_key: "leader.toggle_manager",
        shortcut: Some(ShellShortcut::ToggleManager),
    },
    LeaderBinding {
        sequence: "E",
        description_key: "leader.open_file_picker",
        shortcut: Some(ShellShortcut::OpenFilePicker),
    },
    LeaderBinding {
        sequence: "P",
        description_key: "leader.open_plugins",
        shortcut: Some(ShellShortcut::OpenPluginManager),
    },
    LeaderBinding {
        sequence: "p",
        description_key: "leader.project_group",
        shortcut: None,
    },
    LeaderBinding {
        sequence: "\t",
        description_key: "leader.tabpage_group",
        shortcut: None,
    },
    LeaderBinding {
        sequence: "q",
        description_key: "leader.close_focused",
        shortcut: Some(ShellShortcut::CloseFocused),
    },
    LeaderBinding {
        sequence: "x",
        description_key: "leader.close_tab",
        shortcut: Some(ShellShortcut::CloseTabs(CloseTarget::Tab)),
    },
    LeaderBinding {
        sequence: "X",
        description_key: "leader.close_all_tabs",
        shortcut: Some(ShellShortcut::CloseTabs(CloseTarget::AllTabs)),
    },
    LeaderBinding {
        sequence: "L",
        description_key: "leader.change_language",
        shortcut: Some(ShellShortcut::OpenLanguagePicker),
    },
    LeaderBinding {
        sequence: "?",
        description_key: "leader.open_help",
        shortcut: Some(ShellShortcut::OpenHelp),
    },
    LeaderBinding {
        sequence: "pa",
        description_key: "leader.open_start",
        shortcut: Some(ShellShortcut::OpenStart),
    },
    LeaderBinding {
        sequence: "\t\t",
        description_key: "leader.new_tabpage",
        shortcut: Some(ShellShortcut::NewTabpage),
    },
    LeaderBinding {
        sequence: "\td",
        description_key: "leader.close_tabpage",
        shortcut: Some(ShellShortcut::CloseTabpage),
    },
];

impl LeaderBinding {
    /// Whether this key opens another level of hints.
    pub(super) fn is_group(&self) -> bool {
        self.shortcut.is_none()
    }
}

impl LeaderPrefix {
    /// The menu opened by the leader key alone.
    pub(super) const ROOT: Self = Self("");

    /// Return the group keys already entered after Space.
    pub(super) fn suffix(self) -> &'static str {
        self.0
    }

    /// Remove the most recent group key, returning None when already at the root.
    pub(super) fn parent(self) -> Option<Self> {
        let (end, _) = self.0.char_indices().next_back()?;
        Some(Self(&self.0[..end]))
    }

    /// Yield the group's bindings in display order for the current `availability`.
    pub(super) fn bindings(
        self,
        availability: LeaderAvailability,
    ) -> impl Iterator<Item = (&'static str, &'static LeaderBinding)> {
        LEADER_BINDINGS.iter().filter_map(move |binding| {
            let key = binding.sequence.strip_prefix(self.0)?;
            let available = match binding.shortcut {
                Some(ShellShortcut::CloseFocused) => availability.grid_window,
                Some(ShellShortcut::CloseTabs(CloseTarget::Tab)) => {
                    availability.tabs && availability.grid_window
                }
                Some(ShellShortcut::CloseTabs(_)) => availability.tabs,
                _ if availability.close_only => false,
                Some(ShellShortcut::OpenHelp) => availability.help,
                _ if availability.choosing_file => false,
                _ if binding.sequence.starts_with('\t') => {
                    availability.manager && availability.tabs
                }
                Some(ShellShortcut::ToggleManager) => availability.manager,
                _ => true,
            };
            (key.len() == 1 && available).then_some((key, binding))
        })
    }
}

/// A shell shortcut recognized before the active pane handles the key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShellShortcut {
    /// Wait for the next key in a leader sequence.
    AwaitLeader(LeaderPrefix),
    /// Dismiss the leader hint without sending Escape to the active pane.
    CancelLeader,
    /// Focus the target pane and restore it if hidden.
    Select(Focus),
    /// Hide Explorer or restore and focus it if hidden.
    ToggleManager,
    /// The direction has no target in this layout.
    Stay,
    /// Open the language picker without changing the active pane.
    OpenLanguagePicker,
    /// Open the workspace's three-column file picker.
    OpenFilePicker,
    /// Open the native plugin manager.
    OpenPluginManager,
    /// Show shortcuts for the active pane or file picker.
    OpenHelp,
    /// Add a tab showing the start page.
    OpenStart,
    /// Create another page for the active buffer inside the current project.
    NewTabpage,
    /// Close the active page inside the current project.
    CloseTabpage,
    /// Close the grid surface that owns input.
    CloseFocused,
    /// Close the selected scope of top-level tabs after resolving unsaved buffers.
    CloseTabs(CloseTarget),
}

impl ShellShortcut {
    /// Resolve `key` and its printable `character` after `prefix`; a start page has no pane `focus`.
    /// `mode` gates editor shortcuts, `split` describes pane ordering and orientation,
    /// and `availability` and `hidden_pane` describe the available targets.
    fn resolve(
        focus: Option<Focus>,
        mode: Mode,
        hidden_pane: Option<Focus>,
        split: (bool, bool),
        availability: LeaderAvailability,
        prefix: Option<PendingPrefix>,
        key: &Keystroke,
        character: Option<&str>,
    ) -> Option<Self> {
        let modifiers = key.modifiers;
        let uppercase = matches!(character.map(str::as_bytes), Some([b'A'..=b'Z']));
        let editing = focus == Some(Focus::Editor) && mode.accepts_text();
        let visual = focus == Some(Focus::Editor) && mode.is_visual();
        if editing
            || modifiers.alt
            || modifiers.platform
            || (character != Some("?") && modifiers.shift != uppercase)
            || modifiers.function
        {
            return None;
        }
        if modifiers.control {
            if visual || availability.close_only {
                return None;
            }
            let direction = key.key.as_str();
            if !matches!(direction, "h" | "j" | "k" | "l") {
                return None;
            }
            let target = direction_target(focus?, split, direction).filter(|target| {
                *target == Focus::Editor
                    || availability.manager && hidden_pane != Some(Focus::Manager)
            });
            return Some(target.map_or(Self::Stay, Self::Select));
        }
        if character == Some(LEADER_KEY) || character == Some("\\") && !availability.close_only {
            return Some(Self::AwaitLeader(LeaderPrefix::ROOT));
        }
        let Some(PendingPrefix::Leader(prefix)) = prefix else {
            return None;
        };
        let continuation = if key.key == "tab" {
            Some("\t")
        } else {
            character
        };
        match key.key.as_str() {
            "escape" => Some(Self::CancelLeader),
            "backspace" => Some(
                prefix
                    .parent()
                    .map_or(Self::CancelLeader, Self::AwaitLeader),
            ),
            _ => prefix
                .bindings(LeaderAvailability {
                    close_only: availability.close_only || visual,
                    ..availability
                })
                .find(|(key, _)| Some(*key) == continuation)
                .map(|(_, binding)| {
                    binding
                        .shortcut
                        .unwrap_or(Self::AwaitLeader(LeaderPrefix(binding.sequence)))
                }),
        }
    }
}

impl Workspace {
    /// Report the active surface's bindings for both leader input and its hint.
    pub(super) fn leader_availability(&self) -> LeaderAvailability {
        let project_picker = self
            .projects
            .start()
            .is_some_and(|page| page.picker.is_some());
        let picker_busy = self
            .file_picker
            .as_ref()
            .is_some_and(|session| session.state.busy || session.state.delete.is_some());
        let project = self.projects.active_project();
        let modal_open = self.modal_open();
        let visual = project.is_some_and(|project| {
            project.focus == Focus::Editor
                && matches!(
                    project.editor.vim().mode,
                    Mode::Visual | Mode::VisualLine | Mode::VisualBlock
                )
        });
        LeaderAvailability {
            manager: self.projects.manager_available(),
            help: self.file_picker.is_some() || project.is_some(),
            close_only: self.help.is_open()
                || self.small_modal_open()
                || project_picker
                || picker_busy
                || visual,
            choosing_file: self
                .file_picker
                .as_ref()
                .is_some_and(|session| session.choosing_file()),
            grid_window: project.is_some() || modal_open || project_picker,
            tabs: !modal_open && !project_picker,
        }
    }

    /// Route committed native `text` to the active input in `window` through `cx`.
    pub(super) fn commit_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.accepts_native_text() {
            return;
        }
        if self.plugin_commit(text, window, cx) {
            return;
        }
        #[cfg(feature = "plugin-probe")]
        if self.probe_commit(text, window, cx) {
            return;
        }
        if let Some(list) = &mut self.help.list {
            list.commit_filter(text, self.scroll_epoch.elapsed().as_secs_f64());
            self.repaint_help(ScrollInput::Advance, window, cx);
            return;
        }
        if self.file_picker.is_some() {
            self.file_picker_commit(text, window, cx);
            return;
        }
        if let Some(page) = self.projects.start_mut()
            && let Some(picker) = &mut page.picker
        {
            picker.commit_filter(text);
            page.status.clear();
            self.sync(window, cx);
            return;
        }
        self.commit_editor_text(text, window, cx);
    }

    /// Deliver `text` to the active editor through `window` and `cx`, including a direct paste chord.
    fn commit_editor_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.projects.active().editor.vim().accepts_text() {
            return;
        }
        self.cursor
            .update(cx, |cursor, cx| cursor.activity(window, cx));
        self.projects.active_mut().status.clear();
        let text = text.replace("\r\n", "\n");
        self.run_vim(|vim, register| vim.commit_text(&text, register), window, cx);
        self.repaint_editor_input(None, None, window, cx);
        #[cfg(feature = "plugin-probe")]
        if let Some(client) = self.probe.as_ref().and_then(|probe| probe.client.as_ref()) {
            client.mark("editor_input", serde_json::json!({ "bytes": text.len() }));
        }
    }

    /// Cancel the active pane's partial navigation before a shell shortcut.
    pub(super) fn cancel_pending_pane_input(&mut self) {
        if let Some(project) = self.projects.active_project_mut() {
            if let Some(manager) = &mut project.manager {
                manager.clear_count();
            }
            if project.focus == Focus::Editor {
                let editor = &mut project.editor;
                editor.cancel_pending();
            }
        }
    }

    /// Open shortcut help for `pane` in `window`, retaining the previous pane focus through `cx`.
    pub(super) fn open_shortcut_help(
        &mut self,
        pane: Focus,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.prefix = None;
        self.cancel_pending_pane_input();
        let project = self.projects.active_mut();
        if let Some(manager) = &mut project.manager {
            manager.cancel_pending_undo();
            manager.clear_click();
        }
        project.editor.stop_drag();
        project.editor.composition_mut().clear();
        self.hovered_controls = None;
        self.resizing = false;
        self.help
            .open(pane == Focus::Manager, self.projects.manager_available());
        self.help
            .update_keyboard_modifiers(window.modifiers(), false);
        self.sync(window, cx);
    }

    /// Dispatch shell shortcuts and Vim commands from `event`, updating `window` through `cx`.
    /// Insert, search and command-line text use the native input path.
    pub(super) fn key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dispatch_key_down(event, window, cx);
        self.sync_sequence_hint_delay(cx);
    }

    /// Route `event` through the current modal or pane in `window` through `cx`.
    fn dispatch_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.plugins.owns_input() {
            if self.plugin_key(event, window, cx) {
                cx.stop_propagation();
            }
            return;
        }
        if event.keystroke.key == "escape" {
            self.clear_canvas_focus();
        }
        if self.plugin_shortcut(event, window, cx) {
            cx.stop_propagation();
            return;
        }
        if !self.modal_open() && self.dispatch_project_shortcut(event, window, cx) {
            cx.stop_propagation();
            return;
        }
        #[cfg(feature = "plugin-probe")]
        if self.probe_key(event, window, cx) {
            return;
        }
        let raw = event.keystroke.key.as_str();
        if !event.keystroke.modifiers.modified()
            && let Some(delta) = match raw {
                "pageup" => Some(-1),
                "pagedown" => Some(1),
                _ => None,
            }
            && self.page_sequence_hint(delta, cx)
        {
            cx.stop_propagation();
            return;
        }
        self.hint_page = 0;
        if self.held_close_key.as_deref() == Some(raw) {
            if event.is_held {
                cx.stop_propagation();
                return;
            }
            self.held_close_key = None;
        }
        if self.remove_recent_project_key(event, window, cx) {
            cx.stop_propagation();
            return;
        }
        self.cursor
            .update(cx, |cursor, cx| cursor.activity(window, cx));
        let availability = self.leader_availability();
        if availability.close_only
            && !availability.tabs
            && !self.accepts_native_text()
            && !self
                .projects
                .start()
                .and_then(|page| page.picker.as_ref())
                .is_some_and(|picker| picker.filter_focused())
            && self.dispatch_leader_key(event, None, window, cx)
        {
            self.help.cancel_keyboard_modifier_tap();
            cx.stop_propagation();
            return;
        }
        if self.editor_close.is_some() {
            self.editor_close_key(event, window, cx);
            cx.stop_propagation();
            return;
        }
        if let Some(selected) = self.language_selection {
            self.language_picker_key(selected, event, window, cx);
            cx.stop_propagation();
            return;
        }
        if self.help.is_open() {
            let height = self.help.body(self.help_layout()).h;
            let now = self.scroll_epoch.elapsed().as_secs_f64();
            match self.help.key(event, height, now) {
                HelpCommand::Close => {
                    self.held_close_key = Some(raw.into());
                    self.close_grid_window(ShellWindow::ShortcutHelp, window, cx);
                    self.repaint([ScrollInput::Advance; 3], window, cx);
                }
                HelpCommand::Update(input) => self.repaint_help(input, window, cx),
                HelpCommand::FinishFilter { cancel } => self.finish_help_filter(cancel, window, cx),
                HelpCommand::Text => return,
                HelpCommand::Paste => {
                    if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                        self.help.composition.clear();
                        self.commit_text(&text, window, cx);
                    }
                }
                HelpCommand::DispatchChord => {
                    match ProjectShortcut::resolve(&event.keystroke) {
                        // Switching tabs resets the current workspace popup.
                        Some(ProjectShortcut::Select(_)) => {}
                        Some(shortcut) => self.project_shortcut(shortcut, window, cx),
                        None if !event.keystroke.modifiers.alt => {
                            self.pane_key_down(event, availability, window, cx);
                        }
                        None => {}
                    }
                }
                HelpCommand::Ignore => {}
            }
            cx.stop_propagation();
            return;
        }
        if self.file_picker.is_some() {
            if self.plugin_preview_key(event, window, cx)
                || self.file_picker_shell_key(event, window, cx)
                || self.file_picker_key(event, window, cx)
            {
                cx.stop_propagation();
            }
            return;
        }
        self.pane_key_down(event, availability, window, cx);
    }

    /// Run a tab shortcut from `event` in `window` through `cx` if it matches.
    fn dispatch_project_shortcut(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(shortcut) = ProjectShortcut::resolve(&event.keystroke) else {
            return false;
        };
        #[cfg(feature = "plugin-probe")]
        if self.probe_has_focus() {
            self.set_probe_focus(false, window, cx);
        }
        self.dismiss_sequence_hint(cx);
        self.project_shortcut(shortcut, window, cx);
        true
    }

    /// Dispatch `event` to the pane in `window` through `cx`.
    /// While keyboard help is visible, run only commands that can finish with the popup open.
    /// `availability` describes the leader and pane targets outside keyboard help.
    fn pane_key_down(
        &mut self,
        event: &KeyDownEvent,
        availability: LeaderAvailability,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let raw = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        let character = normal_character(&event.keystroke);
        if self.projects.active_project().is_none() {
            if self.start_key_down(event, character.as_deref(), window, cx) {
                cx.stop_propagation();
            }
            return;
        }
        if !self.help.is_open() && self.dispatch_window_key(event, window, cx) {
            cx.stop_propagation();
            return;
        }
        let project = self.projects.active();
        let printable = character.as_deref().unwrap_or(raw);
        let keyboard_help = self.help.keyboard_visible();
        let focus = if keyboard_help {
            Focus::Editor
        } else {
            project.focus
        };
        let mode = project.editor.vim().mode;
        let awaiting_key = focus == Focus::Editor && project.editor.vim().awaiting_key();
        let shortcut = if awaiting_key || keyboard_help {
            None
        } else {
            ShellShortcut::resolve(
                Some(focus),
                mode,
                project.hidden_pane,
                (project.stacked, project.editor_first),
                availability,
                self.prefix,
                &event.keystroke,
                character.as_deref(),
            )
        };
        let leader_pending = matches!(self.prefix, Some(PendingPrefix::Leader(_)));
        let close_pane = !keyboard_help
            && shortcut.is_none()
            && !leader_pending
            && focus == Focus::Editor
            && EditorWindow::matches_close_key(&event.keystroke);
        if close_pane {
            if !event.is_held {
                self.held_close_key = Some(raw.into());
                self.close_grid_window(ShellWindow::Pane(focus), window, cx);
                self.sync(window, cx);
            }
            cx.stop_propagation();
            return;
        }
        // A leader sequence advances only on distinct key presses.
        if event.is_held
            && (leader_pending
                || matches!(shortcut, Some(ShellShortcut::AwaitLeader(_)))
                || focus == Focus::Manager && matches!(character.as_deref(), Some("z" | "v" | "Z")))
        {
            cx.stop_propagation();
            return;
        }
        let prefix = self.prefix.take();
        if prefix.is_some() {
            cx.notify();
        }
        if let Some(manager) = &mut self.projects.active_mut().manager {
            manager.cancel_pending_undo();
        }
        if !keyboard_help
            && focus == Focus::Editor
            && (modifiers.control || modifiers.platform)
            && raw == "v"
            && self.projects.active().editor.vim().accepts_text()
        {
            if let Some(item) = cx.read_from_clipboard()
                && let Some(text) = item.text()
            {
                self.projects.active_mut().editor.composition_mut().clear();
                self.commit_editor_text(&text, window, cx);
            }
            cx.stop_propagation();
            return;
        }
        if !self.projects.active().editor.composition().is_empty()
            && !modifiers.control
            && !modifiers.platform
            // Windows delivers a composing Escape as a modal command.
            && !(cfg!(windows) && raw == "escape" && !modifiers.modified())
        {
            return;
        }
        if let Some(shortcut) = shortcut {
            if matches!(shortcut, ShellShortcut::Select(_)) {
                self.clear_canvas_focus();
            }
            self.apply_shell_shortcut(shortcut, raw, window, cx);
            cx.stop_propagation();
            return;
        }
        if leader_pending {
            cx.stop_propagation();
            return;
        }
        if raw == "f6" {
            self.cycle_cursor_effect(modifiers.shift, window, cx);
            cx.stop_propagation();
            return;
        } else if raw == "f7" {
            let project = self.projects.active_mut();
            project.status = match CursorConfig::load(&project.root) {
                Ok(config) => {
                    project.cursor_config = config.clone();
                    self.cursor
                        .update(cx, |cursor, cx| cursor.configure(config, window, cx));
                    i18n::text("cursor.config_reloaded").into()
                }
                Err(error) => i18n::format("cursor.config_error", &[("error", &error)]),
            };
            self.repaint([ScrollInput::Advance; 3], window, cx);
            cx.stop_propagation();
            return;
        } else if raw == "f5" {
            let project = self.projects.active_mut();
            if let Some(manager) = &mut project.manager {
                project.status = match manager.refresh() {
                    Ok(()) => i18n::text("workspace.project_refreshed").into(),
                    Err(error) => format!("{error:#}"),
                };
                self.repaint([ScrollInput::Advance; 3], window, cx);
            }
            cx.stop_propagation();
            return;
        } else if prefix.is_none() && !mode.accepts_text() && self.plugin_canvas_key(event) {
            cx.stop_propagation();
            return;
        } else if focus == Focus::Manager {
            let key = if modifiers.control || modifiers.alt || modifiers.platform {
                ""
            } else {
                printable
            };
            let manager_prefix = match prefix {
                Some(PendingPrefix::Manager(key)) => Some(key),
                _ => None,
            };
            if let Some(input) = self.manager_key(key, manager_prefix, window, cx) {
                self.layout = self.derive_layout(window);
                self.repaint(
                    [input, ScrollInput::Reveal, ScrollInput::Reveal],
                    window,
                    cx,
                );
                cx.stop_propagation();
            }
            return;
        }
        let Some(input) = self.projects.active_mut().editor.key(
            &event.keystroke,
            self.layout.editor,
            &self.spacing,
            keyboard_help,
        ) else {
            return;
        };
        if input.clear_status {
            self.projects.active_mut().status.clear();
        }
        #[cfg(feature = "plugin-probe")]
        if let Some(client) = self.probe.as_ref().and_then(|probe| probe.client.as_ref())
            && mode != self.projects.active().editor.vim().mode
        {
            let current = self.projects.active().editor.vim();
            client.mark(
                "editor_mode",
                serde_json::json!({
                    "insert": current.mode.is_insert(),
                    "revision": current.doc.revision,
                }),
            );
        }
        self.apply_edit(input.edit, window, cx);
        self.repaint_editor_input(input.view_top, input.view_left, window, cx);
        cx.stop_propagation();
    }

    /// Honor explicit `view_top` or `view_left` targets while the help view may own repaint.
    fn repaint_editor_input(
        &mut self,
        view_top: Option<usize>,
        view_left: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let explicit = view_top.is_some() || view_left.is_some();
        let inputs = [
            if explicit {
                ScrollInput::Advance
            } else {
                ScrollInput::Reveal
            },
            view_top.map_or(ScrollInput::Reveal, ScrollInput::PositionWithTail),
            view_left.map_or(ScrollInput::Reveal, ScrollInput::PositionWithTail),
        ];
        if self.help.keyboard_visible() {
            // The help repaint owns its own scroll. Apply the editor's viewport intent now.
            self.advance_scrolling(inputs, cx);
        }
        if explicit {
            self.repaint(inputs, window, cx);
        } else {
            self.sync(window, cx);
        }
    }

    /// Dispatch the shared leader for `event` while the picker has no local prompt or prefix.
    fn file_picker_shell_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let session = self.file_picker.as_ref().expect("active picker");
        let state = &session.state;
        if state.busy || state.prompt.is_some() || state.goto || state.delete.is_some() {
            return false;
        }
        if session
            .focused_editor()
            .is_some_and(|editor| editor.vim().mode != Mode::Normal || editor.vim().awaiting_key())
        {
            return false;
        }
        let focus = self.projects.active_project().map(|project| project.focus);
        self.dispatch_leader_key(event, focus, window, cx)
    }

    /// Consume a leader `event` in a command surface, using its optional `focus` in `window` through `cx`.
    /// Both pages of editor help keep leader sequences inactive.
    fn dispatch_leader_key(
        &mut self,
        event: &KeyDownEvent,
        focus: Option<Focus>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.help.is_editor_help() {
            return false;
        }
        let key = &event.keystroke;
        let character = normal_character(key);
        let pending = matches!(self.prefix, Some(PendingPrefix::Leader(_)));
        let shortcut = ShellShortcut::resolve(
            focus,
            Mode::Normal,
            None,
            (false, false),
            self.leader_availability(),
            self.prefix,
            key,
            character.as_deref(),
        );
        // Only a leader binding available on this surface can claim a new sequence.
        if !pending && !matches!(shortcut, Some(ShellShortcut::AwaitLeader(_))) {
            return false;
        }
        if event.is_held {
            return true;
        }
        if !matches!(shortcut, Some(ShellShortcut::AwaitLeader(_))) {
            self.dismiss_leader_hint(cx);
        }
        if let Some(shortcut) = shortcut {
            if matches!(
                shortcut,
                ShellShortcut::Select(_) | ShellShortcut::ToggleManager | ShellShortcut::OpenStart
            ) {
                self.file_picker = None;
            }
            self.apply_shell_shortcut(shortcut, &key.key, window, cx);
        }
        true
    }

    /// Route `event` and its ASCII `character` on the start page in `window` through `cx`.
    /// Return whether it consumed the key, with leader sequences taking priority over launcher keys.
    fn start_key_down(
        &mut self,
        event: &KeyDownEvent,
        character: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if event.keystroke.key == "f6" {
            self.dismiss_leader_hint(cx);
            self.cycle_cursor_effect(event.keystroke.modifiers.shift, window, cx);
            return true;
        }
        if self
            .projects
            .start()
            .is_some_and(|page| page.picker.is_some())
        {
            return self.project_picker_key(event, character, window, cx);
        }
        if !self.dispatch_leader_key(event, None, window, cx) {
            self.start_page_key(event, character, window, cx);
        }
        true
    }

    /// Switch the shared cursor preset in `window` and report it in the current tab.
    /// `reverse` selects the previous preset.
    pub(super) fn cycle_cursor_effect(
        &mut self,
        reverse: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let effect = self
            .cursor
            .update(cx, |cursor, cx| cursor.cycle(reverse, window, cx));
        self.set_status(i18n::format(
            "cursor.changed",
            &[("effect", &effect.label())],
        ));
        self.repaint([ScrollInput::Advance; 3], window, cx);
    }

    /// Focus `pane` in the selected project, restoring it and clearing its close status if hidden.
    fn select_pane(&mut self, pane: Focus) {
        let project = self.projects.active_mut();
        project.focus_pane(pane);
        if project.hidden_pane == Some(pane) {
            project.hidden_pane = None;
            project.status.clear();
        }
    }

    /// Execute a resolved shell `shortcut` from `raw` in `window` through `cx`.
    fn apply_shell_shortcut(
        &mut self,
        shortcut: ShellShortcut,
        raw: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cancel_pending_pane_input();
        match shortcut {
            ShellShortcut::AwaitLeader(prefix) => self.show_leader_hint(prefix, cx),
            ShellShortcut::CancelLeader => self.held_close_key = Some(raw.into()),
            ShellShortcut::Select(target) => {
                self.select_pane(target);
                self.sync(window, cx);
            }
            ShellShortcut::ToggleManager => {
                if self.projects.active().hidden_pane == Some(Focus::Manager) {
                    self.select_pane(Focus::Manager);
                } else {
                    self.hide_pane(Focus::Manager);
                }
                self.hovered_controls = None;
                self.sync(window, cx);
            }
            ShellShortcut::Stay => {}
            ShellShortcut::OpenLanguagePicker => self.open_language_picker(window, cx),
            ShellShortcut::OpenFilePicker => self.open_file_picker(window, cx),
            ShellShortcut::OpenPluginManager => self.open_plugin_manager(window, cx),
            ShellShortcut::OpenHelp => {
                if self.file_picker.is_some() {
                    self.open_file_picker_help(window, cx);
                } else if let Some(pane) =
                    self.projects.active_project().map(|project| project.focus)
                {
                    self.open_shortcut_help(pane, window, cx);
                }
            }
            ShellShortcut::OpenStart => self.open_start_tab(window, cx),
            ShellShortcut::NewTabpage | ShellShortcut::CloseTabpage => {
                self.finish_editor_input(window, cx);
                let action = match shortcut {
                    ShellShortcut::NewTabpage => Action::TabNew(None),
                    _ => {
                        self.held_close_key = Some(raw.into());
                        Action::TabClose
                    }
                };
                self.action(action, window, cx);
                self.sync(window, cx);
            }
            ShellShortcut::CloseFocused => {
                self.held_close_key = Some(raw.into());
                self.close_focused_grid(window, cx);
            }
            ShellShortcut::CloseTabs(target) => {
                self.held_close_key = Some(raw.into());
                self.request_close(target, window, cx);
                self.sync(window, cx);
            }
        }
    }
}

#[cfg(test)]
mod tests;
