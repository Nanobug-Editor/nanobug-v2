//! Verify HTML image dimensions through the native preview's painted pixels.

use super::{MarkdownViewMode, Options, capture_preview, native_test, raster_color_bounds};
use crate::ui::text::UNITS_PER_COLUMN;
use anyhow::{Context as _, Result, ensure};
use gpui::AppContext;
use image::{Rgba, RgbaImage};
use std::{fmt::Write, fs, path::PathBuf, time::Duration};

/// Native size shared by the fixtures, with a 2:1 aspect ratio.
const NATURAL: (u32, u32) = (80, 40);
/// One paint color for each image, in document order.
const COLORS: [Rgba<u8>; 6] = [
    Rgba([228, 37, 156, 255]),
    Rgba([40, 207, 22, 255]),
    Rgba([235, 164, 34, 255]),
    Rgba([51, 120, 238, 255]),
    Rgba([161, 45, 220, 255]),
    Rgba([238, 67, 36, 255]),
];

/// HTML attributes control layout for ready images independently of source raster size.
#[test]
#[ignore = "requires the Windows DirectWrite and DirectX renderer"]
fn native_markdown_html_image_dimensions() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    fs::create_dir(&project)?;
    let variants = [
        r#"width="52" height="65""#,
        r#"width="36""#,
        r#"height="23""#,
        r#"width="600" height="100""#,
        r#"width="50%""#,
        r#"width="60" height="31" style="width: 40px; height: 25px""#,
    ];
    let mut markdown = String::new();
    for (index, (attrs, color)) in variants.into_iter().zip(COLORS).enumerate() {
        let name = format!("swatch-{index}.png");
        RgbaImage::from_pixel(NATURAL.0, NATURAL.1, color).save(project.join(&name))?;
        writeln!(markdown, r#"<img {attrs} src="{name}">"#)?;
        markdown.push('\n');
    }
    fs::write(project.join("README.md"), markdown)?;
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
            let (content_width, inset, scale) = app.update(|app| {
                handle.update(app, |workspace, window, cx| {
                    // An explicitly sized HTML image stays the same when the editor text changes size.
                    let mut appearance = workspace.appearance;
                    appearance.base_size = 19.;
                    workspace.apply_appearance(appearance, window, cx);
                    let editor = &workspace.projects.active().editor;
                    ensure!(editor.markdown_view_mode() == Some(MarkdownViewMode::Split));
                    let bounds = editor
                        .markdown_preview_area(workspace.layout.editor)
                        .context("visible split preview")?
                        .pixels(workspace.metrics);
                    let advance = workspace.spacing.cells("中").next().unwrap().end;
                    let inset = workspace.metrics.cw * advance as f32 / UNITS_PER_COLUMN as f32;
                    let content_width = f32::from(bounds.size.width) - 2. * inset;
                    Ok::<_, anyhow::Error>((content_width, inset, window.scale_factor()))
                })
            })??;
            let frame = capture_preview(handle, app, true)?;
            if let Some(directory) = std::env::var_os("NANOBUG_MARKDOWN_IMAGES_TEST_CAPTURE") {
                let directory = PathBuf::from(directory);
                fs::create_dir_all(&directory)?;
                app.update_window(handle.into(), |_, window, _| window.render_to_image())??
                    .save(directory.join("html-image-dimensions.png"))?;
            }
            let dimensions = |index: usize| raster_dimensions(&frame, COLORS[index], scale);
            let expect = |index, width, height| {
                let actual = dimensions(index)?;
                near(actual.0, width, scale)?;
                near(actual.1, height, scale)
            };
            let (left, _, _, _) = raster_color_bounds(&frame, COLORS[0])
                .context("missing first left-aligned image")?;
            near(left as f32 / scale, inset, scale)?;
            expect(0, 52., 65.)?;
            expect(1, 36., 18.)?;
            expect(2, 46., 23.)?;
            let wide = dimensions(3)?;
            let expected_width = content_width.min(600.);
            near_scaled_width(wide.0, expected_width, scale)?;
            near(wide.1, expected_width / 6., scale)?;
            let half = dimensions(4)?;
            near_scaled_width(half.0, content_width / 2., scale)?;
            near(half.1, content_width / 4., scale)?;
            expect(5, 40., 25.)?;
            Ok(())
        },
    )
}

/// Measure the visible pixels of a solid `color` swatch in logical coordinates.
fn raster_dimensions(frame: &RgbaImage, color: Rgba<u8>, scale: f32) -> Result<(f32, f32)> {
    let (left, top, right, bottom) = raster_color_bounds(frame, color)
        .with_context(|| format!("missing painted swatch {color:?}"))?;
    Ok((
        (right - left + 1) as f32 / scale,
        (bottom - top + 1) as f32 / scale,
    ))
}

/// Compare sizes with a two-device-pixel allowance for antialiased sprite edges.
fn near(actual: f32, expected: f32, scale: f32) -> Result<()> {
    ensure!(
        (actual - expected).abs() <= 2. / scale,
        "painted {actual:.1} px, expected {expected:.1} px"
    );
    Ok(())
}

/// A magnified image can lose one source texel across both filtered edges in the color scan.
fn near_scaled_width(actual: f32, expected: f32, scale: f32) -> Result<()> {
    let tolerance = expected / NATURAL.0 as f32 + 1. / scale;
    ensure!(
        (actual - expected).abs() <= tolerance,
        "painted {actual:.1} px, expected {expected:.1} px"
    );
    Ok(())
}
