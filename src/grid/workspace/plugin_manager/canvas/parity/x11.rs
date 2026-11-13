//! Verify pixels presented by a native X11 window on a disposable test display.

use super::{
    AppContext, HEIGHT, NativeAssets, Result, RgbaImage, SceneView, Surface, WIDTH, WindowBounds,
    WindowOptions, compare_pixels, crop_scene, ensure, linux_text, point, prepare_fixture, px,
    rect, verify_font_measurement, verify_underlines,
};
use anyhow::Context as _;
use std::{
    cell::{Cell, RefCell},
    fs,
    path::Path,
    process::Command,
    rc::Rc,
    time::Duration,
};

#[test]
#[ignore = "requires an isolated X11 display, WGPU, Noto Color Emoji, xdotool and ImageMagick"]
fn sdk_canvas_matches_native_x11_window_pixels() -> Result<()> {
    ensure!(
        std::env::var_os("NANOBUG_PARITY_X11").is_some(),
        "Set NANOBUG_PARITY_X11=1 inside a disposable X11 display"
    );
    let platform = gpui_platform::current_platform(false);
    linux_text(&*platform)?;
    let (surface, assets) = prepare_fixture()?;
    let result = Rc::new(Cell::new(None));
    let saved = Rc::clone(&result);
    gpui::Application::new_inaccessible(platform).run(move |cx| {
        cx.spawn(async move |cx| {
            saved.set(Some(compare_x11(cx, surface, assets).await));
            cx.update(|cx| cx.quit());
        })
        .detach();
    });
    result.take().context("Native X11 Canvas app did not run")?
}

/// Let the native event loop present each scene before the second X11 client reads its pixels.
async fn compare_x11(
    cx: &mut gpui::AsyncApp,
    surface: Surface,
    assets: NativeAssets,
) -> Result<()> {
    let underlines = Rc::new(RefCell::new(None));
    let handle = cx.update(|cx| {
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(rect(
                    point(px(0.), px(0.)),
                    [0., 0., WIDTH, HEIGHT],
                ))),
                titlebar: None,
                ..Default::default()
            },
            |_, cx| {
                cx.new(|_| SceneView {
                    surface,
                    reference: Some(assets.clone()),
                    underlines: Rc::clone(&underlines),
                })
            },
        )
    })?;
    cx.background_executor()
        .timer(Duration::from_millis(200))
        .await;
    let scale = cx.update(|cx| {
        handle.update(cx, |_, window, _| -> Result<f32> {
            ensure!(window.gpu_specs().is_some(), "Native WGPU renderer missing");
            verify_font_measurement(window)?;
            Ok(window.scale_factor())
        })
    })??;
    let mut images = Vec::new();
    let mut reports = Vec::new();
    for reference in [true, false, true, false] {
        cx.update(|cx| {
            handle.update(cx, |view, window, cx| {
                view.reference = reference.then(|| assets.clone());
                window.refresh();
                cx.notify();
            })
        })?;
        cx.background_executor()
            .timer(Duration::from_millis(200))
            .await;
        images.push(crop_scene(
            capture()?,
            (WIDTH * scale).ceil() as u32,
            (HEIGHT * scale).ceil() as u32,
        )?);
        reports.push(underlines.take().context("X11 underline geometry")?);
    }
    let evidence =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("test-output/plugin-canvas-parity-x11");
    fs::create_dir_all(&evidence)?;
    images[2].save(evidence.join("native.png"))?;
    images[3].save(evidence.join("sdk.png"))?;
    verify_underlines(&reports[2], &reports[3], scale)?;
    compare_pixels(&images[2], &images[3], scale, &evidence)
}

fn capture() -> Result<RgbaImage> {
    let lookup = Command::new("xdotool")
        .args(["search", "--onlyvisible", "--pid"])
        .arg(std::process::id().to_string())
        .output()
        .context("Find the test window using xdotool")?;
    ensure!(lookup.status.success(), "Test window is missing");
    let ids = String::from_utf8(lookup.stdout)?;
    let mut ids = ids.lines();
    let id = ids.next().context("X11 test window ID is missing")?;
    ensure!(ids.next().is_none(), "More than one window for this test");
    let image = Command::new("import")
        .args(["-window", id, "png:-"])
        .output()
        .context("Capture the test window using ImageMagick import")?;
    ensure!(
        image.status.success(),
        "Capture failed: {}",
        String::from_utf8_lossy(&image.stderr)
    );
    Ok(image::load_from_memory_with_format(&image.stdout, image::ImageFormat::Png)?.to_rgba8())
}
