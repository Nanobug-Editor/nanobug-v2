//! Exercise retained plugin fields, character buttons and images in a native workspace.

use super::{Input, Prompt, Workspace};
use crate::grid::windows::plugin_manager::layout::ListLayout;
use crate::grid::workspace::native_test;
use anyhow::{Context as _, Result, ensure};
use gpui::{
    App, AppContext, AsyncApp, Bounds, Context, KeyDownEvent, Keystroke, MouseButton,
    MouseDownEvent, Pixels, Point, RenderImage, Window, WindowHandle, point, px, size,
};
use nanobug::{
    animation::cursor::Effect,
    i18n::{self, Locale},
    plugins::{
        drawing::{Asset, PreparedCanvas},
        session,
    },
};
use nanobug_plugin_sdk::{GridSurface, Instance};
use std::{collections::BTreeMap, fs, path::Path, sync::Arc, time::Duration};

#[test]
#[ignore = "requires the Windows DirectWrite and DirectX renderer"]
fn both_plugin_fields_render_all_shared_cursor_presets() -> Result<()> {
    run_workspace(async |handle, cx| cx.update(|cx| verify(handle, cx)))
}

#[test]
#[ignore = "requires Windows font loading and DirectX native painting"]
fn editor_appearance_changes_reach_the_same_grid_and_canvas() -> Result<()> {
    use crate::ui::theme::{AppearanceSettings, GridFont, ThemeChoice, style};
    use gpui::EntityInputHandler;

    run_workspace(async |handle, cx| {
        cx.update(|cx| {
            let before = handle.update(cx, |workspace, window, cx| -> Result<_> {
                install_surface(workspace, true)?;
                semantic_field(workspace, window, "Segoe UI", 17., 3.5)?;
                let key = workspace
                    .plugins
                    .surface
                    .key
                    .as_ref()
                    .context("Canvas owner")?;
                Arc::make_mut(
                    &mut Arc::make_mut(&mut workspace.plugins.view)
                        .surfaces
                        .get_mut(key)
                        .context("Canvas surface")?
                        .frame,
                )
                .canvas
                .as_mut()
                .context("Canvas drawing")?
                .text_input
                .as_mut()
                .context("Canvas input")?
                .font = None;
                workspace.invalidate_plugin_appearance();
                workspace.sync(window, cx);
                Ok((
                    workspace.metrics,
                    workspace.plugins.surface.draft.clone(),
                    workspace.projects.active().editor.vim().doc.text.clone(),
                    workspace
                        .current_native_field()
                        .context("Canvas field")?
                        .caret_bounds(),
                    window.scale_factor(),
                    workspace
                        .current_native_field()
                        .context("Canvas field")?
                        .font_at_start(),
                ))
            })??;
            cx.update_window(handle.into(), |_, window, cx| window.draw(cx).clear(cx))?;
            let old =
                cx.update_window(handle.into(), |_, window, _| window.render_to_image())??;

            handle.update(cx, |workspace, window, cx| -> Result<()> {
                workspace.clear_canvas_focus();
                for command in ["theme light", "font maple", "fontsize 18", "zoom 125%"] {
                    press(workspace, ":", window, cx)?;
                    workspace.replace_text_in_range(None, command, window, cx);
                    press(workspace, "enter", window, cx)?;
                }
                ensure!(workspace.appearance.theme == ThemeChoice::Light);
                ensure!(workspace.appearance.font == GridFont::MapleMono);
                let saved = workspace
                    .appearance_settings_path
                    .as_deref()
                    .context("Fixture settings path")?;
                ensure!(AppearanceSettings::load(saved)? == workspace.appearance);
                ensure!(style(cx).light && style(cx).font == GridFont::MapleMono);
                ensure!(workspace.metrics.font_size == 22.5 && workspace.metrics != before.0);
                ensure!(workspace.editor_panels.text.read(cx).metrics() == workspace.metrics);
                ensure!(workspace.plugin_panel().read(cx).metrics() == workspace.metrics);
                ensure!(window.scale_factor() == before.4);
                ensure!(workspace.plugins.surface.draft == before.1);
                ensure!(workspace.projects.active().editor.vim().doc.text == before.2);
                let field = workspace
                    .current_native_field()
                    .context("resized Canvas field")?;
                ensure!(field.caret_bounds() != before.3);
                let font = workspace
                    .glyphs
                    .font_id(&GridFont::MapleMono.descriptor())?;
                ensure!(field.font_at_start() == Some(font) && before.5 != Some(font));
                Ok(())
            })??;
            cx.update_window(handle.into(), |_, window, cx| window.draw(cx).clear(cx))?;
            let new =
                cx.update_window(handle.into(), |_, window, _| window.render_to_image())??;
            ensure!(
                old.get_pixel(2, 20) != new.get_pixel(2, 20),
                "native editor background did not change"
            );
            Ok(())
        })
    })
}

#[test]
#[ignore = "requires a native Windows window for DPI notifications"]
fn plugin_field_reflows_after_native_dpi_notification() -> Result<()> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::{
        Foundation::RECT,
        UI::WindowsAndMessaging::{GetWindowRect, SendMessageW, WM_DPICHANGED},
    };

    run_workspace(async |handle, cx| {
        let (hwnd, original) = cx.update(|cx| {
            handle.update(cx, |workspace, window, cx| -> Result<_> {
                workspace.projects.active_mut().focus =
                    crate::grid::workspace::layout::Focus::Editor;
                press(workspace, "i", window, cx)?;
                let RawWindowHandle::Win32(raw) = HasWindowHandle::window_handle(window)
                    .ok()
                    .context("native window handle")?
                    .as_raw()
                else {
                    anyhow::bail!("native HWND is missing");
                };
                Ok((
                    raw.hwnd.get() as windows_sys::Win32::Foundation::HWND,
                    window.scale_factor(),
                ))
            })?
        })?;
        let targets = if original == 1.25 {
            [192, 120, 192]
        } else {
            [120, 192, 120]
        };
        for (kind, dpi) in ["editor", "grid", "semantic"].into_iter().zip(targets) {
            let before = cx.update(|cx| {
                handle.update(cx, |workspace, window, cx| -> Result<_> {
                    if kind != "editor" {
                        install_surface(workspace, kind == "semantic")?;
                        if kind == "semantic" {
                            semantic_field(workspace, window, "Segoe UI", 17., 3.5)?;
                        }
                        workspace.plugins.surface.focused = true;
                    }
                    workspace.sync(window, cx);
                    ensure!(workspace.accepts_native_text());
                    Ok((
                        workspace.metrics,
                        workspace.projects.active().editor.vim().doc.text.clone(),
                        workspace.plugins.surface.draft.clone(),
                        workspace.current_native_field().cloned(),
                    ))
                })?
            })?;
            if kind == "semantic" {
                cx.update(|cx| {
                    handle.update(cx, |workspace, window, _| -> Result<()> {
                        // With a replacement layout outstanding, the old scene remains drawable
                        // and its host field usable, while guest actions reject that scene.
                        let key = workspace
                            .plugins
                            .surface
                            .key
                            .clone()
                            .context("Canvas owner")?;
                        let viewport = |id| nanobug_plugin_sdk::canvas::Viewport {
                            id,
                            width: 480.,
                            height: 180.,
                            scale: window.scale_factor(),
                            apple_surface: false,
                            shaped_line: true,
                            appearance: None,
                        };
                        Arc::make_mut(
                            &mut Arc::make_mut(&mut workspace.plugins.view)
                                .surfaces
                                .get_mut(&key)
                                .context("Canvas scene")?
                                .frame,
                        )
                        .canvas
                        .as_mut()
                        .context("Canvas frame")?
                        .viewport = Some(1);
                        workspace.plugins.surface.viewport = Some(viewport(2));
                        ensure!(workspace.current_plugin_surface().is_none());
                        ensure!(
                            super::canvas::element(
                                workspace
                                    .displayed_plugin_surface()
                                    .context("last prepared scene")?,
                                None,
                                None
                            )
                            .is_some()
                        );
                        Ok(())
                    })?
                })?;
            }
            let mut suggested = RECT::default();
            // A different client size makes Windows publish its new native scale through GPUI.
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
            cx.background_executor()
                .timer(Duration::from_millis(60))
                .await;
            cx.update(|cx| {
                cx.update_window(handle.into(), |_, window, cx| window.draw(cx).clear(cx))?;
                handle.update(cx, |workspace, window, cx| -> Result<()> {
                    let scale = dpi as f32 / 96.;
                    ensure!(window.scale_factor() == scale && workspace.display_scale == scale);
                    let metrics = workspace.metrics;
                    for logical in [metrics.cw, metrics.lh] {
                        ensure!((logical * scale - (logical * scale).round()).abs() < 0.001);
                    }
                    ensure!(metrics != before.0 && metrics.font_size == before.0.font_size);
                    ensure!(workspace.editor_panels.text.read(cx).metrics() == metrics);
                    ensure!(workspace.plugin_panel().read(cx).metrics() == metrics);
                    ensure!(workspace.accepts_native_text());
                    ensure!(workspace.projects.active().editor.vim().doc.text == before.1);
                    ensure!(workspace.plugins.surface.draft == before.2);
                    if let Some(before) = &before.3 {
                        let after = workspace
                            .current_native_field()
                            .context("retained semantic layout")?;
                        ensure!(!std::rc::Rc::ptr_eq(before, after));
                        ensure!(
                            workspace
                                .canvas_input_caret_bounds()
                                .is_some_and(|caret| after.clip.contains(&caret.origin))
                        );
                    }
                    Ok(())
                })?
            })?;
        }
        Ok(())
    })
}

