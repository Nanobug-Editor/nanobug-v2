//! Verify that editable Vim history owns the native sequence hint while it is open.

use super::super::HintControl;
use super::*;
use crate::grid::render::{Metrics, Rect};
use gpui::EntityInputHandler;
use nanobug::i18n::{self, Locale};

/// Locate image pixels in the grid `area` using the window's `metrics` and device `scale`.
fn grid_pixels(
    frame: &image::RgbaImage,
    area: Rect,
    metrics: Metrics,
    scale: f32,
) -> impl Iterator<Item = (u32, u32)> {
    let start = metrics.at(area.x as f32, area.y as f32);
    let end = metrics.at((area.x + area.w) as f32, (area.y + area.h) as f32);
    let device = |coordinate| (f32::from(coordinate) * scale).round() as u32;
    let left = device(start.x);
    let right = device(end.x).min(frame.width());
    let bottom = device(end.y).min(frame.height());
    (device(start.y)..bottom).flat_map(move |y| (left..right).map(move |x| (x, y)))
}

/// Open each history with native keys and check built-in and custom delayed continuations.
#[test]
#[ignore = "requires the Windows DirectWrite and DirectX renderer"]
fn native_history_windows_show_delayed_sequence_hints() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    let source = "first cat\nsecond cat\nthird dog\n";
    fs::create_dir(&project)?;
    fs::write(project.join("README.md"), source)?;
    let options = Options::take(&mut vec!["--no-plugins".into()], temp.path())?;
    native_test::run_async(
        project,
        options,
        |workspace| workspace.projects.active_mut().focus_pane(Focus::Editor),
        async move |handle, app| {
            let _keep_temp = temp;
            app.update_window(handle.into(), |_, window, _| {
                window.resize(size(px(920.), px(580.)))
            })?;
            app.background_executor()
                .timer(Duration::from_millis(60))
                .await;
            let source_caret = handle.update(app, |workspace, window, cx| -> Result<_> {
                workspace.sync(window, cx);
                for (prompt, text) in [
                    (":", "nnoremap zx l"),
                    (":", "echo 'history'"),
                    ("/", "cat"),
                    ("?", "dog"),
                ] {
                    press(workspace, prompt, window, cx)?;
                    workspace.replace_text_in_range(None, text, window, cx);
                    press(workspace, "enter", window, cx)?;
                }
                Ok(workspace.projects.active().editor.vim().doc.cursor)
            })??;

            for (key, prompt) in [(":", ':'), ("/", '/'), ("?", '?')] {
                handle.update(app, |workspace, window, cx| -> Result<()> {
                    press_hidden(workspace, "q", window, cx)?;
                    press(workspace, key, window, cx)?;
                    let history = workspace
                        .command_window
                        .as_ref()
                        .context("history window")?;
                    ensure!(history.prompt == prompt);
                    ensure!(workspace.sequence_hint_area().is_none());

                    for key in ["k", "0", "z", "x"] {
                        press(workspace, key, window, cx)?;
                    }
                    let history = workspace
                        .command_window
                        .as_ref()
                        .context("history window")?;
                    ensure!(history.selected().1 == 1, "fast zx in {key} history");
                    ensure!(workspace.sequence_hint_area().is_none());
                    Ok(())
                })??;
                app.background_executor().timer(HINT_SETTLE).await;

                handle.update(app, |workspace, window, cx| -> Result<()> {
                    ensure!(workspace.sequence_hint_area().is_none(), "stale zx hint");
                    press(workspace, "0", window, cx)
                })??;
                let before = if prompt == ':' {
                    Some(app.update_window(handle.into(), |_, window, cx| {
                        window.draw(cx).clear(cx);
                        window.render_to_image()
                    })??)
                } else {
                    None
                };
                handle.update(app, |workspace, window, cx| {
                    press_hidden(workspace, "z", window, cx)
                })??;
                app.background_executor().timer(HINT_SETTLE).await;

                let (hint_area, history_area, metrics, scale) =
                    handle.update(app, |workspace, window, _| -> Result<_> {
                        let hint = workspace
                            .visible_sequence_hint()
                            .context("history's custom mapping hint")?;
                        ensure!(hint.rows() == [("x", "l", false)]);
                        let original_locale = i18n::locale();
                        i18n::set_locale(Locale::English);
                        let english = hint.title();
                        i18n::set_locale(Locale::SimplifiedChinese);
                        let chinese = hint.title();
                        i18n::set_locale(original_locale);
                        ensure!(english == "z[+mapping]" && chinese == "z[+映射]");
                        let hint_area = workspace.sequence_hint_area().context("hint area")?;
                        let history_area =
                            workspace.command_window_area().context("history area")?;
                        ensure!(hint_area.x + hint_area.w == history_area.x + history_area.w);
                        ensure!(hint_area.y + hint_area.h <= history_area.y);
                        Ok((
                            hint_area,
                            history_area,
                            workspace.metrics,
                            window.scale_factor(),
                        ))
                    })??;
                if let Some(before) = &before {
                    let after = app.update_window(handle.into(), |_, window, cx| {
                        window.draw(cx).clear(cx);
                        window.render_to_image()
                    })??;
                    ensure!(before.dimensions() == after.dimensions());
                    let changed = grid_pixels(&after, hint_area, metrics, scale)
                        .filter(|&(x, y)| before.get_pixel(x, y) != after.get_pixel(x, y))
                        .count();
                    ensure!(changed > 100, "hint did not paint above history");
                    let history_header = Rect {
                        w: 20,
                        h: 1,
                        ..history_area
                    };
                    // The history caption should have visible glyph edges in both frames.
                    for frame in [before, &after] {
                        let edges = grid_pixels(frame, history_header, metrics, scale)
                            .filter(|&(x, y)| {
                                x > 0
                                    && frame.get_pixel(x, y).0[..3]
                                        .iter()
                                        .zip(&frame.get_pixel(x - 1, y).0[..3])
                                        .any(|(a, b)| a.abs_diff(*b) > 32)
                            })
                            .count();
                        ensure!(edges > 20, "history caption disappeared");
                    }
                }

                handle.update(app, |workspace, window, cx| -> Result<()> {
                    press(workspace, "x", window, cx)?;
                    let history = workspace
                        .command_window
                        .as_ref()
                        .context("history window")?;
                    ensure!(history.selected().1 == 1);
                    ensure!(workspace.sequence_hint_area().is_none());
                    press_hidden(workspace, "g", window, cx)
                })??;
                app.background_executor().timer(HINT_SETTLE).await;

                handle.update(app, |workspace, window, cx| -> Result<()> {
                    let hint = workspace
                        .visible_sequence_hint()
                        .context("history's built-in g hint")?;
                    ensure!(
                        hint.rows()
                            .contains(&("g", i18n::text("vim_hint.first_line"), false,))
                    );
                    press(workspace, "g", window, cx)?;
                    let history = workspace
                        .command_window
                        .as_ref()
                        .context("history window")?;
                    ensure!(history.editor.buffer().cursor_row() == 0);
                    ensure!(workspace.sequence_hint_area().is_none());
                    press_hidden(workspace, "ctrl-w", window, cx)
                })??;
                app.background_executor().timer(HINT_SETTLE).await;

                handle.update(app, |workspace, window, cx| -> Result<()> {
                    let hint = workspace
                        .visible_sequence_hint()
                        .context("history's Ctrl+w hint")?;
                    ensure!(hint.rows() == [("c q", i18n::text("vim_window.close"), false)]);
                    workspace.apply_hint_control(HintControl::Close, cx);
                    let history = workspace
                        .command_window
                        .as_ref()
                        .context("history window")?;
                    ensure!(!history.window_prefix);
                    ensure!(workspace.sequence_hint_area().is_none());
                    press_hidden(workspace, "ctrl-w", window, cx)?;
                    press(workspace, "escape", window, cx)?;
                    ensure!(workspace.command_window.is_some());
                    ensure!(workspace.active_sequence_hint().is_none());
                    for key in ["ctrl-w", "c"] {
                        press(workspace, key, window, cx)?;
                    }
                    ensure!(workspace.command_window.is_none());
                    let doc = &workspace.projects.active().editor.vim().doc;
                    ensure!(doc.cursor == source_caret && doc.text == source);
                    Ok(())
                })??;
            }
            Ok(())
        },
    )
}

