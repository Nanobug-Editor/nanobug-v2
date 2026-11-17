//! Verify directional shortcuts move the caret between editor splits.

use super::{Focus, Result, Workspace, fs, native_test, press};
use anyhow::ensure;
use gpui::{AppContext, px, size};
use nanobug::{document::Document, plugins::session::Options};
use std::time::Duration;

/// Check that the visible text caret belongs to the expected editor pane `id`.
fn assert_caret_in_pane(workspace: &Workspace, id: usize) -> Result<()> {
    let project = workspace.projects.active();
    ensure!(project.focus == Focus::Editor && project.active_editor_id() == id);
    let area = workspace.active_editor_area();
    let (x, y, _, _, visible) = workspace.caret();
    ensure!(
        visible && area.contains(x.floor() as usize, y.floor() as usize),
        "caret ({x}, {y}) must be inside focused editor pane {id}"
    );
    Ok(())
}

/// Route bare Ctrl directions across both axes in a project with the Explorer visible.
#[test]
#[ignore = "requires the Windows DirectWrite and DirectX renderer"]
fn native_ctrl_directions_focus_new_and_old_editor_splits() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    fs::create_dir(&project)?;
    fs::write(
        project.join("README.md"),
        "# Split focus\n\nOld and new views.\n",
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
                    workspace
                        .projects
                        .active_mut()
                        .editor
                        .retain_document(Document::new("Original pane's other buffer"));
                    workspace.sync(window, cx);
                    let original = workspace.projects.active().active_editor_id();
                    let original_area = workspace.active_editor_area();
                    press(workspace, "space", window, cx)?;
                    press(workspace, "shift-\\", window, cx)?;
                    let right = workspace.projects.active().active_editor_id();
                    let right_area = workspace.active_editor_area();
                    ensure!(right != original && right_area.x > original_area.x);
                    assert_caret_in_pane(workspace, right)?;
                    ensure!(workspace.projects.active().editor.buffers().len() == 1);

                    press(workspace, "ctrl-h", window, cx)?;
                    assert_caret_in_pane(workspace, original)?;
                    ensure!(workspace.projects.active().editor.buffers().len() == 2);
                    press(workspace, "ctrl-h", window, cx)?;
                    ensure!(workspace.projects.active().focus == Focus::Manager);
                    press(workspace, "ctrl-l", window, cx)?;
                    assert_caret_in_pane(workspace, original)?;
                    press(workspace, "ctrl-l", window, cx)?;
                    assert_caret_in_pane(workspace, right)?;

                    press(workspace, "space", window, cx)?;
                    press(workspace, "-", window, cx)?;
                    let lower = workspace.projects.active().active_editor_id();
                    ensure!(lower != right && workspace.active_editor_area().y > right_area.y);
                    assert_caret_in_pane(workspace, lower)?;

                    press(workspace, "ctrl-k", window, cx)?;
                    assert_caret_in_pane(workspace, right)?;
                    press(workspace, "ctrl-j", window, cx)?;
                    assert_caret_in_pane(workspace, lower)?;
                    Ok(())
                })
            })??;
            Ok(())
        },
    )
}