fn run_workspace(
    test: impl AsyncFnOnce(WindowHandle<Workspace>, &mut AsyncApp) -> Result<()> + 'static,
) -> Result<()> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    fs::create_dir(&project)?;
    fs::write(project.join("README.md"), "Cursor field fixture")?;
    let options = session::Options::take(
        &mut vec![
            "--plugin-dev-profile".into(),
            temp.path().join("profile").into_os_string(),
        ],
        temp.path(),
    )?;
    native_test::run_async(
        project,
        options,
        |workspace| workspace.plugins.options = None,
        async move |handle, cx| {
            cx.update(|cx| {
                handle.update(cx, |_, window, _| window.resize(size(px(920.), px(580.))))
            })?;
            // Win32 applies the size on a subsequent foreground message.
            cx.background_executor()
                .timer(Duration::from_millis(60))
                .await;
            test(handle, cx).await
        },
    )
}

fn verify(handle: WindowHandle<Workspace>, cx: &mut App) -> Result<()> {
    let evidence = Path::new(env!("CARGO_MANIFEST_DIR")).join("test-output/plugin-cursor-parity");
    fs::create_dir_all(&evidence)?;
    for (surface, canvas) in [("grid", false), ("canvas", true), ("semantic", true)] {
        handle.update(cx, |workspace, window, cx| -> Result<()> {
            install_surface(workspace, canvas)?;
            if surface == "semantic" {
                semantic_field(workspace, window, "Segoe UI", 17., 3.5)?;
            }
            workspace.sync(window, cx);
            let area = workspace.plugin_surface_area().context("visible surface")?;
            let label = area.pixels(workspace.metrics).origin + point(px(15.), px(2.));
            ensure!(workspace.plugin_click(
                &MouseDownEvent {
                    position: label,
                    ..Default::default()
                },
                window,
                cx
            ));
            ensure!(
                !workspace.plugins.surface.focused,
                "Static labels must not capture native text input"
            );

            let field = workspace
                .plugin_input_bounds(area)
                .context("host text field")?;
            let position = field.origin + point(px(2.), px(5.));
            ensure!(workspace.plugin_click(
                &MouseDownEvent {
                    position,
                    ..Default::default()
                },
                window,
                cx
            ));
            ensure!(
                workspace.accepts_native_text(),
                "The declared field must own native text"
            );
            workspace.end_plugin_field_drag();
            press(workspace, "home", window, cx)?;
            let (_, at_start) = caret(workspace)?;
            press(workspace, "right", window, cx)?;
            let (_, after_latin) = caret(workspace)?;
            press(workspace, "shift-right", window, cx)?;
            let (_, after_chinese) = caret(workspace)?;
            ensure!(at_start.x < after_latin.x && after_latin.x < after_chinese.x);
            press(workspace, "end", window, cx)?;
            let (clip, at_end) = caret(workspace)?;
            ensure!(at_end.x > after_chinese.x && clip.contains(&at_end));

            let status = workspace.projects.active().status.clone();
            workspace.plugins.composition.mark(None, "ni", Some(2..2));
            press(workspace, "f6", window, cx)?;
            ensure!(workspace.projects.active().status == status);
            ensure!(workspace.plugins.composition.text == "ni");
            workspace.plugins.composition.clear();
            Ok(())
        })??;

        for (effect, name) in [
            (Effect::Ripple, "ripple"),
            (Effect::Fluid, "fluid"),
            (Effect::Elastic, "elastic"),
            (Effect::Afterglow, "afterglow"),
        ] {
            handle.update(cx, |workspace, window, cx| -> Result<()> {
                press(workspace, "f6", window, cx)?;
                ensure!(workspace.projects.active().status.contains(effect.label()));
                ensure!(workspace.accepts_native_text());
                press(workspace, "home", window, cx)
            })??;
            cx.update_window(handle.into(), |_, window, cx| {
                advance_frames(window, cx, 16)
            })?;
            let (clip, target, scale) =
                handle.update(cx, |workspace, window, cx| -> Result<_> {
                    press(workspace, "end", window, cx)?;
                    caret(workspace).map(|(clip, target)| (clip, target, window.scale_factor()))
                })??;
            let moving = capture_field(handle, cx, clip, 3)?;
            let image = capture_field(handle, cx, clip, 16)?;
            ensure!(
                moving != image,
                "{surface} {name} did not animate across the field"
            );
            let device = |value: Pixels| (f32::from(value) * scale).round() as u32;
            let x = device(target.x) - device(clip.origin.x);
            let row = image.height() / 2;
            ensure!(
                (x.saturating_sub(2)..(x + 5).min(image.width())).any(|x| {
                    let pixel = image.get_pixel(x, row);
                    pixel[1] >= 100 && pixel[1] > pixel[2]
                }),
                "{surface} {name} cursor did not paint at the native input anchor"
            );
            moving.save(evidence.join(format!("{surface}-{name}-moving.png")))?;
            image.save(evidence.join(format!("{surface}-{name}.png")))?;
            println!(
                "{surface} {name}: native caret remains visible at the field's layout position"
            );
        }
        if surface == "semantic" {
            handle.update(cx, |workspace, window, cx| -> Result<()> {
                let (old_clip, old_caret) = caret(workspace)?;
                semantic_field(workspace, window, "Consolas", 23., 8.5)?;
                workspace.sync(window, cx);
                let (clip, target) = caret(workspace)?;
                ensure!(clip != old_clip && target != old_caret);
                ensure!(clip.contains(&target));
                ensure!(workspace.canvas_input_caret_bounds().is_some_and(|native| {
                    (f32::from(native.origin.x - target.x)).abs() < 0.01
                        && native.size.height == clip.size.height
                }));
                let previous = workspace
                    .current_native_field()
                    .cloned()
                    .context("native layout")?;
                let initial_scale = window.scale_factor();
                for scale in [1., 1.25, 1.5, 2.] {
                    window.set_scale_factor(scale);
                    workspace.sync(window, cx);
                    let field = workspace
                        .current_native_field()
                        .context("scaled native layout")?;
                    ensure!(field.clip == clip);
                    let logical = workspace
                        .canvas_input_caret_bounds()
                        .context("scaled logical caret")?;
                    ensure!(clip.contains(&logical.origin));
                    if scale != initial_scale {
                        ensure!(!std::rc::Rc::ptr_eq(&previous, field));
                    }
                }
                window.set_scale_factor(initial_scale);
                workspace.sync(window, cx);
                Ok(())
            })??;
        }
    }
    Ok(())
}

