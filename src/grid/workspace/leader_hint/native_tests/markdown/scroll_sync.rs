//! Exercise bidirectional Markdown scrolling through the native wheel dispatcher.

use super::{IMAGE_COLOR, MarkdownViewMode, Options, Workspace, native_test, press};
use anyhow::{Context as _, Result, ensure};
use gpui::{AppContext, AsyncApp, ScrollDelta, ScrollEvent, WindowHandle, point, px};
use nanobug::animation::scroll::ScrollInput;
use std::{fs, path::PathBuf, time::Duration};

/// Allow the native animation callback to advance both viewports to rest.
const SCROLL_TIMEOUT: Duration = Duration::from_secs(2);

/// Cover matching content under each wheel, navigation reclaiming the source, and both ends.
#[test]
#[ignore = "requires the Windows DirectWrite and DirectX renderer"]
fn native_markdown_split_scroll_syncs_by_document_content() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    fs::create_dir(&project)?;
    let readme = project.join("README.md");
    let mut markdown = String::from("# Opening\n\nA short introduction.\n");
    markdown.push_str(&"\n".repeat(60));
    let first_row = markdown.lines().count();
    markdown.push_str(concat!(
        "## First Checkpoint\n\n",
        "This paragraph represents many blank source rows compressed into preview space.\n\n",
        "<img src=\"tall.png\" alt=\"Tall image\" width=\"72\" height=\"520\">\n\n",
        "The image above occupies substantial preview space with only one source row.\n\n"
    ));
    let second_row = markdown.lines().count();
    markdown.push_str("## Second Checkpoint\n\n## Tail Checkpoint\n\n");
    markdown.push_str(
        &"The tail provides enough content to scroll beyond the checkpoints.\n\n".repeat(42),
    );
    fs::write(&readme, markdown)?;
    image::RgbaImage::from_pixel(72, 48, IMAGE_COLOR).save(project.join("tall.png"))?;
    for index in 0..48 {
        fs::write(project.join(format!("sibling-{index:02}.txt")), "")?;
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
            let (first_anchor, second_anchor, line_height, source_bottom) =
                app.update(|app| {
                    handle.update(app, |workspace, window, cx| -> Result<_> {
                        workspace.open(readme, window, cx);
                        workspace.sync(window, cx);
                        let editor = &workspace.projects.active().editor;
                        ensure!(editor.markdown_view_mode() == Some(MarkdownViewMode::Split));
                        let source_bottom = editor
                            .vim()
                            .doc
                            .display_rows()
                            .saturating_sub(editor.text_area(workspace.layout.editor).h);
                        let preview = workspace
                            .prepare_markdown_preview(window, cx)
                            .context("split Markdown preview")?;
                        Ok((
                            preview
                                .anchor_top("first-checkpoint")
                                .context("first rendered heading")?
                                as f32,
                            preview
                                .anchor_top("second-checkpoint")
                                .context("second rendered heading")?
                                as f32,
                            workspace.metrics.lh,
                            source_bottom,
                        ))
                    })
                })??;
            ensure!(
                first_anchor < first_row as f32 * line_height * 0.5,
                "fixture should compress a long blank area"
            );
            ensure!(
                second_anchor - first_anchor > (second_row - first_row) as f32 * line_height * 2.,
                "fixture should expand a short source area in the preview"
            );
            let initial = settled(handle, app).await?;
            ensure!(initial.source == 0. && initial.preview == 0., "{initial:?}");

            wheel(
                handle,
                app,
                Pane::Source,
                ScrollDelta::Lines(point(0., -(first_row as f32))),
            )?;
            ensure!(
                position(handle, app)?.moving,
                "a source wheel step should animate"
            );
            let first = settled(handle, app).await?;
            assert_checkpoint(&first, first_row, first_anchor, line_height)?;
            if let Some(path) = std::env::var_os("NANOBUG_MARKDOWN_SCROLL_TEST_CAPTURE") {
                let frame = app.update_window(handle.into(), |_, window, cx| {
                    window.draw(cx).clear(cx);
                    window.render_to_image()
                })??;
                frame.save(PathBuf::from(path))?;
            }

            wheel_preview_to(handle, app, first.preview, second_anchor)?;
            ensure!(
                position(handle, app)?.moving,
                "a preview wheel step should animate"
            );
            let second = settled(handle, app).await?;
            assert_checkpoint(&second, second_row, second_anchor, line_height)?;
            ensure!(
                second.caret == first.caret,
                "preview wheel moved the source caret"
            );
            ensure!(
                (second.caret_row as f32) < second.source,
                "fixture did not scroll the caret out of view"
            );

            wheel_preview_to(handle, app, second.preview, first_anchor)?;
            let reversed = settled(handle, app).await?;
            assert_checkpoint(&reversed, first_row, first_anchor, line_height)?;
            ensure!(reversed.caret == first.caret);

            wheel_preview_to(handle, app, reversed.preview, second_anchor)?;
            settled(handle, app).await?;
            app.update(|app| {
                handle.update(app, |workspace, window, cx| {
                    press(workspace, "j", window, cx)
                })
            })??;
            let navigated = settled(handle, app).await?;
            ensure!(
                navigated.caret_row == first.caret_row + 1,
                "j did not start from the retained caret"
            );
            ensure!(
                navigated.source < second.source && navigated.preview < second.preview,
                "source navigation should restore the caret and lead synchronized scrolling"
            );

            wheel(
                handle,
                app,
                Pane::Source,
                ScrollDelta::Lines(point(0., -10_000.)),
            )?;
            let bottom = settled(handle, app).await?;
            ensure!(bottom.source == source_bottom as f32, "{bottom:?}");
            ensure!(bottom.preview > second.preview, "{bottom:?}");
            wheel_preview_to(handle, app, bottom.preview, 100_000.)?;
            let beyond = settled(handle, app).await?;
            ensure!(
                (beyond.source, beyond.preview, beyond.caret)
                    == (bottom.source, bottom.preview, bottom.caret),
                "scrolling past the preview end moved either viewport or the source caret"
            );

            wheel_preview_to(handle, app, beyond.preview, -100_000.)?;
            let top = settled(handle, app).await?;
            ensure!(top.source == 0. && top.preview == 0., "{top:?}");
            ensure!(
                top.caret == beyond.caret,
                "preview wheel at the opposite end moved the caret"
            );

            wheel(
                handle,
                app,
                Pane::Source,
                ScrollDelta::Lines(point(0., 10.)),
            )?;
            let past_top = settled(handle, app).await?;
            ensure!(
                (past_top.source, past_top.preview, past_top.caret)
                    == (top.source, top.preview, top.caret),
                "source wheel beyond the top jumped to the retained offscreen caret"
            );

            wheel(
                handle,
                app,
                Pane::Explorer,
                ScrollDelta::Lines(point(0., -12.)),
            )?;
            let explorer = settled(handle, app).await?;
            ensure!(
                explorer.explorer > top.explorer,
                "Explorer wheel did not scroll Explorer"
            );
            ensure!(
                explorer.source == top.source && explorer.preview == top.preview,
                "Explorer wheel leaked into the linked Markdown viewports"
            );

            app.update(|app| {
                handle.update(app, |workspace, window, cx| -> Result<()> {
                    for key in ["space", "m", "p"] {
                        press(workspace, key, window, cx)?;
                    }
                    ensure!(
                        workspace.projects.active().editor.markdown_view_mode()
                            == Some(MarkdownViewMode::Preview)
                    );
                    Ok(())
                })
            })??;
            let preview_only = settled(handle, app).await?;
            wheel_preview_to(
                handle,
                app,
                preview_only.preview,
                preview_only.preview + 150.,
            )?;
            let independent = settled(handle, app).await?;
            ensure!(independent.preview > preview_only.preview);
            ensure!(
                independent.source == preview_only.source
                    && independent.caret == preview_only.caret,
                "preview-only scrolling changed the hidden source view"
            );
            assert_preview_lands_smoothly(handle, app, second_row)?;
            Ok(())
        },
    )
}

