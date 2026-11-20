//! Global shortcuts, modal keyboard routing and committed native text.

use crate::grid::windows::{
    editor::{EditorWindow, MarkdownViewMode},
    help::HelpCommand,
};
use crate::ui::keys::normal_character;

use super::{
    Workspace,
    buffer_management::BufferClose,
    editor_close::CloseTarget,
    layout::{Focus, ShellWindow},
    sleep::Source,
    tabs::ProjectShortcut,
    window_keys::{WindowSequence, direction_target},
};
use gpui::{Context, KeyDownEvent, Keystroke, Modifiers, Window};
use nanobug::{
    animation::{cursor::CursorConfig, scroll::ScrollInput},
    i18n,
    vim::{Action, ArgumentCommand, LEADER_KEY, Mode, SequenceHint},
};

/// A sequence awaiting its next key in the workspace or Explorer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PendingPrefix {
    /// Workspace `<leader>` sequence.
    Leader(LeaderPrefix),
    /// Explorer's own sequence, beginning with this character.
    Manager(char),
    /// Window command after Ctrl+w.
    Window(WindowSequence),
    /// First letter or bracket in a Markdown preview navigation command.
    MarkdownNavigation(char),
}

/// The complete suffix entered after the leader, identifying the current group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct LeaderPrefix(&'static str);

