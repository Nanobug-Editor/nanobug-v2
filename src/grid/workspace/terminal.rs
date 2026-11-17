//! Connect the retained terminal buffer to native keys, clipboard, IME and PTY events.

use super::{Workspace, keyboard::PendingPrefix, layout::Focus, projects::SplitDocument};
use crate::grid::windows::{
    editor::EditorWindow,
    terminal::{KeyResult, Terminal},
};
use alacritty_terminal::event::Event;
use gpui::{ClipboardItem, Context, KeyDownEvent, Keystroke, Modifiers, Window};
use nanobug::{
    i18n,
    vim::{Action, PendingMappingHint, SequenceHint, TerminalStroke},
};
use unicode_segmentation::UnicodeSegmentation;

#[cfg(all(test, windows))]
mod lifecycle_tests;

impl Workspace {
    /// Open a PTY using the active project's directory and optional `command`.
    pub(super) fn open_terminal(
        &mut self,
        command: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.projects.active_project() else {
            return;
        };
        if self.active_ex_window_modifiers.orientation.is_some()
            || self.ex_tab_insertion().is_some()
        {
            let original = project.active_editor_id();
            self.open_editor_split_at(true, false, SplitDocument::Empty, None, window, cx);
            if self.projects.active().active_editor_id() == original {
                return;
            }
        }
        let area = EditorWindow::body(self.active_editor_area());
        let directory = self.ex_current_dir().to_owned();
        let (terminal, events) = match Terminal::start(directory, command, area.w, area.h) {
            Ok(terminal) => terminal,
            Err(error) => {
                self.set_status(i18n::format("terminal.failed", &[("error", &error)]));
                return;
            }
        };
        self.finish_editor_input(window, cx);
        let now = self.scroll_epoch.elapsed().as_secs_f64();
        let project = self.projects.active_mut();
        project.focus_pane(Focus::Editor);
        let number = project.retain_terminal(terminal, now);
        project.status.clear();
        self.last_cursor_space = None;
        self.sync(window, cx);

        // Drain batches of output without scheduling a whole repaint for each PTY packet.
        cx.spawn_in(window, async move |workspace, cx| {
            while let Ok(first) = events.recv().await {
                let batch = std::iter::once(first)
                    .chain(std::iter::from_fn(|| events.try_recv().ok()).take(63))
                    .collect::<Vec<_>>();
                let updated = cx.update(|window, cx| {
                    workspace.update(cx, |workspace, cx| {
                        workspace.terminal_events(number, batch, window, cx)
                    })
                });
                if !matches!(updated, Ok(Ok(true))) {
                    break;
                }
            }
        })
        .detach();
    }

    /// Deliver PTY `events` to Session buffer `number`, including while every view is hidden.
    fn terminal_events(
        &mut self,
        number: u64,
        events: Vec<Event>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let visible = self.projects.active_project().is_some_and(|project| {
            project
                .page_editors()
                .any(|(_, editor)| editor.vim().doc.buffer_number() == number)
        });
        let terminal = self
            .projects
            .ordered()
            .find_map(|project| project.terminal(number));
        if let Some(terminal) = terminal {
            for event in events {
                terminal.event(event);
            }
            let running = !terminal.exited();
            if visible {
                self.sync(window, cx);
            }
            running
        } else {
            false
        }
    }

    /// Return the terminal when the active editor, rather than a popup or Ex prompt, owns keys.
    pub(super) fn terminal_keys_active(&self) -> bool {
        !self.modal_open()
            && self.projects.active_project().is_some_and(|project| {
                project.focus == Focus::Editor
                    && project.editor.terminal().is_some()
                    && !project.editor.vim().accepts_text()
            })
    }

    /// Show the active terminal's pending key continuations through the common hint layer.
    pub(super) fn terminal_hint(&self) -> Option<SequenceHint> {
        if !self.terminal_keys_active() {
            return None;
        }
        self.projects.active().editor.terminal()?.sequence_hint()
    }

    /// Return the current terminal input layer when it can receive a mapped sequence.
    fn mapped_terminal_editor(&self) -> Option<&EditorWindow> {
        if !self.terminal_keys_active() {
            return None;
        }
        let editor = &self.projects.active().editor;
        editor.terminal()?.accepts_text().then_some(editor)
    }