fn semantic_field(
    workspace: &mut Workspace,
    window: &Window,
    font: &str,
    size: f32,
    x: f32,
) -> Result<()> {
    let key = workspace
        .plugins
        .surface
        .key
        .as_ref()
        .context("Canvas owner")?;
    let view = Arc::make_mut(&mut workspace.plugins.view);
    let surface = view.surfaces.get_mut(key).context("Canvas surface")?;
    let canvas = Arc::make_mut(&mut surface.frame)
        .canvas
        .as_mut()
        .context("Canvas scene")?;
    canvas.input = None;
    canvas.text_input = Some(serde_json::from_value(serde_json::json!({
        "bounds": [x, 49.5, 179.25, 25.5],
        "font": font, "size": size,
    }))?);
    surface.revision += 1;
    workspace.update_canvas_text_layout(window);
    Ok(())
}

fn advance_frames(window: &mut Window, cx: &mut App, frames: usize) {
    for _ in 0..frames {
        std::thread::sleep(Duration::from_millis(22));
        window.simulate_next_frame(cx);
    }
}

fn capture_field(
    handle: WindowHandle<Workspace>,
    cx: &mut App,
    clip: Bounds<Pixels>,
    frames: usize,
) -> Result<image::RgbaImage> {
    cx.update_window(handle.into(), |_, window, cx| {
        advance_frames(window, cx, frames);
        window.draw(cx).clear(cx);
        let image = window.render_to_image()?;
        let scale = window.scale_factor();
        let device = |value: Pixels| (f32::from(value) * scale).round() as u32;
        let (left, top) = (device(clip.left()), device(clip.top()));
        let (right, bottom) = (device(clip.right()), device(clip.bottom()));
        ensure!(
            right <= image.width() && bottom <= image.height() && right > left && bottom > top,
            "The hidden DirectX target has not reached the field viewport"
        );
        Ok(image::imageops::crop_imm(&image, left, top, right - left, bottom - top).to_image())
    })?
}

fn install_surface(workspace: &mut Workspace, canvas: bool) -> Result<()> {
    let project = workspace.projects.active().plugin_scope;
    let key = session::Key {
        namespace: "cursor-test".into(),
        instance: Instance {
            project,
            module: "field".into(),
        },
    };
    let drawing = canvas.then(|| {
        Arc::new(PreparedCanvas {
            paths: Vec::new(),
            assets: BTreeMap::new(),
            resource_bytes: 0,
            reports_underlines: false,
        })
    });
    let canvas = canvas
        .then(|| {
            serde_json::from_value(serde_json::json!({
                "input": [28, 54, 180, 22],
                "draw": [{"op":"text","origin":[20,6],"text":"Static Canvas label","size":16,
                          "line_height":20,"runs":[]}],
            }))
        })
        .transpose()?;
    let frame = Arc::new(GridSurface {
        title: "Plugin field".into(),
        lines: vec!["Static grid label".into()],
        input: Some(String::new()),
        canvas,
    });
    let draft = "A中文👩‍💻".repeat(80);
    workspace.plugins.view = Arc::new(session::View {
        surfaces: BTreeMap::from([(
            key.clone(),
            session::Surface {
                generation: 1,
                revision: 1,
                frame,
                drawing,
                draft: draft.clone(),
            },
        )]),
        ..session::View::default()
    });
    workspace.plugins.active = Some(project);
    workspace.plugins.open = false;
    workspace.plugins.surface.key = Some(key);
    workspace.plugins.surface.focused = false;
    workspace.plugins.surface.field = Default::default();
    workspace.plugins.surface.draft = draft;
    Ok(())
}

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

fn caret(workspace: &Workspace) -> Result<(Bounds<Pixels>, Point<Pixels>)> {
    let (_, area, field) = workspace
        .plugin_cursor_panel()
        .context("plugin cursor viewport")?;
    let field = field.context("cursor head must use the host field's clip")?;
    ensure!(area.pixels(workspace.metrics).contains(&field.origin));
    let (x, y, _, insert, visible) = workspace.plugin_caret().context("plugin caret")?;
    ensure!(insert && visible);
    let metrics = workspace.metrics;
    Ok((field, metrics.at(x, y)))
}

/// Restore the application's locale after native layout assertions or image capture.
struct RestoreLocale(Locale);

impl Drop for RestoreLocale {
    fn drop(&mut self) {
        i18n::set_locale(self.0);
    }
}

#[test]
#[ignore = "requires the Windows native renderer and serialized locale changes"]
fn plugin_search_buttons_keep_key_underline_and_pointer_actions_in_both_languages() -> Result<()> {
    run_workspace(async |handle, cx| {
        cx.update(|cx| {
            let _locale = RestoreLocale(i18n::locale());
            for (locale, title) in [
                (Locale::English, "Plugins"),
                (Locale::SimplifiedChinese, "插件"),
            ] {
                i18n::set_locale(locale);
                ensure!(i18n::text("plugin_manager.title") == title);
                let (area, row_height, input_row) =
                    handle.update(cx, |workspace, window, cx| {
                        workspace.plugins.open = true;
                        search(workspace);
                        workspace.sync(window, cx);
                        let area = workspace.plugin_manager_area();
                        (
                            area.pixels(workspace.metrics),
                            px(workspace.metrics.lh),
                            workspace
                                .plugin_manager_view()
                                .plugin_list_layout(area)
                                .footer
                                .y as f32,
                        )
                    })?;
                let underlines = cx.update_window(handle.into(), |_, window, cx| {
                    window.draw(cx).clear(cx);
                    let scale = window.scale_factor();
                    let left = area.center().x.scale(scale);
                    let row = (area.origin.y + row_height * input_row).scale(scale)
                        ..(area.origin.y + row_height * (input_row + 1.)).scale(scale);
                    window
                        .painted_underlines()
                        .into_iter()
                        .filter(|line| {
                            line.bounds.origin.x > left && row.contains(&line.bounds.origin.y)
                        })
                        .collect::<Vec<_>>()
                })?;
                ensure!(
                    underlines.len() == 2,
                    "{title} did not draw both underlined key names"
                );
                ensure!(underlines[0].bounds.size.width > underlines[1].bounds.size.width);

                handle.update(cx, |workspace, window, cx| -> Result<()> {
                    verify_search_buttons(workspace, window, cx)
                })??;
            }
            Ok(())
        })
    })
}

