//! Verify rendered Markdown links through the native workspace pointer dispatcher.

use super::{IMAGE_COLOR, MarkdownViewMode, Options, Workspace, native_test};
use crate::ui::theme::style;
use anyhow::{Context as _, Result, ensure};
use gpui::{
    AppContext, AsyncApp, Context, MouseButton, MouseDownEvent, MouseUpEvent, Pixels, Point,
    TextRun, Window, WindowHandle, point, px,
};
use std::{fs, time::Duration};

/// Let the native window settle after a smooth preview jump.
const SCROLL_SETTLE: Duration = Duration::from_millis(600);

/// A rendered link clicks through its painted geometry, including after scrolling.
#[test]
#[ignore = "requires the Windows DirectWrite and DirectX renderer"]
fn native_markdown_preview_clicks_anchors_and_relative_files() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    let docs = project.join("docs");
    fs::create_dir_all(&docs)?;
    let source = project.join("README.md");
    let guide = docs.join("Other Guide.md");
    let plain = project.join("plain.txt");
    let paragraphs = "A paragraph providing distance for smooth scrolling.\n\n".repeat(26);
    fs::write(
        &source,
        format!(
            concat!(
                "[Jump to heading](#visible-markdown-target)\n\n{paragraphs}",
                "## Visible Markdown Target\n\n",
                "[Open the guide](docs/Other%20Guide.md#next-section)\n\n{paragraphs}"
            ),
            paragraphs = paragraphs
        ),
    )?;
    fs::write(
        &guide,
        format!(
            concat!(
                "# Other Guide\n\n{paragraphs}",
                "## Next Section\n\n",
                "[![Open plain text](marker.png)](../plain.txt)\n\n{paragraphs}"
            ),
            paragraphs = paragraphs
        ),
    )?;
    image::RgbaImage::from_pixel(50, 25, IMAGE_COLOR).save(docs.join("marker.png"))?;
    fs::write(&plain, "A local text file")?;
    let source = source.canonicalize()?;
    let guide = guide.canonicalize()?;
    let plain = plain.canonicalize()?;
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
            let (caret, source_top) = app.update(|app| {
                handle.update(app, |workspace, window, cx| {
                    workspace.open(source, window, cx);
                    workspace.sync(window, cx);
                    let editor = &workspace.projects.active().editor;
                    ensure!(editor.markdown_view_mode() == Some(MarkdownViewMode::Split));
                    Ok::<_, anyhow::Error>((
                        editor.vim().doc.cursor,
                        editor.buffer().scroll.target(),
                    ))
                })
            })??;
            redraw(handle, app)?;
            app.update(|app| {
                handle.update(app, |workspace, window, cx| -> Result<()> {
                    let hit = rendered_link(
                        workspace,
                        window,
                        cx,
                        "#visible-markdown-target",
                        "Jump to heading",
                    )?;
                    press_link(workspace, window, cx, hit);
                    ensure!(
                        !workspace
                            .projects
                            .active()
                            .editor
                            .markdown_scroll_active(workspace.layout.editor)
                    );
                    release_link(workspace, window, cx, point(hit.x, px(1.)));
                    ensure!(
                        !workspace
                            .projects
                            .active()
                            .editor
                            .markdown_scroll_active(workspace.layout.editor)
                    );
                    press_link(workspace, window, cx, hit);
                    release_link(workspace, window, cx, hit);
                    let editor = &workspace.projects.active().editor;
                    ensure!(editor.markdown_scroll_active(workspace.layout.editor));
                    ensure!(editor.vim().doc.cursor == caret);
                    Ok(())
                })
            })??;
            app.background_executor().timer(SCROLL_SETTLE).await;
            redraw(handle, app)?;
            app.update(|app| {
                handle.update(app, |workspace, window, cx| -> Result<()> {
                    let editor = &workspace.projects.active().editor;
                    ensure!(
                        editor.vim().doc.cursor == caret
                            && editor.buffer().scroll.target() > source_top,
                        "same-document anchor did not align the source while retaining its caret"
                    );
                    let hit = rendered_link(
                        workspace,
                        window,
                        cx,
                        "docs/Other%20Guide.md#next-section",
                        "Open the guide",
                    )?;
                    press_link(workspace, window, cx, hit);
                    release_link(workspace, window, cx, hit);
                    let editor = &workspace.projects.active().editor;
                    ensure!(editor.vim().doc.path.as_ref() == Some(&guide));
                    ensure!(editor.markdown_view_mode() == Some(MarkdownViewMode::Split));
                    ensure!(editor.markdown_scroll_active(workspace.layout.editor));
                    Ok(())
                })
            })??;
            app.background_executor().timer(SCROLL_SETTLE).await;
            redraw(handle, app)?;
            app.update(|app| {
                handle.update(app, |workspace, window, cx| -> Result<()> {
                    let hit = linked_image(workspace, window, "../plain.txt")?;
                    press_link(workspace, window, cx, hit);
                    release_link(workspace, window, cx, hit);
                    let editor = &workspace.projects.active().editor;
                    ensure!(editor.vim().doc.path.as_ref() == Some(&plain));
                    ensure!(editor.markdown_view_mode().is_none());
                    Ok(())
                })
            })?
        },
    )
}

