//! Parse Ctrl+w sequences and apply them to the current tab's retained panes.

use super::{
    Workspace,
    editor_close::CloseTarget,
    keyboard::PendingPrefix,
    layout::{Focus, ShellWindow},
    projects::{Project, SplitDocument},
};
use crate::ui::keys::normal_character;
use gpui::{Context, KeyDownEvent, Keystroke, Modifiers, Window};
use nanobug::{
    i18n,
    vim::{Continuation, SequenceHint},
};
use std::path::Path;

/// Keys entered after Ctrl+w, with counts on either side of the prefix.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct WindowSequence {
    /// Numeric prefix captured from the pane.
    before: Option<usize>,
    /// Numeric continuation collected before a subcommand.
    after: Option<usize>,
    /// Whether the next key belongs to the Ctrl+w g subgroup.
    tab: bool,
}

impl WindowSequence {
    /// Multiply counts on both sides of the prefix, or return the one given.
    fn count(self) -> Option<usize> {
        match (self.before, self.after) {
            (Some(before), Some(after)) => Some(before.saturating_mul(after)),
            (before, after) => before.or(after),
        }
    }

    /// Update a post-prefix count from `key` when it is a digit or Delete.
    fn edit_count(&mut self, key: &str) -> bool {
        if self.tab {
            return false;
        }
        if let [digit @ b'0'..=b'9'] = key.as_bytes()
            && (*digit != b'0' || self.after.is_some())
        {
            self.after =
                Some((self.after.unwrap_or(0) * 10 + usize::from(*digit - b'0')).min(9999));
        } else if key == "delete"
            && let Some(count) = self.after
        {
            self.after = (count >= 10).then_some(count / 10);
        } else {
            return false;
        }
        true
    }
}

/// Effect of a recognized window command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WindowCommand {
    /// Move to an adjacent pane along the requested direction.
    Direction(&'static str),
    /// Cycle toward the next or previous pane, or focus a numbered pane.
    Cycle { reverse: bool },
    /// Focus the first or last visible pane.
    Edge(bool),
    /// Restore the last focused visible pane.
    Previous,
    /// Open a second editor along the requested split axis.
    Split { stacked: bool },
    /// Open an untitled buffer in a new horizontal split.
    SplitEmpty,
    /// Open the file under the caret in a horizontal split, optionally using its line number.
    SplitFile { line: bool },
    /// Fork the previous or numbered buffer into a horizontal split.
    SplitAlternate,
    /// Jump to an identifier or a macro definition in this buffer or a local include in a new split.
    SplitInclude { defines: bool },
    /// Focus the current tabpage's dedicated preview split.
    PreviewFocus,
    /// Close the current tabpage's dedicated preview split.
    PreviewClose,
    /// Transfer the active editor and its buffers to a new tabpage.
    MoveToTab,
    /// Close an optionally numbered pane, or quit when the last one is closed.
    Close { quit: bool },
    /// Close the other pane and keep an optionally numbered pane.
    Only,
    /// Shift the pane order and keep the current focus, optionally towards the leading edge.
    Rotate { reverse: bool },
    /// Exchange panes and focus the occupant of the old location.
    Exchange,
    /// Move the focused pane to an edge and align the split to that edge.
    Move { stacked: bool, first: bool },
    /// Adjust or set the size of the focused pane along an axis.
    Resize { stacked: bool, amount: Resize },
    /// Set the visible panes to equal sizes along their shared axis.
    Equalize,
    /// Navigate to the next, previous, or last accessed tab.
    Tab(i32),
    /// Open the file under the cursor in a separate editor-only tab.
    OpenFile { line: bool },
    /// Show the current buffer's diagnostic at the caret.
    Diagnostic,
    /// Wait for a second subcommand key.
    TabPrefix,
    /// End the pending sequence without an action.
    Cancel,
}

/// Size adjustment relative to the focused pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Resize {
    /// Add the count to the current size.
    Grow,
    /// Subtract the count from the current size.
    Shrink,
    /// Set the size to the count, or the largest allowed size when omitted.
    Set,
}

/// One displayed row of equivalent commands, a translation, and a shared handler.
struct Binding {
    /// Keys normalized by `window_key`.
    keys: &'static [&'static str],
    /// Complete label rendered in the hint.
    label: &'static str,
    /// Translation key for the command's description.
    caption: &'static str,
    /// Effect used by the dispatcher.
    action: WindowCommand,
}

/// Prefix table used by both dispatch and the localized hint.
const WINDOW_BINDINGS: &[Binding] = &[
    Binding {
        keys: &["s", "S"],
        label: "s/S",
        caption: "vim_window.split_stacked",
        action: WindowCommand::Split { stacked: true },
    },
    Binding {
        keys: &["v"],
        label: "v",
        caption: "vim_window.split_side_by_side",
        action: WindowCommand::Split { stacked: false },
    },
    Binding {
        keys: &["n"],
        label: "n",
        caption: "vim_window.split_empty",
        action: WindowCommand::SplitEmpty,
    },
    Binding {
        keys: &["f"],
        label: "f",
        caption: "vim_window.split_file",
        action: WindowCommand::SplitFile { line: false },
    },
    Binding {
        keys: &["F"],
        label: "F",
        caption: "vim_window.split_file_line",
        action: WindowCommand::SplitFile { line: true },
    },
    Binding {
        keys: &["^"],
        label: "^",
        caption: "vim_window.split_alternate",
        action: WindowCommand::SplitAlternate,
    },
    Binding {
        keys: &["i", "tab"],
        label: "i",
        caption: "vim_window.split_include",
        action: WindowCommand::SplitInclude { defines: false },
    },
    Binding {
        keys: &["ctrl-d"],
        label: "Ctrl+d",
        caption: "vim_window.split_define",
        action: WindowCommand::SplitInclude { defines: true },
    },
    Binding {
        keys: &["P"],
        label: "P",
        caption: "vim_window.preview_focus",
        action: WindowCommand::PreviewFocus,
    },
    Binding {
        keys: &["z"],
        label: "z",
        caption: "vim_window.preview_close",
        action: WindowCommand::PreviewClose,
    },
    Binding {
        keys: &["T"],
        label: "T",
        caption: "vim_window.move_to_tab",
        action: WindowCommand::MoveToTab,
    },
    Binding {
        keys: &["h", "left", "backspace"],
        label: "h ←",
        caption: "vim_window.left",
        action: WindowCommand::Direction("h"),
    },
    Binding {
        keys: &["j", "down"],
        label: "j ↓",
        caption: "vim_window.down",
        action: WindowCommand::Direction("j"),
    },
    Binding {
        keys: &["k", "up"],
        label: "k ↑",
        caption: "vim_window.up",
        action: WindowCommand::Direction("k"),
    },
    Binding {
        keys: &["l", "right"],
        label: "l →",
        caption: "vim_window.right",
        action: WindowCommand::Direction("l"),
    },
    Binding {
        keys: &["w"],
        label: "w",
        caption: "vim_window.cycle",
        action: WindowCommand::Cycle { reverse: false },
    },
    Binding {
        keys: &["W"],
        label: "W",
        caption: "vim_window.cycle_reverse",
        action: WindowCommand::Cycle { reverse: true },
    },
    Binding {
        keys: &["t"],
        label: "t",
        caption: "vim_window.first",
        action: WindowCommand::Edge(true),
    },
    Binding {
        keys: &["b"],
        label: "b",
        caption: "vim_window.last",
        action: WindowCommand::Edge(false),
    },
    Binding {
        keys: &["p"],
        label: "p",
        caption: "vim_window.previous",
        action: WindowCommand::Previous,
    },
    Binding {
        keys: &["c"],
        label: "c",
        caption: "vim_window.close",
        action: WindowCommand::Close { quit: false },
    },
    Binding {
        keys: &["q"],
        label: "q",
        caption: "vim_window.quit",
        action: WindowCommand::Close { quit: true },
    },
    Binding {
        keys: &["o"],
        label: "o",
        caption: "vim_window.only",
        action: WindowCommand::Only,
    },
    Binding {
        keys: &["r"],
        label: "r",
        caption: "vim_window.rotate",
        action: WindowCommand::Rotate { reverse: false },
    },
    Binding {
        keys: &["R"],
        label: "R",
        caption: "vim_window.rotate_reverse",
        action: WindowCommand::Rotate { reverse: true },
    },
    Binding {
        keys: &["x"],
        label: "x",
        caption: "vim_window.exchange",
        action: WindowCommand::Exchange,
    },
    Binding {
        keys: &["H"],
        label: "H",
        caption: "vim_window.move_left",
        action: WindowCommand::Move {
            stacked: false,
            first: true,
        },
    },
    Binding {
        keys: &["J"],
        label: "J",
        caption: "vim_window.move_down",
        action: WindowCommand::Move {
            stacked: true,
            first: false,
        },
    },
    Binding {
        keys: &["K"],
        label: "K",
        caption: "vim_window.move_up",
        action: WindowCommand::Move {
            stacked: true,
            first: true,
        },
    },
    Binding {
        keys: &["L"],
        label: "L",
        caption: "vim_window.move_right",
        action: WindowCommand::Move {
            stacked: false,
            first: false,
        },
    },
    Binding {
        keys: &["="],
        label: "=",
        caption: "vim_window.equalize",
        action: WindowCommand::Equalize,
    },
    Binding {
        keys: &["+"],
        label: "+",
        caption: "vim_window.grow_height",
        action: WindowCommand::Resize {
            stacked: true,
            amount: Resize::Grow,
        },
    },
    Binding {
        keys: &["-"],
        label: "-",
        caption: "vim_window.shrink_height",
        action: WindowCommand::Resize {
            stacked: true,
            amount: Resize::Shrink,
        },
    },
    Binding {
        keys: &["_"],
        label: "_",
        caption: "vim_window.set_height",
        action: WindowCommand::Resize {
            stacked: true,
            amount: Resize::Set,
        },
    },
    Binding {
        keys: &[">"],
        label: ">",
        caption: "vim_window.grow_width",
        action: WindowCommand::Resize {
            stacked: false,
            amount: Resize::Grow,
        },
    },
    Binding {
        keys: &["<"],
        label: "<",
        caption: "vim_window.shrink_width",
        action: WindowCommand::Resize {
            stacked: false,
            amount: Resize::Shrink,
        },
    },
    Binding {
        keys: &["|"],
        label: "|",
        caption: "vim_window.set_width",
        action: WindowCommand::Resize {
            stacked: false,
            amount: Resize::Set,
        },
    },
    Binding {
        keys: &["g"],
        label: "g",
        caption: "vim_window.tabs",
        action: WindowCommand::TabPrefix,
    },
    Binding {
        keys: &["d"],
        label: "d",
        caption: "vim_window.diagnostic",
        action: WindowCommand::Diagnostic,
    },
];

