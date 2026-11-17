//! Route an editable command history back to the editor that opened it.

use super::{Workspace, keyboard::hint_page_direction, layout::Focus, viewport::CursorSpace};
use crate::grid::{render::Rect, windows::editor::CommandWindow};
use gpui::{ClipboardItem, Context, KeyDownEvent, Modifiers, ScrollEvent, Window, px};
use nanobug::{
    animation::scroll::ScrollInput,
    i18n,
    vim::{Action, Mode, SequenceHint},
};

/// The history window shows up to ten editable rows, plus its border and footer.
const COMMAND_WINDOW_HEIGHT: usize = 12;

/// How to resume the editor that opened the scratch window.
#[derive(Clone, Copy)]
enum Resume {
    /// Cancel the scratch window and any suspended prompt.
    Cancel,
    /// Put the selected line back in the command-line prompt.
    Prompt,
    /// Execute the selected line in the original editor.
    Run,
}

impl Workspace {
    /// Bounds of the open command window above the main footer.
    pub(super) fn command_window_area(&self) -> Option<Rect> {
        self.command_window.as_ref()?;
        Some(self.command_window_rect())
    }

    /// Describe the close keys accepted by the history window after Ctrl+w.
    pub(super) fn command_window_sequence_hint(&self) -> Option<SequenceHint> {
        self.command_window
            .as_ref()?
            .window_prefix
            .then(|| SequenceHint {
                prefix: "Ctrl+w".into(),
                group: "vim_window.group",
                continuations: vec![("c q", "vim_window.close", false)],
            })
    }

    /// Cancel only the history window's pending keys when dismissing its hint.
    pub(super) fn cancel_command_window_sequence(&mut self) {
        if let Some(history) = &mut self.command_window {
            history.window_prefix = false;
            history.editor.cancel_pending();
        }
    }

    /// Compute the full-width history area above the footer, clipped to visible rows.
    fn command_window_rect(&self) -> Rect {
        let bottom = (self.layout.editor.y + self.layout.editor.h)
            .max(self.layout.tree.y + self.layout.tree.h)
            .min(self.layout.visible_rows);
        // Keep room for the hint above the history without resizing when it opens.
        let height = bottom
            .saturating_sub(6)
            .clamp(3, COMMAND_WINDOW_HEIGHT)
            .min(bottom.saturating_sub(1));
        Rect {
            x: 0,
            y: bottom - height,
            w: self.layout.visible_cols,
            h: height,
        }
    }

    /// Open `prompt` history without transferring focus across projects or persisting the buffer.
    pub(super) fn open_command_window(
        &mut self,
        prompt: char,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.projects.active_project() else {
            return;
        };
        let area = self.command_window_rect();
        if self.modal_open() || project.focus != Focus::Editor || area.w < 10 || area.h < 3 {
            return;
        }
        self.command_window = Some(CommandWindow::new(
            prompt,
            &project.editor,
            self.metrics,
            cx,
        ));
        self.dismiss_completion(cx);
        self.dismiss_definition_hover(cx);
        self.hovered_controls = None;
        self.last_cursor_space = None;
        self.sync(window, cx);
    }

    /// Close the scratch window and `resume` the original editor or command prompt.
    fn finish_command_window(
        &mut self,
        resume: Resume,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(history) = self.command_window.take() else {
            return;
        };
        self.last_cursor_space = None;
        if matches!(resume, Resume::Cancel) {
            if matches!(
                self.projects.active().editor.vim().mode,
                Mode::Command | Mode::Search
            ) {
                self.run_vim(|vim, register| vim.key("Escape", register), window, cx);
            }
        } else {
            let (line, cursor) = history.selected();
            self.run_vim(
                |vim, register| {
                    vim.restore_command_window_line(history.prompt, line, cursor);
                    if matches!(resume, Resume::Run) {
                        vim.key("Enter", register)
                    } else {
                        Action::None
                    }
                },
                window,
                cx,
            );
        }
        self.sync(window, cx);
    }