    /// Identify the terminal prefix using the ordinary workspace mapping timer.
    pub(super) fn terminal_mapping_token(&self) -> Option<(u64, u64)> {
        self.mapped_terminal_editor()?
            .vim()
            .terminal_mapping_token()
    }

    /// Show reachable terminal mappings in the same localized 150 ms popup as other modes.
    pub(super) fn terminal_mapping_hint(&self) -> Option<PendingMappingHint> {
        let editor = self.mapped_terminal_editor()?;
        editor.vim().terminal_mapping_hint(editor.register())
    }

    /// Resume this editor's child input after its single temporary Normal command.
    pub(super) fn finish_terminal_normal(&mut self) {
        if let Some(project) = self.projects.active_project_mut()
            && !project.editor.vim().accepts_text()
            && let Some(term) = project.editor.terminal_mut()
        {
            term.finish_normal();
        }
    }

    /// Send native input or paste to the active PTY, leaving the source file stationary.
    pub(super) fn commit_terminal_text(
        &mut self,
        text: &str,
        pasted: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if pasted {
            self.finish_terminal_mapping(window, cx);
            if let Some(terminal) = self
                .projects
                .active_project_mut()
                .and_then(|project| project.editor.terminal_mut())
            {
                terminal.commit(text, true);
            }
            return;
        }
        let Some(editor) = self.mapped_terminal_editor() else {
            return;
        };
        if !editor.vim().has_terminal_mappings(editor.register()) {
            self.projects
                .active_mut()
                .editor
                .terminal_mut()
                .expect("input terminal")
                .commit(text, false);
            return;
        }
        let opening = editor.vim().doc.opening;
        for key in text.graphemes(true) {
            if self
                .mapped_terminal_editor()
                .is_none_or(|editor| editor.vim().doc.opening != opening)
            {
                break;
            }
            if !self.apply_terminal_mapping_key(key.to_owned(), window, cx) {
                self.projects
                    .active_mut()
                    .editor
                    .terminal_mut()
                    .expect("input terminal")
                    .commit(key, false);
            }
        }
    }

    /// Replay a terminal prefix at its timeout or before a literal paste.
    pub(super) fn finish_terminal_mapping(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((opening, _)) = self.terminal_mapping_token() else {
            return;
        };
        let mut strokes = Vec::new();
        self.projects.active_mut().editor.edit(|vim, register| {
            strokes = vim.finish_terminal_mapping(register);
            Action::None
        });
        self.deliver_terminal_strokes(opening, strokes, window, cx);
    }

    /// Match one canonical token without creating a separate terminal mapping table or timer.
    fn apply_terminal_mapping_key(
        &mut self,
        key: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let editor = &mut self.projects.active_mut().editor;
        let opening = editor.vim().doc.opening;
        let mut strokes = None;
        editor.edit(|vim, register| {
            strokes = vim.terminal_mapping_key(key, register);
            Action::None
        });
        let Some(strokes) = strokes else {
            return false;
        };
        self.deliver_terminal_strokes(opening, strokes, window, cx);
        true
    }

    /// Deliver literal mapped keys or <Cmd> to the originating terminal view.
    fn deliver_terminal_strokes(
        &mut self,
        opening: u64,
        strokes: Vec<TerminalStroke>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for stroke in strokes {
            let Some(editor) = self
                .projects
                .active_project_mut()
                .map(|project| &mut project.editor)
                .filter(|editor| editor.vim().doc.opening == opening)
            else {
                return;
            };
            match stroke {
                TerminalStroke::Command(command) => {
                    self.run_vim(
                        |vim, register| vim.execute_ex_line(&command, register),
                        window,
                        cx,
                    );
                }
                TerminalStroke::Key(text) => {
                    let Some(terminal) = editor.terminal_mut() else {
                        return;
                    };
                    match terminal.key(&native_terminal_stroke(&text)) {
                        KeyResult::Text => terminal.commit(&text, false),
                        KeyResult::Copy(text) => {
                            cx.write_to_clipboard(ClipboardItem::new_string(text))
                        }
                        KeyResult::Paste => {
                            if let Some(text) =
                                cx.read_from_clipboard().and_then(|item| item.text())
                            {
                                terminal.commit(&text, true);
                            }
                        }
                        KeyResult::Command => {
                            self.run_vim(|vim, register| vim.key(":", register), window, cx)
                        }
                        KeyResult::Close => return,
                        KeyResult::Consume => {}
                    }
                }
            }
        }
    }