/// Ctrl+w g entries implemented by the tab host.
const TAB_BINDINGS: &[Binding] = &[
    Binding {
        keys: &["t"],
        label: "t",
        caption: "vim_window.next_tab",
        action: WindowCommand::Tab(1),
    },
    Binding {
        keys: &["T"],
        label: "T",
        caption: "vim_window.previous_tab",
        action: WindowCommand::Tab(-1),
    },
    Binding {
        keys: &["tab"],
        label: "Tab",
        caption: "vim_window.last_tab",
        action: WindowCommand::Tab(0),
    },
    Binding {
        keys: &["f"],
        label: "f",
        caption: "vim_window.file_tab",
        action: WindowCommand::OpenFile { line: false },
    },
    Binding {
        keys: &["F"],
        label: "F",
        caption: "vim_window.file_tab_line",
        action: WindowCommand::OpenFile { line: true },
    },
];

/// Select the table that follows `sequence`.
fn bindings(sequence: WindowSequence) -> &'static [Binding] {
    if sequence.tab {
        TAB_BINDINGS
    } else {
        WINDOW_BINDINGS
    }
}

/// Translate native `key` into a literal, directional key, or Vim Ctrl alias.
fn window_key(key: &Keystroke) -> Option<String> {
    if let Some(character) = normal_character(key) {
        return Some(character);
    }
    if key.modifiers == Modifiers::default() {
        return Some(key.key.clone());
    }
    if key.modifiers.control
        && !key.modifiers.alt
        && !key.modifiers.platform
        && !key.modifiers.function
    {
        if !key.modifiers.shift {
            return match key.key.as_str() {
                "c" | "[" => Some("escape".into()),
                "d" => Some("ctrl-d".into()),
                "b" | "f" | "g" | "h" | "i" | "j" | "k" | "l" | "n" | "o" | "p" | "q" | "r"
                | "s" | "t" | "v" | "w" | "x" | "z" | "tab" => Some(key.key.clone()),
                "6" | "^" => Some("^".into()),
                "_" => Some("_".into()),
                _ => None,
            };
        }
        if key.key == "-" {
            return Some("_".into());
        }
        if matches!(key.key.as_str(), "6" | "^") {
            return Some("^".into());
        }
    }
    None
}

/// Determine the neighboring pane for `direction` in the project's two-pane `split`
/// (stacked and editor first), assuming both panes are available.
pub(super) fn direction_target(
    focus: Focus,
    split: (bool, bool),
    direction: &str,
) -> Option<Focus> {
    let (stacked, editor_first) = split;
    let first = (focus == Focus::Editor) == editor_first;
    let can_move = match direction {
        "h" => !stacked && !first,
        "l" => !stacked && first,
        "k" => stacked && !first,
        "j" => stacked && first,
        _ => false,
    };
    can_move.then_some(focus.other())
}

impl Workspace {
    /// Execute Ex `:wincmd` with the same command table as Ctrl+w.
    pub(super) fn ex_window_command(
        &mut self,
        keys: &str,
        count: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (table, key) = if keys.chars().count() == 2 && keys.starts_with('g') {
            (TAB_BINDINGS, &keys[1..])
        } else {
            (WINDOW_BINDINGS, keys)
        };
        if let Some(binding) = table.iter().find(|binding| binding.keys.contains(&key))
            && window_binding_available(self.projects.active(), binding.action)
        {
            self.apply_window_command(binding.action, count, window, cx);
        }
    }

    /// Whether closing the focused window can leave another workspace window or tab open.
    pub(super) fn can_close_leader_window(&self) -> bool {
        self.projects.active_project().is_some_and(|project| {
            (project.focus == Focus::Editor && project.split_count() > 1)
                || (project.manager.is_some() && project.hidden_pane.is_none())
                || self.projects.len() > 1
        })
    }

