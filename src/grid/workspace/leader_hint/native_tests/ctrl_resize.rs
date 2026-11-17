//! Exercise LazyVim-style pane resizing through the native workspace dispatcher.

use super::{Focus, Result, Workspace, fs, native_test, press};
use crate::grid::render::Rect;
use anyhow::{Context as _, ensure};
use gpui::{AppContext, px, size};
use nanobug::plugins::session::Options;
use std::time::Duration;

/// Return the visible rectangle of editor pane `id` on the active page.
fn pane_area(workspace: &Workspace, id: usize) -> Result<Rect> {
    workspace
        .editor_rects()
        .into_iter()
        .find_map(|(pane, area)| (pane == id).then_some(area))
        .with_context(|| format!("visible editor pane {id}"))
}

/// Ctrl+arrows change the focused pane's size by two cells when a divider allows it.
#[test]
#[ignore = "requires the Windows DirectWrite and DirectX renderer"]
fn native_ctrl_arrows_resize_the_focused_pane_like_lazyvim() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    fs::create_dir(&project)?;
    fs::write(project.join("README.md"), "# Split resize\n")?;
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
                    let left = workspace.projects.active().active_editor_id();
                    let explorer = workspace.layout.tree;
                    let editor = workspace.active_editor_area();

                    press(workspace, "ctrl-up", window, cx)?;
                    ensure!(workspace.active_editor_area() == editor);
                    press(workspace, "ctrl-right", window, cx)?;
                    let editor_grown = workspace.active_editor_area();
                    ensure!(workspace.layout.tree.w + 2 == explorer.w);
                    ensure!(editor_grown.x + 2 == editor.x && editor_grown.w == editor.w + 2);
                    ensure!(workspace.projects.active().active_editor_id() == left);
                    press(workspace, "ctrl-left", window, cx)?;
                    ensure!(workspace.layout.tree == explorer);
                    ensure!(workspace.active_editor_area() == editor);

                    press(workspace, "ctrl-h", window, cx)?;
                    ensure!(workspace.projects.active().focus == Focus::Manager);
                    press(workspace, "ctrl-left", window, cx)?;
                    ensure!(workspace.layout.tree.w + 2 == explorer.w);
                    ensure!(workspace.active_editor_area() == editor_grown);
                    press(workspace, "ctrl-right", window, cx)?;
                    ensure!(
                        workspace.layout.tree == explorer
                            && workspace.active_editor_area() == editor
                    );
                    ensure!(workspace.projects.active().focus == Focus::Manager);
                    press(workspace, "ctrl-l", window, cx)?;

                    press(workspace, "space", window, cx)?;
                    press(workspace, "shift-\\", window, cx)?;
                    let right = workspace.projects.active().active_editor_id();
                    ensure!(right != left);
                    let right_before = pane_area(workspace, right)?;
                    let left_before = pane_area(workspace, left)?;

                    press(workspace, "ctrl-right", window, cx)?;
                    let right_grown = pane_area(workspace, right)?;
                    ensure!(
                        right_grown.x + 2 == right_before.x && right_grown.w == right_before.w + 2
                    );
                    ensure!(pane_area(workspace, left)?.w + 2 == left_before.w);
                    ensure!(workspace.projects.active().active_editor_id() == right);
                    press(workspace, "ctrl-left", window, cx)?;
                    ensure!(pane_area(workspace, right)? == right_before);
                    ensure!(pane_area(workspace, left)? == left_before);

                    press(workspace, "ctrl-h", window, cx)?;
                    press(workspace, "ctrl-left", window, cx)?;
                    let left_shrunk = pane_area(workspace, left)?;
                    ensure!(left_shrunk.x == left_before.x && left_shrunk.w + 2 == left_before.w);
                    ensure!(pane_area(workspace, right)? == right_grown);
                    press(workspace, "ctrl-right", window, cx)?;
                    ensure!(
                        pane_area(workspace, left)? == left_before
                            && pane_area(workspace, right)? == right_before
                    );
                    ensure!(workspace.projects.active().active_editor_id() == left);
                    press(workspace, "ctrl-l", window, cx)?;

                    press(workspace, "space", window, cx)?;
                    press(workspace, "-", window, cx)?;
                    let lower = workspace.projects.active().active_editor_id();
                    ensure!(lower != right);
                    let lower_before = pane_area(workspace, lower)?;
                    let upper_before = pane_area(workspace, right)?;

                    press(workspace, "ctrl-up", window, cx)?;
                    let lower_grown = pane_area(workspace, lower)?;
                    ensure!(
                        lower_grown.y + 2 == lower_before.y && lower_grown.h == lower_before.h + 2
                    );
                    ensure!(pane_area(workspace, right)?.h + 2 == upper_before.h);
                    ensure!(pane_area(workspace, left)? == left_before);
                    ensure!(workspace.projects.active().active_editor_id() == lower);
                    press(workspace, "ctrl-down", window, cx)?;
                    ensure!(pane_area(workspace, lower)? == lower_before);
                    ensure!(pane_area(workspace, right)? == upper_before);

                    press(workspace, "ctrl-k", window, cx)?;
                    press(workspace, "ctrl-up", window, cx)?;
                    let upper_grown = pane_area(workspace, right)?;
                    ensure!(upper_grown.y == upper_before.y && upper_grown.h == upper_before.h + 2);
                    ensure!(pane_area(workspace, lower)?.h + 2 == lower_before.h);
                    press(workspace, "ctrl-down", window, cx)?;
                    ensure!(
                        pane_area(workspace, right)? == upper_before
                            && pane_area(workspace, lower)? == lower_before
                    );
                    ensure!(workspace.projects.active().active_editor_id() == right);
                    press(workspace, "ctrl-j", window, cx)?;

                    // The opposite pane's minimum stops growth without changing focus or width.
                    for _ in 0..upper_before.h {
                        press(workspace, "ctrl-up", window, cx)?;
                    }
                    let upper_minimum = pane_area(workspace, right)?;
                    let lower_maximum = pane_area(workspace, lower)?;
                    ensure!(upper_minimum.h > 0 && upper_minimum.h < upper_before.h);
                    press(workspace, "ctrl-up", window, cx)?;
                    ensure!(
                        pane_area(workspace, right)? == upper_minimum
                            && pane_area(workspace, lower)? == lower_maximum
                    );
                    ensure!(
                        pane_area(workspace, left)? == left_before
                            && workspace.layout.tree == explorer
                    );
                    ensure!(workspace.projects.active().active_editor_id() == lower);

                    // Resizing the lower pane's width uses the enclosing column divider.
                    press(workspace, "ctrl-right", window, cx)?;
                    let lower_widened = pane_area(workspace, lower)?;
                    let upper_widened = pane_area(workspace, right)?;
                    ensure!(lower_widened.w == lower_maximum.w + 2);
                    ensure!(upper_widened.w == upper_minimum.w + 2);
                    ensure!(lower_widened.h == lower_maximum.h);
                    ensure!(upper_widened.h == upper_minimum.h);
                    ensure!(pane_area(workspace, left)? == left_shrunk);
                    ensure!(workspace.layout.tree == explorer);
                    Ok(())
                })
            })??;
            Ok(())
        },
    )
}
