//! Verify native composition, modal transitions and candidate geometry in a Windows workspace.

use super::{Focus, Workspace};
use crate::grid::workspace::native_test;
use anyhow::{Context as _, Result, ensure};
use gpui::{Bounds, Context, EntityInputHandler, KeyDownEvent, Keystroke, Window};
use nanobug::{plugins::session::Options, vim::Mode};
use std::fs;

/// Send one `key` through `workspace`'s native key handler for `window` using `cx`.
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

/// Expression register prefixes open a nested native text field and restore the original input.
#[test]
#[ignore = "requires the Windows DirectWrite and DirectX renderer"]
fn native_expression_register_restores_input_and_commits_chinese() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    fs::create_dir(&project)?;
    fs::write(project.join("README.md"), "cat\n")?;
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
            app.update(|app| {
                handle.update(app, |workspace, window, cx| -> Result<()> {
                    workspace.sync(window, cx);
                    press(workspace, "i", window, cx)?;
                    press(workspace, "ctrl-r", window, cx)?;
                    ensure!(!workspace.accepts_text_input(window, cx));
                    press(workspace, "=", window, cx)?;
                    ensure!(workspace.accepts_text_input(window, cx));
                    ensure!(workspace.projects.active().editor.vim().prompt() == '=');
                    workspace.replace_and_mark_text_in_range(None, "mao", Some(3..3), window, cx);
                    ensure!(workspace.marked_text_range(window, cx) == Some(0..3));
                    workspace.replace_text_in_range(None, "'猫'", window, cx);
                    ensure!(workspace.marked_text_range(window, cx).is_none());
                    press(workspace, "enter", window, cx)?;
                    ensure!(workspace.projects.active().editor.vim().mode == Mode::Insert);
                    ensure!(workspace.projects.active().editor.vim().doc.text == "猫cat\n");
                    press(workspace, "escape", window, cx)?;
                    press(workspace, ":", window, cx)?;
                    workspace.replace_text_in_range(None, "s/cat/", window, cx);
                    press(workspace, "ctrl-\\", window, cx)?;
                    press(workspace, "e", window, cx)?;
                    workspace.replace_text_in_range(None, "getcmdline() .. 'dog/'", window, cx);
                    press(workspace, "enter", window, cx)?;
                    ensure!(workspace.projects.active().editor.vim().command == "s/cat/dog/");
                    press(workspace, "enter", window, cx)?;
                    ensure!(workspace.projects.active().editor.vim().doc.text == "猫dog\n");
                    Ok(())
                })?
            })
        },
    )
}

