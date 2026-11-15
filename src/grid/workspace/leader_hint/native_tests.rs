//! Exercise the sequence popup through the native workspace keyboard dispatcher.

mod markdown;

use super::super::{Workspace, layout::Focus, native_test};
use crate::grid::windows::editor::MarkdownViewMode;
use anyhow::{Context as _, Result, ensure};
use gpui::{AppContext, Context, KeyDownEvent, Keystroke, Window, px, size};
use nanobug::plugins::session::Options;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{fs, time::Duration};
use windows_sys::Win32::{
    Foundation::RECT,
    UI::WindowsAndMessaging::{GetWindowRect, SendMessageW, WM_DPICHANGED},
};

/// Give the delayed hint time to open and publish its updated frame.
const HINT_SETTLE: Duration = Duration::from_millis(220);

/// Route `key` through `workspace` as a discrete press in its native `window`.
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

/// Start or extend a sequence with `key` while its hint is still delayed.
fn press_hidden(
    workspace: &mut Workspace,
    key: &'static str,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Result<()> {
    press(workspace, key, window, cx)?;
    ensure!(workspace.active_sequence_hint().is_some(), "{key} sequence");
    ensure!(
        workspace.sequence_hint_area().is_none(),
        "{key} hint opened immediately"
    );
    Ok(())
}

/// Set the hidden native window to `dpi` through the notification used for monitor changes.
fn change_dpi(window: &Window, dpi: usize) -> Result<()> {
    let RawWindowHandle::Win32(raw) = HasWindowHandle::window_handle(window)
        .ok()
        .context("native window handle")?
        .as_raw()
    else {
        anyhow::bail!("missing Win32 window handle");
    };
    let hwnd = raw.hwnd.get() as _;
    let mut suggested = RECT::default();
    // A changed size makes Windows publish the updated scale through GPUI.
    unsafe {
        ensure!(GetWindowRect(hwnd, &mut suggested) != 0);
        suggested.right += 1;
        SendMessageW(
            hwnd,
            WM_DPICHANGED,
            dpi | (dpi << 16),
            &suggested as *const RECT as isize,
        );
    }
    Ok(())
}