/// Step the actual split layout to a preview pixel that maps between two source rows.
fn assert_preview_lands_smoothly(
    handle: WindowHandle<Workspace>,
    app: &mut AsyncApp,
    row: usize,
) -> Result<()> {
    app.update(|app| {
        handle.update(app, |workspace, window, cx| {
            for key in ["space", "m", "a"] {
                press(workspace, key, window, cx)?;
            }
            let preview = workspace
                .prepare_markdown_preview(window, cx)
                .context("split Markdown preview")?;
            let left = preview
                .anchor_top("second-checkpoint")
                .context("left source landmark")?;
            let right = preview
                .anchor_top("tail-checkpoint")
                .context("right source landmark")?;
            let gap = right.saturating_sub(left);
            ensure!(gap >= 20, "fixture headings are too close: {left}..{right}");
            // The headings are two rows apart. Aim just below a half-row boundary
            // and leave room for anchor positions to be rounded down to pixels.
            let target = left + (gap as f32 * 0.74) as usize;
            let start = workspace.scroll_epoch.elapsed().as_secs_f64();
            let area = workspace.layout.editor;
            let editor = &mut workspace.projects.active_mut().editor;
            editor.track_markdown_preview(ScrollInput::Position(target), start);
            ensure!(editor.markdown_scroll_active(area));
            for frame in 1..=120 {
                let before = editor.buffer().scroll.position();
                editor.advance_markdown_preview(
                    &workspace.markdown_cache,
                    area,
                    start + f64::from(frame) / 120.,
                );
                if !editor.markdown_scroll_active(area) {
                    let after = editor.buffer().scroll.position();
                    ensure!(
                        (after - before).abs() < 0.1,
                        "source viewport jumped from {before} to {after} on the last frame"
                    );
                    ensure!(after == (row + 1) as f32, "source stopped on row {after}");
                    ensure!(
                        editor.markdown_preview_scroll_position() == Some(target as f32),
                        "source row alignment changed the preview destination"
                    );
                    return Ok(());
                }
            }
            anyhow::bail!("split preview did not settle in one second")
        })
    })?
}