    /// Dispatch a `key` while history owns input, consuming all workspace-level shortcuts.
    /// Return false only when a text key should travel through native IME to the scratch buffer.
    pub(super) fn command_window_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.cursor
            .update(cx, |cursor, cx| cursor.activity(window, cx));
        let key = &event.keystroke;
        if let Some(direction) = hint_page_direction(key)
            && self.command_window.as_ref().is_some_and(|history| {
                history.editor.composition().is_empty() && !history.editor.wants_native_mapping(key)
            })
            && self.page_sequence_hint(direction, cx)
        {
            return true;
        }
        self.hint_page = 0;
        let modifiers = key.modifiers;
        let raw = key.key.as_str();
        let area = self.command_window_rect();
        let directory = self.ex_current_dir().to_owned();
        let history = self.command_window.as_mut().expect("active command window");
        let vim = history.editor.vim();
        let mode = vim.mode;
        let plain = modifiers == Modifiers::default();
        let control = modifiers
            == (Modifiers {
                control: true,
                ..Modifiers::default()
            });
        let preedit = !history.editor.composition().is_empty();
        if history.window_prefix {
            if !event.is_held {
                history.window_prefix = false;
                if plain && matches!(raw, "c" | "q") {
                    self.held_close_key = Some(raw.into());
                    self.finish_command_window(Resume::Cancel, window, cx);
                } else {
                    cx.notify();
                }
            }
            return true;
        }
        if control && raw == "w" && mode == Mode::Normal && !vim.awaiting_key() {
            history.window_prefix = true;
            return true;
        }
        if !preedit
            && !vim.awaiting_key()
            && !matches!(mode, Mode::Command | Mode::Search)
            && (plain && raw == "enter" || control && matches!(raw, "j" | "m"))
        {
            if !event.is_held {
                self.held_close_key = Some(raw.into());
                self.finish_command_window(Resume::Run, window, cx);
            }
            return true;
        }
        if control && raw == "c" {
            if !event.is_held {
                self.held_close_key = Some(raw.into());
                self.finish_command_window(Resume::Prompt, window, cx);
            }
            return true;
        }
        if vim.accepts_text()
            && raw == "v"
            && (modifiers.platform || modifiers.control && modifiers.shift)
            && !modifiers.alt
            && !modifiers.function
        {
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                history.editor.composition_mut().clear();
                self.commit_command_window_text(&text, false, window, cx);
            }
            return true;
        }
        if preedit && !(cfg!(windows) && raw == "escape" && plain) {
            return false;
        }
        let before = history.editor.register().yank_revision();
        let Some(result) = history
            .editor
            .key(key, area, &self.spacing, &directory, false)
        else {
            return false;
        };
        if history.editor.register().yank_revision() != before {
            cx.write_to_clipboard(ClipboardItem::new_string(
                history.editor.register().text.clone(),
            ));
        }
        self.finish_command_window_action(
            result.edit.action,
            [
                result
                    .view_top
                    .map_or(ScrollInput::Reveal, ScrollInput::PositionWithTail),
                result
                    .view_left
                    .map_or(ScrollInput::Reveal, ScrollInput::PositionWithTail),
            ],
            window,
            cx,
        );
        true
    }

    /// Process scratch `action` and preserve `scroll` intent when the history remains open.
    pub(super) fn finish_command_window_action(
        &mut self,
        action: Action,
        [rows, columns]: [ScrollInput; 2],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            Action::Close(_) => self.finish_command_window(Resume::Cancel, window, cx),
            Action::Sleep(seconds) => self.sleep_editor(seconds, window, cx),
            Action::WindowExtras(nanobug::vim::WindowCommand::Sleep {
                duration,
                hide_caret,
            }) => self.sleep_editor_for(duration, hide_caret, window, cx),
            Action::None => self.repaint_keyboard_command_window(rows, columns, window, cx),
            _ => {
                if let Some(history) = &mut self.command_window {
                    history.editor.edit(|vim, _| {
                        vim.message = i18n::text("command_window.scratch_action").into();
                        Action::None
                    });
                    self.sync(window, cx);
                }
            }
        }
    }

    /// Commit `text` into the scratch buffer using its Vim mode. `typed` enables mappings.
    pub(super) fn commit_command_window_text(
        &mut self,
        text: &str,
        typed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(history) = &mut self.command_window {
            let text = text.replace("\r\n", "\n");
            let edit = history.editor.edit(|vim, register| {
                if typed {
                    vim.commit_text(&text, register)
                } else {
                    vim.paste_text(&text, register)
                }
            });
            self.finish_command_window_action(edit.action, [ScrollInput::Reveal; 2], window, cx);
        }
    }

    /// Publish keyboard `rows` and `columns` scroll intent through the shared repaint path.
    fn repaint_keyboard_command_window(
        &mut self,
        rows: ScrollInput,
        columns: ScrollInput,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.repaint([ScrollInput::Advance, rows, columns], window, cx);
    }

    /// Consume wheel `event` in the history window and scroll its own editor.
    pub(super) fn wheel_command_window(
        &mut self,
        event: &ScrollEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let area = self.command_window_rect();
        if !area.pixels(self.metrics).contains(&event.position) {
            return;
        }
        let history = self.command_window.as_mut().expect("active command window");
        if !history.editor.composition().is_empty() {
            return;
        }
        let delta = event.delta.pixel_delta(px(self.metrics.lh));
        let horizontal = event.modifiers.shift || delta.x != px(0.) && delta.y == px(0.);
        let (rows, columns) = if horizontal {
            let pixels = if delta.x == px(0.) { delta.y } else { delta.x };
            let columns = history
                .editor
                .wheel_columns(-f32::from(pixels) / self.metrics.cw);
            (ScrollInput::Advance, ScrollInput::Wheel(columns))
        } else {
            history.wheel -= f32::from(delta.y) / self.metrics.lh;
            let rows = history.wheel.trunc() as isize;
            history.wheel -= rows as f32;
            (ScrollInput::Wheel(rows), ScrollInput::Advance)
        };
        self.scroll_frame([ScrollInput::Advance, rows, columns], window, cx);
    }

    /// Advance the history viewport using `inputs` and rebase its caret during smooth scrolling.
    pub(super) fn advance_command_window(
        &mut self,
        inputs: [ScrollInput; 3],
        cx: &mut Context<Self>,
    ) -> bool {
        let area = self.command_window_rect();
        let [_, rows, columns] = inputs;
        let now = self.scroll_epoch.elapsed().as_secs_f64();
        let history = self.command_window.as_mut().expect("active command window");
        let (changed, dy, dx) =
            history
                .editor
                .track_view(area, true, &self.spacing, [rows, columns], now);
        if self.last_cursor_space == Some(CursorSpace::CommandWindow) {
            let buffer = self
                .command_window
                .as_ref()
                .expect("active command window")
                .editor
                .buffer();
            self.follow_cursor_scroll(&buffer.scroll, dy, Some((&buffer.columns, dx)), cx);
        }
        changed
    }
}
