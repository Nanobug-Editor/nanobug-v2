//! Global shortcuts, modal keyboard routing and committed native text.

use crate::grid::windows::{editor::EditorWindow, help::HelpCommand, manager::ManagerWindow};
use crate::ui::keys::normal_character;

use super::{
    Workspace,
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
        sequence: "q",
        description_key: "leader.project_group",
        shortcut: None,
    },
    LeaderBinding {
        sequence: "L",
        description_key: "leader.change_language",
        shortcut: Some(ShellShortcut::OpenLanguagePicker),
    },
    LeaderBinding {
        sequence: "qq",
        description_key: "leader.close_project",
        shortcut: Some(ShellShortcut::CloseProject),
    },
    LeaderBinding {
        sequence: "qa",
        description_key: "leader.close_all_projects",
        shortcut: Some(ShellShortcut::Quit),
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
    pub(super) fn bindings(self) -> impl Iterator<Item = (&'static str, &'static LeaderBinding)> {
        LEADER_BINDINGS.iter().filter_map(move |binding| {
            let key = binding.sequence.strip_prefix(self.0)?;
            (key.len() == 1).then_some((key, binding))
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
    /// Close the active project after checking its buffers.
    CloseProject,
    /// Exit after checking every project's buffers.
    Quit,
}

impl ShellShortcut {
    /// Resolve `key` and its printable `character` after `prefix` in the active `focus`.
    /// `mode` gates editor shortcuts and `hidden_pane` determines whether Explorer is visible.
    fn resolve(
        focus: Focus,
        mode: Mode,
        hidden_pane: Option<Focus>,
        prefix: Option<PendingPrefix>,
        key: &Keystroke,
        character: Option<&str>,
    ) -> Option<Self> {
        let modifiers = key.modifiers;
        let uppercase = matches!(character.map(str::as_bytes), Some([b'A'..=b'Z']));
        if (focus == Focus::Editor && mode != Mode::Normal)
            || modifiers.alt
            || modifiers.platform
            || modifiers.shift != uppercase
            || modifiers.function
        {
            return None;
        }
        if modifiers.control {
            return match (focus, key.key.as_str()) {
                (Focus::Editor, "h") if hidden_pane != Some(Focus::Manager) => {
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
                .bindings()
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
        self.cursor
            .update(cx, |cursor, cx| cursor.activity(window, cx));
        self.projects.active.status.clear();
        let text = text.replace("\r\n", "\n");
        self.run_vim(|vim, register| vim.text(&text, register), window, cx);
        self.sync(window, cx);
    }

    /// Cancel the active pane's partial navigation before a shell shortcut.
    fn cancel_pending_pane_input(&mut self) {
        self.projects.active.manager.count = 0;
        if self.projects.active.focus == Focus::Editor {
            self.projects.active.editor.buffers[self.projects.active.editor.active]
                .vim
                .cancel_pending();
        }
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
                    self.close_grid_window(ShellWindow::ShortcutHelp, window);
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
        let modifiers = event.keystroke.modifiers;
        let character = normal_character(&event.keystroke);
        let printable = character.as_deref().unwrap_or(raw);
        let focus = self.projects.active.focus;
        let mode = self.projects.active.editor.vim().mode;
        let shortcut = ShellShortcut::resolve(
            focus,
            mode,
            self.projects.active.hidden_pane,
            self.prefix,
            &event.keystroke,
            character.as_deref(),
        );
        // A q in a leader sequence belongs to the workspace before Explorer sees it.
        let close_pane = shortcut.is_none()
            && match focus {
                Focus::Manager => ManagerWindow::matches_close_key(character.as_deref()),
                Focus::Editor => EditorWindow::matches_close_key(&event.keystroke),
            };
        if close_pane {
            if !event.is_held {
                self.held_close_key = Some(raw.into());
                self.close_grid_window(ShellWindow::Pane(focus), window);
                self.sync(window, cx);
            }
            cx.stop_propagation();
            return;
        }
        let vim_keys = focus == Focus::Manager
            || matches!(mode, Mode::Normal | Mode::Visual | Mode::VisualLine);
        let leader_pending = matches!(self.prefix, Some(PendingPrefix::Leader(_)));
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
        self.projects.active.manager.paste_timer = None;
        if vim_keys && character.as_deref() == Some("?") {
            self.cancel_pending_pane_input();
            self.help.open(self.projects.active.focus == Focus::Manager);
            self.hovered_close = None;
            self.resizing = false;
            self.projects.active.editor.dragging = None;
            self.sync(window, cx);
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
                self.projects.active.editor.composition.clear();
                self.commit_text(&text, window, cx);
            }
            cx.stop_propagation();
            return;
        }
        if !self.projects.active.editor.composition.is_empty()
            && !modifiers.control
            && !modifiers.platform
        {
            return;
        }
        if let Some(shortcut) = shortcut {
            self.cancel_pending_pane_input();
            match shortcut {
                ShellShortcut::AwaitLeader(prefix) => self.show_leader_hint(prefix, cx),
                ShellShortcut::CancelLeader => self.held_close_key = Some(raw.into()),
                ShellShortcut::Select(target) => {
                    self.projects.active.focus = target;
                    if self.projects.active.hidden_pane == Some(target) {
                        self.projects.active.hidden_pane = None;
                        self.projects.active.status.clear();
                    }
                    self.sync(window, cx);
                }
                ShellShortcut::Stay => {}
                ShellShortcut::OpenLanguagePicker => self.open_language_picker(window, cx),
                ShellShortcut::CloseProject | ShellShortcut::Quit => {
                    self.held_close_key = Some(raw.into());
                    if shortcut == ShellShortcut::CloseProject {
                        self.close_project(window, cx);
                    } else {
                        self.quit(false, window);
                    }
                    self.sync(window, cx);
                }
            }
            cx.stop_propagation();
            return;
        }
        if raw == "f6" {
            self.cursor_effect = self
                .cursor
                .update(cx, |cursor, cx| cursor.cycle(modifiers.shift, window, cx));
            self.projects.active.status =
                i18n::format("cursor.changed", &[("effect", &self.cursor_effect.label())]);
            self.repaint([ScrollInput::Advance; 3], window, cx);
            cx.stop_propagation();
            return;
        } else if raw == "f7" {
            self.projects.active.status = match CursorConfig::load(&self.projects.active.root) {
                Ok(config) => {
                    self.projects.active.cursor_config = config.clone();
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
            self.projects.active.status = match self.projects.active.manager.tree.refresh() {
                Ok(()) => i18n::text("workspace.project_refreshed").into(),
                Err(error) => format!("{error:#}"),
            };
            self.repaint([ScrollInput::Advance; 3], window, cx);
            cx.stop_propagation();
            return;
        } else if self.projects.active.focus == Focus::Manager {
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
                .active
                .editor
                .key(&event.keystroke, self.layout.editor.h.saturating_sub(2))
            else {
                return;
            };
            if input.clear_status {
                self.projects.active.status.clear();
            }
            self.apply_edit(input.edit, window, cx);
        }
        self.sync(window, cx);
        cx.stop_propagation();
    }
}

#[cfg(test)]
mod tests;
