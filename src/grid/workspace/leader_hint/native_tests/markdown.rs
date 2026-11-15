//! Exercise Markdown image loading, placement and navigation in the native workspace.

mod alignment;
mod dimensions;
mod graphemes;
mod links;
mod scroll_sync;
mod splits;

use super::{MarkdownViewMode, Options, Workspace, native_test, press};
use anyhow::{Context as _, Result, ensure};
use gpui::{AppContext, AsyncApp, WindowHandle};
use image::{ImageFormat, Rgba, RgbaImage};
use std::{
    fs,
    io::{BufRead, BufReader, Cursor, Write},
    net::TcpListener,
    path::PathBuf,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

/// Inclusive device-pixel bounds of the visible pixels matching a fixture swatch `color`.
fn raster_color_bounds(frame: &RgbaImage, color: Rgba<u8>) -> Option<(u32, u32, u32, u32)> {
    let mut bounds: Option<(u32, u32, u32, u32)> = None;
    for (x, y, pixel) in frame.enumerate_pixels() {
        if pixel.0[..3]
            .iter()
            .zip(color.0)
            .all(|(sample, expected)| sample.abs_diff(expected) <= 5)
        {
            bounds = Some(bounds.map_or((x, y, x, y), |(left, top, right, bottom)| {
                (left.min(x), top.min(y), right.max(x), bottom.max(y))
            }));
        }
    }
    bounds
}

/// Distinct fill for the test PNG, visible over the editor palette.
const IMAGE_COLOR: Rgba<u8> = Rgba([231, 42, 173, 255]);

/// Dimensions of the PNG served by the fixture.
const IMAGE_SIZE: (u32, u32) = (72, 36);

/// Maximum wait for loopback requests and the corresponding native repaint.
const LOAD_DEADLINE: Duration = Duration::from_secs(8);

/// Pending images produce neither a raster nor the alternative text, and completion repaints.
#[test]
#[ignore = "requires the Windows DirectWrite and DirectX renderer"]
fn native_markdown_remote_images_replace_pending_content() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    fs::create_dir(&project)?;
    let mut png = Cursor::new(Vec::new());
    RgbaImage::from_pixel(IMAGE_SIZE.0, IMAGE_SIZE.1, IMAGE_COLOR)
        .write_to(&mut png, ImageFormat::Png)?;
    fs::write(project.join("local.png"), png.get_ref())?;
    let reference = project.join("Reference.md");
    let content = |missing: &str, valid: &str| {
        format!(
            "![Image unavailable 图片加载失败]({missing})\n\n---\n\n\
             ## Markdown 正文\n\nEnglish and 中文 mix in one preview.\n\n\
             ![This alternative is replaced by a raster]({valid})\n"
        )
    };
    fs::write(&reference, content("missing.png", "local.png"))?;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let origin = format!("http://{}", listener.local_addr()?);
    let readme = project.join("README.md");
    fs::write(
        &readme,
        content(&format!("{origin}/missing"), &format!("{origin}/redirect")),
    )?;
    let options = Options::take(
        &mut vec![
            "--plugin-profile".into(),
            temp.path().join("profile").into_os_string(),
            "--no-plugins".into(),
        ],
        temp.path(),
    )?;
    let (release, pending) = mpsc::channel();
    let server = thread::spawn(move || respond_to_images(listener, pending, png.into_inner()));
    let test = native_test::run_async_visible(
        project,
        options,
        |_| {},
        async move |handle, app| {
            let capture =
                std::env::var_os("NANOBUG_MARKDOWN_IMAGES_TEST_CAPTURE").map(PathBuf::from);
            if let Some(directory) = &capture {
                fs::create_dir_all(directory)?;
            }
            app.background_executor()
                .timer(Duration::from_millis(60))
                .await;
            open_preview(reference, handle, app)?;
            let expected = capture_preview(handle, app, true)?;
            if let Some(directory) = &capture {
                expected.save(directory.join("remote-expected.png"))?;
            }
            ensure!(
                has_test_image(&expected),
                "local reference raster did not paint"
            );

            open_preview(readme, handle, app)?;
            let pending = capture_preview(handle, app, true)?;
            if let Some(directory) = &capture {
                pending.save(directory.join("remote-pending.png"))?;
            }
            ensure!(
                !has_test_image(&pending),
                "image painted before HTTP response"
            );
            let scale = app.update_window(handle.into(), |_, window, _| window.scale_factor())?;
            let first_paragraph = pending.width() as usize * (44. * scale).round() as usize * 4;
            ensure!(
                pending.as_raw()[..first_paragraph] != expected.as_raw()[..first_paragraph],
                "failed image showed its alternative before the response"
            );
            // Independently decoded images occupy different atlas tiles. Linear sampling may
            // shade their boundary pixels differently even with identical position and content.
            let sprite_edges = (2. * (IMAGE_SIZE.0 + IMAGE_SIZE.1) as f32 * scale).ceil() as usize;
            release.send(())?;

            let deadline = Instant::now() + LOAD_DEADLINE;
            let loaded = loop {
                app.background_executor()
                    .timer(Duration::from_millis(30))
                    .await;
                // A forced draw here could hide a missing asynchronous repaint.
                let frame = capture_preview(handle, app, false)?;
                if frame.dimensions() == expected.dimensions()
                    && frame.as_raw()[..first_paragraph] == expected.as_raw()[..first_paragraph]
                    && has_test_image(&frame)
                    && frame
                        .pixels()
                        .zip(expected.pixels())
                        .filter(|(actual, reference)| actual != reference)
                        .take(sprite_edges + 1)
                        .count()
                        <= sprite_edges
                {
                    break frame;
                }
                if Instant::now() >= deadline {
                    if let Some(directory) = &capture {
                        frame.save(directory.join("remote-actual.png"))?;
                        capture_preview(handle, app, true)?
                            .save(directory.join("remote-redrawn.png"))?;
                    }
                    anyhow::bail!("remote preview did not paint the expected raster and fallback");
                }
            };
            if let Some(directory) = &capture {
                loaded.save(directory.join("remote-loaded.png"))?;
                app.update(|app| {
                    handle.update(app, |workspace, window, cx| -> Result<()> {
                        for key in ["space", "m", "a"] {
                            press(workspace, key, window, cx)?;
                        }
                        Ok(())
                    })
                })??;
                let split = app.update_window(handle.into(), |_, window, cx| {
                    window.draw(cx).clear(cx);
                    window.render_to_image()
                })??;
                split.save(directory.join("remote-split.png"))?;
            }
            Ok(())
        },
    );
    let served = server
        .join()
        .map_err(|_| anyhow::anyhow!("loopback image server panicked"))?;
    test?;
    served
}