/// The native dispatcher changes Markdown views only while the Markdown editor owns input.
#[test]
#[ignore = "requires the Windows DirectWrite and DirectX renderer"]
fn native_markdown_leader_scopes_the_view_to_the_focused_editor() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    fs::create_dir(&project)?;
    let mut original = String::from(concat!(
        "# Markdown preview\n\n",
        "**Bold text**, <em>HTML emphasis</em>, and `inline code`.\n\n",
        "## Checklist\n\n",
        "- [x] GitHub-style task list\n",
        "- [ ] Another item\n\n",
        "| View | Key |\n| --- | --- |\n| Edit + Preview | `a` |\n| Preview only | `p` |\n\n",
        "> Preview content scrolls independently of the Markdown source.\n\n",
        "```rust\nlet preview = true;\n```\n\n",
    ));
    original.push_str(&"A paragraph below the first preview page.\n\n".repeat(50));
    fs::write(project.join("README.md"), &original)?;
    let plain_file = project.join("plain.txt");
    fs::write(&plain_file, "Plain text")?;
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
        |_| {},
        async move |handle, app| {
            let mode =
                |workspace: &Workspace| workspace.projects.active().editor.markdown_view_mode();
            let shows_markdown = |workspace: &Workspace| {
                workspace
                    .active_sequence_hint()
                    .is_some_and(|hint| hint.rows().iter().any(|(key, _, _)| *key == "m"))
            };
            app.update(|app| {
                handle.update(app, |workspace, window, cx| -> Result<()> {
                    workspace.sync(window, cx);
                    ensure!(mode(workspace) == Some(MarkdownViewMode::Split));
                    for key in ["space", "m", "e"] {
                        press(workspace, key, window, cx)?;
                    }
                    ensure!(mode(workspace) == Some(MarkdownViewMode::Edit));
                    press(workspace, "space", window, cx)?;
                    ensure!(shows_markdown(workspace));
                    press(workspace, "m", window, cx)?;
                    let hint = workspace.active_sequence_hint().context("Markdown hint")?;
                    ensure!(
                        hint.rows().iter().map(|row| row.0).collect::<Vec<_>>() == ["a", "e", "p"]
                    );
                    press(workspace, "a", window, cx)?;
                    ensure!(mode(workspace) == Some(MarkdownViewMode::Split));
                    Ok(())
                })
            })??;
            if let Some(capture) = std::env::var_os("NANOBUG_MARKDOWN_TEST_CAPTURE") {
                let split = app.update_window(handle.into(), |_, window, cx| {
                    window.draw(cx).clear(cx);
                    window.render_to_image()
                })??;
                split.save(capture)?;
            }
            app.update(|app| {
                handle.update(app, |workspace, window, cx| -> Result<()> {
                    for key in ["space", "m", "p"] {
                        press(workspace, key, window, cx)?;
                    }
                    ensure!(mode(workspace) == Some(MarkdownViewMode::Preview));
                    Ok(())
                })
            })??;
            app.update_window(handle.into(), |_, window, cx| window.draw(cx).clear(cx))?;
            app.update(|app| {
                handle.update(app, |workspace, window, cx| -> Result<()> {
                    let caret = workspace.projects.active().editor.vim().doc.cursor;
                    for key in ["i", "o", "x", "pagedown"] {
                        press(workspace, key, window, cx)?;
                    }
                    let editor = &workspace.projects.active().editor;
                    ensure!(editor.markdown_scroll_active(workspace.layout.editor));
                    ensure!(editor.vim().doc.cursor == caret);
                    for key in ["g", "g", "end", "home"] {
                        press(workspace, key, window, cx)?;
                    }
                    ensure!(!workspace.accepts_native_text());
                    ensure!(workspace.projects.active().editor.vim().doc.text == original);

                    for key in ["space", "m", "e", "ctrl-h"] {
                        press(workspace, key, window, cx)?;
                    }
                    ensure!(mode(workspace) == Some(MarkdownViewMode::Edit));
                    ensure!(workspace.projects.active().focus == Focus::Manager);
                    press(workspace, "space", window, cx)?;
                    ensure!(!shows_markdown(workspace));
                    press(workspace, "m", window, cx)?;
                    ensure!(workspace.active_sequence_hint().is_none());

                    press(workspace, "ctrl-l", window, cx)?;
                    let now = workspace.scroll_epoch.elapsed().as_secs_f64();
                    workspace
                        .projects
                        .active_mut()
                        .editor
                        .open(plain_file, now)?;
                    workspace.sync(window, cx);
                    ensure!(mode(workspace).is_none());
                    press(workspace, "space", window, cx)?;
                    ensure!(!shows_markdown(workspace));
                    press(workspace, "m", window, cx)?;
                    ensure!(workspace.active_sequence_hint().is_none());
                    Ok(())
                })
            })??;
            Ok(())
        },
    )
}