/// Switch among the recent-project filter, command keys and file picker through the native input handler.
#[test]
#[ignore = "requires the Windows DirectWrite and DirectX renderer"]
fn native_project_and_file_filters_restore_chinese_input_after_list_commands() -> Result<()> {
    use crate::grid::windows::{file_picker::PromptKind, start::ProjectPicker};

    let temp = tempfile::tempdir()?;
    let project = temp.path().join("项目甲");
    let other = temp.path().join("项目乙");
    fs::create_dir(&project)?;
    fs::create_dir(&other)?;
    let options = Options::take(
        &mut vec![
            "--no-plugins".into(),
            "--plugin-profile".into(),
            temp.path().join("profile").into_os_string(),
        ],
        temp.path(),
    )?;
    native_test::run_async_visible(
        project.clone(),
        options,
        |_| {},
        async move |handle, app| {
            let _keep_temp = temp;
            app.update(|app| {
                handle.update(app, |ws, window, cx| -> Result<()> {
                    ws.open_start_tab(window, cx);
                    let roots = [project, other];
                    ws.projects.start_mut().unwrap().picker =
                        Some(Box::new(ProjectPicker::new(&roots)));
                    ws.sync(window, cx);
                    ensure!(ws.accepts_text_input(window, cx));
                    ws.replace_and_mark_text_in_range(None, "xiang", Some(5..5), window, cx);
                    ensure!(ws.marked_text_range(window, cx) == Some(0..5));
                    ws.replace_text_in_range(None, "项目", window, cx);
                    press(ws, "escape", window, cx)?;
                    ensure!(!ws.accepts_text_input(window, cx));
                    press(ws, "j", window, cx)?;
                    ensure!(
                        ws.projects
                            .start()
                            .unwrap()
                            .picker
                            .as_ref()
                            .unwrap()
                            .selected_root()
                            == Some(roots[1].as_path())
                    );
                    press(ws, "/", window, cx)?;
                    press(ws, "ctrl-a", window, cx)?;
                    ws.replace_text_in_range(None, "甲", window, cx);
                    ensure!(
                        ws.projects
                            .start()
                            .unwrap()
                            .picker
                            .as_ref()
                            .unwrap()
                            .selected_root()
                            == Some(roots[0].as_path())
                    );
                    ws.open_file_picker(window, cx);
                    Ok(())
                })
            })??;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
            loop {
                if app.update(|app| {
                    handle
                        .read(app)
                        .is_ok_and(|ws| ws.file_picker.as_ref().is_some_and(|s| !s.state.busy))
                }) {
                    break;
                }
                ensure!(
                    std::time::Instant::now() < deadline,
                    "file picker listing timed out"
                );
                app.background_executor()
                    .timer(std::time::Duration::from_millis(20))
                    .await;
            }
            app.update(|app| {
                handle.update(app, |ws, window, cx| -> Result<()> {
                    press(ws, "f", window, cx)?;
                    ensure!(ws.accepts_text_input(window, cx));
                    ws.replace_and_mark_text_in_range(None, "mu", Some(2..2), window, cx);
                    ws.replace_text_in_range(None, "目录", window, cx);
                    press(ws, "enter", window, cx)?;
                    ensure!(!ws.accepts_text_input(window, cx));
                    ensure!(ws.file_picker.as_ref().unwrap().state.filter == "目录");
                    press(ws, "/", window, cx)?;
                    ensure!(matches!(
                        ws.file_picker
                            .as_ref()
                            .unwrap()
                            .state
                            .prompt
                            .as_ref()
                            .map(|p| &p.kind),
                        Some(PromptKind::Search)
                    ));
                    ws.replace_and_mark_text_in_range(None, "wen", Some(3..3), window, cx);
                    ws.replace_text_in_range(None, "文", window, cx);
                    ensure!(
                        ws.file_picker
                            .as_ref()
                            .unwrap()
                            .state
                            .prompt
                            .as_ref()
                            .unwrap()
                            .value
                            == "文"
                    );
                    Ok(())
                })
            })?
        },
    )
}

/// The command-line history runs edited rows in the original editor and owns native preedit.
#[test]
#[ignore = "requires the Windows DirectWrite and DirectX renderer"]
fn native_command_window_edits_and_executes_a_history_line() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    fs::create_dir(&project)?;
    let source = "first cat\nsecond cat\n";
    fs::write(project.join("README.md"), source)?;
    let options = Options::take(
        &mut vec![
            "--plugin-profile".into(),
            temp.path().join("profile").into_os_string(),
            "--no-plugins".into(),
        ],
        temp.path(),
    )?;
    native_test::run_async_visible(
        project.clone(),
        options,
        |workspace| workspace.projects.active_mut().focus_pane(Focus::Editor),
        async move |handle, app| {
            let _keep_temp = temp;
            app.update(|app| {
                handle.update(app, |workspace, window, cx| -> Result<()> {
                    workspace.sync(window, cx);
                    for line in ["s/cat/dog/", "s/dog/cat/"] {
                        press(workspace, ":", window, cx)?;
                        workspace.replace_text_in_range(None, line, window, cx);
                        press(workspace, "enter", window, cx)?;
                    }
                    press(workspace, "q", window, cx)?;
                    press(workspace, ":", window, cx)?;
                    let history = workspace.command_window.as_ref().context("q: history")?;
                    ensure!(history.editor.vim().doc.text == "s/cat/dog/\ns/dog/cat/\n");
                    ensure!(history.selected() == (String::new(), 0));
                    ensure!(history.editor.vim().line_count() == 3);
                    ensure!(!workspace.accepts_text_input(window, cx));
                    press(workspace, "k", window, cx)?;
                    press(workspace, "k", window, cx)?;
                    press(workspace, "enter", window, cx)?;
                    ensure!(workspace.command_window.is_none());
                    let source = "first dog\nsecond cat\n";
                    ensure!(workspace.projects.active().editor.vim().doc.text == source);
                    press(workspace, "/", window, cx)?;
                    workspace.replace_text_in_range(None, "second", window, cx);
                    press(workspace, "enter", window, cx)?;
                    ensure!(workspace.projects.active().editor.buffer().cursor_row() == 1);
                    press(workspace, ":", window, cx)?;
                    workspace.replace_text_in_range(None, "s/cat/dog/", window, cx);
                    press(workspace, "left", window, cx)?;
                    press(workspace, "ctrl-f", window, cx)?;
                    let history = workspace
                        .command_window
                        .as_ref()
                        .context("Ctrl+f history")?;
                    ensure!(history.selected() == ("s/cat/dog/".into(), 9));
                    press(workspace, "escape", window, cx)?;
                    ensure!(workspace.command_window.is_some());
                    press(workspace, "ctrl-c", window, cx)?;
                    let vim = workspace.projects.active().editor.vim();
                    ensure!(vim.mode == Mode::Command && vim.command == "s/cat/dog/");
                    ensure!(vim.command_cursor() == 9);

                    press(workspace, "ctrl-f", window, cx)?;
                    press(workspace, "c", window, cx)?;
                    press(workspace, "c", window, cx)?;
                    ensure!(workspace.accepts_text_input(window, cx));
                    workspace.replace_and_mark_text_in_range(None, "mao", Some(3..3), window, cx);
                    ensure!(workspace.marked_text_range(window, cx) == Some(0..3));
                    ensure!(workspace.projects.active().editor.vim().doc.text == source);
                    workspace.replace_text_in_range(None, "s/cat/猫/", window, cx);
                    ensure!(workspace.marked_text_range(window, cx).is_none());
                    press(workspace, "enter", window, cx)?;
                    ensure!(workspace.command_window.is_none());
                    ensure!(
                        workspace.projects.active().editor.vim().doc.text
                            == "first dog\nsecond 猫\n"
                    );

                    press(workspace, "q", window, cx)?;
                    press(workspace, ":", window, cx)?;
                    press(workspace, ":", window, cx)?;
                    workspace.replace_text_in_range(None, "w command-window.txt", window, cx);
                    press(workspace, "enter", window, cx)?;
                    ensure!(workspace.command_window.is_some());
                    ensure!(!project.join("command-window.txt").exists());
                    press(workspace, "k", window, cx)?;
                    press(workspace, "ctrl-c", window, cx)?;
                    let vim = workspace.projects.active().editor.vim();
                    ensure!(vim.mode == Mode::Command && vim.command == "s/cat/猫/");
                    press(workspace, "ctrl-f", window, cx)?;
                    press(workspace, "ctrl-w", window, cx)?;
                    press(workspace, "c", window, cx)?;
                    ensure!(workspace.command_window.is_none());
                    ensure!(workspace.projects.active().editor.vim().mode == Mode::Normal);
                    Ok(())
                })?
            })
        },
    )
}