/// Keep the q prefix and its macro register functional before and after its hint appears.
#[test]
#[ignore = "requires the Windows DirectWrite and DirectX renderer"]
fn native_history_hint_preserves_q_macro_recording() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    fs::create_dir(&project)?;
    fs::write(project.join("README.md"), "abcdef\n")?;
    let options = Options::take(&mut vec!["--no-plugins".into()], temp.path())?;
    native_test::run_async(
        project,
        options,
        |workspace| workspace.projects.active_mut().focus_pane(Focus::Editor),
        async move |handle, app| {
            let _keep_temp = temp;
            app.update_window(handle.into(), |_, window, _| {
                window.resize(size(px(920.), px(580.)))
            })?;
            app.background_executor()
                .timer(Duration::from_millis(60))
                .await;
            handle.update(app, |workspace, window, cx| {
                workspace.sync(window, cx);
                press_hidden(workspace, "q", window, cx)
            })??;
            app.background_executor().timer(HINT_SETTLE).await;
            handle.update(app, |workspace, window, cx| -> Result<()> {
                let hint = workspace.visible_sequence_hint().context("q hint")?;
                ensure!(
                    hint.rows().iter().any(|(_, action, _)| {
                        *action == i18n::text("vim_hint.record_register")
                    })
                );
                press(workspace, "a", window, cx)?;
                ensure!(
                    workspace
                        .projects
                        .active()
                        .editor
                        .register()
                        .is_recording_macro()
                );
                ensure!(workspace.sequence_hint_area().is_none());
                for key in ["l", "q"] {
                    press(workspace, key, window, cx)?;
                }
                ensure!(
                    !workspace
                        .projects
                        .active()
                        .editor
                        .register()
                        .is_recording_macro()
                );
                ensure!(workspace.projects.active().editor.vim().doc.cursor == 1);
                ensure!(workspace.active_sequence_hint().is_none());
                ensure!(workspace.command_window.is_none());

                for key in ["@", "a"] {
                    press(workspace, key, window, cx)?;
                }
                ensure!(workspace.projects.active().editor.vim().doc.cursor == 2);
                press(workspace, ":", window, cx)?;
                workspace.replace_text_in_range(None, "echo 'history'", window, cx);
                press(workspace, "enter", window, cx)?;
                for key in ["q", ":", "k", "0"] {
                    press(workspace, key, window, cx)?;
                }
                let history = workspace
                    .command_window
                    .as_ref()
                    .context("history window")?;
                ensure!(history.selected().0 == "echo 'history'");
                press_hidden(workspace, "q", window, cx)
            })??;
            app.background_executor().timer(HINT_SETTLE).await;
            handle.update(app, |workspace, window, cx| -> Result<()> {
                let hint = workspace
                    .visible_sequence_hint()
                    .context("history's q hint")?;
                let rows = hint.rows();
                ensure!(rows.len() == 1, "unexpected scratch q hint rows: {rows:?}");
                ensure!(rows[0].1 == i18n::text("vim_hint.record_register"));
                for key in ["a", "x", "q", "@", "a"] {
                    press(workspace, key, window, cx)?;
                }
                let history = workspace
                    .command_window
                    .as_ref()
                    .context("history window")?;
                ensure!(history.selected().0 == "ho 'history'");
                ensure!(!history.editor.register().is_recording_macro());
                for key in ["ctrl-w", "c", "@", "a"] {
                    press(workspace, key, window, cx)?;
                }
                let doc = &workspace.projects.active().editor.vim().doc;
                ensure!(doc.cursor == 3 && doc.text == "abcdef\n");
                Ok(())
            })?
        },
    )
}
