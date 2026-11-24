//! Windows renderer and real workspace input coverage for the unified settings dialog.

use super::super::{Workspace, native_test};
use crate::{grid::windows::settings::Action, ui::mnemonic::Mnemonic};
use anyhow::{Context as _, Result, ensure};
use gpui::{
    AppContext, AsyncApp, Context, EntityInputHandler, KeyDownEvent, Keystroke, MouseButton,
    MouseDownEvent, PlatformInput, Window, WindowHandle, px, size,
};
use nanobug::{
    i18n::{self, Locale},
    plugins::session::Options,
};
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

/// Send normal native key events rather than changing the dialog's selection or draft directly.
fn keys(
    ws: &mut Workspace,
    sequence: &[&'static str],
    win: &mut Window,
    cx: &mut Context<Workspace>,
) -> Result<()> {
    for key in sequence {
        ws.key_down(
            &KeyDownEvent {
                keystroke: Keystroke::parse(key)?,
                is_held: false,
                prefer_character_input: false,
            },
            win,
            cx,
        );
    }
    Ok(())
}

/// A single click folds a settings branch. Late plugin pages respect its fold and keep their stable IDs.
#[test]
#[ignore = "requires the Windows DirectWrite and DirectX renderer"]
fn native_settings_tree_clicks_follow_late_pages() -> Result<()> {
    use crate::grid::windows::settings::{Page, Settings};

    let temp = tempfile::tempdir()?;
    let options = Options::take(
        &mut vec![
            "--no-plugins".into(),
            "--plugin-profile".into(),
            "profile".into(),
        ],
        temp.path(),
    )?;
    native_test::run_async_visible(
        temp.path().to_owned(),
        options,
        |_| {},
        async move |handle, app| {
            let _temp = temp;
            handle.update(app, |ws, win, cx| -> Result<()> {
                ws.sync(win, cx);
                let page = |id: &str, root: &str, leaf: &str| Page {
                    id: id.into(),
                    path: vec![root.into(), leaf.into()],
                    fields: vec![],
                };
                let mut settings = Settings::new(
                    vec![
                        page("indent", "Editor", "Indent"),
                        page("build", "Plugins", "Build"),
                    ],
                    ws.metrics,
                    cx,
                );
                let area = ws.settings_area();
                ensure!(
                    area.w >= 32 && area.h >= 9,
                    "unmeasured settings viewport {area:?}"
                );
                let click = |settings: &mut Settings, row: usize| {
                    let point = ws
                        .metrics
                        .at((area.x + 2) as f32, (area.y + 1 + row) as f32 + 0.5);
                    settings.click(point, area, ws.metrics, &ws.spacing);
                };
                click(&mut settings, 0);
                ensure!(
                    settings.selected_tree_path() == ["Editor"],
                    "root click landed on {:?}",
                    settings.selected_tree_path()
                );
                settings.append_pages(vec![page("theme", "Editor", "Theme")]);
                click(&mut settings, 1);
                ensure!(
                    settings.selected_tree_path() == ["Plugins"],
                    "collapsed row hit {:?}",
                    settings.selected_tree_path()
                );
                click(&mut settings, 0);
                click(&mut settings, 2);
                ensure!(
                    settings.selected_page_id() == "theme",
                    "expanded row opened {}",
                    settings.selected_page_id()
                );
                Ok(())
            })??;
            Ok(())
        },
    )
}

/// Locate a footer button by the visible widths of its translated character spans.
fn button_event(ws: &Workspace, target: &str) -> Result<MouseDownEvent> {
    let area = ws.settings_area();
    let mut right = area.x + area.w - 2;
    for key in [
        "settings.confirm",
        "settings.apply",
        "settings.restore",
        "settings.reset",
        "settings.cancel",
    ] {
        let width = Mnemonic::parse(i18n::text(key))
            .spans()
            .iter()
            .map(|(text, _)| ws.spacing.columns(text))
            .sum::<usize>();
        if key == target {
            let position = ws.metrics.at(
                (right - width) as f32 + 0.5,
                (area.y + area.h - 3) as f32 + 0.5,
            );
            return Ok(MouseDownEvent {
                position,
                button: MouseButton::Left,
                click_count: 1,
                ..Default::default()
            });
        }
        right -= width + 1;
    }
    anyhow::bail!("Unknown button {target}")
}

fn click_button(
    ws: &mut Workspace,
    target: &str,
    win: &mut Window,
    cx: &mut Context<Workspace>,
) -> Result<()> {
    ws.mouse_down(&button_event(ws, target)?, win, cx);
    Ok(())
}

/// Find a capability through its localized tree using only normal navigation and disclosure keys.
fn browse_to(
    ws: &mut Workspace,
    page: &str,
    win: &mut Window,
    cx: &mut Context<Workspace>,
) -> Result<()> {
    let target = ws
        .settings
        .as_ref()
        .context("settings")?
        .pages()
        .find(|p| p.id == page)
        .context("catalog page")?
        .path
        .clone();
    keys(ws, &["home"], win, cx)?;
    for _ in 0..128 {
        let current = ws.settings.as_ref().unwrap().selected_tree_path();
        if current == target {
            return Ok(());
        }
        keys(
            ws,
            &[if target.starts_with(current) {
                "l"
            } else {
                "j"
            }],
            win,
            cx,
        )?;
    }
    anyhow::bail!("Could not reach {}", target.join(" / "))
}

/// Read the actual DirectX client image after the requested animation interval.
async fn native_image(
    handle: WindowHandle<Workspace>,
    app: &mut AsyncApp,
    after_ms: u64,
) -> Result<image::RgbaImage> {
    app.background_executor()
        .timer(Duration::from_millis(after_ms))
        .await;
    app.update_window(handle.into(), |_, win, cx| {
        win.draw(cx).clear(cx);
        win.render_to_image()
    })?
}

/// Export the client image when a capture directory is requested for visual verification.
async fn capture(
    handle: WindowHandle<Workspace>,
    app: &mut AsyncApp,
    directory: Option<&Path>,
    name: &str,
) -> Result<()> {
    let Some(directory) = directory else {
        return Ok(());
    };
    let image = native_image(handle, app, 220).await?;
    fs::create_dir_all(directory)?;
    image.save(directory.join(name))?;
    Ok(())
}

/// Sample the pixels between both logical carets, where only the travelling cursor should change.
#[test]
#[ignore = "requires the Windows DirectWrite and DirectX renderer"]
fn native_settings_cursor_travels_between_regions() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    fs::create_dir(&project)?;
    // Slow the standard fluid preset so there is a wide sampling window for the intermediate frame.
    fs::write(
        project.join("cursor-effects.toml"),
        "[stages.prepare]\nduration_ms = 0\n[stages.travel]\nresponse_ms = 600\n",
    )?;
    let options = Options::take(
        &mut vec![
            "--no-plugins".into(),
            "--plugin-profile".into(),
            "profile".into(),
        ],
        temp.path(),
    )?;
    native_test::run_async_visible(
        project,
        options,
        |_| {},
        async move |handle, app| {
            let _temp = temp;
            handle.update(app, |ws, w, cx| -> Result<()> {
                ws.cursor.update(cx, |cursor, cx| {
                    cursor.select_effect(nanobug::animation::cursor::Effect::Fluid, w, cx)
                });
                ws.open_settings(w, cx);
                browse_to(ws, "core.vim.display", w, cx)?;
                keys(ws, &["tab", "j", "j", "tab"], w, cx)
            })??;
            app.background_executor()
                .timer(Duration::from_millis(900))
                .await;
            let mut missing = Vec::new();
            for (key, route) in [
                (Some("tab"), "tree → form"),
                (Some("enter"), "form → choices"),
                (Some("escape"), "choices → form"),
                (None, "click form → choices"),
                (None, "click choices → form"),
                (Some("tab"), "form → tree"),
            ] {
                let (left, right, top, bottom) =
                    handle.update(app, |ws, w, cx| -> Result<_> {
                        let (x0, y0, ..) = ws.caret();
                        if let Some(key) = key {
                            keys(ws, &[key], w, cx)?;
                        } else {
                            let viewport = ws
                                .settings
                                .as_ref()
                                .context("settings")?
                                .cursor_viewport(ws.settings_area());
                            let position = gpui::point(
                                viewport.clip(ws.metrics).right() - px(ws.metrics.cw * 2.),
                                ws.metrics.at(x0, y0 + 0.5).y,
                            );
                            ws.mouse_down(
                                &MouseDownEvent {
                                    position,
                                    button: MouseButton::Left,
                                    click_count: 1,
                                    ..Default::default()
                                },
                                w,
                                cx,
                            );
                        }
                        let (x1, y1, _, _, visible) = ws.caret();
                        ensure!(
                            visible && (x1 - x0).abs() > 5.,
                            "expected two visible positions for {route}"
                        );
                        let device =
                            |p: gpui::Pixels| (f32::from(p) * w.scale_factor()).round() as u32;
                        let span = (x1 - x0).abs();
                        let start = ws.metrics.at(x0.min(x1) + span * 0.15, y0.min(y1) - 0.5);
                        let end = ws.metrics.at(x0.max(x1) - span * 0.15, y0.max(y1) + 1.5);
                        Ok((
                            device(start.x),
                            device(end.x),
                            device(start.y),
                            device(end.y),
                        ))
                    })??;
                let moving = native_image(handle, app, 120).await?;
                let settled = native_image(handle, app, 850).await?;
                let mut changed = 0;
                for y in top..bottom {
                    for x in left..right {
                        let during = moving[(x, y)];
                        let after = settled[(x, y)];
                        if (0..3).any(|channel| during[channel].abs_diff(after[channel]) > 40) {
                            changed += 1;
                        }
                    }
                }
                if changed < 6 {
                    missing.push((route, changed));
                }
            }
            ensure!(
                missing.is_empty(),
                "no visible cursor between endpoints: {missing:?}"
            );
            Ok(())
        },
    )
}