    /// Close the focused window through the guarded Ctrl+w c path.
    pub(super) fn close_leader_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.can_close_leader_window() {
            self.apply_window_command(WindowCommand::Close { quit: false }, None, window, cx);
        }
    }

    /// Move the focus in a `direction` from h/j/k/l, reusing native window navigation.
    pub(super) fn leader_window_focus(
        &mut self,
        direction: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.apply_window_command(WindowCommand::Direction(direction), None, window, cx);
    }

    /// Reset window sizes using the Ctrl+w = implementation.
    pub(super) fn equalize_leader_windows(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_window_command(WindowCommand::Equalize, None, window, cx);
    }

    /// Apply LazyVim's two-cell size change through the Ctrl+w resize command.
    pub(super) fn resize_window_by_key(
        &mut self,
        stacked: bool,
        grow: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let amount = if grow { Resize::Grow } else { Resize::Shrink };
        self.apply_window_command(
            WindowCommand::Resize { stacked, amount },
            Some(2),
            window,
            cx,
        );
    }

    /// Describe the supported next keys in `sequence` for the active pane.
    pub(super) fn window_sequence_hint(&self, sequence: WindowSequence) -> Option<SequenceHint> {
        let project = self.projects.active_project()?;
        let mut continuations = bindings(sequence)
            .iter()
            .filter(|binding| window_binding_available(project, binding.action))
            .map(|binding| {
                (
                    binding.label,
                    binding.caption,
                    binding.action == WindowCommand::TabPrefix,
                )
            })
            .collect::<Vec<Continuation>>();
        if !sequence.tab {
            let digits = if sequence.after.is_some() {
                "0–9"
            } else {
                "1–9"
            };
            continuations.insert(0, (digits, "vim_window.count", false));
        }
        let prefix = match (sequence.after, sequence.tab) {
            (Some(count), true) => format!("Ctrl+w {count} g"),
            (Some(count), false) => format!("Ctrl+w {count}"),
            (None, true) => "Ctrl+w g".into(),
            (None, false) => "Ctrl+w".into(),
        };
        Some(SequenceHint {
            prefix,
            group: if sequence.tab {
                "vim_window.tabs"
            } else {
                "vim_window.group"
            },
            continuations,
        })
    }

    /// Open or advance a Ctrl+w sequence from a native `event`.
    /// `window` and `cx` apply the resulting action or refresh its hint.
    pub(super) fn dispatch_window_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let mut sequence = match self.prefix {
            Some(PendingPrefix::Window(sequence)) => sequence,
            Some(PendingPrefix::Leader(_)) => return false,
            _ => {
                let key = &event.keystroke;
                if key.key != "w"
                    || key.modifiers
                        != (Modifiers {
                            control: true,
                            ..Modifiers::default()
                        })
                {
                    return false;
                }
                let project = self.projects.active_mut();
                if event.is_held {
                    return !project.editor.vim().accepts_text() || project.focus == Focus::Manager;
                }
                let before = match project.focus {
                    Focus::Editor => match if let Some(terminal) = project.editor.terminal_mut() {
                        terminal.take_window_count()
                    } else {
                        project.editor.take_window_count()
                    } {
                        Some(count) => count,
                        None => return false,
                    },
                    Focus::Manager => project
                        .manager
                        .as_mut()
                        .and_then(|manager| manager.take_window_count()),
                };
                if let Some(manager) = &mut project.manager {
                    manager.cancel_pending_undo();
                }
                self.show_window_sequence(
                    WindowSequence {
                        before,
                        ..WindowSequence::default()
                    },
                    cx,
                );
                return true;
            }
        };
        if event.is_held {
            return true;
        }
        let key = window_key(&event.keystroke);
        if key.as_deref().is_some_and(|key| sequence.edit_count(key)) {
            self.show_window_sequence(sequence, cx);
            return true;
        }
        let command = key
            .as_deref()
            .and_then(|key| {
                bindings(sequence)
                    .iter()
                    .find(|binding| binding.keys.contains(&key))
            })
            .filter(|binding| window_binding_available(self.projects.active(), binding.action))
            .map_or(WindowCommand::Cancel, |binding| binding.action);
        if command == WindowCommand::TabPrefix {
            sequence.tab = true;
            self.show_window_sequence(sequence, cx);
            return true;
        }
        self.prefix = None;
        self.apply_window_command(command, sequence.count(), window, cx);
        cx.notify();
        true
    }

    /// Retain `sequence` and draw its continuation hint through `cx`.
    fn show_window_sequence(&mut self, sequence: WindowSequence, cx: &mut Context<Self>) {
        self.prefix = Some(PendingPrefix::Window(sequence));
        self.refresh_sequence_hint(cx);
        cx.notify();
    }

    /// Dispatch a resolved `command` with optional `count` to the active window through `cx`.
    fn apply_window_command(
        &mut self,
        command: WindowCommand,
        count: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match command {
            WindowCommand::Split { stacked } => {
                self.open_editor_split_at(stacked, true, SplitDocument::Current, count, window, cx);
                return;
            }
            WindowCommand::SplitEmpty => {
                self.open_editor_split_at(true, true, SplitDocument::Empty, count, window, cx);
                return;
            }
            WindowCommand::SplitFile { line } => {
                self.open_window_file_split(line, count, window, cx);
                return;
            }
            WindowCommand::SplitAlternate => {
                self.open_editor_split_at(
                    true,
                    true,
                    SplitDocument::Alternate(count),
                    None,
                    window,
                    cx,
                );
                return;
            }
            WindowCommand::MoveToTab => {
                self.move_editor_split_to_page(count, window, cx);
                return;
            }
            WindowCommand::SplitInclude { defines } => {
                let vim = self.projects.active().editor.vim();
                if let Some((path, row, column)) = vim.split_include_target(count, defines) {
                    let source = SplitDocument::Include(path, row, column);
                    self.open_editor_split_at(true, true, source, None, window, cx);
                } else {
                    self.set_status(i18n::text("vim_window.no_include").into());
                }
                return;
            }
            WindowCommand::PreviewFocus => {
                if let Some(id) = self.projects.active().preview_pane() {
                    self.focus_editor_pane(id, window, cx);
                    self.sync(window, cx);
                }
                return;
            }
            WindowCommand::PreviewClose => {
                self.close_vim_preview(window, cx);
                return;
            }
            _ => {}
        }
        if let WindowCommand::Equalize = command {
            self.equalize_visible_editors();
            self.sync(window, cx);
            return;
        }
        if self.apply_split_window_command(command, count, window, cx) {
            self.sync(window, cx);
            return;
        }
        let project = self.projects.active();
        let focus = project.focus;
        let first = if project.editor_first {
            Focus::Editor
        } else {
            Focus::Manager
        };
        let both = project.manager.is_some() && project.hidden_pane.is_none();
        let numbered = |n: usize| {
            if !both {
                focus
            } else if n == 1 {
                first
            } else {
                first.other()
            }
        };
        let split = (project.stacked, project.editor_first);
        let mut target = None;
        match command {
            WindowCommand::Direction(key) => {
                target = direction_target(focus, split, key).filter(|_| both)
            }
            WindowCommand::Cycle { .. } => {
                target =
                    Some(count.map_or_else(|| if both { focus.other() } else { focus }, numbered))
            }
            WindowCommand::Edge(start) => target = Some(numbered(if start { 1 } else { 2 })),
            WindowCommand::Previous => target = project.previous_focus.filter(|_| both),
            WindowCommand::Close { quit } => {
                let pane = count.map_or(focus, numbered);
                self.projects.active_mut().editor.end_window_selection();
                if both || self.projects.len() > 1 {
                    self.close_grid_window(ShellWindow::Pane(pane), window, cx);
                } else if quit {
                    self.request_close(CloseTarget::Quit, window, cx);
                } else {
                    self.set_status(i18n::text("vim_window.last_close").into());
                }
            }
            WindowCommand::Only if both => {
                let pane = count.map_or(focus, numbered).other();
                self.projects.active_mut().editor.end_window_selection();
                self.close_grid_window(ShellWindow::Pane(pane), window, cx);
            }
            WindowCommand::Rotate { .. } if both && count.unwrap_or(1) % 2 != 0 => {
                let project = self.projects.active_mut();
                project.editor.end_window_selection();
                project.editor_first = !project.editor_first;
            }
            WindowCommand::Exchange
                if both && count.is_none_or(|n| n <= 2 && numbered(n) != focus) =>
            {
                self.projects.active_mut().editor_first = !self.projects.active().editor_first;
                target = Some(focus.other());
            }
            WindowCommand::Move { stacked, first } if both => {
                let project = self.projects.active_mut();
                project.stacked = stacked;
                project.editor_first = (focus == Focus::Editor) == first;
                if count.is_some() {
                    self.resize_window_pane(stacked, Resize::Set, count);
                }
            }
            WindowCommand::Resize { stacked, amount } if both && stacked == project.stacked => {
                self.resize_window_pane(stacked, amount, count);
            }
            WindowCommand::Tab(direction) => {
                self.projects.active_mut().editor.end_window_selection();
                self.navigate_vim_tab(direction, count, window, cx);
                return;
            }
            WindowCommand::OpenFile { line } if focus == Focus::Editor => {
                self.open_window_file_tab(line, window, cx)
            }
            WindowCommand::Diagnostic if focus == Focus::Editor => {
                let message = project
                    .editor
                    .buffer()
                    .caret_diagnostic()
                    .unwrap_or_else(|| i18n::text("editor.no_diagnostics"))
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_owned();
                self.set_status(message);
                return;
            }
            WindowCommand::Cancel => return,
            _ => {}
        }
        if let Some(pane) = target.filter(|&pane| pane != focus) {
            self.clear_canvas_focus();
            let project = self.projects.active_mut();
            project.editor.end_window_selection();
            project.editor.cancel_pending();
            project.focus_pane(pane);
        }
        self.sync(window, cx);
    }

    /// Open the `count`th existing file referred to at the caret in a horizontal split.
    /// Honor its one-based destination when `with_line` is set, in `window` through `cx`.
    fn open_window_file_split(
        &mut self,
        with_line: bool,
        count: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let project = self.projects.active();
        let vim = project.editor.vim();
        let Some((file, line)) = vim.path_reference_under_cursor(with_line) else {
            self.set_status(i18n::text("vim_window.no_file").into());
            return;
        };
        let local = vim
            .doc
            .path
            .as_deref()
            .and_then(Path::parent)
            .map(|dir| dir.join(file));
        let directory = self.ex_current_dir();
        let mut candidates = [local, Some(directory.join(file))]
            .into_iter()
            .flatten()
            .filter_map(|path| path.canonicalize().ok().filter(|path| path.is_file()))
            .collect::<Vec<_>>();
        candidates.dedup();
        let occurrence = count.unwrap_or(1).max(1);
        for nth in 1..=occurrence.saturating_add(candidates.len()) {
            if candidates.len() >= occurrence {
                break;
            }
            let Some(candidate) = nanobug::vim::find_file(
                directory,
                vim.doc.path.as_deref(),
                &vim.file_search_path(),
                file,
                nth,
            ) else {
                break;
            };
            if !candidates.contains(&candidate) {
                candidates.push(candidate);
            }
        }
        let Some(path) = candidates.into_iter().nth(occurrence - 1) else {
            self.set_status(i18n::format("vim_g.missing_file", &[("path", &file)]));
            return;
        };
        self.open_editor_split_at(
            true,
            true,
            SplitDocument::File(path, line),
            None,
            window,
            cx,
        );
    }

    /// Give `command` and `count` to the editor splits before the outer panes handle them.
    /// Return whether a split or the pane order handled the command in `window` through `cx`.
    fn apply_split_window_command(
        &mut self,
        command: WindowCommand,
        count: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match command {
            WindowCommand::Cycle { reverse } => self.cycle_editor_split(reverse, count, window, cx),
            WindowCommand::Edge(start) => {
                self.cycle_editor_split(false, Some(if start { 1 } else { usize::MAX }), window, cx)
            }
            _ if self.projects.active().focus != Focus::Editor => false,
            WindowCommand::Direction(key) => {
                let (horizontal, vertical) = match key {
                    "h" => (-1, 0),
                    "j" => (0, 1),
                    "k" => (0, -1),
                    "l" => (1, 0),
                    _ => unreachable!("window bindings only contain cardinal directions"),
                };
                self.focus_editor_split(horizontal, vertical, count.unwrap_or(1), window, cx)
            }
            WindowCommand::Previous => self.focus_previous_editor_split(window, cx),
            WindowCommand::Close { .. } => self.close_editor_split(count, window, cx),
            WindowCommand::Only => self.only_editor_split(window, cx),
            WindowCommand::Rotate { reverse } if self.projects.active().split_count() > 1 => {
                let area = self.layout.editor;
                self.projects
                    .active_mut()
                    .splits_mut()
                    .rotate(area, count.unwrap_or(1), reverse);
                true
            }
            WindowCommand::Exchange if self.projects.active().split_count() > 1 => {
                let area = self.layout.editor;
                if let Some(id) = self
                    .projects
                    .active_mut()
                    .splits_mut()
                    .exchange(area, count)
                {
                    self.focus_editor_pane(id, window, cx);
                }
                true
            }
            WindowCommand::Move { stacked, first } if self.projects.active().split_count() > 1 => {
                use super::editor_splits::SplitAxis;
                let axis = if stacked {
                    SplitAxis::Rows
                } else {
                    SplitAxis::Columns
                };
                let area = self.layout.editor;
                let project = self.projects.active_mut();
                if project.editor_zoomed() {
                    project.toggle_editor_zoom();
                }
                let splits = project.splits_mut();
                if splits.move_to_edge(axis, first)
                    && let Some(size) = count
                    && let Some(pane) = splits.rect(splits.active(), area)
                {
                    let current = if stacked { pane.h } else { pane.w };
                    let delta = size.min(isize::MAX as usize) as isize - current as isize;
                    splits.resize(area, axis, delta);
                }
                true
            }
            WindowCommand::Resize { stacked, amount } => {
                let area = self.active_editor_area();
                let (current, total) = if stacked {
                    (area.h, self.layout.editor.h)
                } else {
                    (area.w, self.layout.editor.w)
                };
                let size = count.unwrap_or(1).min(isize::MAX as usize) as isize;
                let delta = match amount {
                    Resize::Grow => size,
                    Resize::Shrink => -size,
                    Resize::Set => {
                        count.unwrap_or(total).min(isize::MAX as usize) as isize - current as isize
                    }
                };
                self.resize_editor_split(!stacked, delta, window, cx)
            }
            _ => false,
        }
    }

    /// Resize the focused pane along the `stacked` axis according to `amount` and `count`.
    fn resize_window_pane(&mut self, stacked: bool, amount: Resize, count: Option<usize>) {
        let project = self.projects.active_mut();
        let (manager, editor) = if stacked {
            let total = self.layout.rows - super::layout::FOOTER_ROWS - 1;
            let manager = if self.layout.tree.y == self.layout.editor.y {
                project.manager_height.unwrap_or(total / 2).min(total)
            } else {
                self.layout.tree.h
            };
            (manager, total.saturating_sub(manager))
        } else {
            let total = self.layout.cols.saturating_sub(1);
            let manager = if self.layout.tree.x == self.layout.editor.x {
                project.sidebar.min(total)
            } else {
                self.layout.tree.w
            };
            (manager, total.saturating_sub(manager))
        };
        let current = if project.focus == Focus::Manager {
            manager
        } else {
            editor
        };
        let value = match amount {
            Resize::Grow => current.saturating_add(count.unwrap_or(1)),
            Resize::Shrink => current.saturating_sub(count.unwrap_or(1)),
            Resize::Set => count.unwrap_or(manager + editor),
        };
        let manager = if project.focus == Focus::Manager {
            value
        } else {
            (manager + editor).saturating_sub(value)
        };
        if stacked {
            project.manager_height = Some(manager);
        } else {
            project.sidebar = manager;
        }
    }

    /// Open the filename under the caret in a tab, honoring `line` for Ctrl+w gF.
    fn open_window_file_tab(&mut self, line: bool, window: &mut Window, cx: &mut Context<Self>) {
        let project = self.projects.active();
        let Some((file, line)) = project.editor.vim().path_under_cursor(line) else {
            self.set_status(i18n::text("vim_window.no_file").into());
            return;
        };
        let path = match self.vim_file_path(&file).canonicalize() {
            Ok(path) => path,
            Err(error) => {
                self.set_status(error.to_string());
                return;
            }
        };
        if self.projects.ordered().any(|project| {
            project
                .editor
                .buffers()
                .iter()
                .any(|buffer| buffer.vim.doc.path.as_deref() == Some(path.as_path()))
        }) {
            self.set_status(i18n::text("vim_window.already_open").into());
            return;
        }
        match self.open_standalone_file(path, window, cx) {
            Ok(()) => {
                self.projects.active_mut().editor.end_window_selection();
                if let Some(line) = line {
                    let editor = &mut self.projects.active_mut().editor;
                    let row = line.saturating_sub(1).min(editor.vim().line_count() - 1);
                    editor.jump_to(editor.vim().doc.starts[row]);
                }
            }
            Err(error) => self.set_status(format!("{error:#}")),
        }
    }
}