/// Showing the hint leaves the visible editor text at its original pixel positions.
#[test]
#[ignore = "requires the Windows DirectWrite and DirectX renderer"]
fn native_leader_hint_keeps_visible_editor_rows_in_place() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    fs::create_dir(&project)?;
    let text = (0..60)
        .map(|row| format!("Text at row {row:02} stays in place.\n"))
        .collect::<String>();
    fs::write(project.join("README.md"), text)?;
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
            let dpi = 144;
            app.update_window(handle.into(), |_, window, _| change_dpi(window, dpi))??;
            app.background_executor()
                .timer(Duration::from_millis(60))
                .await;
            app.update(|app| {
                handle.update(app, |workspace, window, cx| -> Result<()> {
                    let mut appearance = workspace.appearance;
                    // A 19-device-pixel row puts the independent clip and text origins
                    // on opposite sides of a half-pixel rounding boundary.
                    appearance.base_size = 9.4;
                    workspace.apply_appearance(appearance, window, cx);
                    ensure!((workspace.metrics.lh * window.scale_factor()).round() == 19.);
                    Ok(())
                })
            })??;
            let before = app.update_window(handle.into(), |_, window, cx| {
                window.draw(cx).clear(cx);
                window.render_to_image()
            })??;
            app.update(|app| {
                handle.update(app, |workspace, window, cx| {
                    press_hidden(workspace, "space", window, cx)
                })
            })??;
            app.background_executor().timer(HINT_SETTLE).await;
            let (scale, left, right, top, bottom) = app.update(|app| {
                handle.update(app, |workspace, window, _| -> Result<_> {
                    ensure!(window.scale_factor() == dpi as f32 / 96.);
                    let hint = workspace.sequence_hint_area().context("leader hint")?;
                    let metrics = workspace.metrics;
                    let left = workspace.layout.editor.x + 7;
                    let right = hint.x.saturating_sub(2).min(left + 23);
                    ensure!(right > left + 4, "hint leaves too little text visible");
                    let start = metrics.at(left as f32, hint.y as f32 + 1.);
                    let end = metrics.at(right as f32, (hint.y + hint.h - 1) as f32);
                    Ok((window.scale_factor(), start.x, end.x, start.y, end.y))
                })
            })??;
            let after = app.update_window(handle.into(), |_, window, cx| {
                window.draw(cx).clear(cx);
                window.render_to_image()
            })??;
            let device = |value| (f32::from(value) * scale).round() as u32;
            let (left, right, top, bottom) =
                (device(left), device(right), device(top), device(bottom));
            let background = before.get_pixel(left, top);
            let mut text_pixels = 0;
            let mut changed_pixels = 0;
            for y in top..bottom {
                for x in left..right {
                    let pixel = before.get_pixel(x, y);
                    text_pixels += usize::from(pixel != background);
                    changed_pixels += usize::from(pixel != after.get_pixel(x, y));
                }
            }
            ensure!(
                text_pixels > 100,
                "test did not capture visible editor text"
            );
            ensure!(
                changed_pixels == 0,
                "{changed_pixels} pixels changed to the left of the visible hint at scale {scale}"
            );
            Ok(())
        },
    )
}