/// Wheel destination in the rendered workspace.
#[derive(Clone, Copy)]
enum Pane {
    /// Editable source pane.
    Source,
    /// Rendered Markdown pane.
    Preview,
    /// Project Explorer, which owns its own scroll.
    Explorer,
}

/// Painted positions of both Markdown viewports, their source caret, and Explorer state.
#[derive(Debug)]
struct ScrollPosition {
    /// First visible source row, including fractional rows during motion.
    source: f32,
    /// Preview scroll offset in logical pixels.
    preview: f32,
    /// Explorer offset in logical rows.
    explorer: f32,
    /// Source caret byte offset.
    caret: usize,
    /// Source caret document row.
    caret_row: usize,
    /// Whether a tested viewport is still advancing toward its destination.
    moving: bool,
}

/// Sample `handle`'s current view through `app` without triggering layout or scroll.
fn position(handle: WindowHandle<Workspace>, app: &mut AsyncApp) -> Result<ScrollPosition> {
    app.update(|app| {
        handle.update(app, |workspace, _, _| {
            let project = workspace.projects.active();
            let editor = &project.editor;
            let buffer = editor.buffer();
            let doc = &editor.vim().doc;
            let explorer = project
                .manager
                .as_ref()
                .context("project Explorer")?
                .scroll();
            Ok(ScrollPosition {
                source: buffer.scroll.position(),
                preview: editor
                    .markdown_preview_scroll_position()
                    .context("Markdown scroll position")?,
                explorer: explorer.position(),
                caret: doc.cursor,
                caret_row: doc.location(doc.cursor).0,
                moving: (editor.markdown_view_mode() == Some(MarkdownViewMode::Split)
                    && buffer.scroll.is_active())
                    || editor.markdown_scroll_active(workspace.layout.editor)
                    || explorer.is_active(),
            })
        })
    })?
}

/// Wait through `app` for `handle`'s native animation frames to settle.
async fn settled(handle: WindowHandle<Workspace>, app: &mut AsyncApp) -> Result<ScrollPosition> {
    let started = std::time::Instant::now();
    loop {
        let current = position(handle, app)?;
        if !current.moving {
            // The source can publish its settled row just before GPUI paints the following
            // preview offset. Render that final scene without advancing the animation.
            app.update_window(handle.into(), |_, window, cx| window.draw(cx).clear(cx))?;
            let final_position = position(handle, app)?;
            if !final_position.moving {
                return Ok(final_position);
            }
        }
        ensure!(
            started.elapsed() < SCROLL_TIMEOUT,
            "viewport did not settle: {current:?}"
        );
        app.background_executor()
            .timer(Duration::from_millis(20))
            .await;
    }
}

/// Send a wheel `delta` under the center of `pane` in the native workspace `handle`.
fn wheel(
    handle: WindowHandle<Workspace>,
    app: &mut AsyncApp,
    pane: Pane,
    delta: ScrollDelta,
) -> Result<()> {
    app.update(|app| {
        handle.update(app, |workspace, window, cx| {
            let editor = &workspace.projects.active().editor;
            let area = match pane {
                Pane::Source => editor.text_area(workspace.layout.editor),
                Pane::Preview => editor
                    .markdown_preview_area(workspace.layout.editor)
                    .context("preview viewport")?,
                Pane::Explorer => workspace.layout.tree,
            };
            workspace.scroll(
                &ScrollEvent {
                    position: area.pixels(workspace.metrics).center(),
                    delta,
                    ..Default::default()
                },
                window,
                cx,
            );
            Ok(())
        })
    })?
}

/// Wheel from the settled preview offset `from` to the logical pixel offset `to`.
fn wheel_preview_to(
    handle: WindowHandle<Workspace>,
    app: &mut AsyncApp,
    from: f32,
    to: f32,
) -> Result<()> {
    wheel(
        handle,
        app,
        Pane::Preview,
        ScrollDelta::Pixels(point(px(0.), px(from - to))),
    )
}

/// Require `view` to show the same landmark `row` and `anchor`, allowing a small alignment margin.
fn assert_checkpoint(
    view: &ScrollPosition,
    row: usize,
    anchor: f32,
    row_height: f32,
) -> Result<()> {
    ensure!(
        (view.source - row as f32).abs() <= 1. && (view.preview - anchor).abs() <= row_height * 1.5,
        "checkpoint row {row}, preview y {anchor} was not aligned: {view:?}"
    );
    Ok(())
}