/// Verify the moved input row shares its actions with the keyboard and retains IME ownership.
fn verify_search_buttons(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Result<()> {
    use gpui::EntityInputHandler;

    let area = workspace.plugin_manager_area();
    let layout = workspace.plugin_manager_view().plugin_list_layout(area);
    for (button, key, result) in [(0, "enter", "编辑器"), (1, "escape", "previous")] {
        search(workspace);
        press(workspace, key, window, cx)?;
        ensure!(workspace.plugins.manager.search == result && !workspace.accepts_native_text());
        search(workspace);
        let input = workspace
            .plugins
            .manager
            .prompt
            .as_ref()
            .context("Filter prompt")?;
        let slot = &layout
            .prompt(&input.kind, &workspace.spacing)
            .button_columns()[button];
        click_manager(
            workspace,
            (slot.start + slot.len() / 2, layout.footer.y),
            1,
            window,
            cx,
        )?;
        ensure!(workspace.plugins.manager.search == result && !workspace.accepts_native_text());
    }

    // Switch tabs through their blank cells before reopening the footer input.
    let tabs = layout.tabs(&workspace.plugins.manager, &workspace.spacing);
    ensure!(tabs.visible().count() == 3);
    let zip = tabs.hit_columns(2);
    click_manager(workspace, (zip.end - 1, layout.list.y), 1, window, cx)?;
    ensure!(workspace.plugins.manager.page == super::Page::Zip && !workspace.accepts_native_text());
    let installed = tabs.hit_columns(0);
    let divider = installed.end;
    click_manager(workspace, (divider, layout.list.y), 1, window, cx)?;
    ensure!(workspace.plugins.manager.page == super::Page::Zip);
    click_manager(workspace, (installed.start, layout.list.y), 1, window, cx)?;
    ensure!(workspace.plugins.manager.page == super::Page::Installed);
    press(workspace, "/", window, cx)?;
    ensure!(workspace.accepts_native_text());
    ensure!(workspace.plugin_manager_area() == area);
    let input = workspace
        .plugins
        .manager
        .prompt
        .as_ref()
        .context("Filter prompt")?;
    ensure!(matches!(input.kind, Prompt::Search { .. }) && input.value == "previous");
    let field = layout.prompt(&input.kind, &workspace.spacing).input;
    let (x, y, _, insert, visible) = workspace.plugin_caret().context("Filter caret")?;
    ensure!(insert && visible && y == (area.y + layout.footer.y) as f32);
    ensure!(((area.x + field.start) as f32..(area.x + field.end) as f32).contains(&x));
    workspace.replace_and_mark_text_in_range(None, "ni", Some(2..2), window, cx);
    press(workspace, "escape", window, cx)?;
    ensure!(!workspace.accepts_native_text() && workspace.plugins.composition.is_empty());
    press(workspace, "3", window, cx)?;
    ensure!(workspace.plugins.manager.page == super::Page::Zip);
    press(workspace, "enter", window, cx)?;
    ensure!(
        workspace
            .file_picker
            .as_ref()
            .is_some_and(|picker| picker.choosing_file())
    );
    ensure!(!workspace.plugins.open && !workspace.accepts_native_text());
    press(workspace, "escape", window, cx)?;
    ensure!(workspace.file_picker.is_none() && workspace.plugins.open);
    ensure!(workspace.plugins.manager.page == super::Page::Zip);
    Ok(())
}

#[test]
#[ignore = "requires the Windows native renderer"]
fn plugin_zip_picker_reuses_browsing_and_returns_to_the_manager() -> Result<()> {
    use gpui::EntityInputHandler;

    run_workspace(async |handle, cx| {
        let (root, nested, archive) = cx.update(|cx| {
            handle.update(cx, |workspace, window, cx| -> Result<_> {
                let root = workspace.projects.active().root.canonicalize()?;
                let nested = root.join("bundles.zip");
                fs::create_dir(&nested)?;
                let archive = nested.join("测试.ZIP");
                fs::write(&archive, b"PK\x03\x04\0")?;
                fs::write(nested.join("测试.txt"), "preview fixture")?;
                press(workspace, "space", window, cx)?;
                press(workspace, "shift-e", window, cx)?;
                Ok((root, nested, archive))
            })
        })??;

        let ordinary_location = root.parent().context("project parent")?;
        wait_for_picker_directory(handle, cx, ordinary_location).await?;
        cx.update(|cx| {
            handle.update(cx, |workspace, window, cx| -> Result<()> {
                workspace.plugins.open = true;
                press(workspace, "3", window, cx)?;
                press(workspace, "enter", window, cx)?;
                Ok(())
            })
        })??;

        wait_for_picker_directory(handle, cx, &root).await?;
        cx.update(|cx| {
            handle.update(cx, |workspace, window, cx| -> Result<()> {
                let picker = workspace.file_picker.as_ref().context("ZIP picker")?;
                ensure!(picker.choosing_file() && picker.state.visible.len() == 1);
                ensure!(
                    picker
                        .state
                        .entry()
                        .is_some_and(|entry| entry.path == nested)
                );
                press(workspace, "enter", window, cx)?;
                ensure!(workspace.projects.active().root == root);
                Ok(())
            })
        })??;

        wait_for_picker_directory(handle, cx, &nested).await?;
        cx.update(|cx| {
            handle.update(cx, |workspace, window, cx| -> Result<()> {
                press(workspace, "f", window, cx)?;
                ensure!(workspace.accepts_native_text());
                workspace.replace_and_mark_text_in_range(None, "ce", Some(2..2), window, cx);
                workspace.replace_text_in_range(None, "测试", window, cx);
                press(workspace, "enter", window, cx)?;
                ensure!(!workspace.accepts_native_text());
                let state = &workspace.file_picker.as_ref().context("ZIP picker")?.state;
                ensure!(state.filter == "测试" && state.composition.is_empty());
                ensure!(state.visible.len() == 1);
                ensure!(state.entry().is_some_and(|entry| entry.path == archive));
                press(workspace, "enter", window, cx)?;
                ensure!(workspace.file_picker.is_none() && workspace.plugins.open);
                ensure!(workspace.plugins.manager.page == super::Page::Zip);
                ensure!(workspace.projects.active().root == root);
                ensure!(workspace.file_picker_directory.as_deref() == Some(ordinary_location));

                // Feed the real supervisor's published location into this session-less window.
                Arc::make_mut(&mut workspace.plugins.view).local_zip_directory =
                    Some(nested.clone());
                press(workspace, "enter", window, cx)?;
                Ok(())
            })
        })??;
        wait_for_picker_directory(handle, cx, &nested).await?;
        cx.update(|cx| {
            handle.update(cx, |workspace, window, cx| -> Result<()> {
                press(workspace, "h", window, cx)
            })
        })??;
        wait_for_picker_directory(handle, cx, &root).await?;
        cx.update(|cx| {
            handle.update(cx, |workspace, window, cx| -> Result<()> {
                press(workspace, "escape", window, cx)?;
                press(workspace, "enter", window, cx)
            })
        })??;
        wait_for_picker_directory(handle, cx, &nested).await?;
        cx.update(|cx| {
            handle.update(cx, |workspace, window, cx| -> Result<()> {
                press(workspace, "escape", window, cx)?;
                press(workspace, "escape", window, cx)?;
                press(workspace, "space", window, cx)?;
                press(workspace, "shift-e", window, cx)?;
                Ok(())
            })
        })??;
        wait_for_picker_directory(handle, cx, ordinary_location).await?;
        cx.update(|cx| {
            handle.update(cx, |workspace, window, cx| {
                press(workspace, "l", window, cx)
            })
        })??;
        wait_for_picker_directory(handle, cx, &root).await?;
        cx.update(|cx| {
            handle.update(cx, |workspace, window, cx| -> Result<()> {
                let picker = workspace.file_picker.as_ref().context("ordinary picker")?;
                ensure!(!picker.choosing_file() && picker.state.visible.len() == 2);
                press(workspace, "shift-g", window, cx)?;
                press(workspace, "enter", window, cx)?;
                ensure!(workspace.file_picker.is_none());
                ensure!(
                    workspace.projects.active().editor.vim().doc.path.as_deref()
                        == Some(root.join("README.md").as_path())
                );
                Ok(())
            })
        })?
    })
}

/// Wait for the native window's background directory load to reveal `directory`.
async fn wait_for_picker_directory(
    handle: WindowHandle<Workspace>,
    cx: &mut AsyncApp,
    directory: &Path,
) -> Result<()> {
    for _ in 0..100 {
        if cx.update(|cx| {
            handle.update(cx, |workspace, _, _| {
                workspace.file_picker.as_ref().is_some_and(|picker| {
                    !picker.state.busy && picker.state.directory() == Some(directory)
                })
            })
        })? {
            return Ok(());
        }
        cx.background_executor()
            .timer(Duration::from_millis(20))
            .await;
    }
    anyhow::bail!("Picker did not load {}", directory.display());
}

/// Send a press to a cell of the visible manager and keep the window's real grid transform.
fn click_manager(
    workspace: &mut Workspace,
    (column, row): (usize, usize),
    count: usize,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Result<()> {
    let area = workspace.plugin_manager_area();
    let position = workspace
        .metrics
        .at((area.x + column) as f32 + 0.5, (area.y + row) as f32 + 0.5);
    ensure!(workspace.plugin_click(
        &MouseDownEvent {
            position,
            click_count: count,
            ..Default::default()
        },
        window,
        cx
    ));
    Ok(())
}

/// Scroll to the last completion, reverse at its boundary and click the visible top candidate.
#[test]
#[ignore = "requires the Windows native renderer"]
fn native_completion_wheel_and_click_share_the_visible_rows() -> Result<()> {
    use crate::grid::workspace::layout::Focus;
    use gpui::{ScrollDelta, ScrollEvent};
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    fs::create_dir(&project)?;
    let options = session::Options::take(
        &mut vec![
            "--no-plugins".into(),
            "--plugin-profile".into(),
            temp.path().join("profile").into_os_string(),
        ],
        temp.path(),
    )?;
    native_test::run_async_visible(
        project,
        options,
        |ws| ws.projects.active_mut().focus_pane(Focus::Editor),
        async move |handle, app| {
            let _keep_temp = temp;
            app.update(|app| {
                handle.update(app, |ws, window, cx| -> Result<()> {
                    ws.sync(window, cx);
                    press(ws, "i", window, cx)?;
                    ws.show_insert_candidates(
                        0,
                        (0..30).map(|i| format!("item{i}")).collect(),
                        false,
                        cx,
                    );
                    let (menu, _, _) = ws
                        .completion_menu(window)
                        .context("visible completion list")?;
                    let position = menu.bounds().center();
                    for (delta, expected) in [(-64., 29), (-1., 29), (2., 27)] {
                        ws.scroll(
                            &ScrollEvent {
                                position,
                                delta: ScrollDelta::Lines(point(0., delta)),
                                ..Default::default()
                            },
                            window,
                            cx,
                        );
                        ensure!(
                            ws.completion_items().context("wheel dismissed popup")?.1 == expected
                        );
                    }
                    ensure!(ws.projects.active().editor.buffer().scroll.target() == 0);
                    Ok(())
                })
            })??;
            let deadline = std::time::Instant::now() + Duration::from_secs(6);
            loop {
                let top = app.update(|app| {
                    handle.update(app, |ws, window, _| {
                        let (menu, ..) = ws.completion_menu(window)?;
                        let bounds = menu.bounds();
                        menu.hit(point(bounds.center().x, bounds.top() + px(1.)), ws.metrics)
                    })
                })?;
                if top.is_some_and(|index| index > 0) {
                    break;
                }
                ensure!(
                    std::time::Instant::now() < deadline,
                    "completion viewport did not scroll"
                );
                app.background_executor()
                    .timer(Duration::from_millis(12))
                    .await;
            }
            app.update(|app| {
                handle.update(app, |ws, window, cx| -> Result<()> {
                    let (menu, labels, _) = ws.completion_menu(window).context("scrolled popup")?;
                    let bounds = menu.bounds();
                    let position = point(bounds.center().x, bounds.top() + px(1.));
                    let index = menu.hit(position, ws.metrics).context("painted top row")?;
                    let expected = labels[index].to_owned();
                    ws.mouse_down(
                        &MouseDownEvent {
                            button: MouseButton::Left,
                            position,
                            click_count: 1,
                            ..Default::default()
                        },
                        window,
                        cx,
                    );
                    ensure!(ws.completion_items().is_none());
                    ensure!(
                        ws.projects
                            .active()
                            .editor
                            .vim()
                            .doc
                            .text
                            .starts_with(&expected)
                    );
                    Ok(())
                })
            })?
        },
    )
}

/// Verify that the list can scroll and open the row that the cursor and mouse show as selected.
#[test]
#[ignore = "requires the Windows native renderer"]
fn plugin_list_uses_the_same_rows_for_render_caret_and_clicks() -> Result<()> {
    run_workspace(async |handle, cx| {
        cx.update(|app| {
            handle.update(app, |workspace, window, cx| -> Result<()> {
                workspace.plugins.view = Arc::new(manager_list_view()?);
                workspace.plugins.open = true;
                workspace.sync(window, cx);
                let area = workspace.plugin_manager_area();
                let list = workspace.plugin_manager_view().plugin_list_layout(area);
                ensure!(list.entries.h > 1 && list.entries.h < 30);
                press(workspace, "end", window, cx)?;
                let (x, y, _, insert, visible) = workspace.plugin_caret().context("List caret")?;
                ensure!(visible && !insert && x == (area.x + list.entries.x) as f32);
                ensure!(y == (area.y + list.entries.y + list.entries.h - 1) as f32);
                press(workspace, "/", window, cx)?;
                ensure!(workspace.plugin_commit("Rust", window, cx));
                press(workspace, "escape", window, cx)?;
                ensure!(workspace.plugins.manager.search.is_empty());
                ensure!(workspace.plugin_caret() == Some((x, y, 1., insert, visible)));
                let first = workspace
                    .plugin_manager_view()
                    .plugin_list(list.entries.h, 30)
                    .range()
                    .start;
                click_manager(workspace, (list.entries.x, list.entries.y), 2, window, cx)?;
                let record = &workspace.plugins.view.installed[first];
                let namespace = record.release.manifest.identity().namespace();
                ensure!(workspace.plugins.manager.detail.as_deref() == Some(namespace.as_str()));
                ensure!(workspace.plugin_manager_area() == area);
                let overview_end = 2 + workspace
                    .spacing
                    .columns(i18n::text("plugin_manager.overview"));
                click_manager(workspace, (overview_end + 2, 2), 1, window, cx)?;
                ensure!(workspace.plugins.manager.detail_page == super::Detail::Modules);
                click_manager(workspace, (overview_end, 2), 1, window, cx)?;
                ensure!(workspace.plugins.manager.detail_page == super::Detail::Overview);
                press(workspace, "q", window, cx)?;
                ensure!(
                    workspace.plugins.manager.detail.is_none()
                        && workspace.plugins.manager.selection == first
                );
                for (cols, rows, has_feedback) in [
                    (30, 9, false),
                    (35, 12, true),
                    (56, 13, false),
                    (92, 25, true),
                    (142, 40, false),
                    (240, 90, true),
                ] {
                    let area = super::ManagerWindow::area(cols, rows);
                    let list = ListLayout::new(area, has_feedback);
                    ensure!(area.x + area.w <= cols && area.y + area.h <= rows);
                    ensure!(area.w <= 112 && area.h <= 32);
                    ensure!((cols - area.w).abs_diff(2 * area.x) <= 1);
                    ensure!((rows - area.h).abs_diff(2 * area.y) <= 1);
                    let filter = list.prompt(
                        &Prompt::Search {
                            original: String::new(),
                            selection: 0,
                            offset: 0,
                        },
                        &workspace.spacing,
                    );
                    ensure!(list.entries.h > 0 && !filter.input.is_empty());
                    ensure!(filter.input.end < filter.button_columns()[0].start);
                    ensure!(filter.button_columns()[1].end == list.footer.x + list.footer.w);
                    ensure!(
                        list.tabs(&workspace.plugins.manager, &workspace.spacing)
                            .visible()
                            .count()
                            == 3
                    );
                    ensure!(list.list.y + list.list.h == list.description.y);
                    if list.description.h > 0 {
                        let slot = list.description.border_title_columns(None);
                        let title = list.description_title(&workspace.spacing);
                        ensure!(title.start == slot.start && title.end <= slot.end);
                        ensure!(!title.is_empty() && list.description.h <= 4);
                    }
                    ensure!(list.description.y + list.description.h <= list.footer.y);
                    ensure!(list.footer.y + list.footer.h == area.h - 1);
                }
                Ok(())
            })?
        })?;
        if let Some(evidence) = std::env::var_os("NANOBUG_PLUGIN_LAYOUT_EVIDENCE") {
            capture_plugin_manager(handle, cx, Path::new(&evidence)).await?;
        }
        cx.update(|app| handle.update(app, verify_manager_title_closes))?
    })
}

/// Read the title strip from the first frame drawn after each numeric shortcut.
#[test]
#[ignore = "requires the Windows native renderer"]
fn plugin_tabs_repaint_on_the_first_keypress() -> Result<()> {
    run_workspace(async |handle, cx| {
        let list_row = cx.update(|app| {
            handle.update(app, |workspace, window, cx| -> Result<_> {
                workspace.plugins.view = Arc::new(manager_list_view()?);
                workspace.plugins.open = true;
                workspace.sync(window, cx);
                let area = workspace.plugin_manager_area();
                Ok(workspace
                    .plugin_manager_view()
                    .plugin_list_layout(area)
                    .list
                    .y)
            })
        })??;

        let capture = |app: &mut App, row| -> Result<image::RgbaImage> {
            let (bounds, scale) = handle.update(app, |workspace, window, _| {
                let mut area = workspace.plugin_manager_area();
                area.y += row;
                area.h = 1;
                (area.pixels(workspace.metrics), window.scale_factor())
            })?;
            app.update_window(handle.into(), |_, window, app| {
                window.draw(app).clear(app);
                let image = window.render_to_image()?;
                let to_pixel = |value: Pixels| (f32::from(value) * scale).round() as u32;
                Ok(image::imageops::crop_imm(
                    &image,
                    to_pixel(bounds.left()),
                    to_pixel(bounds.top()),
                    to_pixel(bounds.size.width),
                    to_pixel(bounds.size.height),
                )
                .to_image())
            })?
        };

        for (row, keys) in [(list_row, &["3", "2", "1"][..]), (2, &["2", "3", "4", "1"])] {
            if row == 2 {
                cx.update(|app| {
                    handle.update(app, |workspace, window, cx| {
                        press(workspace, "enter", window, cx)?;
                        ensure!(workspace.plugins.manager.detail.is_some());
                        Ok::<_, anyhow::Error>(())
                    })
                })??;
            }
            let mut before = cx.update(|app| capture(app, row))?;
            for &binding in keys {
                cx.update(|app| {
                    handle.update(app, |workspace, window, cx| {
                        press(workspace, binding, window, cx)
                    })
                })??;
                let after = cx.update(|app| capture(app, row))?;
                ensure!(
                    before.pixels().zip(after.pixels()).any(|(before, after)| {
                        before.0[..3]
                            .iter()
                            .zip(&after.0[..3])
                            .any(|(left, right)| left.abs_diff(*right) > 30)
                    }),
                    "first press of {binding} did not repaint tabs on row {row}"
                );
                before = after;
            }
        }
        Ok(())
    })
}

/// Use the workspace's middle button route with live caption positions after each close.
fn verify_manager_title_closes(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Result<()> {
    use super::Page;

    for (page, next) in [
        (Page::Zip, Some(Page::Installed)),
        (Page::Installed, Some(Page::Marketplace)),
        (Page::Marketplace, None),
    ] {
        let area = workspace.plugin_manager_area();
        let layout = workspace.plugin_manager_view().plugin_list_layout(area);
        let tabs = layout.tabs(&workspace.plugins.manager, &workspace.spacing);
        let hit = tabs.hit_columns(page as usize);
        ensure!(!hit.is_empty());
        let position = workspace.metrics.at(
            (area.x + hit.end - 1) as f32 + 0.5,
            (area.y + layout.list.y) as f32 + 0.5,
        );
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
        ensure!(workspace.plugins.open == next.is_some());
        if let Some(next) = next {
            ensure!(workspace.plugins.manager.page == next);
            let tabs = layout.tabs(&workspace.plugins.manager, &workspace.spacing);
            ensure!(
                tabs.hit_columns(page as usize).is_empty()
                    && !tabs.hit_columns(next as usize).is_empty()
            );
        }
    }
    Ok(())
}

/// Provide package rows with different versions, enablement, and one matching catalog update.
fn manager_list_view() -> Result<session::View> {
    let names = ["Rust", "Vue", "Markdown"];
    let versions = ["0.1.0", "0.2.1", "0.1.1"];
    let mut installed = Vec::new();
    for index in 0..30 {
        let name = names.get(index).map_or_else(|| {
            format!("Plugin {index:02} with a long display name to check the width of the Name column")
        }, |name| (*name).to_owned());
        installed.push(serde_json::from_value::<nanobug::plugins::package::Installed>(serde_json::json!({
            "release": {
                "manifest": {
                    "name": name, "id": format!("fixture-{index}"), "version": versions.get(index).unwrap_or(&"0.1.0"),
                    "engines": { "nanobug": "^0.1.0" },
                    "description": "Preview Rust files, show syntax and jump to symbols. ".repeat(6),
                    "translations": { "zh-CN": { "description": "为当前项目提供语法高亮、符号跳转与工作区导航。用于检查长描述在字符面板中能显示两行，并在空间用尽时加上省略号。".repeat(3) } }
                },
                "digest": "fixture", "files": {}, "source": "fixture.zip"
            },
            "grants": [], "enabled": index != 1
        }))?);
    }
    let catalog = vec![session::CatalogEntry {
        identity: installed[2].release.manifest.identity(),
        version: "0.2.0".parse()?,
        description: "A newer Markdown release".into(),
        zip: "markdown.zip".into(),
    }];
    Ok(session::View {
        installed: Arc::new(installed),
        catalog: Arc::new(catalog),
        ..Default::default()
    })
}

/// Save the full workspace for `handle` through DirectX in `evidence` using the native app `cx`.
async fn capture_plugin_manager(
    handle: WindowHandle<Workspace>,
    cx: &mut AsyncApp,
    evidence: &Path,
) -> Result<()> {
    fs::create_dir_all(evidence)?;
    let _locale = RestoreLocale(i18n::locale());
    for (locale, name, width, height, filtering, detail) in [
        (Locale::English, "manager-en.png", 1120., 680., false, false),
        (
            Locale::SimplifiedChinese,
            "manager-zh.png",
            1120.,
            680.,
            false,
            false,
        ),
        (
            Locale::SimplifiedChinese,
            "manager-detail-zh.png",
            1120.,
            680.,
            false,
            true,
        ),
        (
            Locale::SimplifiedChinese,
            "manager-capped-zh.png",
            1600.,
            1000.,
            false,
            false,
        ),
        (
            Locale::SimplifiedChinese,
            "manager-detail-capped-zh.png",
            1600.,
            1000.,
            false,
            true,
        ),
        (
            Locale::SimplifiedChinese,
            "manager-filter-zh.png",
            1120.,
            680.,
            true,
            false,
        ),
        (
            Locale::SimplifiedChinese,
            "manager-compact-zh.png",
            360.,
            300.,
            true,
            false,
        ),
        (
            Locale::SimplifiedChinese,
            "manager-detail-compact-zh.png",
            360.,
            300.,
            false,
            true,
        ),
    ] {
        cx.update(|cx| {
            handle.update(cx, |_, window, _| {
                window.resize(size(px(width), px(height)))
            })
        })?;
        cx.background_executor()
            .timer(Duration::from_millis(60))
            .await;
        cx.update(|cx| -> Result<()> {
            handle.update(cx, |workspace, window, cx| -> Result<()> {
                i18n::set_locale(locale);
                workspace.plugins.manager.selection = 0;
                workspace.plugins.manager.detail = None;
                workspace.plugins.manager.prompt = None;
                workspace.plugins.manager.search.clear();
                workspace.sync(window, cx);
                let area = workspace.plugin_manager_area();
                if detail {
                    press(workspace, "enter", window, cx)?;
                    ensure!(workspace.plugins.manager.detail.is_some());
                    ensure!(workspace.plugin_manager_area() == area);
                }
                if filtering && workspace.plugins.manager.prompt.is_none() {
                    press(workspace, "/", window, cx)?;
                    ensure!(workspace.plugin_commit("Rust", window, cx));
                }
                // Keep the still image focused on layout; caret geometry is verified above.
                let cell_width = workspace.metrics.cw;
                workspace.cursor.update(cx, |cursor, cx| {
                    cursor.set(Default::default(), false, cell_width, false, window, cx);
                });
                window.refresh();
                Ok(())
            })??;
            Ok(())
        })?;
        cx.update(|cx| -> Result<()> {
            cx.update_window(handle.into(), |_, window, cx| -> Result<()> {
                window.draw(cx).clear(cx);
                window.render_to_image()?.save(evidence.join(name))?;
                Ok(())
            })??;
            Ok(())
        })?;
    }
    cx.update(|cx| {
        handle.update(cx, |workspace, window, cx| {
            workspace.plugins.manager.detail = None;
            workspace.sync(window, cx);
        })
    })?;
    Ok(())
}

fn search(workspace: &mut Workspace) {
    workspace.plugins.manager.prompt = Some(Input::new(
        Prompt::Search {
            original: "previous".into(),
            selection: 0,
            offset: 0,
        },
        "编辑器".into(),
    ));
    workspace.plugins.manager.live_search();
}

#[test]
#[ignore = "requires a Windows desktop and a working DirectX device"]
fn canvas_svg_atlas_releases_retired_keys_in_real_workspace() -> Result<()> {
    run_workspace(async |handle, cx| cx.update(|cx| verify_svg_release(handle, cx)))
}

fn verify_svg_release(handle: WindowHandle<Workspace>, cx: &mut App) -> Result<()> {
    use crate::grid::workspace::editor_close::CloseTarget;
    use crate::ui::native_atlas::NativeAtlases;

    // A square source lets both native dimensions use the exact SVG key in the lookup.
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20"><rect width="20" height="20"/></svg>"#;
    let draw = |count| {
        serde_json::from_value::<Vec<nanobug_plugin_sdk::canvas::Draw>>(serde_json::Value::Array(
            [3.25, 31.25]
                .into_iter()
                .take(count)
                .map(|left| {
                    serde_json::json!({"op": "svg", "asset": "mark.svg",
                    "bounds": [left, 3.25, 20.0, 20.0], "color": 0xffffffffu32})
                })
                .collect(),
        ))
    };
    for (scale, close) in [(1., false), (1.25, true), (1.5, false), (2., true)] {
        let (root, tab_count) = handle.update(cx, |workspace, _, _| {
            (
                workspace.projects.active().root.clone(),
                workspace.projects.len(),
            )
        })?;
        let fixture = root.join(format!("atlas-project-{scale}"));
        fs::create_dir(&fixture)?;
        let key = handle.update(cx, |workspace, window, cx| -> Result<_> {
            let (index, _) = workspace.projects.add(fixture, None);
            ensure!(workspace.select_tab(index, window, cx));
            install_surface(workspace, true)?;
            let key = workspace
                .plugins
                .surface
                .key
                .clone()
                .context("active Canvas")?;
            let surface = Arc::make_mut(&mut workspace.plugins.view)
                .surfaces
                .get_mut(&key)
                .context("active Canvas frame")?;
            Arc::make_mut(&mut surface.frame)
                .canvas
                .as_mut()
                .context("Canvas drawing")?
                .draw = draw(2)?;
            surface.drawing = Some(Arc::new(PreparedCanvas {
                paths: Vec::new(),
                assets: BTreeMap::from([(
                    "mark.svg".into(),
                    Asset::Svg {
                        id: format!("nanobug-plugin-svg:probe-{scale}").into(),
                        bytes: svg.to_vec(),
                    },
                )]),
                resource_bytes: svg.len(),
                reports_underlines: false,
            }));
            window.set_scale_factor(scale);
            workspace.sync(window, cx);
            Ok(key)
        })??;

        let (atlas, keys) = cx
            .update_window(handle.into(), |_, window, cx| {
                window.draw(cx).clear(cx);
                cx.global::<NativeAtlases>()
                    .test_atlas(window.window_handle().window_id())
            })?
            .context("real native atlas")?;
        ensure!(
            keys.len() == 1,
            "both draws must share one SVG key at {scale}"
        );
        let key_params = &keys[0];
        let cached = || atlas.get_or_insert_with(key_params.clone().into(), &mut || Ok(None));
        let tile = cached()?.context("calculated SVG key must match GPUI at native scale")?;

        handle.update(cx, |workspace, window, cx| -> Result<()> {
            let surface = Arc::make_mut(&mut workspace.plugins.view)
                .surfaces
                .get_mut(&key)
                .context("active Canvas frame")?;
            Arc::make_mut(&mut surface.frame)
                .canvas
                .as_mut()
                .context("Canvas drawing")?
                .draw = draw(1)?;
            workspace.sync(window, cx);
            Ok(())
        })??;
        cx.update_window(handle.into(), |_, window, cx| {
            window.draw(cx).clear(cx);
            window.simulate_next_frame(cx);
        })?;
        ensure!(
            cached()? == Some(tile),
            "the surviving draw still uses the shared key"
        );

        handle.update(cx, |workspace, window, cx| -> Result<()> {
            if close {
                workspace.request_close(CloseTarget::Tab, window, cx);
                ensure!(
                    workspace.projects.len() == tab_count,
                    "fixture project did not close"
                );
                // The fixture supplies views directly; the runtime would remove the closed scope.
                Arc::make_mut(&mut workspace.plugins.view)
                    .surfaces
                    .remove(&key);
            } else {
                workspace.plugins.surface.key = None;
            }
            workspace.sync(window, cx);
            Ok(())
        })??;
        ensure!(
            cached()? == Some(tile),
            "the old scene remains until replaced"
        );
        cx.update_window(handle.into(), |_, window, cx| -> Result<()> {
            window.draw(cx).clear(cx);
            ensure!(
                cached()? == Some(tile),
                "retirement must be deferred past paint"
            );
            window.simulate_next_frame(cx);
            ensure!(
                cached()?.is_none(),
                "closed scene left a native SVG atlas entry at {scale}"
            );
            let (_, keys) = cx
                .global::<NativeAtlases>()
                .test_atlas(window.window_handle().window_id())
                .context("real native atlas")?;
            ensure!(keys.is_empty());
            Ok(())
        })??;
    }
    Ok(())
}

#[test]
#[ignore = "requires native Windows text and the inspectable GPUI test atlas"]
fn canvas_atlas_keeps_open_images_and_releases_all_frames_when_last_scene_closes() -> Result<()> {
    run_workspace(async |native, cx| {
        let (project, glyphs) = cx.update(|cx| {
            native.update(cx, |workspace, _, _| {
                (
                    workspace.projects.active().root.clone(),
                    Arc::clone(&workspace.glyphs),
                )
            })
        })?;
        let options = session::Options::take(
            &mut vec![
                "--plugin-dev-profile".into(),
                project.join("atlas-profile").into_os_string(),
            ],
            &project,
        )?;
        // DirectX draws both frames, but its pinned atlas does not implement cache inspection.
        let mut headless = gpui::HeadlessAppContext::new(Arc::clone(&glyphs));
        headless.update(|cx| cx.set_global(crate::ui::icons::IconCache::default()));
        let handle = headless.open_window(size(px(920.), px(580.)), |window, cx| {
            cx.new(|cx| {
                let mut workspace = Workspace::new(
                    crate::grid::WorkspaceTabs::new(vec![project]),
                    glyphs,
                    Default::default(),
                    window,
                    cx,
                    options,
                    None,
                    #[cfg(feature = "plugin-probe")]
                    None,
                );
                workspace.plugins.options = None;
                workspace
            })
        })?;
        headless.update(|cx| verify_image_release(handle, cx))
    })
}

fn verify_image_release(handle: WindowHandle<Workspace>, cx: &mut App) -> Result<()> {
    for _ in 0..4 {
        let image = Arc::new(RenderImage::new(smallvec::smallvec![
            image::Frame::new(image::RgbaImage::from_pixel(
                4,
                4,
                image::Rgba([90, 90, 90, 255])
            )),
            image::Frame::new(image::RgbaImage::from_pixel(
                4,
                4,
                image::Rgba([170, 170, 170, 255])
            )),
        ]));
        let weak = Arc::downgrade(&image);
        let (active, other) = handle.update(cx, |workspace, window, cx| -> Result<_> {
            install_surface(workspace, true)?;
            let key = workspace
                .plugins
                .surface
                .key
                .clone()
                .context("active Canvas")?;
            let view = Arc::make_mut(&mut workspace.plugins.view);
            let surface = view.surfaces.get_mut(&key).context("active Canvas frame")?;
            let frame = Arc::make_mut(&mut surface.frame)
                .canvas
                .as_mut()
                .context("Canvas drawing")?;
            frame.draw = serde_json::from_value(serde_json::json!([
                {"op": "image", "bounds": [10, 10, 30, 30], "asset": "image.gif", "frame": 0},
                {"op": "image", "bounds": [50, 10, 30, 30], "asset": "image.gif", "frame": 1},
            ]))?;
            surface.drawing = Some(Arc::new(PreparedCanvas {
                paths: Vec::new(),
                assets: BTreeMap::from([("image.gif".into(), Asset::Image(Arc::clone(&image)))]),
                resource_bytes: 4 * 4 * 4 * 2,
                reports_underlines: false,
            }));
            let mut other = key.clone();
            other.instance.project += 1;
            let retained = surface.clone();
            view.surfaces.insert(other.clone(), retained);
            workspace.plugins.collect_retired_images();
            workspace.sync(window, cx);
            Ok((key, other))
        })??;
        cx.update_window(handle.into(), |_, window, cx| -> Result<()> {
            window.draw(cx).clear(cx);
            ensure!(
                window.has_image_atlas_entry(&image),
                "both frame textures must be populated"
            );
            Ok(())
        })??;

        handle.update(cx, |workspace, window, cx| -> Result<()> {
            workspace.plugins.open = true;
            Arc::make_mut(&mut workspace.plugins.view)
                .surfaces
                .remove(&active);
            workspace.plugins.collect_retired_images();
            ensure!(
                window.has_image_atlas_entry(&image),
                "another open scene still retains the image"
            );
            Arc::make_mut(&mut workspace.plugins.view)
                .surfaces
                .remove(&other);
            workspace.plugins.collect_retired_images();
            ensure!(
                window.has_image_atlas_entry(&image),
                "the currently painted scene still holds frame textures until its replacement"
            );
            workspace.plugins.surface.key = None;
            workspace.sync(window, cx);
            Ok(())
        })??;
        cx.update_window(handle.into(), |_, window, cx| -> Result<()> {
            window.draw(cx).clear(cx);
            ensure!(window.has_image_atlas_entry(&image));
            window.simulate_next_frame(cx);
            ensure!(
                !window.has_image_atlas_entry(&image),
                "the replaced scene must release all frame textures"
            );
            Ok(())
        })??;
        drop(image);
        ensure!(
            weak.upgrade().is_none(),
            "prepared image pixels remained after the new view painted"
        );
    }
    Ok(())
}
