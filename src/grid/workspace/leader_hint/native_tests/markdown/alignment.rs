//! Check the positions of HTML images in the painted Markdown preview.

use super::{Options, capture_preview, native_test, raster_color_bounds};
use anyhow::{Context as _, Result, ensure};
use gpui::{AppContext, point, px};
use image::{Rgba, RgbaImage};
use std::{fs, path::PathBuf, time::Duration};

/// Fixture pixel color used to measure the painted image independently of the palette.
const CENTER_IMAGE: Rgba<u8> = Rgba([231, 42, 173, 255]);

/// A README-style HTML paragraph centers its linked image without changing the declared size.
#[test]
#[ignore = "requires the Windows DirectWrite and DirectX renderer"]
fn native_markdown_html_centers_readme_picture() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    fs::create_dir(&project)?;
    RgbaImage::from_pixel(128, 128, CENTER_IMAGE).save(project.join("logo.png"))?;
    fs::write(
        project.join("README.md"),
        concat!(
            "# Nanobug\n\n",
            "<p align=\"center\">\n",
            "  <a href=\"#简介\"><img src=\"logo.png\" alt=\"Nanobug 图标\" width=\"128\" height=\"128\" /></a>\n",
            "</p>\n\n",
            "[English](README.md) · **简体中文**\n\n",
            "## 简介\n\n",
            "Nanobug 是一个使用 Rust 与 GPUI 构建的原生桌面代码编辑器，优先支持 Windows。\n\n",
            "```powershell\ncargo build --release\n```\n",
        ),
    )?;
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
            app.background_executor()
                .timer(Duration::from_millis(60))
                .await;
            let frame = capture_preview(handle, app, true)?;
            if let Some(directory) = std::env::var_os("NANOBUG_MARKDOWN_IMAGES_TEST_CAPTURE") {
                let directory = PathBuf::from(directory);
                fs::create_dir_all(&directory)?;
                app.update_window(handle.into(), |_, window, _| window.render_to_image())??
                    .save(directory.join("html-image-alignment.png"))?;
            }
            let (left, top, right, bottom) = raster_color_bounds(&frame, CENTER_IMAGE)
                .context("expected the HTML image to be painted in the preview")?;
            let center_delta = f64::from(left + right + 1) - f64::from(frame.width());
            ensure!(
                center_delta.abs() <= 4.,
                "centered image differs from the pane's center by {:.1} device pixels",
                center_delta / 2.
            );
            app.update(|app| {
                handle.update(app, |workspace, window, _| {
                    let scale = window.scale_factor();
                    for extent in [right - left + 1, bottom - top + 1] {
                        ensure!((extent as f32 - 128. * scale).abs() <= 2.);
                    }
                    let editor = &workspace.projects.active().editor;
                    let preview = editor
                        .markdown_preview_area(workspace.layout.editor)
                        .context("Markdown preview must be visible")?
                        .pixels(workspace.metrics);
                    let image_center = preview.origin
                        + point(
                            px((left + right + 1) as f32 / (2. * scale)),
                            px((top + bottom + 1) as f32 / (2. * scale)),
                        );
                    ensure!(
                        workspace
                            .markdown_link_at(image_center)
                            .map(|(_, href)| href)
                            == Some("#简介")
                    );
                    Ok(())
                })
            })?
        },
    )
}
