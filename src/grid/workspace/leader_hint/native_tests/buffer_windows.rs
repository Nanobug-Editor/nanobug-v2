//! Run Bufferline and editor-window Leader bindings through the native keyboard dispatcher.

use super::{Result, Workspace, fs, native_test, press, press_hidden};
use crate::grid::workspace::layout::FOOTER_ROWS;
use anyhow::{Context as _, ensure};
use gpui::{AppContext, Context, EntityInputHandler, Window, px, size};
use nanobug::plugins::session::Options;
use std::time::Duration;

/// Send each key in a Leader `sequence` through the native `window` dispatcher.
fn keys(
    workspace: &mut Workspace,
    sequence: &[&'static str],
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Result<()> {
    for &key in sequence {
        press(workspace, key, window, cx)?;
    }
    Ok(())
}

/// Verify an uppercase mark follows its original retained buffer and Ctrl+6 returns to the alternate.
#[test]
#[ignore = "requires the Windows DirectWrite and DirectX renderer"]
fn native_file_marks_and_alternate_buffer_use_retained_documents() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("project");
    fs::create_dir(&root)?;
    let source = root.join("README.md");
    let destination = root.join("other.txt");
    fs::write(&source, "  alpha beta\n")?;
    fs::write(&destination, "other\n")?;
    let options = Options::take(
        &mut vec![
            "--plugin-profile".into(),
            temp.path().join("profile").into_os_string(),
            "--no-plugins".into(),
        ],
        temp.path(),
    )?;
    native_test::run_async(
        root,
        options,
        |_| {},
        async move |handle, app| {
            app.update(|app| {
                handle.update(app, |workspace, window, cx| -> Result<()> {
                    workspace.open(source, window, cx);
                    workspace.sync(window, cx);
                    let opening = workspace.projects.active().editor.vim().doc.opening;
                    keys(workspace, &["$", "m", "shift-a", "^", "i"], window, cx)?;
                    workspace.replace_text_in_range(None, "增", window, cx);
                    press(workspace, "escape", window, cx)?;
                    let other = {
                        let editor = &mut workspace.projects.active_mut().editor;
                        editor.open(destination, 0.)?;
                        editor.vim().doc.opening
                    };
                    keys(workspace, &["`", "shift-a"], window, cx)?;
                    let editor = &workspace.projects.active().editor;
                    ensure!(editor.vim().doc.opening == opening);
                    ensure!(editor.vim().doc.cursor == "  增alpha beta".len() - 1);
                    press(workspace, "ctrl-6", window, cx)?;
                    ensure!(workspace.projects.active().editor.vim().doc.opening == other);
                    keys(workspace, &["'", "shift-a"], window, cx)?;
                    let editor = &workspace.projects.active().editor;
                    ensure!(editor.vim().doc.opening == opening && editor.vim().doc.cursor == 2);
                    Ok(())
                })
            })?
        },
    )
}

/// File and mark jumps share one history with counted backward and forward traversal.
#[test]
#[ignore = "requires the Windows DirectWrite and DirectX renderer"]
fn native_vim_jumplist_follows_files_and_marks() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("project");
    fs::create_dir(&root)?;
    let source = root.join("README.md");
    let destination = root.join("other.txt");
    fs::write(&source, "missing.txt\nother.txt:3\nalpha\n")?;
    fs::write(&destination, "first\nsecond\n  third\n")?;
    let options = Options::take(&mut vec!["--no-plugins".into()], temp.path())?;
    native_test::run_async(
        root,
        options,
        |_| {},
        async move |handle, app| {
            app.update_window(handle.into(), |_, window, _| {
                window.resize(size(px(920.), px(580.)));
            })?;
            app.background_executor()
                .timer(Duration::from_millis(60))
                .await;
            app.update(|app| {
                handle.update(app, |workspace, window, cx| -> Result<()> {
                    workspace.open(source, window, cx);
                    workspace.sync(window, cx);
                    let at = |workspace: &Workspace, file: &str, row_col| {
                        let doc = &workspace.projects.active().editor.vim().doc;
                        doc.path.as_ref().is_some_and(|path| path.ends_with(file))
                            && doc.location(doc.cursor) == row_col
                    };

                    keys(workspace, &["g", "f", "ctrl-o"], window, cx)?;
                    ensure!(at(workspace, "README.md", (0, 0)));
                    keys(workspace, &["j", "l", "l", "m", "shift-a"], window, cx)?;
                    keys(workspace, &["g", "f"], window, cx)?;
                    ensure!(at(workspace, "other.txt", (0, 0)));
                    press(workspace, "ctrl-o", window, cx)?;
                    ensure!(at(workspace, "README.md", (1, 2)));
                    keys(workspace, &["g", "shift-f"], window, cx)?;
                    ensure!(at(workspace, "other.txt", (2, 0)));
                    press(workspace, "ctrl-o", window, cx)?;
                    ensure!(at(workspace, "README.md", (1, 2)));
                    press(workspace, "tab", window, cx)?;
                    ensure!(at(workspace, "other.txt", (2, 0)));

                    keys(workspace, &["`", "shift-a"], window, cx)?;
                    ensure!(at(workspace, "README.md", (1, 2)));
                    press(workspace, "ctrl-o", window, cx)?;
                    ensure!(at(workspace, "other.txt", (2, 0)));
                    press(workspace, "ctrl-i", window, cx)?;
                    ensure!(at(workspace, "README.md", (1, 2)));
                    keys(workspace, &["2", "ctrl-o"], window, cx)?;
                    ensure!(at(workspace, "other.txt", (0, 0)));
                    keys(workspace, &["2", "ctrl-i"], window, cx)?;
                    ensure!(at(workspace, "README.md", (1, 2)));

                    press(workspace, ":", window, cx)?;
                    workspace.replace_text_in_range(None, "jumps", window, cx);
                    press(workspace, "enter", window, cx)?;
                    let project = workspace.projects.active();
                    ensure!(
                        project.split_count() == 2,
                        "opening the jump list: {} {:?}",
                        project.status,
                        project.editor.vim().message
                    );
                    ensure!(
                        workspace
                            .projects
                            .active()
                            .editor
                            .vim()
                            .doc
                            .text
                            .contains("other.txt")
                    );
                    keys(workspace, &["ctrl-w", "q"], window, cx)?;
                    ensure!(at(workspace, "README.md", (1, 2)));
                    press(workspace, ":", window, cx)?;
                    workspace.replace_text_in_range(None, "clearjumps", window, cx);
                    keys(workspace, &["enter", "ctrl-o"], window, cx)?;
                    ensure!(at(workspace, "README.md", (1, 2)));
                    Ok(())
                })
            })?
        },
    )
}