/// Targets available to the leader hint and keyboard dispatcher.
#[derive(Clone, Copy, Default)]
pub(super) struct LeaderAvailability {
    /// The selected tab has an Explorer.
    manager: bool,
    /// The active project pane or file picker has shortcut help.
    help: bool,
    /// The focused editor can switch the active Markdown document's view.
    markdown: bool,
    /// Restrict the current surface to close bindings.
    close_only: bool,
    /// File selection keeps shell actions scoped to the picker.
    choosing_file: bool,
    /// Top-level tab actions are available on this surface.
    tabs: bool,
    /// The active editor can receive normal-mode buffer and split commands.
    editor: bool,
    /// The active project can receive window commands.
    windows: bool,
    /// The current editor shares its page with another editor pane.
    editor_splits: bool,
    /// The focused window can close through the normal window command.
    close_window: bool,
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
        sequence: ",",
        description_key: "leader.open_settings",
        shortcut: Some(ShellShortcut::OpenSettings),
    },
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
        sequence: "b",
        description_key: "leader.buffer_group",
        shortcut: None,
    },
    LeaderBinding {
        sequence: "w",
        description_key: "leader.window_group",
        shortcut: None,
    },
    LeaderBinding {
        sequence: "m",
        description_key: "leader.markdown_group",
        shortcut: None,
    },
    LeaderBinding {
        sequence: "\t",
        description_key: "leader.tabpage_group",
        shortcut: None,
    },
    LeaderBinding {
        sequence: "q",
        description_key: "leader.quit_group",
        shortcut: None,
    },
    LeaderBinding {
        sequence: "qq",
        description_key: "leader.quit_app",
        shortcut: Some(ShellShortcut::CloseTabs(CloseTarget::Quit)),
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
        sequence: "-",
        description_key: "leader.split_below",
        shortcut: Some(ShellShortcut::SplitWindow { stacked: true }),
    },
    LeaderBinding {
        sequence: "|",
        description_key: "leader.split_right",
        shortcut: Some(ShellShortcut::SplitWindow { stacked: false }),
    },
    LeaderBinding {
        sequence: "`",
        description_key: "leader.alternate_buffer",
        shortcut: Some(ShellShortcut::AlternateBuffer),
    },
    LeaderBinding {
        sequence: "pa",
        description_key: "leader.open_start",
        shortcut: Some(ShellShortcut::OpenStart),
    },
    LeaderBinding {
        sequence: "bb",
        description_key: "leader.alternate_buffer",
        shortcut: Some(ShellShortcut::AlternateBuffer),
    },
    LeaderBinding {
        sequence: "bd",
        description_key: "leader.close_buffer",
        shortcut: Some(ShellShortcut::CloseBuffers(BufferClose::Current)),
    },
    LeaderBinding {
        sequence: "bo",
        description_key: "leader.close_other_buffers",
        shortcut: Some(ShellShortcut::CloseBuffers(BufferClose::Others)),
    },
    LeaderBinding {
        sequence: "bl",
        description_key: "leader.close_left_buffers",
        shortcut: Some(ShellShortcut::CloseBuffers(BufferClose::Left)),
    },
    LeaderBinding {
        sequence: "br",
        description_key: "leader.close_right_buffers",
        shortcut: Some(ShellShortcut::CloseBuffers(BufferClose::Right)),
    },
    LeaderBinding {
        sequence: "bD",
        description_key: "leader.close_buffer_window",
        shortcut: Some(ShellShortcut::CloseBuffers(BufferClose::WithWindow)),
    },
    LeaderBinding {
        sequence: "wd",
        description_key: "vim_window.close",
        shortcut: Some(ShellShortcut::CloseWindow),
    },
    LeaderBinding {
        sequence: "wm",
        description_key: "leader.zoom_window",
        shortcut: Some(ShellShortcut::ZoomWindow),
    },
    LeaderBinding {
        sequence: "wh",
        description_key: "vim_window.left",
        shortcut: Some(ShellShortcut::FocusWindow("h")),
    },
    LeaderBinding {
        sequence: "wj",
        description_key: "vim_window.down",
        shortcut: Some(ShellShortcut::FocusWindow("j")),
    },
    LeaderBinding {
        sequence: "wk",
        description_key: "vim_window.up",
        shortcut: Some(ShellShortcut::FocusWindow("k")),
    },
    LeaderBinding {
        sequence: "wl",
        description_key: "vim_window.right",
        shortcut: Some(ShellShortcut::FocusWindow("l")),
    },
    LeaderBinding {
        sequence: "w=",
        description_key: "vim_window.equalize",
        shortcut: Some(ShellShortcut::EqualizeWindows),
    },
    LeaderBinding {
        sequence: "ma",
        description_key: "leader.markdown_split",
        shortcut: Some(ShellShortcut::SetMarkdownView(MarkdownViewMode::Split)),
    },
    LeaderBinding {
        sequence: "me",
        description_key: "leader.markdown_edit",
        shortcut: Some(ShellShortcut::SetMarkdownView(MarkdownViewMode::Edit)),
    },
    LeaderBinding {
        sequence: "mp",
        description_key: "leader.markdown_preview",
        shortcut: Some(ShellShortcut::SetMarkdownView(MarkdownViewMode::Preview)),
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

    /// Return the current group's description key, or None for the root.
    pub(super) fn group_description_key(self) -> Option<&'static str> {
        LEADER_BINDINGS
            .iter()
            .find(|binding| binding.sequence == self.0 && binding.is_group())
            .map(|binding| binding.description_key)
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
                None if binding.sequence == "q" => availability.tabs,
                Some(ShellShortcut::CloseTabs(_)) => availability.tabs,
                _ if availability.close_only => false,
                Some(ShellShortcut::OpenHelp) => availability.help,
                _ if availability.choosing_file => false,
                Some(
                    ShellShortcut::CloseBuffers(BufferClose::WithWindow)
                    | ShellShortcut::ZoomWindow,
                ) => availability.editor_splits,
                Some(
                    ShellShortcut::SplitWindow { .. }
                    | ShellShortcut::AlternateBuffer
                    | ShellShortcut::CloseBuffers(_),
                ) => availability.editor,
                Some(ShellShortcut::CloseWindow) => availability.close_window,
                None if binding.sequence == "b" => availability.editor,
                _ if binding.sequence.starts_with('w') => availability.windows,
                _ if binding.sequence.starts_with('m') => availability.markdown,
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
    /// Edit core and plugin preferences in the two-pane settings overlay.
    OpenSettings,
    /// Show shortcuts for the active pane or file picker.
    OpenHelp,
    /// Add a tab showing the start page.
    OpenStart,
    /// Create another page for the active buffer inside the current project.
    NewTabpage,
    /// Close the active page inside the current project.
    CloseTabpage,
    /// Select the source and preview layout for the active Markdown document.
    SetMarkdownView(MarkdownViewMode),
    /// Close the selected scope of top-level tabs after resolving unsaved buffers.
    CloseTabs(CloseTarget),
    /// Return to the current pane's previously displayed buffer.
    AlternateBuffer,
    /// Close the indicated buffers or their owning pane through the save prompt.
    CloseBuffers(BufferClose),
    /// Create a split on the lower or right side of the focused editor.
    SplitWindow { stacked: bool },
    /// Close the focused window using the normal pane close command.
    CloseWindow,
    /// Fill the editor region with the active split, or restore its siblings.
    ZoomWindow,
    /// Move focus toward an adjacent window in the given Vim direction.
    FocusWindow(&'static str),
    /// Grow or shrink the focused pane along one axis.
    ResizeWindow { stacked: bool, grow: bool },
    /// Give sibling windows equal sizes using the existing window command.
    EqualizeWindows,
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
            || (!matches!(character, Some("?" | "|")) && modifiers.shift != uppercase)
            || modifiers.function
        {
            return None;
        }
        if modifiers.control {
            if visual || availability.close_only {
                return None;
            }
            if availability.windows && focus.is_some() {
                let resize = match key.key.as_str() {
                    "up" => Some((true, true)),
                    "down" => Some((true, false)),
                    "left" => Some((false, false)),
                    "right" => Some((false, true)),
                    _ => None,
                };
                if let Some((stacked, grow)) = resize {
                    return Some(Self::ResizeWindow { stacked, grow });
                }
            }
            let direction = ["h", "j", "k", "l"]
                .into_iter()
                .find(|&direction| direction == key.key)?;
            let focus = focus?;
            // Use the same split-aware movement as Leader+w before falling back to Explorer.
            if focus == Focus::Editor && availability.editor_splits {
                return Some(Self::FocusWindow(direction));
            }
            let target = direction_target(focus, split, direction).filter(|target| {
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
        let windows = !modal_open && !self.plugins.owns_input() && project.is_some();
        #[cfg(feature = "plugin-probe")]
        let windows = windows && !self.probe_has_focus();
        let editor = windows
            && project.is_some_and(|project| {
                project.focus == Focus::Editor && project.editor.vim().mode == Mode::Normal
            });
        let markdown =
            editor && project.is_some_and(|project| project.editor.markdown_view_mode().is_some());
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
            markdown,
            close_only: self.help.is_open()
                || self.small_modal_open()
                || project_picker
                || picker_busy
                || visual,
            choosing_file: self
                .file_picker
                .as_ref()
                .is_some_and(|session| session.choosing_file()),
            tabs: !modal_open && !project_picker,
            editor,
            windows,
            editor_splits: editor && self.can_zoom_leader_window(),
            close_window: windows && self.can_close_leader_window(),
        }
    }

    /// Route committed native `text` to the active input in `window` through `cx`.
    pub(super) fn commit_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.activate_input_mappings();
        self.hint_page = 0;
        self.deliver_text(text, true, window, cx);
        self.sync_sequence_hint_delay(cx);
        self.track_mapping_timeout(window, cx);
    }

    /// Route clipboard `text` to the active input without requesting editor completion.
    pub(super) fn commit_pasted_text(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate_input_mappings();
        self.deliver_text(text, false, window, cx);
        self.sync_sequence_hint_delay(cx);
        self.track_mapping_timeout(window, cx);
    }

    /// Deliver `text` to its focused surface. `typed` enables completion in the project editor.
    fn deliver_text(
        &mut self,
        text: &str,
        typed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.accepts_native_text() {
            return;
        }
        if let Some(settings) = &mut self.settings {
            settings.insert_text(text);
            self.repaint_settings(window, cx);
            return;
        }
        if self.terminal_keys_active() {
            self.commit_terminal_text(text, !typed, window, cx);
            self.sync(window, cx);
            return;
        }
        if self.command_window.is_some() {
            self.commit_command_window_text(text, typed, window, cx);
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
            self.file_picker_commit(text, typed, window, cx);
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
        self.commit_editor_text(text, typed, window, cx);
    }

    /// Insert `text` in `window` through `cx`. `typed` distinguishes native input from paste.
    fn commit_editor_text(
        &mut self,
        text: &str,
        typed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let vim = self.projects.active().editor.vim();
        if !vim.accepts_text() {
            return;
        }
        let revision = vim.doc.revision;
        let caret = vim.doc.cursor;
        let suggest = typed && vim.mode.is_insert();
        self.cursor
            .update(cx, |cursor, cx| cursor.activity(window, cx));
        self.projects.active_mut().status.clear();
        let text = text.replace("\r\n", "\n");
        self.run_vim(
            |vim, register| {
                if typed {
                    vim.commit_text(&text, register)
                } else {
                    vim.paste_text(&text, register)
                }
            },
            window,
            cx,
        );
        if suggest && self.projects.active().editor.vim().doc.revision != revision {
            self.refresh_automatic_completion((revision, caret), window, cx);
        } else {
            self.dismiss_completion(cx);
        }
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
        self.pane_resize = None;
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
        if self.settings.is_some() {
            self.settings_key(event, window, cx);
            return;
        }
        self.activate_input_mappings();
        if self.editor_sleep.is_none()
            && self.prefix.is_none()
            && self.active_vim_input().is_some_and(|(source, editor)| {
                source == Source::Editor
                    && editor.vim().mapping_timeout_token().is_some()
                    && editor.composition().is_empty()
                    && !editor.wants_native_mapping(&event.keystroke)
            })
        {
            // Paging leaves the held keys in Vim, including prefixes shared with Leader or Ctrl+w.
            if let Some(direction) = hint_page_direction(&event.keystroke)
                && self.page_sequence_hint(direction, cx)
            {
                cx.stop_propagation();
                return;
            }
            self.restore_mapped_shell_prefix(window, cx);
        }
        self.dispatch_key_down(event, window, cx);
        self.sync_sequence_hint_delay(cx);
        self.track_mapping_timeout(window, cx);
    }

    /// Replay the built-in shortcut if a reserved prefix matched only longer custom mappings.
    pub(super) fn restore_mapped_shell_prefix(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(keys) = self
            .projects
            .active_mut()
            .editor
            .take_unmapped_shell_prefix()
        else {
            return false;
        };
        let mut keys = keys.iter().map(|key| mapping_fallback_event(key));
        if let Some(first) = keys.next() {
            self.pane_key_down(&first, self.leader_availability(), window, cx);
        }
        for event in keys {
            self.dispatch_key_down(&event, window, cx);
        }
        true
    }

    /// Route `event` through the current modal or pane in `window` through `cx`.
    fn dispatch_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.vim_menu.is_some() {
            self.vim_menu_key(event, window, cx);
            cx.stop_propagation();
            return;
        }
        if self.pause_editor_input(event, window, cx) {
            cx.stop_propagation();
            return;
        }
        if self.command_window.is_some() {
            if self.command_window_key(event, window, cx) {
                cx.stop_propagation();
            }
            return;
        }
        if let Some(consumed) = self.terminal_key_down(event, window, cx) {
            if consumed {
                cx.stop_propagation();
            }
            return;
        }
        let raw = event.keystroke.key.as_str();
        let page = hint_page_direction(&event.keystroke);
        if let Some(direction) = page
            && self.scroll_definition_hover(direction, window, cx)
        {
            cx.stop_propagation();
            return;
        }
        if self.dismiss_definition_hover(cx)
            && raw == "escape"
            && !event.keystroke.modifiers.modified()
        {
            self.held_close_key = Some("escape".into());
            cx.stop_propagation();
            return;
        }
        if self.plugins.owns_input() {
            if self.plugin_key(event, window, cx) {
                cx.stop_propagation();
            }
            return;
        }
        if let Some((Source::Editor, editor)) = self.active_vim_input()
            && self.prefix.is_none()
            && editor.markdown_view_mode() != Some(MarkdownViewMode::Preview)
            && !(event.is_held && self.held_close_key.as_deref() == Some(raw))
        {
            let modifiers = event.keystroke.modifiers;
            let confirming = editor.vim().substitute_confirmation_active();
            let composing = !editor.composition().is_empty()
                && !modifiers.control
                && !(cfg!(windows) && raw == "escape" && !modifiers.modified());
            if confirming || !composing && editor.wants_native_mapping(&event.keystroke) {
                self.hint_page = 0;
                self.cursor
                    .update(cx, |cursor, cx| cursor.activity(window, cx));
                if self.editor_key_down(event, false, window, cx) || confirming {
                    cx.stop_propagation();
                    return;
                }
            }
        }
        if !self.modal_open() && self.completion_keydown(event, window, cx) {
            cx.stop_propagation();
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
        if let Some(direction) = page
            && !self.active_vim_input().is_some_and(|(_, editor)| {
                editor.wants_native_mapping(&event.keystroke)
                    || editor.vim().mapping_timeout_token().is_some()
                        && !editor.composition().is_empty()
            })
            && self.page_sequence_hint(direction, cx)
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
                        self.commit_pasted_text(&text, window, cx);
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
        let preview_only = project.editor.markdown_view_mode() == Some(MarkdownViewMode::Preview);
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
            && !preview_only
            && focus == Focus::Editor
            && (modifiers.control || modifiers.platform)
            && raw == "v"
            && self.projects.active().editor.vim().accepts_text()
            && (modifiers.platform
                || modifiers.shift
                || !self.projects.active().editor.vim().mode.is_insert())
        {
            if let Some(item) = cx.read_from_clipboard()
                && let Some(text) = item.text()
            {
                self.projects.active_mut().editor.composition_mut().clear();
                self.commit_editor_text(&text, false, window, cx);
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
                self.update_layout(window);
                self.repaint(
                    [input, ScrollInput::Reveal, ScrollInput::Reveal],
                    window,
                    cx,
                );
                cx.stop_propagation();
            }
            return;
        }
        let save = raw == "s" && (modifiers.control || modifiers.platform);
        if preview_only && !save {
            self.markdown_preview_key(event, character.as_deref(), prefix, window, cx);
            cx.stop_propagation();
            return;
        }
        if self.editor_key_down(event, keyboard_help, window, cx) {
            cx.stop_propagation();
        }
    }

    /// Dispatch a native key to Vim, applying host effects and revealing the resulting viewport.
    fn editor_key_down(
        &mut self,
        event: &KeyDownEvent,
        keyboard_help: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let doc = &self.projects.active().editor.vim().doc;
        let (revision, caret) = (doc.revision, doc.cursor);
        let area = self.active_editor_area();
        let directory = self.ex_current_dir().to_owned();
        if matches!(event.keystroke.key.as_str(), "tab" | "a" | "d" | "l") {
            self.refresh_ex_language_completion();
        }
        let project = self.projects.active_mut();
        let mode = project.editor.vim().mode;
        project
            .editor
            .set_numbered_marks(self.numbered_marks.clone());
        if !project.status.is_empty() {
            project.editor.edit(|vim, _| {
                vim.remember_host_message(&project.status);
                Action::None
            });
        }
        let Some(input) = project.editor.key(
            &event.keystroke,
            area,
            &self.spacing,
            &directory,
            keyboard_help,
        ) else {
            return false;
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
        self.finish_terminal_normal();
        if mode.is_insert()
            && event.keystroke.key == "backspace"
            && !event.keystroke.modifiers.modified()
            && self.projects.active().editor.vim().doc.revision != revision
        {
            self.refresh_automatic_completion((revision, caret), window, cx);
        }
        self.repaint_editor_input(input.view_top, input.view_left, window, cx);
        true
    }

    /// Interpret `event` and its printable `character` as preview or file navigation.
    /// `prefix` carries the previous key. `window` and `cx` apply the resulting action.
    fn markdown_preview_key(
        &mut self,
        event: &KeyDownEvent,
        character: Option<&str>,
        prefix: Option<PendingPrefix>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = &event.keystroke;
        if key.key == "tab"
            && key.modifiers
                == (gpui::Modifiers {
                    control: true,
                    ..Default::default()
                })
        {
            self.action(Action::TabNavigate(0, None), window, cx);
            self.sync(window, cx);
            return;
        }
        if let Some(PendingPrefix::MarkdownNavigation(previous)) = prefix {
            if event.is_held {
                self.prefix = prefix;
                return;
            }
            let continuation = if key.key == "tab" && !key.modifiers.modified()
                || key.key == "i"
                    && key.modifiers
                        == (gpui::Modifiers {
                            control: true,
                            ..Default::default()
                        }) {
                Some("Tab")
            } else {
                character
            };
            if previous == 'g' && continuation == Some("g") {
                self.scroll_markdown_preview(ScrollInput::Position(0), window, cx);
                return;
            }
            if let Some(action) = markdown_navigation_action(previous, continuation) {
                self.action(action, window, cx);
                self.sync(window, cx);
                return;
            }
        }
        if let Some([character]) = character.map(str::as_bytes)
            && matches!(*character, b'g' | b'[' | b']')
        {
            self.prefix = Some(PendingPrefix::MarkdownNavigation(char::from(*character)));
            return;
        }
        let page_rows = self.markdown_preview_page_rows();
        if let Some(input) = markdown_scroll_input(key, page_rows) {
            self.scroll_markdown_preview(input, window, cx);
        }
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
        if let Some(project) = self.projects.active_project_mut() {
            project.editor.markdown_source_takes_scroll();
            if explicit {
                project.refresh_shared_documents();
            }
        }
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
            ShellShortcut::OpenSettings => self.open_settings(window, cx),
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
            ShellShortcut::SetMarkdownView(mode) => {
                if self
                    .projects
                    .active_mut()
                    .editor
                    .set_markdown_view_mode(mode)
                {
                    self.dismiss_completion(cx);
                    self.last_cursor_space = None;
                    self.sync(window, cx);
                }
            }
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
            ShellShortcut::CloseTabs(target) => {
                self.held_close_key = Some(raw.into());
                self.request_close(target, window, cx);
                self.sync(window, cx);
            }
            ShellShortcut::AlternateBuffer => self.switch_alternate_buffer(None, window, cx),
            ShellShortcut::CloseBuffers(target) => {
                self.held_close_key = Some(raw.into());
                self.request_buffer_close(target, window, cx);
            }
            ShellShortcut::SplitWindow { stacked } => {
                self.open_leader_editor_split(stacked, window, cx)
            }
            ShellShortcut::CloseWindow => {
                self.held_close_key = Some(raw.into());
                self.close_leader_window(window, cx);
            }
            ShellShortcut::ZoomWindow => self.toggle_leader_window_zoom(window, cx),
            ShellShortcut::FocusWindow(direction) => {
                self.leader_window_focus(direction, window, cx)
            }
            ShellShortcut::ResizeWindow { stacked, grow } => {
                self.resize_window_by_key(stacked, grow, window, cx);
            }
            ShellShortcut::EqualizeWindows => self.equalize_leader_windows(window, cx),
        }
    }
}

/// Return the page delta for the unmodified keys shared by hint and hover paging.
pub(super) fn hint_page_direction(key: &Keystroke) -> Option<isize> {
    if key.modifiers.modified() {
        return None;
    }
    match key.key.as_str() {
        "pageup" => Some(-1),
        "pagedown" => Some(1),
        _ => None,
    }
}

/// Translate a held Vim key back to the native notation used by shell bindings.
fn mapping_fallback_event(mut key: &str) -> KeyDownEvent {
    let mut modifiers = Modifiers::default();
    while let Some((modifier, tail)) = key.split_once('+') {
        match modifier {
            "Ctrl" => modifiers.control = true,
            "Alt" => modifiers.alt = true,
            "Shift" => modifiers.shift = true,
            _ => break,
        }
        key = tail;
    }
    modifiers.shift |= matches!(key.as_bytes(), [b'A'..=b'Z']);
    let raw = if key == " " {
        "space".into()
    } else {
        key.to_ascii_lowercase()
    };
    KeyDownEvent {
        keystroke: Keystroke {
            modifiers,
            key: raw,
            key_char: (key.len() == 1).then(|| key.to_owned()),
        },
        is_held: false,
        prefer_character_input: false,
    }
}

/// Resolve `key` after `prefix` into an existing host action that can change buffers or tabpages.
fn markdown_navigation_action(prefix: char, key: Option<&str>) -> Option<Action> {
    Some(match (prefix, key?) {
        ('g', "t") => Action::TabNavigate(1, None),
        ('g', "T") => Action::TabNavigate(-1, None),
        ('g', "Tab") => Action::TabNavigate(0, None),
        ('[' | ']', key @ ("b" | "B")) => Action::BracketBuffer {
            forward: prefix == ']',
            edge: key == "B",
            count: None,
        },
        ('[' | ']', key @ ("a" | "A")) => Action::Arguments {
            command: if key == "A" {
                ArgumentCommand::Edge {
                    last: prefix == ']',
                }
            } else {
                ArgumentCommand::Step {
                    forward: prefix == ']',
                }
            },
            count: None,
            arguments: String::new(),
        },
        _ => return None,
    })
}

/// List the navigation commands available after `prefix` in the Markdown preview.
pub(super) fn markdown_navigation_hint(prefix: char) -> Option<SequenceHint> {
    use nanobug::vim::Continuation;
    let (group, continuations): (_, &[Continuation]) = match prefix {
        '[' => (
            "vim_hint.previous_group",
            &[
                ("b", "vim_bracket.previous_buffer", false),
                ("B", "vim_bracket.first_buffer", false),
                ("a", "vim_bracket.previous_argument", false),
                ("A", "vim_bracket.first_argument", false),
            ],
        ),
        ']' => (
            "vim_hint.next_group",
            &[
                ("b", "vim_bracket.next_buffer", false),
                ("B", "vim_bracket.last_buffer", false),
                ("a", "vim_bracket.next_argument", false),
                ("A", "vim_bracket.last_argument", false),
            ],
        ),
        'g' => (
            "vim_hint.goto_group",
            &[
                ("g", "vim_hint.first_line", false),
                ("t", "vim_window.next_tab", false),
                ("T", "vim_window.previous_tab", false),
                ("Tab", "vim_window.last_tab", false),
            ],
        ),
        _ => return None,
    };
    Some(SequenceHint {
        prefix: prefix.to_string(),
        group,
        continuations: continuations.to_vec(),
    })
}

/// Resolve the preview scroll requested by `key` given a visible page of `page_rows` grid rows.
fn markdown_scroll_input(key: &Keystroke, page_rows: usize) -> Option<ScrollInput> {
    let modifiers = key.modifiers;
    if modifiers.alt || modifiers.function || modifiers.platform {
        return None;
    }
    let page = page_rows as isize;
    let half_page = (page / 2).max(1);
    let raw = key.key.as_str();
    if modifiers.control && !modifiers.shift {
        return match raw {
            "b" => Some(ScrollInput::Wheel(-page)),
            "f" => Some(ScrollInput::Wheel(page)),
            "u" => Some(ScrollInput::Wheel(-half_page)),
            "d" => Some(ScrollInput::Wheel(half_page)),
            _ => None,
        };
    }
    if modifiers.shift && normal_character(key).as_deref() == Some("G") && !modifiers.control {
        return Some(ScrollInput::Position(usize::MAX));
    }
    if modifiers.modified() {
        return None;
    }
    match raw {
        "k" | "up" => Some(ScrollInput::Wheel(-1)),
        "j" | "down" => Some(ScrollInput::Wheel(1)),
        "pageup" => Some(ScrollInput::Wheel(-page)),
        "pagedown" => Some(ScrollInput::Wheel(page)),
        "home" => Some(ScrollInput::Position(0)),
        "end" => Some(ScrollInput::Position(usize::MAX)),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
