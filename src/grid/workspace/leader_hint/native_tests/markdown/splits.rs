//! Verify the native paint, link geometry and wheel routing of independent Markdown editors.

use super::{MarkdownViewMode, Options, native_test, raster_color_bounds};
use anyhow::{Context as _, Result, ensure};
use gpui::{AppContext, ScrollDelta, ScrollEvent, point, px};
use image::{Rgba, RgbaImage};
use std::{
    fs,
    time::{Duration, Instant},
};

/// Both previews paint and resolve their own links while passive preview scrolling leaves focus alone.
#[test]
#[ignore = "requires the Windows DirectWrite and DirectX renderer"]
fn native_editor_splits_keep_markdown_previews_independent() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    fs::create_dir(&project)?;
    let swatches = [Rgba([231, 42, 173, 255]), Rgba([203, 167, 18, 255])];
    for (file, color, label) in [
        ("README.md", swatches[0], "first"),
        ("Other.md", swatches[1], "second"),
    ] {
        RgbaImage::from_pixel(48, 28, color).save(project.join(format!("{label}.png")))?;
        fs::write(
            project.join(file),
            format!(
                "# {label}\n\n<p align=\"center\">\n<a href=\"#{label}-tail\"><img src=\"{label}.png\" width=\"48\" height=\"28\" /></a>\n</p>\n\n## {label} tail\n\n{}",
                "Enough text to make this preview scroll independently.\n\n".repeat(45)
            ),
        )?;
    }
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
            let (first, second) = app.update(|app| {
                handle.update(app, |workspace, window, cx| {
                    workspace.sync(window, cx);
                    let first = workspace.projects.active().active_editor_id();
                    workspace
                        .projects
                        .active_mut()
                        .editor
                        .set_markdown_view_mode(MarkdownViewMode::Preview);
                    workspace.open_editor_split(false, Some("Other.md".into()), window, cx);
                    let second = workspace.projects.active().active_editor_id();
                    ensure!(first != second, "second editor must own focus");
                    workspace
                        .projects
                        .active_mut()
                        .editor
                        .set_markdown_view_mode(MarkdownViewMode::Preview);
                    workspace.sync(window, cx);
                    Ok::<_, anyhow::Error>((first, second))
                })
            })??;
            let (frame, scale) = app.update_window(handle.into(), |_, window, cx| {
                window.draw(cx).clear(cx);
                window
                    .render_to_image()
                    .map(|image| (image, window.scale_factor()))
            })??;
            app.update(|app| {
                handle.update(app, |workspace, window, cx| {
                    for (pane, color, href) in [
                        (first, swatches[0], "#first-tail"),
                        (second, swatches[1], "#second-tail"),
                    ] {
                        let (left, top, right, bottom) = raster_color_bounds(&frame, color)
                            .with_context(|| format!("{href} image was not painted"))?;
                        let position = point(
                            px((left + right + 1) as f32 / (2. * scale)),
                            px((top + bottom + 1) as f32 / (2. * scale)),
                        );
                        let (x, y) = workspace.metrics.cell(position);
                        ensure!(workspace.editor_at(x, y).map(|(owner, _)| owner) == Some(pane));
                        ensure!(
                            workspace.markdown_link_at(position).map(|(_, link)| link)
                                == Some(href)
                        );
                        if pane == first {
                            workspace.scroll(
                                &ScrollEvent {
                                    position,
                                    delta: ScrollDelta::Pixels(point(px(0.), px(-240.))),
                                    ..Default::default()
                                },
                                window,
                                cx,
                            );
                        }
                    }
                    Ok::<_, anyhow::Error>(())
                })
            })??;
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                let moved = app.update(|app| {
                    handle.update(app, |workspace, _, _| {
                        let project = workspace.projects.active();
                        ensure!(project.active_editor_id() == second);
                        ensure!(project.editor.markdown_preview_scroll_position() == Some(0.));
                        Ok::<_, anyhow::Error>(
                            project
                                .editor_by_id(first)
                                .and_then(|editor| editor.markdown_preview_scroll_position())
                                .is_some_and(|offset| offset > 15.),
                        )
                    })
                })??;
                if moved {
                    break;
                }
                ensure!(
                    Instant::now() < deadline,
                    "passive Markdown preview did not scroll"
                );
                app.background_executor()
                    .timer(Duration::from_millis(20))
                    .await;
            }
            Ok(())
        },
    )
}
