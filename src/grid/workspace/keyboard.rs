//! Global shortcuts, modal keyboard routing and committed native text.

use crate::grid::windows::{editor::EditorWindow, help::HelpCommand, manager::ManagerWindow};
use crate::ui::keys::normal_character;

use super::{
    Workspace,
    editor_close::CloseTarget,
    layout::{Focus, ShellWindow},
    tabs::ProjectShortcut,
};
use gpui::{Context, KeyDownEvent, Keystroke, Window};
use nanobug::{
    animation::{cursor::CursorConfig, scroll::ScrollInput},
    i18n,
    vim::Mode,
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
}

/// The complete suffix entered after the leader, identifying the current group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct LeaderPrefix(&'static str);

/// One continuation shared by keyboard dispatch and the leader hint.
pub(super) struct LeaderBinding {
    /// Entire printable sequence after the leader.
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
        description_key: "leader.open_manager",
        shortcut: Some(ShellShortcut::Select(Focus::Manager)),
    },
    LeaderBinding {
        sequence: "f",
        description_key: "leader.open_file_picker",
        shortcut: Some(ShellShortcut::OpenFilePicker),
    },
    LeaderBinding {
        sequence: "p",
        description_key: "leader.project_group",
        shortcut: None,
    },
    LeaderBinding {
        sequence: "q",
        description_key: "leader.quit_group",
        shortcut: None,
    },
    LeaderBinding {
        sequence: "L",
        description_key: "leader.change_language",
        shortcut: Some(ShellShortcut::OpenLanguagePicker),
    },
    LeaderBinding {
        sequence: "pa",
        description_key: "leader.open_start",
        shortcut: Some(ShellShortcut::OpenStart),
    },
    LeaderBinding {
        sequence: "qq",
        description_key: "leader.close_project",
        shortcut: Some(ShellShortcut::Close(CloseTarget::Project)),
    },
    LeaderBinding {
        sequence: "qa",
        description_key: "leader.quit",
        shortcut: Some(ShellShortcut::Close(CloseTarget::Quit)),
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

    /// Yield each immediately available key and binding, in display order.
    pub(super) fn bindings(
        self,
        project_active: bool,
        manager_available: bool,
    ) -> impl Iterator<Item = (&'static str, &'static LeaderBinding)> {
        LEADER_BINDINGS.iter().filter_map(move |binding| {
            let key = binding.sequence.strip_prefix(self.0)?;
            let available = match binding.shortcut {
                Some(ShellShortcut::Select(Focus::Manager)) => manager_available,
                Some(ShellShortcut::Close(CloseTarget::Project)) => project_active,
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
    /// The direction has no target in this layout.
    Stay,
    /// Open the language picker without changing the active pane.
    OpenLanguagePicker,
    /// Open the workspace's three-column file picker.
    OpenFilePicker,
    /// Add a tab showing the start page.
    OpenStart,
    /// Close the current project or window after resolving unsaved buffers.
    Close(CloseTarget),
}

impl ShellShortcut {
    /// Resolve `key` and its printable `character` after `prefix`; a start page has no pane `focus`.
    /// The tab's Explorer availability and `hidden_pane` gate pane navigation.
    fn resolve(
        focus: Option<Focus>,
        mode: Mode,
        hidden_pane: Option<Focus>,
        manager_available: bool,
        prefix: Option<PendingPrefix>,
        key: &Keystroke,
        character: Option<&str>,
    ) -> Option<Self> {
        let modifiers = key.modifiers;
        let uppercase = matches!(character.map(str::as_bytes), Some([b'A'..=b'Z']));
        if (focus == Some(Focus::Editor) && mode != Mode::Normal)
            || modifiers.alt
            || modifiers.platform
            || modifiers.shift != uppercase
            || modifiers.function
        {
            return None;
        }
        if modifiers.control {
            return match (focus?, key.key.as_str()) {
                (Focus::Editor, "h")
                    if manager_available && hidden_pane != Some(Focus::Manager) =>
                {
                    Some(Self::Select(Focus::Manager))
                }
                (Focus::Manager, "l") => Some(Self::Select(Focus::Editor)),
                (_, "h" | "j" | "k" | "l") => Some(Self::Stay),
                _ => None,
            };
        }
        if character == Some(LEADER_KEY) {
            return Some(Self::AwaitLeader(LeaderPrefix::ROOT));
        }
        let Some(PendingPrefix::Leader(prefix)) = prefix else {
            return None;
        };
        match key.key.as_str() {
            "escape" => Some(Self::CancelLeader),
            "backspace" => Some(
                prefix
                    .parent()
                    .map_or(Self::CancelLeader, Self::AwaitLeader),
            ),
            _ => prefix
                .bindings(focus.is_some(), manager_available)
                .find(|(key, _)| Some(*key) == character)
                .map(|(_, binding)| {
                    binding
                        .shortcut
                        .unwrap_or(Self::AwaitLeader(LeaderPrefix(binding.sequence)))
                }),
        }
    }
}

impl Workspace {
    /// Route committed native `text` to the active filter or editor in `window` through `cx`.
    pub(super) fn commit_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.accepts_native_text() {
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
        self.cursor
            .update(cx, |cursor, cx| cursor.activity(window, cx));
        self.projects.active_mut().status.clear();
        let text = text.replace("\r\n", "\n");
        self.run_vim(|vim, register| vim.text(&text, register), window, cx);
        self.sync(window, cx);
    }

    /// Cancel the active pane's partial navigation before a shell shortcut.
    fn cancel_pending_pane_input(&mut self) {
        if let Some(project) = self.projects.active_project_mut() {
            if let Some(manager) = &mut project.manager {
                manager.count = 0;
            }
            if project.focus == Focus::Editor {
                let editor = &mut project.editor;
                editor.buffers[editor.active].vim.cancel_pending();
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
            manager.paste_timer = None;
            manager.click = None;
        }
        project.editor.dragging = None;
        project.editor.composition.clear();
        self.hovered_controls = None;
        self.resizing = false;
        self.help
            .open(pane == Focus::Manager, self.projects.manager_available());
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
        if !self.modal_open()
            && let Some(shortcut) = ProjectShortcut::resolve(&event.keystroke)
        {
            self.dismiss_leader_hint(cx);
            self.project_shortcut(shortcut, window, cx);
            cx.stop_propagation();
            return;
        }
        let raw = event.keystroke.key.as_str();
        if self.held_close_key.as_deref() == Some(raw) {
            if event.is_held {
                cx.stop_propagation();
                return;
            }
            self.held_close_key = None;
        }
        self.cursor
            .update(cx, |cursor, cx| cursor.activity(window, cx));
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
                HelpCommand::Ignore => {}
            }
            cx.stop_propagation();
            return;
        }
        if self.file_picker.is_some() {
            if self.file_picker_shell_key(event, window, cx)
                || self.file_picker_key(event, window, cx)
            {
                cx.stop_propagation();
            }
            return;
        }
        let modifiers = event.keystroke.modifiers;
        let character = normal_character(&event.keystroke);
        let Some(project) = self.projects.active_project() else {
            self.start_key_down(event, character.as_deref(), window, cx);
            cx.stop_propagation();
            return;
        };
        let printable = character.as_deref().unwrap_or(raw);
        let focus = project.focus;
        let mode = project.editor.vim().mode;
        let shortcut = ShellShortcut::resolve(
            Some(focus),
            mode,
            project.hidden_pane,
            project.manager.is_some(),
            self.prefix,
            &event.keystroke,
            character.as_deref(),
        );
        // A q in a leader sequence belongs to the workspace before Explorer sees it.
        let leader_pending = matches!(self.prefix, Some(PendingPrefix::Leader(_)));
        let close_pane = shortcut.is_none()
            && !leader_pending
            && match focus {
                Focus::Manager => ManagerWindow::matches_close_key(character.as_deref()),
                Focus::Editor => EditorWindow::matches_close_key(&event.keystroke),
            };
        if close_pane {
            if !event.is_held {
                self.held_close_key = Some(raw.into());
                self.close_grid_window(ShellWindow::Pane(focus), window, cx);
                self.sync(window, cx);
            }
            cx.stop_propagation();
            return;
        }
        let vim_keys = focus == Focus::Manager
            || matches!(mode, Mode::Normal | Mode::Visual | Mode::VisualLine);
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
        if leader_pending {
            cx.notify();
        }
        if let Some(manager) = &mut self.projects.active_mut().manager {
            manager.paste_timer = None;
        }
        if vim_keys && character.as_deref() == Some("?") {
            self.open_shortcut_help(focus, window, cx);
            cx.stop_propagation();
            return;
        }
        if focus == Focus::Editor
            && (modifiers.control || modifiers.platform)
            && raw == "v"
            && matches!(mode, Mode::Insert | Mode::Command | Mode::Search)
        {
            if let Some(item) = cx.read_from_clipboard()
                && let Some(text) = item.text()
            {
                self.projects.active_mut().editor.composition.clear();
                self.commit_text(&text, window, cx);
            }
            cx.stop_propagation();
            return;
        }
        if !self.projects.active().editor.composition.is_empty()
            && !modifiers.control
            && !modifiers.platform
        {
            return;
        }
        if let Some(shortcut) = shortcut {
            self.apply_shell_shortcut(shortcut, raw, window, cx);
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
                project.status = match manager.tree.refresh() {
                    Ok(()) => i18n::text("workspace.project_refreshed").into(),
                    Err(error) => format!("{error:#}"),
                };
                self.repaint([ScrollInput::Advance; 3], window, cx);
            }
            cx.stop_propagation();
            return;
        } else if self.projects.active().focus == Focus::Manager {
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
        } else {
            let Some(input) = self
                .projects
                .active_mut()
                .editor
                .key(&event.keystroke, self.layout.editor.h.saturating_sub(2))
            else {
                return;
            };
            if input.clear_status {
                self.projects.active_mut().status.clear();
            }
            self.apply_edit(input.edit, window, cx);
        }
        self.sync(window, cx);
        cx.stop_propagation();
    }

    /// Dispatch the shared leader for `event` while the picker has no local prompt or prefix.
    fn file_picker_shell_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let state = &self.file_picker.as_ref().expect("active picker").state;
        if state.busy || state.prompt.is_some() || state.goto || state.delete.is_some() {
            return false;
        }
        let key = &event.keystroke;
        let character = normal_character(key);
        let pending = matches!(self.prefix, Some(PendingPrefix::Leader(_)));
        if !pending && character.as_deref() != Some(LEADER_KEY) {
            return false;
        }
        if event.is_held {
            return true;
        }
        let focus = self.projects.active_project().map(|project| project.focus);
        let shortcut = ShellShortcut::resolve(
            focus,
            Mode::Normal,
            None,
            self.projects.manager_available(),
            self.prefix,
            key,
            character.as_deref(),
        );
        self.dismiss_leader_hint(cx);
        if let Some(shortcut) = shortcut {
            if matches!(
                shortcut,
                ShellShortcut::Select(_) | ShellShortcut::OpenStart | ShellShortcut::Close(_)
            ) {
                self.file_picker = None;
            }
            self.apply_shell_shortcut(shortcut, &key.key, window, cx);
        }
        true
    }

    /// Give leader sequences priority over launcher mnemonics on the start page.
    fn start_key_down(
        &mut self,
        event: &KeyDownEvent,
        character: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.keystroke.key == "f6" {
            self.dismiss_leader_hint(cx);
            self.cycle_cursor_effect(event.keystroke.modifiers.shift, window, cx);
            return;
        }
        if self
            .projects
            .start()
            .is_some_and(|page| page.picker.is_some())
        {
            self.project_picker_key(event, character, window, cx);
            return;
        }
        let pending = matches!(self.prefix, Some(PendingPrefix::Leader(_)));
        let shortcut = ShellShortcut::resolve(
            None,
            Mode::Normal,
            None,
            false,
            self.prefix,
            &event.keystroke,
            character,
        );
        if event.is_held && (pending || matches!(shortcut, Some(ShellShortcut::AwaitLeader(_)))) {
            return;
        }
        self.dismiss_leader_hint(cx);
        if let Some(shortcut) = shortcut {
            self.apply_shell_shortcut(shortcut, &event.keystroke.key, window, cx);
        } else if !pending {
            self.start_page_key(event, window, cx);
        }
    }

    /// Switch the shared cursor preset in `window` and report it in the current tab.
    /// `reverse` selects the previous preset.
    fn cycle_cursor_effect(&mut self, reverse: bool, window: &mut Window, cx: &mut Context<Self>) {
        let effect = self
            .cursor
            .update(cx, |cursor, cx| cursor.cycle(reverse, window, cx));
        self.set_status(i18n::format(
            "cursor.changed",
            &[("effect", &effect.label())],
        ));
        self.repaint([ScrollInput::Advance; 3], window, cx);
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
                let project = self.projects.active_mut();
                project.focus = target;
                if project.hidden_pane == Some(target) {
                    project.hidden_pane = None;
                    project.status.clear();
                }
                self.sync(window, cx);
            }
            ShellShortcut::Stay => {}
            ShellShortcut::OpenLanguagePicker => self.open_language_picker(window, cx),
            ShellShortcut::OpenFilePicker => self.open_file_picker(window, cx),
            ShellShortcut::OpenStart => self.open_start_tab(window, cx),
            ShellShortcut::Close(target) => {
                self.held_close_key = Some(raw.into());
                self.request_close(target, window, cx);
                self.sync(window, cx);
            }
        }
    }
}

#[cfg(test)]
mod tests;