/// Check the rendered field positions and route real pointer events through the overlapping selector.
#[test]
#[ignore = "requires the Windows DirectWrite and DirectX renderer"]
fn native_settings_floating_selector_keeps_form_and_tracks_cursor() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("nanobug");
    fs::create_dir_all(&project)?;
    fs::write(project.join("main.rs"), "fn main() {}\n")?;
    let profile = temp.path().join("profile");
    fs::create_dir(&profile)?;
    if let Some(index) = std::env::var_os("NANOBUG_SETTINGS_TEST_INDEX") {
        fs::copy(index, profile.join("installed.json"))?;
    }
    let options = Options::take(
        &mut vec![
            "--plugin-profile".into(),
            profile.into_os_string(),
            "--no-plugins".into(),
        ],
        temp.path(),
    )?;
    let path = temp.path().join("settings.toml");
    fs::write(
        &path,
        "[vim.options.window]\nnumber = true\nrelativenumber = false\n",
    )?;
    let output = std::env::var_os("NANOBUG_SETTINGS_CAPTURE").map(PathBuf::from);
    let previous = i18n::locale();
    i18n::set_locale(Locale::SimplifiedChinese);
    let result = native_test::run_async_visible(
        project,
        options,
        move |ws| {
            ws.appearance_settings_path = Some(path.clone());
            ws.vim_settings_path = Some(path);
            ws.load_vim_config();
        },
        async move |handle, app| {
            let _temp = temp;
            app.update_window(handle.into(), |_, win, _| {
                win.resize(size(px(1440.), px(900.)))
            })?;
            app.background_executor()
                .timer(Duration::from_millis(160))
                .await;
            let footer_sample = handle.update(app, |ws, w, cx| {
                let mut appearance = ws.appearance;
                appearance.zoom = 1.35;
                ws.apply_appearance(appearance, w, cx);
                ws.execute_ex_input("edit main.rs", "settings QA", 1, false, w, cx);
                let footer = ws.footer_bounds();
                let scale = w.scale_factor();
                (
                    ((f32::from(footer.left()) + f32::from(footer.size.width) * 0.45) * scale)
                        as u32,
                    ((f32::from(footer.top()) + ws.metrics.lh * 0.1) * scale) as u32,
                )
            })?;
            let workspace_image = native_image(handle, app, 220).await?;
            let footer_color = workspace_image[footer_sample];
            let (wrap_row, labels, settings_samples) =
                handle.update(app, |ws, w, cx| -> Result<_> {
                    keys(ws, &["space", ","], w, cx)?;
                    browse_to(ws, "core.vim.display", w, cx)?;
                    let (tree_x, _, _, insert, visible) = ws.caret();
                    ensure!(visible && !insert, "tree needs a navigation cursor");
                    keys(ws, &["tab", "j", "j"], w, cx)?;
                    let (x, y, _, insert, visible) = ws.caret();
                    ensure!(visible && !insert && x > tree_x && !ws.accepts_native_text());
                    let settings = ws.settings.as_ref().context("settings")?;
                    ensure!(settings.selected_field_id() == Some("wrap"));
                    let form = settings.layers(ws.settings_area(), ws.metrics)[2]
                        .clip
                        .context("form viewport")?;
                    // Sample text on the left of the controls, from the next field through the value legend.
                    let scale = w.scale_factor();
                    let sample = (
                        ((f32::from(form.left()) + 2. * ws.metrics.cw) * scale).ceil() as u32,
                        (f32::from(ws.metrics.at(0., y + 1.).y) * scale).ceil() as u32,
                        (16. * ws.metrics.cw * scale).floor() as u32,
                        (3. * ws.metrics.lh * scale).floor() as u32,
                    );
                    let area = ws.settings_area();
                    let samples = [(area.w / 2, area.h / 2), (2, area.h - 3)].map(|(col, row)| {
                        let p = ws
                            .metrics
                            .at((area.x + col) as f32, (area.y + row) as f32 + 0.5);
                        (
                            (f32::from(p.x) * scale) as u32,
                            (f32::from(p.y) * scale) as u32,
                        )
                    });
                    Ok((y, sample, samples))
                })??;
            let closed = native_image(handle, app, 220).await?;
            for sample in settings_samples {
                ensure!(
                    closed[sample] == footer_color,
                    "settings background {:?} must match workspace footer {footer_color:?}",
                    closed[sample]
                );
            }
            handle.update(app, |ws, w, cx| -> Result<()> {
                keys(ws, &["enter"], w, cx)?;
                let (_, y, _, insert, visible) = ws.caret();
                ensure!(visible && !insert && y > wrap_row && !ws.accepts_native_text());
                Ok(())
            })??;
            let opened = native_image(handle, app, 220).await?;
            if let Some(directory) = output {
                fs::create_dir_all(&directory)?;
                closed.save(directory.join("settings-switches-zh.png"))?;
                opened.save(directory.join("settings-floating-selector-zh.png"))?;
            }
            let (x, y, width, height) = labels;
            let before = image::imageops::crop_imm(&closed, x, y, width, height).to_image();
            let after = image::imageops::crop_imm(&opened, x, y, width, height).to_image();
            ensure!(
                before == after,
                "opening selector moved or obscured the field labels"
            );
            ensure!(
                before.pixels().any(|p| p != before.get_pixel(0, 0)),
                "sampled area must contain the painted field and legend"
            );
            let border = handle.update(app, |ws, _, _| MouseDownEvent {
                position: ws.metrics.at(ws.caret().0 + 2.5, wrap_row + 1.5),
                button: MouseButton::Left,
                click_count: 1,
                ..Default::default()
            })?;
            app.update_window(handle.into(), |_, w, cx| {
                w.dispatch_event(PlatformInput::MouseDown(border), cx);
            })?;
            let click = handle.update(app, |ws, w, cx| -> Result<_> {
                let old = ws.caret().1;
                ensure!(
                    old > wrap_row + 1. && !ws.accepts_native_text(),
                    "menu border activated the text width behind it"
                );
                keys(ws, &["j"], w, cx)?;
                let (x, y, _, insert, visible) = ws.caret();
                ensure!(visible && !insert && (y - old - 1.).abs() < 0.01);
                Ok(MouseDownEvent {
                    position: ws.metrics.at(x + 1.5, y + 1.5),
                    button: MouseButton::Left,
                    click_count: 1,
                    ..Default::default()
                })
            })??;
            app.update_window(handle.into(), |_, w, cx| {
                w.draw(cx).clear(cx);
                w.dispatch_event(PlatformInput::MouseDown(click), cx);
            })?;
            handle.update(app, |ws, w, cx| -> Result<()> {
                let settings = ws.settings.as_ref().context("settings after choice")?;
                ensure!(settings.selected_field_id() == Some("wrap") && ws.caret().1 == wrap_row);
                let plan = settings.plan(Action::Apply);
                ensure!(
                    plan.len() == 1 && plan[0].1 == [("wrap".into(), json!("off"))],
                    "native click must select the overlay option: {plan:?}"
                );
                keys(ws, &["j", "enter"], w, cx)?;
                let (_, y, _, insert, visible) = ws.caret();
                ensure!(visible && insert && y == wrap_row + 1. && ws.accepts_native_text());
                keys(ws, &["escape", "escape"], w, cx)?;
                ensure!(ws.settings.is_none());
                Ok(())
            })??;
            Ok(())
        },
    );
    i18n::set_locale(previous);
    result
}