    /// Intercept a terminal's `event`, retaining native text handling when requested by the PTY.
    /// A terminal Normal Ex command continues through the document's usual prompt.
    pub(super) fn terminal_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<bool> {
        if !self.terminal_keys_active() {
            return None;
        }
        let input = self.projects.active().editor.terminal()?.accepts_text();
        // Physical nontext keys can map immediately. Printable keys reach the matcher only
        // after native composition commits, just like other insert-style input surfaces.
        if input
            && let Some(key) = EditorWindow::mapping_key_token(&event.keystroke, true)
            && self.apply_terminal_mapping_key(key, window, cx)
        {
            self.sync(window, cx);
            return Some(true);
        }
        if !input && self.dispatch_window_key(event, window, cx) {
            if !matches!(self.prefix, Some(PendingPrefix::Window(_))) {
                self.finish_terminal_normal();
                self.sync(window, cx);
            }
            return Some(true);
        }
        let project = self.projects.active_mut();
        match project.editor.terminal_mut()?.key(&event.keystroke) {
            KeyResult::Text => return Some(false),
            KeyResult::Command => return None,
            KeyResult::Copy(text) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
            KeyResult::Paste => {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    project.editor.composition_mut().clear();
                    project.editor.terminal_mut()?.commit(&text, true);
                }
            }
            KeyResult::Close if !event.is_held => {
                let now = self.scroll_epoch.elapsed().as_secs_f64();
                let number = project.editor.vim().doc.buffer_number();
                project.remove_session_buffer(number, now);
                self.held_close_key = Some(event.keystroke.key.to_string());
                self.last_cursor_space = None;
            }
            KeyResult::Consume | KeyResult::Close => {}
        }
        self.sync(window, cx);
        Some(true)
    }
}