/// Publish the current viewport and its pointer regions in the native window.
fn redraw(handle: WindowHandle<Workspace>, app: &mut AsyncApp) -> Result<()> {
    app.update_window(handle.into(), |_, window, cx| window.draw(cx).clear(cx))
}

/// Find the painted underline for `url` and verify its `label` uses the editor's base size.
fn rendered_link(
    workspace: &Workspace,
    window: &Window,
    cx: &Context<Workspace>,
    url: &str,
    label: &str,
) -> Result<Point<Pixels>> {
    let font_size = px(workspace.metrics.font_size);
    let scaled_pixel = px(1.).scale(window.scale_factor());
    let (hit, width) = window
        .painted_underlines()
        .into_iter()
        .find_map(|line| {
            let bounds = line.bounds;
            let hit = point(
                px(bounds.center().x / scaled_pixel),
                px(bounds.origin.y / scaled_pixel) - font_size * 0.35,
            );
            let (_, link) = workspace.markdown_link_at(hit)?;
            (link == url).then_some((hit, px(bounds.size.width / scaled_pixel)))
        })
        .with_context(|| format!("link {url} must be visible and hittable"))?;
    let run = TextRun {
        font: style(cx).font.descriptor(),
        len: label.len(),
        ..TextRun::default()
    };
    let expected_width = window
        .text_system()
        .shape_line(label.to_owned().into(), font_size, &[run], None)
        .width();
    ensure!(
        f32::from(width - expected_width).abs() < 1.5,
        "link {url} width differs from editor-size native text"
    );
    Ok(hit)
}

/// Find a point within the fixture image painted over `url` in the native window.
fn linked_image(workspace: &Workspace, window: &Window, url: &str) -> Result<Point<Pixels>> {
    let frame = window.render_to_image()?;
    let scale = window.scale_factor();
    frame
        .enumerate_pixels()
        .find_map(|(x, y, color)| {
            if color.0[..3]
                .iter()
                .zip(IMAGE_COLOR.0)
                .any(|(actual, expected)| actual.abs_diff(expected) > 5)
            {
                return None;
            }
            let hit = point(px((x as f32 + 0.5) / scale), px((y as f32 + 0.5) / scale));
            let (_, link) = workspace.markdown_link_at(hit)?;
            (link == url).then_some(hit)
        })
        .with_context(|| format!("image linked to {url} must be visible and hittable"))
}

/// Begin a left press over `position` through the workspace dispatcher.
fn press_link(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
    position: Point<Pixels>,
) {
    workspace.mouse_down(
        &MouseDownEvent {
            position,
            button: MouseButton::Left,
            click_count: 1,
            ..Default::default()
        },
        window,
        cx,
    );
}

/// Complete a left press anywhere in the native `window`.
fn release_link(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
    position: Point<Pixels>,
) {
    workspace.mouse_up(
        &MouseUpEvent {
            position,
            button: MouseButton::Left,
            click_count: 1,
            ..Default::default()
        },
        window,
        cx,
    );
}