/// A pending find accepts committed Chinese and immediately restores command input.
#[test]
#[ignore = "requires the Windows DirectWrite and DirectX renderer"]
fn native_character_find_commits_and_cancels_composition() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    fs::create_dir(&project)?;
    fs::write(project.join("README.md"), "a中!\n")?;
    fs::write(project.join("asset-example.txt"), "")?;
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
            app.update(|app| {
                handle.update(app, |workspace, window, cx| -> Result<()> {
                    workspace.sync(window, cx);
                    ensure!(!workspace.accepts_native_text());

                    press(workspace, "f", window, cx)?;
                    ensure!(workspace.accepts_native_text());
                    workspace.replace_and_mark_text_in_range(None, "zhong", Some(5..5), window, cx);
                    ensure!(workspace.marked_text_range(window, cx) == Some(0..5));
                    let vim = workspace.projects.active().editor.vim();
                    ensure!(vim.doc.text == "a中!\n" && vim.doc.cursor == 0);
                    workspace.replace_text_in_range(None, "中", window, cx);
                    let vim = workspace.projects.active().editor.vim();
                    ensure!(vim.doc.text == "a中!\n" && vim.doc.cursor == 1);
                    ensure!(!workspace.accepts_native_text());
                    ensure!(workspace.marked_text_range(window, cx).is_none());

                    press(workspace, "l", window, cx)?;
                    ensure!(workspace.projects.active().editor.vim().doc.cursor == "a中".len());

                    press(workspace, "f", window, cx)?;
                    workspace.replace_and_mark_text_in_range(None, "zhong", Some(5..5), window, cx);
                    ensure!(workspace.marked_text_range(window, cx) == Some(0..5));
                    press(workspace, "escape", window, cx)?;
                    ensure!(!workspace.accepts_native_text());
                    ensure!(workspace.marked_text_range(window, cx).is_none());
                    let vim = workspace.projects.active().editor.vim();
                    ensure!(vim.mode == Mode::Normal && !vim.awaiting_key());
                    ensure!(vim.doc.text == "a中!\n" && vim.doc.cursor == "a中".len());

                    press(workspace, "i", window, cx)?;
                    ensure!(workspace.accepts_native_text());
                    workspace.replace_and_mark_text_in_range(None, "ni", Some(2..2), window, cx);
                    workspace.replace_text_in_range(None, "你", window, cx);
                    let vim = workspace.projects.active().editor.vim();
                    ensure!(vim.mode == Mode::Insert && vim.doc.text == "a中你!\n");

                    press(workspace, "ctrl-o", window, cx)?;
                    ensure!(!workspace.accepts_native_text());
                    press(workspace, "h", window, cx)?;
                    ensure!(workspace.accepts_native_text());
                    press(workspace, "ctrl-v", window, cx)?;
                    ensure!(workspace.accepts_native_text());
                    workspace.replace_and_mark_text_in_range(None, "hao", Some(3..3), window, cx);
                    workspace.replace_text_in_range(None, "好", window, cx);
                    let vim = workspace.projects.active().editor.vim();
                    ensure!(vim.mode == Mode::Insert && vim.doc.text == "a中好你!\n");
                    ensure!(workspace.marked_text_range(window, cx).is_none());

                    press(workspace, "escape", window, cx)?;
                    press(workspace, "o", window, cx)?;
                    workspace.replace_text_in_range(None, "asset", window, cx);
                    press(workspace, "ctrl-x", window, cx)?;
                    press(workspace, "ctrl-f", window, cx)?;
                    let (items, selected) =
                        workspace.completion_items().context("path completion")?;
                    ensure!(items == ["asset-example.txt"] && selected == 0);
                    press(workspace, "ctrl-y", window, cx)?;
                    ensure!(
                        workspace.projects.active().editor.vim().doc.line(1) == "asset-example.txt"
                    );
                    Ok(())
                })
            })??;
            Ok(())
        },
    )
}