/// Verify Leader hints, native split placement and buffer operations scoped to the focused pane.
#[test]
#[ignore = "requires the Windows DirectWrite and DirectX renderer"]
fn native_leader_manages_buffers_and_editor_windows() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    fs::create_dir(&project)?;
    let first = project.join("README.md");
    let second = project.join("second.txt");
    let third = project.join("third.txt");
    fs::write(&first, "first window\n")?;
    fs::write(&second, "second buffer\n")?;
    fs::write(&third, "third buffer\n")?;
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
                window.resize(size(px(920.), px(580.)))
            })?;
            app.background_executor()
                .timer(Duration::from_millis(60))
                .await;
            app.update(|app| {
                handle.update(app, |workspace, window, cx| -> Result<()> {
                    workspace.sync(window, cx);
                    let original_area = workspace.active_editor_area();
                    let original_pane = workspace.projects.active().active_editor_id();
                    let original_buffer = workspace.projects.active().editor.buffer().id;

                    press_hidden(workspace, "space", window, cx)?;
                    let hint = workspace.active_sequence_hint().context("Leader root")?;
                    for (key, group) in [("b", true), ("w", true), ("-", false), ("|", false)] {
                        ensure!(hint.rows().iter().any(|row| row.0 == key && row.2 == group));
                    }
                    press(workspace, "-", window, cx)?;
                    let below = workspace.active_editor_area();
                    ensure!(below.y > original_area.y && below.w == original_area.w);

                    keys(workspace, &["space", "shift-\\"], window, cx)?;
                    let right = workspace.active_editor_area();
                    ensure!(right.x > below.x && right.y == below.y);
                    let active_pane = workspace.projects.active().active_editor_id();
                    ensure!(workspace.projects.active().split_count() == 3);

                    press_hidden(workspace, "space", window, cx)?;
                    press_hidden(workspace, "w", window, cx)?;
                    let hint = workspace.active_sequence_hint().context("Window group")?;
                    for key in ["d", "m", "h", "j", "k", "l", "="] {
                        ensure!(hint.rows().iter().any(|row| row.0 == key && !row.2));
                    }
                    press(workspace, "m", window, cx)?;
                    ensure!(workspace.editor_rects().len() == 1);
                    ensure!(workspace.projects.active().active_editor_id() == active_pane);
                    keys(workspace, &["space", "w", "m"], window, cx)?;
                    ensure!(workspace.editor_rects().len() == 3);
                    keys(workspace, &["space", "w", "h"], window, cx)?;
                    ensure!(workspace.active_editor_area().x == below.x);

                    for key in ["space", "ctrl-w", "]"] {
                        press_hidden(workspace, key, window, cx)?;
                        let hint = workspace.active_sequence_hint().context("split hint")?;
                        let area = workspace.sequence_hint_layout(&hint).area;
                        ensure!(area.x + area.w == workspace.layout.visible_cols);
                        ensure!(
                            area.y + area.h
                                == workspace
                                    .layout
                                    .visible_rows
                                    .min(workspace.layout.rows - FOOTER_ROWS)
                        );
                        press(workspace, "escape", window, cx)?;
                    }

                    keys(workspace, &["space", "w", "l"], window, cx)?;
                    ensure!(workspace.projects.active().active_editor_id() == active_pane);

                    let now = workspace.scroll_epoch.elapsed().as_secs_f64();
                    let editor = &mut workspace.projects.active_mut().editor;
                    editor.open(second, now)?;
                    let second_buffer = editor.buffer().id;
                    editor.open(third, now)?;
                    let third_buffer = editor.buffer().id;
                    workspace.sync(window, cx);

                    press_hidden(workspace, "space", window, cx)?;
                    press_hidden(workspace, "b", window, cx)?;
                    let hint = workspace.active_sequence_hint().context("Buffer group")?;
                    for key in ["b", "d", "o", "l", "r", "D"] {
                        ensure!(hint.rows().iter().any(|row| row.0 == key && !row.2));
                    }
                    ensure!(!workspace.accepts_native_text());
                    press(workspace, "b", window, cx)?;
                    ensure!(workspace.projects.active().editor.buffer().id == second_buffer);
                    press(workspace, "i", window, cx)?;
                    ensure!(workspace.accepts_native_text());
                    workspace.replace_text_in_range(None, "中", window, cx);
                    keys(workspace, &["escape", "space", "b", "d"], window, cx)?;
                    ensure!(workspace.editor_close.is_some());
                    press(workspace, "escape", window, cx)?;
                    ensure!(
                        workspace
                            .projects
                            .active()
                            .editor
                            .vim()
                            .doc
                            .text
                            .starts_with('中')
                    );

                    keys(workspace, &["space", "`"], window, cx)?;
                    ensure!(workspace.projects.active().editor.buffer().id == third_buffer);
                    keys(workspace, &["space", "b", "o"], window, cx)?;
                    ensure!(workspace.editor_close.is_some());
                    press(workspace, "escape", window, cx)?;
                    ensure!(workspace.projects.active().editor.buffers().len() == 3);
                    keys(
                        workspace,
                        &["space", "b", "b", "u", "space", "b", "o"],
                        window,
                        cx,
                    )?;

                    let project = workspace.projects.active();
                    ensure!(workspace.editor_close.is_none());
                    ensure!(project.active_editor_id() == active_pane);
                    ensure!(project.editor.buffer().id == second_buffer);
                    ensure!(project.editor.buffers().len() == 1);
                    ensure!(
                        project
                            .editor_by_id(original_pane)
                            .context("original pane")?
                            .buffer()
                            .id
                            == original_buffer
                    );
                    keys(workspace, &["space", "b", "d"], window, cx)?;
                    ensure!(workspace.projects.active().split_count() == 3);
                    ensure!(workspace.projects.active().editor.vim().doc.path.is_none());
                    keys(workspace, &["space", "b", "shift-d"], window, cx)?;
                    ensure!(workspace.projects.active().split_count() == 2);
                    keys(workspace, &["space", "w", "d"], window, cx)?;
                    ensure!(workspace.projects.active().split_count() == 1);
                    Ok(())
                })
            })??;
            Ok(())
        },
    )
}