/// Exercise all five actions across independent pages and observe their actual portable values.
#[test]
#[ignore = "requires the Windows DirectWrite and DirectX renderer"]
fn native_settings_five_actions_edit_real_pages() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("nanobug");
    fs::create_dir_all(project.join("src"))?;
    fs::write(
        project.join("src/main.rs"),
        "fn main() {\n    println!(\"Nanobug\");\n}\n",
    )?;
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = 'nanobug'\nversion = '0.1.0'\n",
    )?;
    let profile = temp.path().join("profile");
    fs::create_dir_all(&profile)?;
    // An optional installed index supplies real catalog rows for visual QA without running workers.
    if let Some(index) = std::env::var_os("NANOBUG_SETTINGS_TEST_INDEX") {
        fs::copy(index, profile.join("installed.json"))?;
    }
    let options = Options::take(
        &mut vec![
            "--plugin-profile".into(),
            profile.into_os_string(),
            "--no-plugins".into(),
        ],
        temp.path(),
    )?;
    let persistent = temp.path().join("settings.toml");
    let initial = "[vim.options.buffer]\nshiftwidth = 8\n";
    fs::write(&persistent, initial)?;
    let path = persistent.clone();
    let output = std::env::var_os("NANOBUG_SETTINGS_CAPTURE").map(PathBuf::from);
    let original_locale = i18n::locale();
    i18n::set_locale(Locale::SimplifiedChinese);
    let result = native_test::run_async_visible(
        project,
        options,
        move |ws| {
            ws.appearance_settings_path = Some(path.clone());
            ws.vim_settings_path = Some(path);
            ws.load_vim_config();
        },
        async move |handle, app| {
            let _temp = temp;
            app.update_window(handle.into(), |_, w, _| w.resize(size(px(1440.), px(900.))))?;
            app.background_executor()
                .timer(Duration::from_millis(160))
                .await;
            handle.update(app, |ws, win, cx| -> Result<()> {
                let mut appearance = ws.appearance;
                appearance.zoom = 1.35;
                ws.apply_appearance(appearance, win, cx);
                ws.execute_ex_input("edit src/main.rs", "settings QA", 1, false, win, cx);
                let start = ws.projects.add_start();
                ws.projects.select(start);
                ws.sync(win, cx);
                keys(ws, &["c"], win, cx)?;
                ensure!(ws.settings.is_some(), "launcher c must open settings");
                click_button(ws, "settings.cancel", win, cx)?;
                ensure!(ws.settings.is_none(), "mouse cancel failed");
                ws.projects.select(0);
                ws.sync(win, cx);
                keys(ws, &["space", ",", "tab", "j", "enter"], win, cx)?;
                ensure!(
                    ws.settings.is_some() && ws.accepts_native_text(),
                    "numeric editor must receive IME"
                );
                ws.replace_and_mark_text_in_range(None, "２", Some(1..1), win, cx);
                ensure!(ws.marked_text_range(win, cx).is_some(), "preedit missing");
                ws.replace_text_in_range(None, "10000", win, cx);
                keys(ws, &["enter"], win, cx)?;
                ensure!(ws.accepts_native_text(), "invalid width left input");
                keys(ws, &["ctrl-a"], win, cx)?;
                ws.replace_text_in_range(None, "2", win, cx);
                keys(ws, &["enter", "j", "space", "k"], win, cx)?;
                ensure!(
                    !ws.accepts_native_text(),
                    "navigation must exit native input immediately"
                );
                let changes = ws.settings.as_ref().unwrap().plan(Action::Apply);
                ensure!(
                    changes.len() == 1 && changes[0].1.len() == 2,
                    "two draft indentation settings expected: {changes:?}"
                );
                Ok(())
            })??;
            capture(handle, app, output.as_deref(), "settings-indent-zh.png").await?;
            handle.update(app, |ws, win, cx| -> Result<()> {
                keys(ws, &["tab", "j", "tab", "space", "a"], win, cx)?;
                ensure!(
                    ws.input_mappings.options().preference("number")? == false,
                    "apply displayed page"
                );
                ensure!(
                    ws.input_mappings.options().preference("shiftwidth")? == 8,
                    "indentation draft applied from another page"
                );
                keys(ws, &["tab", "k"], win, cx)?;
                click_button(ws, "settings.apply", win, cx)?;
                ensure!(ws.input_mappings.options().preference("shiftwidth")? == 2);
                keys(ws, &["d", "a"], win, cx)?;
                ensure!(
                    ws.input_mappings.options().preference("shiftwidth")? == 4,
                    "reset must use system default"
                );
                keys(ws, &["r", "a"], win, cx)?;
                ensure!(
                    ws.input_mappings.options().preference("shiftwidth")? == 8,
                    "restore must use opening value"
                );
                click_button(ws, "settings.cancel", win, cx)?;
                ensure!(
                    ws.settings.is_none()
                        && ws.input_mappings.options().preference("number")? == true
                );
                ensure!(
                    fs::read_to_string(&persistent)? == initial,
                    "cancel must restore applied pages on disk"
                );
                keys(
                    ws,
                    &[
                        "space", ",", "tab", "j", "j", "space", "tab", "j", "tab", "space", "o",
                    ],
                    win,
                    cx,
                )?;
                ensure!(ws.settings.is_none(), "confirm must close");
                ensure!(ws.input_mappings.options().preference("expandtab")? == true);
                ensure!(ws.input_mappings.options().preference("number")? == false);
                let saved = fs::read_to_string(&persistent)?;
                ensure!(saved.contains("expandtab = true") && saved.contains("number = false"));
                keys(ws, &["space", ","], win, cx)?;
                browse_to(ws, "core.highlight.Comment", win, cx)?;
                ensure!(
                    ws.settings.as_ref().unwrap().selected_page_id() == "core.highlight.Comment"
                );
                keys(
                    ws,
                    &[
                        "tab", "j", "j", "enter", "space", "j", "space", "enter", "enter", "j",
                    ],
                    win,
                    cx,
                )?;
                ensure!(ws.settings.as_ref().unwrap().selected_field_id() == Some("decorations"));
                Ok(())
            })??;
            capture(handle, app, output.as_deref(), "settings-highlight-zh.png").await?;
            handle.update(app, |ws, win, cx| -> Result<()> {
                keys(ws, &["enter"], win, cx)?;
                let changes = ws.settings.as_ref().unwrap().plan(Action::Apply);
                ensure!(
                    changes.len() == 1
                        && changes[0].0.id == "core.highlight.Comment"
                        && changes[0].1[0].1 == json!(["bold", "italic"]),
                    "multi-select changed {changes:?}"
                );
                keys(ws, &["a"], win, cx)?;
                let attributes = ws
                    .input_mappings
                    .highlight_override("Comment")
                    .context("applied style")?
                    .attributes;
                ensure!(attributes.bold && attributes.italic);
                keys(ws, &["tab"], win, cx)?;
                browse_to(ws, "core.highlight.DocComment", win, cx)?;
                let child = ws
                    .input_mappings
                    .highlight_style("DocComment")
                    .context("child style")?;
                ensure!(child.attributes.bold && child.attributes.italic);
                keys(ws, &["tab", "j", "j", "j"], win, cx)?;
                ensure!(ws.settings.as_ref().unwrap().selected_field_id() == Some("foreground"));
                keys(ws, &["enter"], win, cx)?;
                ws.replace_text_in_range(None, "#12ab34", win, cx);
                keys(ws, &["enter", "a"], win, cx)?;
                ensure!(
                    ws.input_mappings
                        .highlight_style("@comment.documentation")
                        .unwrap()
                        .foreground
                        == Some(0x12ab34)
                );
                ensure!(fs::read_to_string(&persistent)?.contains("#12ab34"));
                Ok(())
            })??;
            capture(
                handle,
                app,
                output.as_deref(),
                "settings-syntax-color-zh.png",
            )
            .await?;
            handle.update(app, |ws, win, cx| -> Result<()> {
                keys(ws, &["escape"], win, cx)?;
                ensure!(
                    ws.input_mappings
                        .highlight_foreground("DocComment")
                        .is_none()
                );
                ensure!(ws.input_mappings.highlight_override("Comment").is_none());
                i18n::set_locale(Locale::English);
                keys(ws, &["space", ","], win, cx)?;
                ensure!(ws.settings.is_some());
                Ok(())
            })??;
            let click = handle.update(app, |ws, _, _| button_event(ws, "settings.cancel"))??;
            app.update_window(handle.into(), |_, window, cx| {
                window.draw(cx).clear(cx);
                window.dispatch_event(PlatformInput::MouseDown(click), cx);
            })?;
            handle.update(app, |ws, _, _| -> Result<()> {
                ensure!(
                    ws.settings.is_none(),
                    "English cancel blocked by native scrim hit testing"
                );
                Ok(())
            })??;
            Ok(())
        },
    );
    i18n::set_locale(original_locale);
    result
}

