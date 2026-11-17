//! Native integration checks for title-close routing and picker shortcuts.

use super::*;
use anyhow::ensure;
use gpui::{KeyDownEvent, Keystroke};
use nanobug::plugins::session::Options;
use std::time::Duration;

/// Drive the picker, both help titles and Explorer through the workspace's input handlers.
#[test]
#[ignore = "requires Windows DirectWrite and DirectX"]
fn native_middle_click_closes_the_hit_grid_title() -> anyhow::Result<()> {
    let project = tempfile::tempdir()?;
    let options = Options::take(
        &mut vec![
            "--plugin-profile".into(),
            project.path().join("profile").into_os_string(),
            "--no-plugins".into(),
        ],
        project.path(),
    )?;
    super::super::native_test::run_async(
        project.path().to_owned(),
        options,
        |_| {},
        async move |handle, app| {
            let _keep_project = project;
            let key = |workspace: &mut Workspace,
                       key: &'static str,
                       window: &mut Window,
                       cx: &mut Context<Workspace>|
             -> anyhow::Result<()> {
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
            };
            let middle = |workspace: &mut Workspace,
                          position: Point<Pixels>,
                          window: &mut Window,
                          cx: &mut Context<Workspace>| {
                workspace.middle_mouse_down(
                    &MouseDownEvent {
                        button: MouseButton::Middle,
                        position,
                        click_count: 1,
                        ..Default::default()
                    },
                    window,
                    cx,
                );
            };
            app.update(|app| {
                handle.update(app, |workspace, window, cx| {
                    workspace.sync(window, cx);
                    ensure!(workspace.projects.active().manager.is_some());
                    key(workspace, "space", window, cx)?;
                    key(workspace, "shift-e", window, cx)?;
                    ensure!(workspace.file_picker.is_some());
                    Ok(())
                })?
            })?;
            let mut ready = false;
            for _ in 0..50 {
                ready = app.update(|app| {
                    handle.update(app, |workspace, _, _| {
                        workspace
                            .file_picker
                            .as_ref()
                            .is_some_and(|session| session.state.directory().is_some())
                    })
                })?;
                if ready {
                    break;
                }
                app.background_executor()
                    .timer(Duration::from_millis(20))
                    .await;
            }
            ensure!(ready, "the picker did not load its initial directory");
            app.update(|app| {
                handle.update(app, |workspace, window, cx| {
                    let picker = workspace.file_picker_layout().area;
                    let title = |workspace: &Workspace, area: Rect| {
                        workspace.metrics.at(
                            area.x as f32 + area.border_title_columns(None).start as f32 + 0.5,
                            area.y as f32 + 0.5,
                        )
                    };
                    let right = workspace
                        .metrics
                        .at((picker.x + picker.w - 3) as f32, picker.y as f32 + 0.5);
                    ensure!(workspace.corner_controls_at(right).is_none());
                    middle(workspace, right, window, cx);
                    ensure!(workspace.file_picker.is_some());

                    key(workspace, "space", window, cx)?;
                    key(workspace, "shift-/", window, cx)?;
                    ensure!(workspace.help.is_open());
                    let help_titles = |workspace: &Workspace| {
                        workspace.help.list.as_ref().unwrap().tab_targets(
                            workspace.help_layout(),
                            workspace.metrics,
                            &workspace.spacing,
                            false,
                        )
                    };
                    let [(_, first), (_, last)] = help_titles(workspace);
                    ensure!(!first.is_empty() && !last.is_empty());
                    middle(workspace, first.center(), window, cx);
                    ensure!(workspace.help.is_open());
                    let [(_, first), (_, last)] = help_titles(workspace);
                    ensure!(first.is_empty() && !last.is_empty());
                    middle(workspace, last.center(), window, cx);
                    ensure!(!workspace.help.is_open() && workspace.file_picker.is_some());

                    middle(workspace, title(workspace, picker), window, cx);
                    ensure!(workspace.file_picker.is_none());
                    let explorer = workspace.layout.tree;
                    middle(workspace, title(workspace, explorer), window, cx);
                    ensure!(workspace.projects.active().hidden_pane == Some(Focus::Manager));
                    Ok(())
                })?
            })?;
            Ok(())
        },
    )
}