/// Convert a modal key token back to the physical form understood by the PTY encoder.
fn native_terminal_stroke(mut token: &str) -> Keystroke {
    let mut modifiers = Modifiers::default();
    while let Some((modifier, tail)) = token.split_once('+') {
        match modifier {
            "Ctrl" => modifiers.control = true,
            "Alt" => modifiers.alt = true,
            "Shift" => modifiers.shift = true,
            _ => break,
        }
        token = tail;
    }
    let key_char = (token.graphemes(true).count() == 1).then(|| token.to_owned());
    modifiers.shift |= token.len() == 1 && token.as_bytes()[0].is_ascii_uppercase();
    Keystroke {
        key: if token == " " {
            "space".into()
        } else {
            token.to_ascii_lowercase()
        },
        key_char,
        modifiers,
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::grid::workspace::native_test;
    use anyhow::{Context as _, Result, ensure};
    use gpui::{AppContext, EntityInputHandler, Keystroke};
    use nanobug::plugins::session::Options;
    use std::{
        fs,
        time::{Duration, Instant},
    };

    /// Send one physical `key` through the actual native dispatcher for `window`.
    fn press(
        workspace: &mut Workspace,
        key: &'static str,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Result<()> {
        workspace.key_down(
            &KeyDownEvent {
                keystroke: Keystroke::parse(key)?,
                is_held: false,
                prefer_character_input: false,
            },
            window,
            cx,
        );
        Ok(())
    }

    /// Submit an Ex `command` through native input in the focused pane.
    pub(super) fn ex(
        workspace: &mut Workspace,
        command: &str,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Result<()> {
        press(workspace, ":", window, cx)?;
        workspace.replace_text_in_range(None, command, window, cx);
        press(workspace, "enter", window, cx)
    }

    /// Match native terminal chords and committed text using the ordinary hint and timeout state.
    #[test]
    #[ignore = "requires Windows DirectWrite and ConPTY"]
    fn native_terminal_mappings_run_commands_and_release_unmapped_text() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let project = temporary.path().join("project");
        fs::create_dir(&project)?;
        fs::write(project.join("README.md"), "base\n")?;
        let options = Options::take(&mut vec!["--no-plugins".into()], temporary.path())?;
        native_test::run_async(
            project,
            options,
            |_| {},
            async move |handle, app| {
                app.update(|app| {
                    handle.update(app, |workspace, window, cx| -> Result<()> {
                        ex(workspace, "tmap <F8> <Cmd>set ignorecase<CR>", window, cx)?;
                        ex(workspace, r"tnoremap jj <C-\><C-n>", window, cx)?;
                        ex(workspace, "terminal pause", window, cx)?;
                        press(workspace, "i", window, cx)?;
                        ensure!(
                            workspace.mapped_terminal_editor().is_some(),
                            "Terminal input before mapping"
                        );
                        press(workspace, "f8", window, cx)?;
                        ensure!(workspace.input_mappings.options().ignorecase);
                        workspace.replace_text_in_range(None, "j", window, cx);
                        let hint = workspace
                            .terminal_mapping_hint()
                            .context("Terminal mapping hint")?;
                        ensure!(hint.prefix == "j" && hint.continuations[0].keys == "j");
                        ensure!(workspace.mapping_timer.is_some());
                        workspace.replace_text_in_range(None, "j", window, cx);
                        ensure!(workspace.mapped_terminal_editor().is_none());
                        ensure!(workspace.terminal_mapping_hint().is_none());
                        ex(workspace, "tunmap jj", window, cx)?;
                        ex(workspace, "tmapclear", window, cx)?;
                        press(workspace, "i", window, cx)?;
                        workspace.replace_text_in_range(None, "jj", window, cx);
                        ensure!(
                            workspace.mapped_terminal_editor().is_some(),
                            "Terminal input after removing mappings"
                        );
                        ensure!(workspace.terminal_mapping_hint().is_none());
                        Ok(())
                    })
                })?
            },
        )
    }

    /// Keep one running ConPTY through closing all views, preview it and protect its deletion.
    #[test]
    #[ignore = "requires the Windows DirectWrite and DirectX renderer and ConPTY"]
    fn native_shared_terminal_survives_hidden_views_and_preview() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let project = temp.path().join("project");
        fs::create_dir(&project)?;
        fs::write(project.join("README.txt"), "keep the editor layout\n")?;
        let options = Options::take(&mut vec!["--no-plugins".into()], temp.path())?;
        native_test::run_async(
            project,
            options,
            |workspace| workspace.projects.active_mut().focus_pane(Focus::Editor),
            async move |handle, app| {
                let _keep_temp = temp;
                app.update_window(handle.into(), |_, window, _| {
                    window.resize(gpui::size(gpui::px(920.), gpui::px(580.)));
                })?;
                app.background_executor()
                    .timer(Duration::from_millis(60))
                    .await;
                let number = app.update(|app| {
                    handle.update(app, |workspace, window, cx| -> Result<u64> {
                        workspace.sync(window, cx);
                        let source = workspace.projects.active().active_editor_id();
                        ex(workspace, "vsplit", window, cx)?;
                        ex(
                            workspace,
                            "terminal echo SHARED_READY & set /p T= & echo SHARED_DONE & set /p T=",
                            window,
                            cx,
                        )?;
                        let project = workspace.projects.active();
                        let original = project.active_editor_id();
                        let number = project.editor.vim().doc.buffer_number();
                        ensure!(original != source && project.terminal(number).is_some());
                        ex(workspace, &format!("sbuffer {number}"), window, cx)?;
                        let project = workspace.projects.active();
                        let mirror = project.active_editor_id();
                        ensure!(
                            mirror != source && mirror != original && project.split_count() == 3
                        );
                        ensure!(project.editor.vim().doc.buffer_number() == number);
                        ensure!(project.editor.terminal().is_some());
                        let now = workspace.scroll_epoch.elapsed().as_secs_f64();
                        workspace
                            .projects
                            .active_mut()
                            .focus_editor_by_id(original, now);
                        ex(workspace, "close", window, cx)?;
                        ensure!(workspace.projects.active().editor_by_id(original).is_none());
                        workspace
                            .projects
                            .active_mut()
                            .focus_editor_by_id(mirror, now);
                        ex(workspace, "close", window, cx)?;
                        let project = workspace.projects.active();
                        ensure!(project.active_editor_id() == source && project.split_count() == 1);
                        ensure!(
                            project
                                .terminal(number)
                                .is_some_and(|terminal| !terminal.exited())
                        );
                        ensure!(project.session_buffer_numbers().contains(&number));
                        ensure!(
                            project
                                .editors()
                                .all(|(_, editor)| editor.terminal().is_none())
                        );
                        Ok(number)
                    })
                })??;
                let deadline = Instant::now() + Duration::from_secs(8);
                loop {
                    if app.update(|app| {
                        handle.update(app, |workspace, _, _| {
                            workspace
                                .projects
                                .active()
                                .terminal(number)
                                .is_some_and(|terminal| terminal.contains_text("SHARED_READY"))
                        })
                    })? {
                        break;
                    }
                    ensure!(
                        Instant::now() < deadline,
                        "hidden terminal did not receive output"
                    );
                    app.background_executor()
                        .timer(Duration::from_millis(20))
                        .await;
                }
                app.update(|app| {
                    handle.update(app, |workspace, window, cx| -> Result<()> {
                        ex(workspace, "tabnew", window, cx)?;
                        let project = workspace.projects.active();
                        let source = project.active_editor_id();
                        ensure!(project.page_count() == 2);
                        ex(workspace, &format!("pbuffer {number}"), window, cx)?;
                        let project = workspace.projects.active();
                        ensure!(project.active_editor_id() == source && project.split_count() == 2);
                        let (preview, view) = project
                            .page_editors()
                            .find(|(_, editor)| editor.vim().doc.buffer_number() == number)
                            .context("missing terminal preview")?;
                        ensure!(
                            view.terminal()
                                .is_some_and(|terminal| terminal.contains_text("SHARED_READY"))
                        );
                        let now = workspace.scroll_epoch.elapsed().as_secs_f64();
                        workspace
                            .projects
                            .active_mut()
                            .focus_editor_by_id(preview, now);
                        press(workspace, "i", window, cx)?;
                        workspace.replace_text_in_range(None, "preview input", window, cx);
                        press(workspace, "enter", window, cx)?;
                        press(workspace, "ctrl-\\", window, cx)?;
                        press(workspace, "ctrl-n", window, cx)?;
                        ex(workspace, &format!("bdelete {number}"), window, cx)?;
                        let project = workspace.projects.active();
                        ensure!(
                            project
                                .terminal(number)
                                .is_some_and(|terminal| !terminal.exited())
                        );
                        ensure!(
                            project.status.contains(&format!("bdelete! {number}")),
                            "{}",
                            project.status
                        );
                        Ok(())
                    })
                })??;
                let deadline = Instant::now() + Duration::from_secs(8);
                loop {
                    if app.update(|app| {
                        handle.update(app, |workspace, _, _| {
                            workspace
                                .projects
                                .active()
                                .terminal(number)
                                .is_some_and(|terminal| terminal.contains_text("SHARED_DONE"))
                        })
                    })? {
                        break;
                    }
                    ensure!(
                        Instant::now() < deadline,
                        "preview input did not reach the retained ConPTY"
                    );
                    app.background_executor()
                        .timer(Duration::from_millis(20))
                        .await;
                }
                app.update(|app| {
                    handle.update(app, |workspace, window, cx| -> Result<()> {
                        ex(workspace, &format!("bdelete! {number}"), window, cx)?;
                        let project = workspace.projects.active();
                        ensure!(
                            project.terminal(number).is_none()
                                && project.session_buffer(number).is_none()
                        );
                        ensure!(project.split_count() == 2 && project.page_count() == 2);
                        Ok(())
                    })
                })??;
                Ok(())
            },
        )
    }

    /// Use a real ConPTY shell and verify command entry, mode escapes, window focus and native preedit.
    #[test]
    #[ignore = "requires the Windows DirectWrite and DirectX renderer and ConPTY"]
    fn native_terminal_modes_route_conpty_and_vim_keys() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let project = temp.path().join("project");
        fs::create_dir(&project)?;
        fs::write(project.join("README.txt"), "retained source\n")?;
        let options = Options::take(
            &mut vec![
                "--no-plugins".into(),
                "--plugin-profile".into(),
                temp.path().join("profile").into_os_string(),
            ],
            temp.path(),
        )?;
        native_test::run_async(
            project,
            options,
            |workspace| workspace.projects.active_mut().focus_pane(Focus::Editor),
            async move |handle, app| {
                let _keep_temp = temp;
                app.update_window(handle.into(), |_, window, _| {
                    window.resize(gpui::size(gpui::px(920.), gpui::px(580.)));
                })?;
                app.background_executor()
                    .timer(Duration::from_millis(60))
                    .await;
                app.update(|app| handle.update(app, |workspace, window, cx| -> Result<()> {
                    workspace.sync(window, cx);
                    press(workspace, "ctrl-w", window, cx)?;
                    press(workspace, "v", window, cx)?;
                    ensure!(workspace.projects.active().split_count() == 2);
                    press(workspace, ":", window, cx)?;
                    workspace.replace_text_in_range(None,
                        "terminal for /L %i in (1,1,50) do @echo HISTORY%i & echo NANO_READY & set /p T= & echo NANO_DONE & pause",
                        window, cx,
                    );
                    press(workspace, "enter", window, cx)?;
                    ensure!(workspace.projects.active().editor.terminal().is_some(),
                        "{:?}", workspace.projects.active().status);
                    ensure!(!workspace.accepts_text_input(window, cx));
                    Ok(())
                }))??;
                for marker in ["NANO_READY", "NANO_DONE"] {
                    let deadline = Instant::now() + Duration::from_secs(8);
                    loop {
                        let ready = app.update(|app| {
                            handle.update(app, |workspace, _, _| {
                                workspace
                                    .projects
                                    .active()
                                    .editor
                                    .terminal()
                                    .is_some_and(|terminal| terminal.contains_text(marker))
                            })
                        })?;
                        if ready {
                            break;
                        }
                        ensure!(Instant::now() < deadline, "ConPTY output missing {marker}");
                        app.background_executor()
                            .timer(Duration::from_millis(20))
                            .await;
                    }
                    if marker == "NANO_DONE" {
                        break;
                    }
                    app.update(|app| {
                        handle.update(app, |workspace, window, cx| -> Result<()> {
                            let pane = workspace.projects.active().active_editor_id();
                            press(workspace, "i", window, cx)?;
                            ensure!(workspace.accepts_text_input(window, cx));
                            press(workspace, "ctrl-\\", window, cx)?;
                            let hint = workspace.terminal_hint().context("terminal escape hint")?;
                            ensure!(hint.continuations.iter().any(|(key, ..)| *key == "Ctrl+o"));
                            press(workspace, "ctrl-n", window, cx)?;
                            ensure!(!workspace.accepts_text_input(window, cx));
                            press(workspace, "ctrl-w", window, cx)?;
                            press(workspace, "w", window, cx)?;
                            ensure!(workspace.projects.active().active_editor_id() != pane);
                            press(workspace, "ctrl-w", window, cx)?;
                            press(workspace, "p", window, cx)?;
                            ensure!(workspace.projects.active().active_editor_id() == pane);
                            press(workspace, "a", window, cx)?;
                            press(workspace, "ctrl-\\", window, cx)?;
                            press(workspace, "ctrl-o", window, cx)?;
                            press(workspace, "g", window, cx)?;
                            ensure!(!workspace.accepts_text_input(window, cx));
                            press(workspace, "g", window, cx)?;
                            ensure!(workspace.accepts_text_input(window, cx));
                            workspace.replace_and_mark_text_in_range(
                                None,
                                "zhong",
                                Some(5..5),
                                window,
                                cx,
                            );
                            ensure!(workspace.marked_text_range(window, cx) == Some(0..5));
                            workspace.replace_text_in_range(None, "中文", window, cx);
                            ensure!(workspace.marked_text_range(window, cx).is_none());
                            press(workspace, "enter", window, cx)?;
                            Ok(())
                        })
                    })??;
                }
                Ok(())
            },
        )
    }
}