/// Open `path` and select only its preview through the native dispatcher.
fn open_preview(path: PathBuf, handle: WindowHandle<Workspace>, app: &mut AsyncApp) -> Result<()> {
    app.update(|app| {
        handle.update(app, |workspace, window, cx| -> Result<()> {
            workspace.open(path, window, cx);
            workspace.sync(window, cx);
            for key in ["space", "m", "p"] {
                press(workspace, key, window, cx)?;
            }
            ensure!(
                workspace.projects.active().editor.markdown_view_mode()
                    == Some(MarkdownViewMode::Preview),
                "Markdown file did not enter preview mode"
            );
            Ok(())
        })
    })?
}

/// Serve an HTTP failure and a redirected `png` after the test releases the `pending` gate.
fn respond_to_images(
    listener: TcpListener,
    pending: mpsc::Receiver<()>,
    png: Vec<u8>,
) -> Result<()> {
    pending.recv_timeout(LOAD_DEADLINE)?;
    listener.set_nonblocking(true)?;
    let deadline = Instant::now() + LOAD_DEADLINE;
    for _ in 0..3 {
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    ensure!(
                        Instant::now() < deadline,
                        "loopback image request timed out"
                    );
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => return Err(error.into()),
            }
        };
        stream.set_nonblocking(false)?;
        stream.set_read_timeout(Some(LOAD_DEADLINE))?;
        stream.set_write_timeout(Some(LOAD_DEADLINE))?;
        let mut reader = BufReader::new(&stream);
        let mut request = String::new();
        reader.read_line(&mut request)?;
        let mut header = String::new();
        while reader.read_line(&mut header)? > 0 && header != "\r\n" {
            header.clear();
        }
        let path = request
            .split_whitespace()
            .nth(1)
            .context("image request path")?;
        let (status, headers, body) = match path {
            "/missing" => ("404 Not Found", "", &[][..]),
            "/redirect" => ("302 Found", "Location: /image.png\r\n", &[][..]),
            "/image.png" => ("200 OK", "Content-Type: image/png\r\n", png.as_slice()),
            _ => anyhow::bail!("unexpected image request {path}"),
        };
        write!(
            stream,
            "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )?;
        stream.write_all(body)?;
    }
    Ok(())
}

/// Read the preview pixels in `handle`'s scene through `app`, drawing first if requested.
fn capture_preview(
    handle: WindowHandle<Workspace>,
    app: &mut AsyncApp,
    redraw: bool,
) -> Result<RgbaImage> {
    let bounds = app.update(|app| {
        handle.update(app, |workspace, _, _| {
            let area = workspace
                .projects
                .active()
                .editor
                .markdown_preview_area(workspace.layout.editor);
            area.map(|area| area.pixels(workspace.metrics))
                .context("visible Markdown preview")
        })
    })??;
    app.update_window(handle.into(), |_, window, cx| {
        if redraw {
            window.draw(cx).clear(cx);
        }
        let image = window.render_to_image()?;
        let scale = window.scale_factor();
        let device = |value| (f32::from(value) * scale).round() as u32;
        let (left, top) = (device(bounds.left()), device(bounds.top()));
        let (right, bottom) = (device(bounds.right()), device(bounds.bottom()));
        ensure!(
            right <= image.width() && bottom <= image.height() && right > left && bottom > top,
            "the DirectX target does not include the preview viewport"
        );
        Ok(image::imageops::crop_imm(&image, left, top, right - left, bottom - top).to_image())
    })?
}

/// Identify the test PNG by an interior region of its distinctive color in `frame`.
fn has_test_image(frame: &RgbaImage) -> bool {
    frame
        .pixels()
        .filter(|pixel| {
            pixel.0[..3]
                .iter()
                .zip(IMAGE_COLOR.0)
                .all(|(sample, expected)| sample.abs_diff(expected) <= 5)
        })
        .take(256)
        .count()
        == 256
}