/// A real language-plugin catalog and project TOML participate in the same Cancel as core settings.
#[test]
#[ignore = "requires Windows DirectX and an installed Rust language plugin index"]
fn native_settings_rust_scan_applies_and_rolls_back() -> Result<()> {
    let sandbox = tempfile::tempdir()?;
    let root = sandbox.path().join("nanobug");
    let scan = root.join(".nb/plugin-setting/rust.toml");
    fs::create_dir_all(scan.parent().unwrap())?;
    let opening = "# QA workspace\nexcludeDirs = [\"test-output\"] # keep\n";
    fs::write(&scan, opening)?;
    fs::write(
        root.join("main.rs"),
        "fn main() {\n    println!(\"Nanobug\");\n}\n",
    )?;
    let profile = sandbox.path().join("profile");
    fs::create_dir_all(&profile)?;
    let index = std::env::var_os("NANOBUG_SETTINGS_TEST_INDEX")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("dist/plugins/installed.json")
        });
    fs::copy(&index, profile.join("installed.json"))
        .with_context(|| format!("Supply a Rust plugin index at {}", index.display()))?;
    let opts = Options::take(
        &mut vec![
            "--plugin-profile".into(),
            profile.into_os_string(),
            "--no-plugins".into(),
        ],
        sandbox.path(),
    )?;
    let path = sandbox.path().join("settings.toml");
    let initialized = path.clone();
    fs::write(&path, "# opening\n")?;
    let output = std::env::var_os("NANOBUG_SETTINGS_CAPTURE").map(PathBuf::from);
    let original_locale = i18n::locale();
    i18n::set_locale(Locale::SimplifiedChinese);
    let result = native_test::run_async_visible(
        root,
        opts,
        move |ws| {
            ws.appearance_settings_path = Some(initialized.clone());
            ws.vim_settings_path = Some(initialized);
        },
        async move |handle, app| {
            let _sandbox = sandbox;
            app.update_window(handle.into(), |_, window, _| {
                window.resize(size(px(1440.), px(900.)))
            })?;
            app.background_executor()
                .timer(Duration::from_millis(160))
                .await;
            handle.update(app, |ws, w, cx| -> Result<()> {
                let mut appearance = ws.appearance;
                appearance.zoom = 1.25;
                ws.apply_appearance(appearance, w, cx);
                ws.execute_ex_input("edit main.rs", "settings QA", 1, false, w, cx);
                keys(ws, &["space", ",", "tab", "j", "j", "space", "tab"], w, cx)
            })??;
            let deadline = std::time::Instant::now() + Duration::from_secs(8);
            let page = loop {
                let found = handle.update(app, |ws, _, _| {
                    ws.settings.as_ref().and_then(|settings| {
                        settings
                            .pages()
                            .find(|p| {
                                p.id.ends_with(".scan") && p.path.iter().any(|part| part == "Rust")
                            })
                            .map(|p| p.id.clone())
                    })
                })?;
                if let Some(page) = found {
                    break page;
                }
                ensure!(
                    std::time::Instant::now() < deadline,
                    "Rust scan page unavailable in installed index"
                );
                app.background_executor()
                    .timer(Duration::from_millis(30))
                    .await;
            };
            handle.update(app, |ws, w, cx| -> Result<()> {
                browse_to(ws, &page, w, cx)?;
                keys(ws, &["tab", "enter"], w, cx)?;
                ensure!(ws.accepts_native_text());
                ws.replace_text_in_range(None, r#"["test-output","generated"]"#, w, cx);
                keys(ws, &["enter"], w, cx)
            })??;
            capture(handle, app, output.as_deref(), "settings-rust-zh.png").await?;
            handle.update(app, |ws, w, cx| click_button(ws, "settings.apply", w, cx))??;
            wait_for_write(handle, app).await?;
            let applied: toml::Value = toml::from_str(&fs::read_to_string(&scan)?)?;
            let expected = vec![
                toml::Value::from("test-output"),
                toml::Value::from("generated"),
            ];
            ensure!(
                applied["excludeDirs"].as_array() == Some(&expected),
                "wrong project scan exclusions: {applied}"
            );
            handle.update(app, |ws, w, cx| -> Result<()> {
                ensure!(
                    ws.settings.is_some()
                        && ws.input_mappings.options().preference("expandtab")? == false
                );
                keys(ws, &["tab"], w, cx)?;
                browse_to(ws, "core.vim.indent", w, cx)?;
                keys(ws, &["a"], w, cx)?;
                ensure!(ws.input_mappings.options().preference("expandtab")? == true);
                keys(ws, &["escape"], w, cx)
            })??;
            wait_for_write(handle, app).await?;
            ensure!(
                fs::read_to_string(&scan)? == opening,
                "plugin rollback changed project TOML"
            );
            ensure!(
                fs::read_to_string(&path)? == "# opening\n",
                "core rollback changed disk baseline"
            );
            handle.update(app, |ws, _, _| -> Result<()> {
                ensure!(
                    ws.settings.is_none()
                        && ws.input_mappings.options().preference("expandtab")? == false
                );
                Ok(())
            })??;
            Ok(())
        },
    );
    i18n::set_locale(original_locale);
    result
}

/// Let the plugin supervisor finish the page write and notify the real workspace.
async fn wait_for_write(handle: WindowHandle<Workspace>, app: &mut AsyncApp) -> Result<()> {
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    while handle.update(app, |ws, _, _| ws.settings_writing)? {
        ensure!(
            std::time::Instant::now() < deadline,
            "settings write timed out"
        );
        app.background_executor()
            .timer(Duration::from_millis(20))
            .await;
    }
    Ok(())
}