/// Whether `command` has a target within the current `project` and its layout.
fn window_binding_available(project: &Project, command: WindowCommand) -> bool {
    let both = project.manager.is_some() && project.hidden_pane.is_none();
    let split = project.split_count() > 1;
    let editor = project.focus == Focus::Editor;
    match command {
        WindowCommand::OpenFile { .. }
        | WindowCommand::Diagnostic
        | WindowCommand::Split { .. }
        | WindowCommand::SplitEmpty
        | WindowCommand::SplitFile { .. }
        | WindowCommand::SplitAlternate
        | WindowCommand::SplitInclude { .. } => editor,
        WindowCommand::PreviewFocus | WindowCommand::PreviewClose => {
            project.preview_pane().is_some()
        }
        WindowCommand::MoveToTab => editor && split,
        WindowCommand::Direction(key) => {
            (split && editor)
                || (both
                    && direction_target(
                        project.focus,
                        (project.stacked, project.editor_first),
                        key,
                    )
                    .is_some())
        }
        WindowCommand::Cycle { .. } | WindowCommand::Edge(_) | WindowCommand::Only => both || split,
        WindowCommand::Equalize => {
            split && !project.editor_zoomed() && project.hidden_pane != Some(Focus::Editor)
        }
        WindowCommand::Rotate { .. } | WindowCommand::Exchange | WindowCommand::Move { .. } => {
            both || (split && editor)
        }
        WindowCommand::Previous => (split && editor) || (both && project.previous_focus.is_some()),
        WindowCommand::Resize { stacked, .. } => {
            (split && editor) || (both && stacked == project.stacked)
        }
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ctrl aliases, arrows, and shifted commands resolve to the same shared binding table.
    #[test]
    fn window_command_aliases_preserve_shift_and_cancel() {
        let sequence = WindowSequence::default();
        for (key, action) in [
            ("ctrl-h", WindowCommand::Direction("h")),
            ("backspace", WindowCommand::Direction("h")),
            ("down", WindowCommand::Direction("j")),
            ("ctrl-w", WindowCommand::Cycle { reverse: false }),
            ("shift-w", WindowCommand::Cycle { reverse: true }),
            ("shift-s", WindowCommand::Split { stacked: true }),
            ("ctrl-s", WindowCommand::Split { stacked: true }),
            ("ctrl-v", WindowCommand::Split { stacked: false }),
            ("ctrl-n", WindowCommand::SplitEmpty),
            ("ctrl-f", WindowCommand::SplitFile { line: false }),
            ("shift-f", WindowCommand::SplitFile { line: true }),
            ("ctrl-6", WindowCommand::SplitAlternate),
            ("ctrl-i", WindowCommand::SplitInclude { defines: false }),
            ("ctrl-d", WindowCommand::SplitInclude { defines: true }),
            ("shift-p", WindowCommand::PreviewFocus),
            ("ctrl-z", WindowCommand::PreviewClose),
            ("shift-t", WindowCommand::MoveToTab),
            (
                "shift-k",
                WindowCommand::Move {
                    stacked: true,
                    first: true,
                },
            ),
            ("ctrl-q", WindowCommand::Close { quit: true }),
            ("ctrl-g", WindowCommand::TabPrefix),
            ("ctrl-c", WindowCommand::Cancel),
            ("alt-w", WindowCommand::Cancel),
        ] {
            let key = window_key(&Keystroke::parse(key).unwrap());
            let action_for_key = key
                .as_deref()
                .and_then(|key| {
                    bindings(sequence)
                        .iter()
                        .find(|binding| binding.keys.contains(&key))
                })
                .map_or(WindowCommand::Cancel, |binding| binding.action);
            assert_eq!(action_for_key, action);
        }
    }

    /// Exercise argument-list counts and the retained preview split from native keys.
    #[cfg(windows)]
    #[test]
    #[ignore = "requires Windows DirectWrite and DirectX"]
    fn native_argument_and_preview_window_keys() -> anyhow::Result<()> {
        use super::super::native_test;
        use anyhow::ensure;
        use gpui::AppContext;
        use nanobug::plugins::session::Options;
        use std::{fs, time::Duration};

        let temp = tempfile::tempdir()?;
        let project = temp.path().join("project");
        fs::create_dir(&project)?;
        for file in ["first.txt", "two words.txt", "last.txt", "preview.txt"] {
            fs::write(project.join(file), file)?;
        }
        let options = Options::take(
            &mut vec![
                "--plugin-profile".into(),
                temp.path().join("profile").into_os_string(),
                "--no-plugins".into(),
            ],
            temp.path(),
        )?;
        native_test::run_async(
            project,
            options,
            |_| {},
            async move |handle, app| {
                app.update_window(handle.into(), |_, window, _| {
                    window.resize(gpui::size(gpui::px(920.), gpui::px(580.)));
                })?;
                app.background_executor()
                    .timer(Duration::from_millis(60))
                    .await;
                app.update(|app| {
                    handle.update(app, |workspace, window, cx| {
                        workspace.sync(window, cx);
                        let send = |workspace: &mut Workspace,
                                    keys: &[&'static str],
                                    window: &mut Window,
                                    cx: &mut Context<Workspace>|
                         -> anyhow::Result<()> {
                            for key in keys {
                                workspace.key_down(
                                    &KeyDownEvent {
                                        keystroke: Keystroke::parse(key)?,
                                        is_held: false,
                                        prefer_character_input: false,
                                    },
                                    window,
                                    cx,
                                );
                            }
                            Ok(())
                        };
                        let shown = |workspace: &Workspace, file: &str| {
                            workspace
                                .projects
                                .active()
                                .editor
                                .vim()
                                .doc
                                .path
                                .as_deref()
                                .and_then(Path::file_name)
                                .is_some_and(|name| name == file)
                        };
                        workspace.run_vim(
                            |vim, register| {
                                vim.key(":", register);
                                vim.commit_text(
                                    "args first.txt two\\ words.txt last.txt",
                                    register,
                                );
                                vim.key("Enter", register)
                            },
                            window,
                            cx,
                        );
                        ensure!(
                            shown(workspace, "first.txt"),
                            ":args failed: {:?}, status {:?}, Vim {:?}",
                            workspace.projects.active().editor.vim().doc.path,
                            workspace.projects.active().status,
                            workspace.projects.active().editor.vim().message
                        );
                        send(workspace, &["2", "]", "a"], window, cx)?;
                        ensure!(shown(workspace, "last.txt"));
                        send(workspace, &["[", "a"], window, cx)?;
                        ensure!(shown(workspace, "two words.txt"));
                        send(workspace, &["[", "shift-a"], window, cx)?;
                        ensure!(
                            shown(workspace, "first.txt"),
                            "[A failed: {:?}, status {:?}",
                            workspace.projects.active().editor.vim().doc.path,
                            workspace.projects.active().status
                        );
                        send(workspace, &["]", "shift-a"], window, cx)?;
                        ensure!(shown(workspace, "last.txt"));
                        let original = workspace.projects.active().active_editor_id();
                        workspace.run_vim(
                            |vim, register| {
                                vim.key(":", register);
                                vim.commit_text("pedit preview.txt", register);
                                vim.key("Enter", register)
                            },
                            window,
                            cx,
                        );
                        ensure!(workspace.projects.active().active_editor_id() == original);
                        ensure!(workspace.projects.active().preview_pane().is_some());
                        send(workspace, &["ctrl-w", "shift-p"], window, cx)?;
                        ensure!(shown(workspace, "preview.txt"));
                        send(workspace, &["ctrl-w", "ctrl-z"], window, cx)?;
                        ensure!(workspace.projects.active().preview_pane().is_none());
                        ensure!(workspace.projects.active().active_editor_id() == original);
                        Ok(())
                    })
                })?
            },
        )
    }

    /// Exercise counted blank splits, file targets and preservation of a moved editor on Windows.
    #[cfg(windows)]
    #[test]
    #[ignore = "requires Windows DirectWrite and DirectX"]
    fn native_window_keys_create_and_transfer_splits() -> anyhow::Result<()> {
        use super::super::native_test;
        use anyhow::ensure;
        use gpui::AppContext;
        use nanobug::plugins::session::Options;
        use std::{fs, time::Duration};

        let temp = tempfile::tempdir()?;
        let project = temp.path().join("project");
        fs::create_dir(&project)?;
        fs::write(
            project.join("README.md"),
            "second.txt:2\nmissing.txt\n#include \"header.h\"\nreturn native_value;",
        )?;
        fs::write(project.join("second.txt"), "first\nsecond\nthird\n")?;
        fs::write(project.join("header.h"), "\nint native_value;\n")?;
        let options = Options::take(
            &mut vec![
                "--plugin-profile".into(),
                temp.path().join("profile").into_os_string(),
                "--no-plugins".into(),
            ],
            temp.path(),
        )?;
        native_test::run_async(
            project,
            options,
            |_| {},
            async move |handle, app| {
                app.update_window(handle.into(), |_, window, _| {
                    window.resize(gpui::size(gpui::px(920.), gpui::px(580.)))
                })?;
                app.background_executor()
                    .timer(Duration::from_millis(60))
                    .await;
                app.update(|app| {
                    handle.update(app, |workspace, window, cx| -> anyhow::Result<()> {
                        workspace.sync(window, cx);
                        let mut send = |workspace: &mut Workspace,
                                        keys: &[&'static str]|
                         -> anyhow::Result<()> {
                            for key in keys {
                                workspace.key_down(
                                    &KeyDownEvent {
                                        keystroke: Keystroke::parse(key)?,
                                        is_held: false,
                                        prefer_character_input: false,
                                    },
                                    window,
                                    cx,
                                );
                            }
                            Ok(())
                        };
                        let original = workspace.projects.active().active_editor_id();
                        send(workspace, &["ctrl-w", "shift-f"])?;
                        let project = workspace.projects.active_mut();
                        let pane = project.active_editor_id();
                        let doc = &project.editor.vim().doc;
                        ensure!(
                            doc.path
                                .as_deref()
                                .and_then(Path::file_name)
                                .is_some_and(|name| name == "second.txt")
                        );
                        ensure!(doc.location(doc.cursor).0 == 1);
                        project.editor.edit(|vim, register| {
                            vim.text("ifresh-", register);
                            vim.key("Escape", register)
                        });
                        let buffer = project.editor.buffer().id;
                        let opening = project.editor.vim().doc.opening;
                        ensure!(project.editor.vim().doc.modified());
                        send(workspace, &["ctrl-w", "1", "shift-t"])?;
                        let project = workspace.projects.active();
                        ensure!(project.active_page() == 0 && project.page_count() == 2);
                        ensure!(project.previous_page() == Some(1) && project.split_count() == 1);
                        ensure!(
                            project.active_editor_id() == pane
                                && project.editor.buffer().id == buffer
                        );
                        ensure!(project.document_owner(opening) == Some((pane, 0)));
                        ensure!(project.editor.vim().doc.text.contains("fresh-second"));
                        ensure!(project.editor.vim().doc.modified());
                        send(workspace, &["g", "t"])?;
                        ensure!(workspace.projects.active().active_editor_id() == original);
                        ensure!(workspace.projects.active().split_count() == 1);

                        send(workspace, &["ctrl-w", "ctrl-f"])?;
                        let doc = &workspace.projects.active().editor.vim().doc;
                        ensure!(doc.location(doc.cursor).0 == 0);
                        send(workspace, &["ctrl-w", "c"])?;
                        send(workspace, &["j", "ctrl-w", "f"])?;
                        ensure!(workspace.projects.active().active_editor_id() == original);
                        ensure!(workspace.projects.active().split_count() == 1);

                        send(workspace, &["ctrl-w", "8", "ctrl-n"])?;
                        let project = workspace.projects.active();
                        ensure!(project.editor.vim().doc.path.is_none());
                        ensure!(project.editor.vim().doc.text.is_empty());
                        ensure!(project.split_count() == 2);
                        ensure!(workspace.active_editor_area().h == 8);
                        send(workspace, &["ctrl-w", "c", "shift-g", "w"])?;
                        let project = workspace.projects.active();
                        let origin = project.editor.vim().doc.cursor;
                        ensure!(project.active_editor_id() == original);
                        send(workspace, &["ctrl-w", "i"])?;
                        let project = workspace.projects.active();
                        let doc = &project.editor.vim().doc;
                        ensure!(
                            doc.path
                                .as_deref()
                                .and_then(Path::file_name)
                                .is_some_and(|name| name == "header.h")
                        );
                        ensure!(doc.location(doc.cursor) == (1, 4));
                        ensure!(project.editor_by_id(original).unwrap().vim().doc.cursor == origin);
                        Ok(())
                    })
                })?
            },
        )
    }

    /// Digits before and after Ctrl+w multiply, while Delete removes a continuation digit.
    #[test]
    fn window_count_can_be_edited_after_the_prefix() {
        let mut sequence = WindowSequence {
            before: Some(2),
            ..WindowSequence::default()
        };
        assert!(!sequence.edit_count("0"));
        assert!(sequence.edit_count("1"));
        assert!(sequence.edit_count("2"));
        assert_eq!(sequence.count(), Some(24));
        assert!(sequence.edit_count("delete"));
        assert_eq!(sequence.count(), Some(2));
        assert!(sequence.edit_count("delete"));
        assert_eq!(sequence.after, None);
        assert_eq!(sequence.count(), Some(2));
    }

    /// Direction follows the real split axis and pane order after moving a pane.
    #[test]
    fn directional_commands_follow_reordered_panes() {
        for (split, toward_manager, toward_editor) in [
            ((false, false), "h", "l"),
            ((false, true), "l", "h"),
            ((true, false), "k", "j"),
            ((true, true), "j", "k"),
        ] {
            assert_eq!(
                direction_target(Focus::Editor, split, toward_manager),
                Some(Focus::Manager)
            );
            assert_eq!(
                direction_target(Focus::Manager, split, toward_editor),
                Some(Focus::Editor)
            );
            assert_eq!(direction_target(Focus::Editor, split, toward_editor), None);
        }
    }

    /// Equalize the nested editor pane tree from either focus while keeping Explorer geometry.
    #[cfg(windows)]
    #[test]
    #[ignore = "requires Windows DirectWrite and DirectX"]
    fn native_equalize_preserves_explorer_size_and_visible_split_structure() -> anyhow::Result<()> {
        use super::super::native_test;
        use anyhow::ensure;
        use gpui::AppContext;
        use nanobug::plugins::session::Options;
        use std::time::Duration;

        let temp = tempfile::tempdir()?;
        let options = Options::take(&mut vec!["--no-plugins".into()], temp.path())?;
        native_test::run_async(
            temp.path().to_owned(),
            options,
            |_| {},
            async move |handle, app| {
                app.update_window(handle.into(), |_, window, _| {
                    window.resize(gpui::size(gpui::px(1000.), gpui::px(620.)));
                })?;
                app.background_executor()
                    .timer(Duration::from_millis(60))
                    .await;
                app.update(|app| {
                    handle.update(app, |workspace, window, cx| -> anyhow::Result<()> {
                        workspace.sync(window, cx);
                        let bottom = workspace.projects.active().active_editor_id();
                        workspace.open_editor_split(true, None, window, cx);
                        let top_right = workspace.projects.active().active_editor_id();
                        workspace.open_editor_split(false, None, window, cx);
                        let top_left = workspace.projects.active().active_editor_id();
                        let send = |workspace: &mut Workspace,
                                    window: &mut Window,
                                    cx: &mut Context<Workspace>|
                         -> anyhow::Result<()> {
                            for key in ["ctrl-w", "="] {
                                workspace.key_down(
                                    &KeyDownEvent {
                                        keystroke: Keystroke::parse(key)?,
                                        is_held: false,
                                        prefer_character_input: false,
                                    },
                                    window,
                                    cx,
                                );
                            }
                            Ok(())
                        };
                        for stacked in [false, true] {
                            let mut expected = None;
                            for (focus, pane) in [
                                (Focus::Editor, top_left),
                                (Focus::Manager, top_left),
                                (Focus::Editor, bottom),
                            ] {
                                let project = workspace.projects.active_mut();
                                project.focus_pane(Focus::Editor);
                                project.stacked = stacked;
                                project.sidebar = 20;
                                project.manager_height = Some(7);
                                workspace.sync(window, cx);
                                let before = workspace.layout.tree;
                                workspace.focus_editor_pane(top_left, window, cx);
                                ensure!(workspace.resize_editor_split(true, 4, window, cx));
                                ensure!(workspace.resize_editor_split(false, 3, window, cx));
                                workspace.focus_editor_pane(pane, window, cx);
                                workspace.projects.active_mut().focus_pane(focus);
                                send(workspace, window, cx)?;
                                ensure!(workspace.projects.active().focus == focus);
                                ensure!(workspace.projects.active().active_editor_id() == pane);
                                let panes = workspace.editor_rects();
                                ensure!(
                                    panes
                                        .iter()
                                        .map(|(id, _)| *id)
                                        .eq([top_left, top_right, bottom])
                                );
                                let [left, right, bottom] = [panes[0].1, panes[1].1, panes[2].1];
                                let explorer = workspace.layout.tree;
                                ensure!(
                                    left.x + left.w + 1 == right.x && left.y + left.h == bottom.y
                                );
                                ensure!(
                                    right.x + right.w == bottom.x + bottom.w && left.x == bottom.x
                                );
                                ensure!(
                                    left.w.abs_diff(right.w) <= 1 && left.h.abs_diff(bottom.h) <= 1
                                );
                                ensure!(explorer == before);
                                let layout = (explorer, panes);
                                ensure!(expected.as_ref().is_none_or(|other| other == &layout));
                                expected = Some(layout);
                            }
                        }
                        let project = workspace.projects.active_mut();
                        project.toggle_editor_zoom();
                        let split_sizes = project.splits().clone();
                        workspace.sync(window, cx);
                        let explorer = workspace.layout.tree;
                        send(workspace, window, cx)?;
                        ensure!(workspace.layout.tree == explorer);
                        ensure!(workspace.projects.active().splits() == &split_sizes);
                        workspace.projects.active_mut().toggle_editor_zoom();
                        workspace.hide_pane(Focus::Manager);
                        workspace.sync(window, cx);
                        let hidden_height = workspace.projects.active().manager_height;
                        send(workspace, window, cx)?;
                        ensure!(workspace.projects.active().manager_height == hidden_height);
                        ensure!(workspace.projects.active().hidden_pane == Some(Focus::Manager));
                        Ok(())
                    })
                })?
            },
        )
    }

    /// Route real window key events through focus, layout, viewport, and close handling.
    #[cfg(windows)]
    #[test]
    #[ignore = "requires Windows DirectWrite and DirectX"]
    fn native_window_keys_reorder_and_resize_panes() -> anyhow::Result<()> {
        use super::super::{native_test, tabs::CaptionTarget};
        use anyhow::ensure;
        use gpui::{AppContext, EntityInputHandler, MouseButton, MouseDownEvent};
        use nanobug::plugins::session::Options;
        use std::{fs, time::Duration};

        let temp = tempfile::tempdir()?;
        let project = temp.path().join("project");
        fs::create_dir(&project)?;
        fs::write(project.join("README.md"), "second.txt:2")?;
        fs::write(project.join("second.txt"), "one\ntwo")?;
        fs::write(project.join("split.txt"), "split file")?;
        let options = Options::take(
            &mut vec![
                "--plugin-profile".into(),
                temp.path().join("profile").into_os_string(),
                "--no-plugins".into(),
            ],
            temp.path(),
        )?;
        native_test::run_async(
            project,
            options,
            |_| {},
            async move |handle, app| {
                app.update_window(handle.into(), |_, window, _| {
                    window.resize(gpui::size(gpui::px(920.), gpui::px(580.)))
                })?;
                app.background_executor()
                    .timer(Duration::from_millis(60))
                    .await;
                app.update(|app| {
                    handle.update(app, |workspace, window, cx| {
                        workspace.sync(window, cx);
                        let send = |workspace: &mut Workspace,
                                    keys: &[&'static str],
                                    window: &mut Window,
                                    cx: &mut Context<Workspace>|
                         -> anyhow::Result<()> {
                            for key in keys {
                                workspace.key_down(
                                    &KeyDownEvent {
                                        keystroke: Keystroke::parse(key)?,
                                        is_held: false,
                                        prefer_character_input: false,
                                    },
                                    window,
                                    cx,
                                );
                            }
                            Ok(())
                        };
                        let original = workspace.layout.editor;
                        send(workspace, &["ctrl-w", "shift-k"], window, cx)?;
                        ensure!(workspace.layout.editor.y < workspace.layout.tree.y);
                        ensure!(workspace.layout.editor.x == workspace.layout.tree.x);
                        let height = workspace.layout.editor.h;
                        send(workspace, &["ctrl-w", "2", "+"], window, cx)?;
                        ensure!(workspace.layout.editor.h == height + 2);
                        send(workspace, &["ctrl-w", "down"], window, cx)?;
                        ensure!(workspace.projects.active().focus == Focus::Manager);
                        send(workspace, &["ctrl-w", "p", "ctrl-w", "shift-l"], window, cx)?;
                        ensure!(workspace.projects.active().focus == Focus::Editor);
                        ensure!(workspace.layout.editor == original);
                        let now = workspace.scroll_epoch.elapsed().as_secs_f64();
                        workspace.projects.active_mut().open_page(None, now)?;
                        workspace.sync(window, cx);
                        send(workspace, &["ctrl-w", "shift-k"], window, cx)?;
                        ensure!(workspace.layout.editor.y < workspace.layout.tree.y);
                        send(workspace, &["g", "shift-t"], window, cx)?;
                        ensure!(workspace.projects.active().active_page() == 0);
                        ensure!(workspace.layout.editor == original);
                        send(workspace, &["i"], window, cx)?;
                        workspace.replace_and_mark_text_in_range(None, "中国", None, window, cx);
                        ensure!(workspace.projects.active().editor.composition().text == "中国");
                        let caption = |target| {
                            workspace
                                .tabs
                                .click_targets(workspace.tab_metrics())
                                .find_map(|(candidate, bounds)| {
                                    (candidate == target).then_some(bounds)
                                })
                                .ok_or_else(|| anyhow::anyhow!("Missing caption target {target:?}"))
                        };
                        let project = caption(CaptionTarget::Project(0))?;
                        let first_page = caption(CaptionTarget::Page(0))?;
                        let second_page = caption(CaptionTarget::Page(1))?;
                        ensure!(project.origin.y == second_page.origin.y);
                        ensure!(project.right() < first_page.origin.x);
                        let position = second_page.center();
                        let press = workspace.caption_tab_press(position).ok_or_else(|| {
                            anyhow::anyhow!("Second page must accept a caption press")
                        })?;
                        workspace.finish_caption_tab_click(press, position, window, cx);
                        ensure!(workspace.projects.active().active_page() == 1);
                        ensure!(
                            workspace
                                .projects
                                .active()
                                .editor
                                .composition()
                                .text
                                .is_empty()
                        );
                        ensure!(!workspace.accepts_native_text());
                        send(workspace, &["i"], window, cx)?;
                        ensure!(workspace.accepts_native_text());
                        workspace.replace_text_in_range(None, "中", window, cx);
                        ensure!(
                            workspace
                                .projects
                                .active()
                                .editor
                                .vim()
                                .doc
                                .text
                                .starts_with("中")
                        );
                        send(workspace, &["escape"], window, cx)?;
                        let area = workspace.layout.editor;
                        let editor = &workspace.projects.active().editor;
                        let buffer = editor.buffer().id;
                        let header =
                            editor.header(area, &workspace.spacing, workspace.layout.visible_cols);
                        let file = (area.x..area.x + area.w)
                            .find(|&x| {
                                header.target_at(x, area.y)
                                    == Some(crate::grid::windows::editor::HeaderTarget::Buffer(
                                        buffer,
                                    ))
                            })
                            .ok_or_else(|| anyhow::anyhow!("File label must be visible"))?;
                        let close_file = MouseDownEvent {
                            button: MouseButton::Middle,
                            position: workspace.metrics.at(file as f32 + 0.5, area.y as f32 + 0.5),
                            click_count: 1,
                            ..Default::default()
                        };
                        workspace.middle_mouse_down(&close_file, window, cx);
                        ensure!(workspace.editor_close.is_some());
                        send(workspace, &["escape", "u"], window, cx)?;
                        workspace.middle_mouse_down(&close_file, window, cx);
                        ensure!(workspace.editor_close.is_none());
                        ensure!(workspace.projects.active().hidden_pane == Some(Focus::Editor));
                        ensure!(workspace.projects.active().editor.vim().doc.path.is_none());
                        workspace.select_editor_page(0, window, cx);
                        ensure!(
                            workspace.projects.active().editor.vim().doc.text == "second.txt:2"
                        );
                        workspace.select_editor_page(1, window, cx);
                        ensure!(workspace.projects.active_mut().close_page(now));
                        workspace.sync(window, cx);
                        ensure!(workspace.layout.editor == original);
                        ensure!(
                            workspace
                                .tabs
                                .click_targets(workspace.tab_metrics())
                                .all(|(target, _)| matches!(target, CaptionTarget::Project(_)))
                        );
                        send(workspace, &["ctrl-w", "r"], window, cx)?;
                        ensure!(workspace.layout.editor.x < workspace.layout.tree.x);
                        send(workspace, &["ctrl-w", "x"], window, cx)?;
                        ensure!(workspace.projects.active().focus == Focus::Manager);
                        ensure!(workspace.layout.editor == original);
                        send(
                            workspace,
                            &["ctrl-w", "2", "w", "ctrl-w", "g", "shift-f"],
                            window,
                            cx,
                        )?;
                        ensure!(workspace.projects.len() == 2);
                        let vim = workspace.projects.active().editor.vim();
                        ensure!(vim.doc.location(vim.doc.cursor).0 == 1);
                        send(workspace, &["ctrl-w", "g", "tab"], window, cx)?;
                        ensure!(workspace.projects.active().manager.is_some());
                        send(workspace, &["ctrl-w", "o"], window, cx)?;
                        ensure!(workspace.projects.active().hidden_pane == Some(Focus::Manager));

                        let original = workspace.active_editor_area();
                        let right_id = workspace.projects.active().active_editor_id();
                        workspace.open_leader_editor_split(false, window, cx);
                        let new_right = workspace.active_editor_area();
                        ensure!(new_right.x > original.x && new_right.y == original.y);
                        let split_rects = workspace.editor_rects();
                        workspace.toggle_leader_window_zoom(window, cx);
                        ensure!(workspace.editor_rects().len() == 1);
                        ensure!(workspace.active_editor_area() == original);
                        ensure!(workspace.editor_dividers().is_empty());
                        workspace.toggle_leader_window_zoom(window, cx);
                        ensure!(workspace.editor_rects() == split_rects);
                        workspace.open_leader_editor_split(true, window, cx);
                        let below = workspace.active_editor_area();
                        ensure!(below.y > new_right.y && below.x == new_right.x);
                        workspace.close_leader_window(window, cx);
                        workspace.close_leader_window(window, cx);
                        ensure!(workspace.projects.active().active_editor_id() == right_id);
                        ensure!(workspace.active_editor_area() == original);

                        let hint = workspace
                            .window_sequence_hint(WindowSequence::default())
                            .ok_or_else(|| anyhow::anyhow!("Missing window command hint"))?;
                        ensure!(hint.continuations.contains(&(
                            "s/S",
                            "vim_window.split_stacked",
                            false
                        )));
                        ensure!(hint.continuations.contains(&(
                            "v",
                            "vim_window.split_side_by_side",
                            false
                        )));
                        send(workspace, &["ctrl-w", "v"], window, cx)?;
                        let left = workspace.active_editor_area();
                        let left_id = workspace.projects.active().active_editor_id();
                        ensure!(left_id != right_id);
                        ensure!(
                            left.x == original.x && left.w < original.w && left.h == original.h
                        );
                        let path = workspace.projects.active().root.join("split.txt");
                        workspace.open(path, window, cx);
                        workspace.sync(window, cx);
                        ensure!(workspace.projects.active().editor.vim().doc.text == "split file");
                        send(workspace, &["ctrl-w", "l"], window, cx)?;
                        ensure!(workspace.projects.active().active_editor_id() == right_id);
                        ensure!(
                            workspace.projects.active().editor.vim().doc.text == "second.txt:2"
                        );
                        let right = workspace.active_editor_area();
                        ensure!(right.x > left.x && right.y == left.y);

                        send(workspace, &["ctrl-w", "s"], window, cx)?;
                        let upper = workspace.active_editor_area();
                        ensure!(workspace.projects.active().split_count() == 3);
                        ensure!(upper.y == right.y && upper.h < right.h && upper.x == right.x);
                        send(workspace, &["ctrl-w", "2", "+"], window, cx)?;
                        ensure!(workspace.active_editor_area().h == upper.h + 2);
                        send(workspace, &["i"], window, cx)?;
                        workspace.replace_text_in_range(None, "中", window, cx);
                        send(workspace, &["escape", "ctrl-w", "c"], window, cx)?;
                        ensure!(workspace.editor_close.is_some());
                        ensure!(workspace.projects.active().split_count() == 3);
                        send(workspace, &["escape", "u", "ctrl-w", "c"], window, cx)?;
                        ensure!(workspace.editor_close.is_none());
                        ensure!(workspace.projects.active().split_count() == 2);

                        workspace.projects.active_mut().open_page(None, now)?;
                        workspace.sync(window, cx);
                        ensure!(workspace.projects.active().split_count() == 1);
                        send(workspace, &["g", "shift-t"], window, cx)?;
                        ensure!(workspace.projects.active().split_count() == 2);
                        send(workspace, &["ctrl-w", "h"], window, cx)?;
                        ensure!(workspace.projects.active().active_editor_id() == left_id);
                        ensure!(workspace.projects.active().editor.vim().doc.text == "split file");

                        send(workspace, &[":"], window, cx)?;
                        ensure!(workspace.accepts_native_text());
                        workspace.replace_text_in_range(None, "vsplit README.md", window, cx);
                        send(workspace, &["enter"], window, cx)?;
                        ensure!(workspace.projects.active().split_count() == 3);
                        ensure!(
                            workspace.projects.active().editor.vim().doc.text == "second.txt:2"
                        );
                        ensure!(
                            workspace
                                .projects
                                .active()
                                .editor_by_id(left_id)
                                .unwrap()
                                .vim()
                                .doc
                                .text
                                == "split file"
                        );
                        Ok(())
                    })
                })?
            },
        )
    }

    /// Drag both axes of a nested editor layout and keep input in the focused pane.
    #[cfg(windows)]
    #[test]
    #[ignore = "requires Windows DirectWrite and DirectX"]
    fn native_editor_dividers_resize_both_axes_without_moving_focus() -> anyhow::Result<()> {
        use super::super::native_test;
        use anyhow::{Context as _, ensure};
        use gpui::{
            AppContext, EntityInputHandler, MouseButton, MouseDownEvent, MouseMoveEvent,
            MouseUpEvent,
        };
        use nanobug::plugins::session::Options;
        use std::time::Duration;

        let temp = tempfile::tempdir()?;
        let options = Options::take(
            &mut vec![
                "--plugin-profile".into(),
                temp.path().join("profile").into_os_string(),
                "--no-plugins".into(),
            ],
            temp.path(),
        )?;
        native_test::run_async(
            temp.path().to_owned(),
            options,
            |_| {},
            async move |handle, app| {
                app.update_window(handle.into(), |_, window, _| {
                    window.resize(gpui::size(gpui::px(920.), gpui::px(580.)))
                })?;
                app.background_executor()
                    .timer(Duration::from_millis(60))
                    .await;
                app.update(|app| {
                    handle.update(app, |workspace, window, cx| {
                        workspace.sync(window, cx);
                        let lower_id = workspace.projects.active().active_editor_id();
                        workspace.open_editor_split(false, None, window, cx);
                        let left_id = workspace.projects.active().active_editor_id();
                        ensure!(workspace.focus_editor_pane(lower_id, window, cx));
                        workspace.open_editor_split(true, None, window, cx);
                        let upper_id = workspace.projects.active().active_editor_id();
                        ensure!(workspace.focus_editor_pane(left_id, window, cx));
                        workspace.sync(window, cx);

                        workspace.key_down(
                            &KeyDownEvent {
                                keystroke: Keystroke::parse("i")?,
                                is_held: false,
                                prefer_character_input: false,
                            },
                            window,
                            cx,
                        );
                        workspace.replace_and_mark_text_in_range(None, "中国", None, window, cx);
                        ensure!(workspace.projects.active().editor.composition().text == "中国");

                        let area = |workspace: &Workspace, id| {
                            workspace
                                .editor_rects()
                                .into_iter()
                                .find_map(|(pane, rect)| (pane == id).then_some(rect))
                                .with_context(|| format!("Editor pane {id} must be visible"))
                        };
                        let drag =
                            |workspace: &mut Workspace,
                             start,
                             end,
                             release,
                             window: &mut Window,
                             cx: &mut Context<Workspace>| {
                                let [start, end, release] =
                                    [start, end, release].map(|(x, y)| workspace.metrics.at(x, y));
                                workspace.mouse_down(
                                    &MouseDownEvent {
                                        button: MouseButton::Left,
                                        position: start,
                                        click_count: 1,
                                        ..Default::default()
                                    },
                                    window,
                                    cx,
                                );
                                workspace.mouse_move(
                                    &MouseMoveEvent {
                                        position: end,
                                        pressed_button: Some(MouseButton::Left),
                                        ..Default::default()
                                    },
                                    window,
                                    cx,
                                );
                                workspace.mouse_up(
                                    &MouseUpEvent {
                                        button: MouseButton::Left,
                                        position: release,
                                        click_count: 1,
                                        ..Default::default()
                                    },
                                    window,
                                    cx,
                                );
                            };

                        let left = area(workspace, left_id)?;
                        let upper = area(workspace, upper_id)?;
                        let lower = area(workspace, lower_id)?;
                        let gutter = (left.x + left.w) as f32 + 0.5;
                        let row = (left.y + 2) as f32 + 0.5;
                        let outside_row = workspace.layout.rows as f32 + 10.;
                        drag(
                            workspace,
                            (gutter, row),
                            (gutter + 3., row),
                            (gutter + 3., outside_row),
                            window,
                            cx,
                        );
                        let wide_left = area(workspace, left_id)?;
                        let narrow_upper = area(workspace, upper_id)?;
                        let narrow_lower = area(workspace, lower_id)?;
                        ensure!(wide_left.w == left.w + 3 && wide_left.h == left.h);
                        ensure!(narrow_upper.x == upper.x + 3 && narrow_upper.h == upper.h);
                        ensure!(narrow_lower.x == lower.x + 3 && narrow_lower.y == lower.y);

                        let column = (narrow_upper.x + 2) as f32 + 0.5;
                        let gutter = (narrow_upper.y + narrow_upper.h) as f32 - 0.1;
                        drag(
                            workspace,
                            (column, gutter),
                            (column, gutter - 2.),
                            (-2., gutter - 2.),
                            window,
                            cx,
                        );
                        ensure!(area(workspace, left_id)? == wide_left);
                        let short_upper = area(workspace, upper_id)?;
                        let tall_lower = area(workspace, lower_id)?;
                        ensure!(
                            short_upper.h == narrow_upper.h - 2 && short_upper.w == narrow_upper.w
                        );
                        ensure!(
                            tall_lower.y == narrow_lower.y - 2
                                && tall_lower.h == narrow_lower.h + 2
                        );
                        ensure!(workspace.projects.active().active_editor_id() == left_id);
                        ensure!(workspace.projects.active().editor.composition().text == "中国");

                        // A later press outside this window can enter while already held.
                        let released = workspace.editor_rects();
                        workspace.mouse_move(
                            &MouseMoveEvent {
                                position: workspace.metrics.at(column, gutter + 2.),
                                pressed_button: Some(MouseButton::Left),
                                ..Default::default()
                            },
                            window,
                            cx,
                        );
                        ensure!(workspace.editor_rects() == released);
                        Ok(())
                    })
                })?
            },
        )
    }
}
