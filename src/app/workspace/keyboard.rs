//! Global shortcuts, modal keyboard routing and committed native text.

use super::{
    Editor,
    layout::{Focus, ShellWindow},
};
use gpui::{Context, KeyDownEvent, Keystroke, Window};
use nanobug::{
    animation::{cursor::CursorConfig, scroll::ScrollInput},
    editor::{Action, Mode, normal_character},
};

/// A shell shortcut recognized before the active pane handles the key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PaneShortcut {
    /// Remember Space until the next key.
    ExplorerPrefix,
    /// Focus the target pane and restore it if hidden.
    Select(Focus),
    /// The direction has no target in this layout.
    Stay,
}

impl PaneShortcut {
    /// Resolve `key` and its printable `character` after `prefix` in the active `focus`.
    /// `mode` gates editor shortcuts and `hidden_pane` determines whether Explorer is visible.
    fn resolve(
        focus: Focus,
        mode: Mode,
        hidden_pane: Option<Focus>,
        prefix: Option<char>,
        key: &Keystroke,
        character: Option<&str>,
    ) -> Option<Self> {
        let modifiers = key.modifiers;
        if (focus == Focus::Editor && mode != Mode::Normal)
            || modifiers.alt
            || modifiers.platform
            || modifiers.shift
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
        match (prefix, character) {
            (_, Some(" ")) => Some(Self::ExplorerPrefix),
            (Some(' '), Some("e")) => Some(Self::Select(Focus::Manager)),
            _ => None,
        }
    }
}

