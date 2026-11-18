//! Connect workspace-shared input definitions and mapped-prefix timeouts to native keyboard routing.

use super::{Workspace, sleep::Source};
use crate::grid::windows::editor::EditorWindow;
use gpui::{Context, Task, Window};
use std::time::Duration;

/// An incomplete mapping whose timeout is bound to its source buffer.
pub(super) struct MappingTimer {
    /// Buffer opening identity and key-prefix serial reported by the Vim model.
    token: (u64, u64),
    /// Dropping the task cancels the native timeout.
    _task: Task<()>,
}

impl Workspace {
    /// Give the focused editor the shared workspace definitions before it receives typed input.
    pub(super) fn activate_input_mappings(&mut self) {
        let Some((source, _)) = self.active_vim_input() else {
            return;
        };
        let definitions = self.input_mappings.clone();
        definitions.set_ex_log_directory(self.ex_current_dir());
        let scope = self
            .projects
            .active_project()
            .map(|project| project.script_tab_scope());
        let editor = match source {
            Source::Editor => &mut self.projects.active_mut().editor,
            Source::Preview => self
                .file_picker
                .as_mut()
                .and_then(|picker| picker.focused_editor_mut())
                .expect("active preview"),
            Source::History => &mut self.command_window.as_mut().expect("active history").editor,
        };
        editor.set_input_mappings(definitions);
        if let Some(scope) = scope {
            editor.set_script_tab_scope(scope);
        }
    }

