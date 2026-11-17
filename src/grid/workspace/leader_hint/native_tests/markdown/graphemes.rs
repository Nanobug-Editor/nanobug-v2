//! Verify that the source viewport edges admit whole graphemes.

use super::{Options, Workspace, native_test};
use anyhow::{Result, ensure};
use gpui::{AppContext, AsyncApp, Bounds, Pixels, WindowHandle, point, px};
use image::RgbaImage;
use std::{fs, path::PathBuf, time::Duration};

/// Compare clipped source and divider padding pixels against an otherwise identical empty row.
#[test]
#[ignore = "requires the Windows DirectWrite and DirectX renderer"]
fn native_markdown_source_clips_only_whole_graphemes() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    fs::create_dir(&project)?;
    fs::write(project.join("README.md"), "# Preview fixture")?;
    let fixture = project.clone();
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
            let (columns, crop, continuation, highlight_crop, highlight_inset) =
                app.update(|app| {
                    handle.update(app, |workspace, window, cx| {
                        workspace.sync(window, cx);
                        let metrics = workspace.metrics;
                        let editor = &workspace.projects.active().editor;
                        let source = editor.text_area(workspace.layout.editor);
                        let preview = editor
                            .markdown_preview_area(workspace.layout.editor)
                            .expect("split preview");
                        let first = workspace.spacing.cells("中").next().unwrap();
                        ensure!(source.w >= 3);
                        ensure!(first.end > crate::ui::text::UNITS_PER_COLUMN);
                        ensure!(first.end <= 2 * crate::ui::text::UNITS_PER_COLUMN);
                        ensure!(preview.x == source.x + source.w + 2);
                        let bounds = source.pixels(metrics);
                        Ok::<_, anyhow::Error>((
                            source.w,
                            Bounds::from_corners(
                                point(
                                    bounds.right() - px(2. * metrics.cw),
                                    bounds.top() + px(metrics.lh),
                                ),
                                point(
                                    metrics.at((preview.x - 1) as f32, 0.).x,
                                    bounds.top() + px(2. * metrics.lh),
                                ),
                            ),
                            Bounds::from_corners(
                                point(bounds.left(), bounds.top() + px(2. * metrics.lh)),
                                point(
                                    bounds.left() + px(2. * metrics.cw),
                                    bounds.top() + px(3. * metrics.lh),
                                ),
                            ),
                            Bounds::from_corners(
                                metrics.at(workspace.layout.editor.x as f32 + 0.5, source.y as f32),
                                metrics.at((preview.x - 1) as f32, (source.y + 2) as f32),
                            ),
                            metrics.cw * window.scale_factor() * 0.5,
                        ))
                    })
                })??;
            let mut capture = |name: &str, count: usize, text: &str| {
                let path = fixture.join(name);
                fs::write(&path, format!("\n{}{text}\n", " ".repeat(count)))?;
                source_crop(handle, app, path, crop)
            };
            let blank = capture("blank.md", columns - 1, "")?;
            let clipped = capture("clipped.md", columns - 1, "中")?;
            let whole = capture("whole.md", columns - 2, "中")?;
            let overflow = capture("overflow.md", columns, "ABC")?;
            ensure!(
                clipped == blank,
                "a Han grapheme paints inside the last column without room to fit"
            );
            ensure!(
                whole != blank,
                "the fully fitting Han grapheme did not paint"
            );
            ensure!(
                overflow == blank,
                "source glyphs reached the divider padding"
            );
            let next_row = source_crop(handle, app, fixture.join("clipped.md"), continuation)?;
            let blank_row = source_crop(handle, app, fixture.join("blank.md"), continuation)?;
            ensure!(
                next_row != blank_row,
                "Han grapheme did not paint on the continuation"
            );
            let blank_line = fixture.join("blank.md");
            let highlighted = source_crop(handle, app, blank_line, highlight_crop)?;
            let half_row = highlighted.height() / 4;
            let row_differs =
                |x| highlighted.get_pixel(x, half_row) != highlighted.get_pixel(x, 3 * half_row);
            let left = (0..highlighted.width()).find(|&x| row_differs(x));
            let right = (0..highlighted.width()).rev().find(|&x| row_differs(x));
            if let (Some(left), Some(right)) = (left, right) {
                for inset in [left, highlighted.width() - 1 - right] {
                    ensure!(
                        (inset as f32 - highlight_inset).abs() <= 1.,
                        "current row is {inset} device pixels from its border, expected {highlight_inset}"
                    );
                }
            } else {
                anyhow::bail!("current row tint was not painted");
            }
            Ok(())
        },
    )
}

/// Open `path` and capture the unchanged source strip `crop` from `handle` through `app`.
fn source_crop(
    handle: WindowHandle<Workspace>,
    app: &mut AsyncApp,
    path: PathBuf,
    crop: Bounds<Pixels>,
) -> Result<RgbaImage> {
    app.update(|app| {
        handle.update(app, |workspace, window, cx| {
            workspace.open(path, window, cx);
            workspace.sync(window, cx);
        })
    })?;
    app.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        let frame = window.render_to_image()?;
        let device = |value: Pixels| (f32::from(value) * window.scale_factor()).round() as u32;
        let (left, top) = (device(crop.left()), device(crop.top()));
        let (right, bottom) = (device(crop.right()), device(crop.bottom()));
        ensure!(right <= frame.width() && bottom <= frame.height());
        Ok(image::imageops::crop_imm(&frame, left, top, right - left, bottom - top).to_image())
    })?
}