/// A full Markdown row leaves the native candidate anchor inside the next visual row.
#[test]
#[ignore = "requires the Windows DirectWrite and DirectX renderer"]
fn native_markdown_wrap_keeps_ime_anchor_in_the_text_viewport() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    fs::create_dir(&project)?;
    let readme = project.join("README.md");
    fs::write(&readme, "")?;
    let options = Options::take(
        &mut vec![
            "--plugin-profile".into(),
            temp.path().join("profile").into_os_string(),
            "--no-plugins".into(),
        ],
        temp.path(),
    )?;
    native_test::run_async_visible(
        project,
        options,
        |workspace| workspace.projects.active_mut().focus_pane(Focus::Editor),
        async move |handle, app| {
            app.update(|app| {
                handle.update(app, |workspace, window, cx| -> Result<()> {
                    workspace.open(readme, window, cx);
                    workspace.sync(window, cx);
                    let source = workspace
                        .projects
                        .active()
                        .editor
                        .text_area(workspace.active_editor_area());
                    ensure!(source.w > 4 && source.h >= 3);
                    let line = "a".repeat(source.w);
                    let anchor = workspace.metrics.at(source.x as f32, (source.y + 1) as f32);

                    press(workspace, "i", window, cx)?;
                    ensure!(workspace.accepts_native_text());
                    workspace.replace_text_in_range(None, &line, window, cx);
                    let (_, _, _, insert, visible) = workspace.caret();
                    ensure!(insert && visible, "full row hid the insertion caret");
                    let bounds = workspace
                        .bounds_for_range(0..0, Bounds::default(), window, cx)
                        .context("insertion candidate anchor")?;
                    ensure!(bounds.origin == anchor, "full row anchor {bounds:?}");

                    workspace.replace_and_mark_text_in_range(None, "中", Some(1..1), window, cx);
                    ensure!(workspace.marked_text_range(window, cx) == Some(0..1));
                    ensure!(workspace.projects.active().editor.vim().doc.text == line);
                    let bounds = workspace
                        .bounds_for_range(0..1, Bounds::default(), window, cx)
                        .context("composing candidate anchor")?;
                    ensure!(bounds.origin == anchor, "wrapped preedit anchor {bounds:?}");

                    workspace.replace_text_in_range(None, "中", window, cx);
                    let vim = workspace.projects.active().editor.vim();
                    ensure!(vim.doc.text == format!("{line}中"));
                    ensure!(vim.doc.starts.len() == 1);
                    press(workspace, "escape", window, cx)?;
                    ensure!(!workspace.accepts_native_text());
                    press(workspace, "0", window, cx)?;
                    ensure!(workspace.projects.active().editor.vim().doc.cursor == 0);
                    Ok(())
                })
            })??;
            Ok(())
        },
    )
}