impl Editor {
    /// Route committed native `text` solely to the editor's Vim state machine.
    pub(super) fn commit_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.focus != Focus::Editor {
            return;
        }
        self.cursor
            .update(cx, |cursor, cx| cursor.activity(window, cx));
        self.status.clear();
        let text = text.replace("\r\n", "\n");
        self.run_vim(|vim, register| vim.text(&text, register), window);
        self.sync(window, cx);
    }

    /// Cancel the active pane's partial navigation before a shell shortcut.
    fn cancel_pending_pane_input(&mut self) {
        self.tree_count = 0;
        if self.focus == Focus::Editor {
            self.buffers[self.active].vim.cancel_pending();
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
        self.cursor
            .update(cx, |cursor, cx| cursor.activity(window, cx));
        let raw = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        let character = normal_character(&event.keystroke);
        let printable = character.as_deref().unwrap_or(raw);
        let vim_keys = self.focus == Focus::Manager
            || matches!(
                self.vim().mode,
                Mode::Normal | Mode::Visual | Mode::VisualLine
            );
        if event.is_held
            && ((vim_keys && matches!(character.as_deref(), Some("q" | "X")))
                || (self.focus == Focus::Manager
                    && matches!(character.as_deref(), Some("z" | "v" | "Z"))))
        {
            cx.stop_propagation();
            return;
        }
        let prefix = self.prefix.take();
        self.paste_timer = None;
        if self.help_top.is_some() {
            match printable {
                "escape" | "X" => self.close_grid_window(ShellWindow::ShortcutHelp, window),
                "q" if prefix == Some('q') => {
                    self.close_grid_window(ShellWindow::ShortcutHelp, window);
                }
                "q" => self.prefix = Some('q'),
                "j" | "down" => self.scroll_help(1, cx),
                "k" | "up" => self.scroll_help(-1, cx),
                _ => {}
            }
            if self.help_top.is_none() {
                self.repaint([ScrollInput::Advance; 3], window, cx);
            }
            cx.stop_propagation();
            return;
        }
        if vim_keys && matches!(character.as_deref(), Some("X" | "q" | "?")) {
            self.cancel_pending_pane_input();
            match printable {
                "?" => {
                    self.help_top = Some(0);
                    self.resizing = false;
                    self.dragging = None;
                }
                "X" => self.close_grid_window(ShellWindow::Pane(self.focus), window),
                "q" if prefix == Some('q') => {
                    self.close_grid_window(ShellWindow::Pane(self.focus), window);
                }
                _ => self.prefix = Some('q'),
            }
            self.sync(window, cx);
            cx.stop_propagation();
            return;
        }
        if self.focus == Focus::Editor
            && (modifiers.control || modifiers.platform)
            && raw == "v"
            && matches!(self.vim().mode, Mode::Insert | Mode::Command | Mode::Search)
        {
            if let Some(item) = cx.read_from_clipboard()
                && let Some(text) = item.text()
            {
                self.composition.clear();
                self.composition_selection = 0..0;
                self.commit_text(&text, window, cx);
            }
            cx.stop_propagation();
            return;
        }
        if !self.composition.is_empty() && !modifiers.control && !modifiers.platform {
            return;
        }
        if let Some(shortcut) = PaneShortcut::resolve(
            self.focus,
            self.vim().mode,
            self.hidden_pane,
            prefix,
            &event.keystroke,
            character.as_deref(),
        ) {
            self.cancel_pending_pane_input();
            match shortcut {
                PaneShortcut::ExplorerPrefix => self.prefix = Some(' '),
                PaneShortcut::Select(target) => {
                    self.focus = target;
                    if self.hidden_pane == Some(target) {
                        self.hidden_pane = None;
                        self.status.clear();
                    }
                    self.sync(window, cx);
                }
                PaneShortcut::Stay => {}
            }
            cx.stop_propagation();
            return;
        }
        if raw == "f6" {
            self.cursor_effect = self
                .cursor
                .update(cx, |cursor, cx| cursor.cycle(modifiers.shift, window, cx));
            self.status = format!(
                "光标动效：{} · F6 下一种，Shift+F6 上一种",
                self.cursor_effect.label()
            );
            self.repaint([ScrollInput::Advance; 3], window, cx);
            cx.stop_propagation();
            return;
        } else if raw == "f7" {
            self.status = match CursorConfig::load(&self.project) {
                Ok(config) => {
                    self.cursor
                        .update(cx, |cursor, cx| cursor.configure(config, window, cx));
                    "已重载光标效果配置".into()
                }
                Err(error) => format!("光标配置：{error}"),
            };
            self.repaint([ScrollInput::Advance; 3], window, cx);
            cx.stop_propagation();
            return;
        } else if raw == "f5" {
            self.status = match self.tree.refresh() {
                Ok(()) => "项目已刷新".into(),
                Err(error) => format!("{error:#}"),
            };
            self.repaint([ScrollInput::Advance; 3], window, cx);
            cx.stop_propagation();
            return;
        } else if self.focus == Focus::Manager {
            let key = if modifiers.control || modifiers.alt || modifiers.platform {
                ""
            } else {
                printable
            };
            if let Some(input) = self.manager_key(key, prefix, window, cx) {
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
            let key = if modifiers.control || modifiers.platform {
                format!("Ctrl+{raw}")
            } else {
                match raw {
                    "escape" => "Escape".into(),
                    "enter" => "Enter".into(),
                    "backspace" => "Backspace".into(),
                    "delete" => "Delete".into(),
                    "tab" => "Tab".into(),
                    "left" => "Left".into(),
                    "right" => "Right".into(),
                    "up" => "Up".into(),
                    "down" => "Down".into(),
                    "home" => "Home".into(),
                    "end" => "End".into(),
                    "pageup" | "pagedown" => {
                        let direction = if raw == "pageup" { "Up" } else { "Down" };
                        let height = self.layout.editor.h.saturating_sub(2);
                        self.run_vim(
                            |vim, register| {
                                for _ in 0..height {
                                    vim.key(direction, register);
                                }
                                Action::None
                            },
                            window,
                        );
                        self.sync(window, cx);
                        cx.stop_propagation();
                        return;
                    }
                    _ if matches!(
                        self.vim().mode,
                        Mode::Normal | Mode::Visual | Mode::VisualLine
                    ) =>
                    {
                        let Some(character) = character else {
                            return;
                        };
                        character
                    }
                    _ => return,
                }
            };
            self.status.clear();
            self.run_vim(|vim, register| vim.key(&key, register), window);
        }
        self.sync(window, cx);
        cx.stop_propagation();
    }
}

#[cfg(test)]
mod tests;