/// A pause reveals the hint while fast or canceled sequences leave it hidden.
#[test]
#[ignore = "requires the Windows DirectWrite and DirectX renderer"]
fn native_key_sequences_update_the_workspace_hint() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    fs::create_dir(&project)?;
    let original = "alpha beta\ngamma delta\nteh\n";
    fs::write(project.join("README.md"), original)?;
    fs::write(project.join("second.txt"), "Explorer fixture")?;
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
                    ensure!(workspace.sequence_hint_area().is_none());

                    press(workspace, "shift-g", window, cx)?;
                    ensure!(workspace.projects.active().editor.vim().doc.cursor > 0);
                    press_hidden(workspace, "g", window, cx)?;
                    press(workspace, "g", window, cx)?;
                    ensure!(workspace.sequence_hint_area().is_none());
                    ensure!(workspace.projects.active().editor.vim().doc.cursor == 0);

                    press_hidden(workspace, "z", window, cx)?;
                    press(workspace, "z", window, cx)?;
                    ensure!(!workspace.projects.active().editor.vim().awaiting_key());
                    ensure!(workspace.sequence_hint_area().is_none());
                    Ok(())
                })
            })??;
            app.background_executor().timer(HINT_SETTLE).await;
            app.update(|app| {
                handle.update(app, |workspace, window, cx| -> Result<()> {
                    ensure!(
                        workspace.sequence_hint_area().is_none(),
                        "a completed zz reopened its hint"
                    );
                    press_hidden(workspace, "g", window, cx)
                })
            })??;
            app.background_executor().timer(HINT_SETTLE).await;
            let vim_hint = app.update(|app| {
                handle.update(app, |workspace, window, cx| -> Result<_> {
                    let vim_hint = workspace
                        .sequence_hint_area()
                        .context("g hint after pause")?;
                    press(workspace, "escape", window, cx)?;
                    ensure!(workspace.sequence_hint_area().is_none());

                    for key in ["2", "z", "shift-f"] {
                        press(workspace, key, window, cx)?;
                    }
                    ensure!(workspace.projects.active().editor.vim().doc.folded_at(0));
                    for key in ["z", "o"] {
                        press(workspace, key, window, cx)?;
                    }
                    ensure!(!workspace.projects.active().editor.vim().doc.folded_at(0));

                    for key in ["shift-g", "z", "=", "1"] {
                        press(workspace, key, window, cx)?;
                        if key == "=" {
                            ensure!(workspace.sequence_hint_area().is_some(), "z= picker");
                        }
                    }
                    ensure!(workspace.sequence_hint_area().is_none());
                    ensure!(workspace.projects.active().editor.vim().doc.line(2) == "the");
                    press(workspace, "g", window, cx)?;
                    press(workspace, "g", window, cx)?;

                    press_hidden(workspace, "g", window, cx)?;
                    press_hidden(workspace, "shift-u", window, cx)?;
                    press(workspace, "w", window, cx)?;
                    ensure!(workspace.projects.active().editor.vim().doc.line(0) == "ALPHA beta");
                    press(workspace, "u", window, cx)?;

                    press_hidden(workspace, "ctrl-w", window, cx)?;
                    press(workspace, "h", window, cx)?;
                    ensure!(workspace.projects.active().focus == Focus::Manager);
                    press(workspace, "ctrl-w", window, cx)?;
                    press(workspace, "l", window, cx)?;
                    ensure!(workspace.projects.active().focus == Focus::Editor);
                    ensure!(workspace.sequence_hint_area().is_none());

                    press_hidden(workspace, "d", window, cx)?;
                    press_hidden(workspace, "i", window, cx)?;
                    press(workspace, "w", window, cx)?;
                    ensure!(workspace.sequence_hint_area().is_none());
                    ensure!(workspace.projects.active().editor.vim().doc.line(0) == " beta");

                    press_hidden(workspace, "g", window, cx)?;
                    press(workspace, "escape", window, cx)?;
                    ensure!(workspace.sequence_hint_area().is_none());
                    ensure!(!workspace.projects.active().editor.vim().awaiting_key());

                    press_hidden(workspace, "d", window, cx)?;
                    workspace.dismiss_sequence_hint(cx);
                    ensure!(workspace.sequence_hint_area().is_none());
                    ensure!(!workspace.projects.active().editor.vim().awaiting_key());

                    press_hidden(workspace, "space", window, cx)?;
                    Ok(vim_hint)
                })
            })??;
            app.background_executor().timer(HINT_SETTLE).await;
            app.update(|app| {
                handle.update(app, |workspace, window, cx| -> Result<()> {
                    let leader_hint = workspace.sequence_hint_area().context("leader hint")?;
                    ensure!(vim_hint.x + vim_hint.w == leader_hint.x + leader_hint.w);
                    ensure!(vim_hint.y + vim_hint.h == leader_hint.y + leader_hint.h);
                    press(workspace, "p", window, cx)?;
                    let group_hint = workspace.sequence_hint_area().context("leader p hint")?;
                    ensure!(group_hint.h < leader_hint.h);
                    press(workspace, "backspace", window, cx)?;
                    ensure!(workspace.sequence_hint_area() == Some(leader_hint));
                    press(workspace, "escape", window, cx)?;
                    ensure!(workspace.sequence_hint_area().is_none());

                    press_hidden(workspace, "space", window, cx)?;
                    press_hidden(workspace, "tab", window, cx)?;
                    let hint = workspace.active_sequence_hint().context("Tabpage hint")?;
                    ensure!(
                        hint.rows()
                            == [
                                ("Tab", nanobug::i18n::text("leader.new_tabpage"), false),
                                ("d", nanobug::i18n::text("leader.close_tabpage"), false),
                            ]
                    );
                    ensure!(!workspace.accepts_native_text());
                    press(workspace, "tab", window, cx)?;
                    ensure!(workspace.projects.active().page_count() == 2);
                    ensure!(workspace.projects.active().active_page() == 1);
                    ensure!(workspace.active_sequence_hint().is_none());
                    for closing_draft in [true, false] {
                        for key in ["space", "tab", "d"] {
                            press(workspace, key, window, cx)?;
                        }
                        if closing_draft {
                            ensure!(workspace.editor_close.is_some(), "closing an unsaved page");
                            press(workspace, "n", window, cx)?;
                        }
                        ensure!(workspace.projects.active().page_count() == 1);
                        ensure!(workspace.active_sequence_hint().is_none());
                    }

                    workspace.projects.active_mut().focus = Focus::Manager;
                    workspace.sync(window, cx);
                    press(workspace, "shift-g", window, cx)?;
                    let manager = |workspace: &Workspace| {
                        workspace
                            .projects
                            .active()
                            .manager
                            .as_ref()
                            .map(|manager| manager.tree().selected)
                    };
                    ensure!(manager(workspace).is_some_and(|selected| selected > 0));
                    press_hidden(workspace, "g", window, cx)?;
                    press(workspace, "g", window, cx)?;
                    ensure!(workspace.sequence_hint_area().is_none());
                    ensure!(manager(workspace) == Some(0));

                    workspace.projects.active_mut().focus = Focus::Editor;
                    workspace.sync(window, cx);
                    press(workspace, "i", window, cx)?;
                    ensure!(workspace.accepts_native_text());
                    press_hidden(workspace, "ctrl-g", window, cx)?;
                    ensure!(!workspace.accepts_native_text());
                    press(workspace, "u", window, cx)?;
                    ensure!(workspace.accepts_native_text());
                    workspace.commit_text("中文", window, cx);
                    ensure!(
                        workspace
                            .projects
                            .active()
                            .editor
                            .vim()
                            .doc
                            .text
                            .contains("中文")
                    );
                    press(workspace, "escape", window, cx)?;
                    ensure!(!workspace.accepts_native_text());
                    press_hidden(workspace, "g", window, cx)?;
                    press(workspace, "escape", window, cx)?;

                    let mut appearance = workspace.appearance;
                    appearance.base_size = 24.;
                    appearance.zoom = 1.5;
                    workspace.apply_appearance(appearance, window, cx);
                    window.resize(size(px(800.), px(480.)));
                    Ok(())
                })
            })??;
            app.background_executor()
                .timer(Duration::from_millis(60))
                .await;
            app.update(|app| {
                handle.update(app, |workspace, window, cx| -> Result<()> {
                    workspace.projects.active_mut().focus = Focus::Editor;
                    workspace.sync(window, cx);
                    press_hidden(workspace, "z", window, cx)
                })
            })??;
            app.background_executor().timer(HINT_SETTLE).await;
            app.update(|app| {
                handle.update(app, |workspace, window, cx| -> Result<()> {
                    let hint = workspace
                        .sequence_hint_area()
                        .context("z hint after resize")?;
                    ensure!(hint.x + hint.w <= workspace.layout.visible_cols);
                    ensure!(hint.y + hint.h <= workspace.layout.visible_rows);
                    press(workspace, "escape", window, cx)?;
                    press_hidden(workspace, "g", window, cx)
                })
            })??;
            app.background_executor().timer(HINT_SETTLE).await;
            app.update(|app| {
                handle.update(app, |workspace, window, cx| -> Result<()> {
                    let hint = workspace
                        .sequence_hint_area()
                        .context("g hint after resize")?;
                    ensure!(hint.y + hint.h <= workspace.layout.visible_rows);
                    press(workspace, "pagedown", window, cx)?;
                    ensure!(workspace.hint_page == 1);
                    press(workspace, "g", window, cx)?;
                    ensure!(workspace.sequence_hint_area().is_none());
                    ensure!(workspace.hint_page == 0);
                    press_hidden(workspace, "d", window, cx)
                })
            })??;
            app.background_executor().timer(HINT_SETTLE).await;
            app.update(|app| {
                handle.update(app, |workspace, window, cx| -> Result<()> {
                    let hint = workspace.sequence_hint_area().context("operator hint")?;
                    ensure!(hint.h < 15, "operator keys should wrap across columns");
                    ensure!(hint.x + hint.w <= workspace.layout.visible_cols);
                    ensure!(hint.y + hint.h <= workspace.layout.visible_rows);
                    press(workspace, "i", window, cx)?;
                    ensure!(workspace.sequence_hint_area().is_some(), "visible di hint");
                    Ok(())
                })
            })??;
            app.update_window(handle.into(), |_, window, cx| window.draw(cx).clear(cx))?;
            let image =
                app.update_window(handle.into(), |_, window, _| window.render_to_image())??;
            ensure!(image.width() > 0 && image.height() > 0);
            Ok(())
        },
    )
}
