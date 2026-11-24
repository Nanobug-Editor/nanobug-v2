//! Timer and drawing integration for transient picker feedback.

use super::*;
use crate::grid::workspace::native_test;
use anyhow::{Context as _, Result, ensure};
use nanobug::plugins::session::Options;
use std::time::Duration;

/// A blank message clears the old timer and the replacement stays up until its own deadline.
#[test]
#[ignore = "requires the Windows native renderer"]
fn native_picker_toast_replacement_has_its_own_deadline() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    fs::create_dir(&project)?;
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
        |_| {},
        async |handle, app| {
            app.update(|app| {
                handle.update(app, |workspace, window, cx| {
                    workspace.open_file_picker(window, cx);
                    workspace.show_picker_message(MessageLevel::Info, "First".into(), window, cx);
                })
            })?;
            app.background_executor()
                .timer(Duration::from_millis(1100))
                .await;
            app.update(|app| {
                handle.update(app, |workspace, window, cx| -> Result<()> {
                    workspace.show_picker_message(MessageLevel::Warning, "   ".into(), window, cx);
                    ensure!(workspace.file_picker.as_ref().unwrap().message.is_none());
                    workspace.show_picker_message(
                        MessageLevel::Error,
                        "确认\n第二条消息".into(),
                        window,
                        cx,
                    );
                    let message = &workspace
                        .file_picker
                        .as_ref()
                        .unwrap()
                        .message
                        .as_ref()
                        .context("replacement toast")?
                        .toast;
                    let viewport = Rect {
                        x: 3,
                        y: 7,
                        w: 60,
                        h: 10,
                    };
                    let (area, lines) = message.layout(viewport, 54, &workspace.spacing);
                    ensure!(lines.as_slice() == ["确认 第二条消息"]);
                    ensure!(
                        area.x + area.w == viewport.x + viewport.w - 1 && area.y == viewport.y + 1
                    );
                    let (tiny, _) =
                        message.layout(Rect { h: 1, ..viewport }, 2, &workspace.spacing);
                    ensure!(tiny.w == 2 && tiny.h == 1 && tiny.y == viewport.y);
                    Ok(())
                })
            })??;
            app.background_executor()
                .timer(Duration::from_millis(1100))
                .await;
            app.update(|app| {
                handle.update(app, |workspace, _, _| -> Result<()> {
                    let message = workspace
                        .file_picker
                        .as_ref()
                        .unwrap()
                        .message
                        .as_ref()
                        .context("first timer dismissed replacement")?;
                    ensure!(message.toast.level == MessageLevel::Error);
                    Ok(())
                })
            })??;
            app.background_executor()
                .timer(Duration::from_millis(1100))
                .await;
            app.update(|app| {
                handle.update(app, |workspace, _, _| -> Result<()> {
                    ensure!(
                        workspace.file_picker.as_ref().unwrap().message.is_none(),
                        "replacement did not expire"
                    );
                    Ok(())
                })
            })??;
            Ok(())
        },
    )
}