    /// Schedule a timeout in `window` for the current mapping prefix, or release a stale timer.
    pub(super) fn track_mapping_timeout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let options = self.input_mappings.options();
        if !options.timeout {
            self.mapping_timer = None;
            return;
        }
        let Some((_, token)) = self.active_mapping() else {
            self.mapping_timer = None;
            return;
        };
        if self
            .mapping_timer
            .as_ref()
            .is_some_and(|timer| timer.token == token)
        {
            return;
        }
        let timer = cx
            .background_executor()
            .timer(Duration::from_millis(options.timeoutlen.into()));
        let task = cx.spawn_in(window, async move |workspace, cx| {
            timer.await;
            let _ = cx.update(|window, cx| {
                let _ = workspace.update(cx, |workspace, cx| {
                    if workspace
                        .mapping_timer
                        .as_ref()
                        .is_none_or(|timer| timer.token != token)
                    {
                        return;
                    }
                    workspace.mapping_timer = None;
                    let Some((source, current)) = workspace.active_mapping() else {
                        return;
                    };
                    if current == token {
                        if workspace.terminal_mapping_token() == Some(current) {
                            workspace.finish_terminal_mapping(window, cx);
                            workspace.sync(window, cx);
                        } else if source != Source::Editor
                            || !workspace.restore_mapped_shell_prefix(window, cx)
                        {
                            workspace.finish_deferred_vim_input(
                                source,
                                window,
                                cx,
                                EditorWindow::finish_mapping_timeout,
                            );
                        }
                        workspace.sync_sequence_hint_delay(cx);
                        workspace.track_mapping_timeout(window, cx);
                    }
                });
            });
        });
        self.mapping_timer = Some(MappingTimer { token, _task: task });
    }

    /// Report the pending map at the keyboard layer that can currently accept it.
    fn active_mapping(&self) -> Option<(Source, (u64, u64))> {
        if let Some(token) = self.terminal_mapping_token() {
            return Some((Source::Editor, token));
        }
        let (source, editor) = self.active_vim_input()?;
        Some((source, editor.vim().mapping_timeout_token()?))
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::grid::workspace::{layout::Focus, native_test};
    use anyhow::{Result, ensure};
    use gpui::{EntityInputHandler, KeyDownEvent, Keystroke};
    use nanobug::{plugins::session::Options, vim::Mode};
    use std::{fs, time::Instant};

    /// Type a symbolic `key` through the native handler of `workspace` in `window` and `cx`.
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

    /// Exercise general mappings through the same native key and text routes as the editor.
    #[test]
    #[ignore = "opens a native Windows workspace"]
    fn native_general_vim_mappings_route_all_four_modes() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let project = temp.path().join("project");
        fs::create_dir(&project)?;
        fs::write(project.join("README.md"), "cat dog\n")?;
        let options = Options::take(&mut vec!["--no-plugins".into()], temp.path())?;
        native_test::run_async(
            project,
            options,
            |workspace| workspace.projects.active_mut().focus_pane(Focus::Editor),
            async move |handle, app| {
                let _keep_temp = temp;
                use gpui::AppContext as _;
                app.update_window(handle.into(), |_, window, _| {
                    window.resize(gpui::size(gpui::px(920.), gpui::px(580.)))
                })?;
                app.background_executor()
                    .timer(Duration::from_millis(60))
                    .await;
                handle.update(app, |workspace, window, cx| -> Result<()> {
                    workspace.sync(window, cx);
                    for command in [
                        "nnoremap <F6> w",
                        "nnoremap <C-Left> h",
                        "nnoremap <Leader>zz 0",
                        "nnoremap <C-w>zz 0",
                        "inoremap jj <Esc>",
                        "cnoremap <F2> echo 'native'<CR>",
                        "onoremap <F3> iw",
                    ] {
                        press(workspace, ":", window, cx)?;
                        workspace.replace_text_in_range(None, command, window, cx);
                        press(workspace, "enter", window, cx)?;
                    }
                    press(workspace, "f6", window, cx)?;
                    ensure!(workspace.projects.active().editor.vim().doc.cursor == 4);
                    press(workspace, "ctrl-left", window, cx)?;
                    ensure!(workspace.projects.active().editor.vim().doc.cursor == 3);
                    press(workspace, "f6", window, cx)?;
                    press(workspace, "d", window, cx)?;
                    press(workspace, "f3", window, cx)?;
                    ensure!(workspace.projects.active().editor.vim().doc.text == "cat \n");

                    press(workspace, ":", window, cx)?;
                    press(workspace, "f2", window, cx)?;
                    ensure!(workspace.projects.active().editor.vim().mode == Mode::Normal);
                    ensure!(workspace.projects.active().editor.vim().message == "native");

                    press(workspace, "a", window, cx)?;
                    workspace.commit_pasted_text("jj", window, cx);
                    workspace.replace_and_mark_text_in_range(None, "mao", Some(3..3), window, cx);
                    ensure!(workspace.marked_text_range(window, cx).is_some());
                    workspace.replace_text_in_range(None, "猫", window, cx);
                    workspace.replace_text_in_range(None, "j", window, cx);
                    ensure!(workspace.mapping_timer.is_some());
                    workspace.replace_text_in_range(None, "j", window, cx);
                    let vim = workspace.projects.active().editor.vim();
                    ensure!(vim.doc.text == "cat jj猫\n");
                    ensure!(vim.mode == Mode::Normal);
                    for key in ["space", "z", "z"] {
                        press(workspace, key, window, cx)?;
                    }
                    ensure!(workspace.projects.active().editor.vim().doc.cursor == 0);
                    for key in ["space", "?"] {
                        press(workspace, key, window, cx)?;
                    }
                    ensure!(workspace.help.is_open());
                    press(workspace, "escape", window, cx)?;
                    press(workspace, "w", window, cx)?;
                    ensure!(workspace.projects.active().editor.vim().doc.cursor > 0);
                    for key in ["ctrl-w", "z", "z"] {
                        press(workspace, key, window, cx)?;
                    }
                    ensure!(workspace.projects.active().editor.vim().doc.cursor == 0);
                    let split_count = workspace.projects.active().split_count();
                    for key in ["ctrl-w", "n"] {
                        press(workspace, key, window, cx)?;
                    }
                    ensure!(workspace.projects.active().split_count() == split_count + 1);
                    Ok(())
                })?
            },
        )
    }

    /// Exercise ordinary IME input, project keymaps, and buffer scope through native input.
    #[test]
    #[ignore = "opens a native Windows workspace"]
    fn native_vim_keymap_loads_project_tables_and_preserves_input() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let project = temp.path().join("project");
        let keymaps = project.join("keymap");
        fs::create_dir_all(&keymaps)?;
        fs::write(project.join("README.md"), "cat\n")?;
        fs::write(project.join("second.txt"), "dog\n")?;
        fs::write(
            keymaps.join("nativebase.vim"),
            "let b:keymap_name = 'base'\nloadkeymap\nab W\nq Q\n",
        )?;
        fs::write(keymaps.join("nativeprobe.vim"), "loadkeymap\nq X\n")?;
        fs::write(
            keymaps.join("nativeprobe_utf-8.vim"),
            "let b:keymap_name = 'probe'\nloadkeymap\nq Y\nab <char-0x5a>\n",
        )?;
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
            |workspace| workspace.projects.active_mut().focus_pane(Focus::Editor),
            async move |handle, app| {
                let _keep_temp = temp;
                handle.update(app, |workspace, window, cx| -> Result<()> {
                    workspace.sync(window, cx);
                    let command = |workspace: &mut Workspace,
                                   text: &str,
                                   window: &mut Window,
                                   cx: &mut Context<Workspace>|
                     -> Result<()> {
                        press(workspace, ":", window, cx)?;
                        workspace.replace_text_in_range(None, text, window, cx);
                        press(workspace, "enter", window, cx)
                    };
                    press(workspace, "i", window, cx)?;
                    workspace.replace_text_in_range(None, "en", window, cx);
                    workspace.replace_and_mark_text_in_range(None, "mao", Some(3..3), window, cx);
                    ensure!(workspace.marked_text_range(window, cx) == Some(0..3));
                    workspace.replace_text_in_range(None, "猫", window, cx);
                    ensure!(workspace.marked_text_range(window, cx).is_none());
                    press(workspace, "escape", window, cx)?;
                    ensure!(!workspace.accepts_text_input(window, cx));
                    press(workspace, "0", window, cx)?;
                    let vim = workspace.projects.active().editor.vim();
                    ensure!(vim.doc.cursor == 0 && vim.doc.text == "en猫cat\n");

                    command(workspace, "lmap q G", window, cx)?;
                    command(workspace, "set keymap=nativebase", window, cx)?;
                    command(workspace, "set keymap?", window, cx)?;
                    ensure!(
                        workspace
                            .projects
                            .active()
                            .editor
                            .vim()
                            .message
                            .contains("keymap=nativebase"),
                        "{}",
                        workspace.projects.active().editor.vim().message
                    );

                    press(workspace, "i", window, cx)?;
                    ensure!(
                        workspace
                            .projects
                            .active()
                            .editor
                            .vim()
                            .active_keymap_label()
                            == Some("base")
                    );
                    workspace.replace_text_in_range(None, "ab", window, cx);
                    press(workspace, "ctrl-shift-6", window, cx)?;
                    workspace.replace_text_in_range(None, "q", window, cx);
                    press(workspace, "escape", window, cx)?;
                    ensure!(workspace.projects.active().editor.vim().doc.text == "Wqen猫cat\n");

                    command(workspace, "e second.txt", window, cx)?;
                    press(workspace, "i", window, cx)?;
                    workspace.replace_text_in_range(None, "q", window, cx);
                    press(workspace, "escape", window, cx)?;
                    ensure!(workspace.projects.active().editor.vim().doc.text == "Qdog\n");

                    press(workspace, ":", window, cx)?;
                    workspace.replace_text_in_range(None, "setlocal keymap=nativepr", window, cx);
                    press(workspace, "tab", window, cx)?;
                    ensure!(
                        workspace.projects.active().editor.vim().command
                            == "setlocal keymap=nativeprobe"
                    );
                    press(workspace, "enter", window, cx)?;
                    command(workspace, "setlocal keymap=missing", window, cx)?;
                    command(workspace, "setlocal keymap?", window, cx)?;
                    ensure!(
                        workspace
                            .projects
                            .active()
                            .editor
                            .vim()
                            .message
                            .contains("keymap=nativeprobe"),
                        "{}",
                        workspace.projects.active().editor.vim().message
                    );
                    press(workspace, "a", window, cx)?;
                    workspace.replace_text_in_range(None, "qab", window, cx);
                    press(workspace, "escape", window, cx)?;
                    ensure!(workspace.projects.active().editor.vim().doc.text == "QYZdog\n");

                    command(workspace, "setlocal keymap=", window, cx)?;
                    press(workspace, "a", window, cx)?;
                    workspace.replace_text_in_range(None, "q", window, cx);
                    press(workspace, "escape", window, cx)?;
                    ensure!(workspace.projects.active().editor.vim().doc.text == "QYZqdog\n");
                    Ok(())
                })?
            },
        )
    }

    /// Committed IME text, literal paste, timed lmap prefixes and gV use the native input layer.
    #[test]
    #[ignore = "opens a native Windows workspace"]
    fn native_language_mapping_abbreviation_timeout_and_gv() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let project = temp.path().join("project");
        fs::create_dir(&project)?;
        fs::write(project.join("README.md"), "cat\n")?;
        let options = Options::take(&mut vec!["--no-plugins".into()], temp.path())?;
        native_test::run_async(
            project,
            options,
            |workspace| workspace.projects.active_mut().focus_pane(Focus::Editor),
            async move |handle, app| {
                let _keep_temp = temp;
                handle.update(app, |workspace, window, cx| -> Result<()> {
                    workspace.sync(window, cx);
                    for command in [
                        "lmap 猫 喵",
                        "lmap 猫猫 双喵",
                        "iabbrev tt word",
                        "vnoremap <F2> <Esc>",
                        "vnoremap <F3> <Esc>gV",
                    ] {
                        press(workspace, ":", window, cx)?;
                        workspace.replace_text_in_range(None, command, window, cx);
                        press(workspace, "enter", window, cx)?;
                    }
                    press(workspace, "i", window, cx)?;
                    press(workspace, "ctrl-6", window, cx)?;
                    ensure!(workspace.accepts_text_input(window, cx));
                    workspace.replace_and_mark_text_in_range(None, "mao", Some(3..3), window, cx);
                    ensure!(workspace.marked_text_range(window, cx) == Some(0..3));
                    workspace.replace_text_in_range(None, "猫", window, cx);
                    ensure!(workspace.marked_text_range(window, cx).is_none());
                    ensure!(workspace.mapping_timer.is_some());
                    ensure!(workspace.projects.active().editor.vim().doc.text == "cat\n");
                    Ok(())
                })??;
                let deadline = Instant::now() + Duration::from_secs(3);
                loop {
                    let complete = handle.update(app, |workspace, _, _| {
                        workspace.mapping_timer.is_none()
                            && workspace.projects.active().editor.vim().doc.text == "喵cat\n"
                    })?;
                    if complete {
                        break;
                    }
                    ensure!(
                        Instant::now() < deadline,
                        "Language mapping did not finish its native timeout"
                    );
                    app.background_executor()
                        .timer(Duration::from_millis(20))
                        .await;
                }
                handle.update(app, |workspace, window, cx| -> Result<()> {
                    workspace.commit_pasted_text("猫 tt", window, cx);
                    workspace.replace_text_in_range(None, " tt ", window, cx);
                    ensure!(
                        workspace.projects.active().editor.vim().doc.text == "喵猫 word word cat\n"
                    );
                    for key in ["escape", "0", "g", "h", "ctrl-o", "l", "f2"] {
                        press(workspace, key, window, cx)?;
                    }
                    ensure!(workspace.accepts_text_input(window, cx));
                    ensure!(
                        workspace
                            .projects
                            .active()
                            .editor
                            .vim()
                            .selection()
                            .is_some()
                    );
                    press(workspace, "f3", window, cx)?;
                    ensure!(!workspace.accepts_text_input(window, cx));
                    ensure!(workspace.projects.active().editor.vim().mode == Mode::Normal);
                    Ok(())
                })?
            },
        )
    }
}
