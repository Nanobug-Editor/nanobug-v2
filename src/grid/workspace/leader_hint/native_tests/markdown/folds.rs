//! Exercise Markdown source folding through native keyboard and pointer routing.

use super::{MarkdownViewMode, Options, native_test, press};
use anyhow::{Context as _, Result, ensure};
use gpui::{AppContext, MouseButton, MouseDownEvent, Pixels};

/// Fold a nested section from the source, then open it from the gutter in a plugin-free window.
#[test]
#[ignore = "requires the Windows DirectWrite and DirectX renderer"]
fn native_markdown_folds_the_source_and_keeps_the_preview() -> Result<()> {
    let project = tempfile::tempdir()?;
    let path = project.path().join("README.md");
    let source = "# Top\n\n## Child\n\n### Preview target\n\n正文\n\n# Next\n\nConclusion\n";
    std::fs::write(&path, source)?;
    let options = Options::take(
        &mut vec![
            "--plugin-profile".into(),
            project.path().join("profile").into_os_string(),
            "--no-plugins".into(),
        ],
        project.path(),
    )?;
    native_test::run_async_visible(
        project.path().to_owned(),
        options,
        |_| {},
        async move |handle, app| {
            let _keep_project = project;
            let (columns, rows) = app.update(|app| {
                handle.update(app, |workspace, window, cx| {
                    workspace.open(path, window, cx);
                    workspace.sync(window, cx);
                    let editor = &workspace.projects.active().editor;
                    ensure!(editor.markdown_view_mode() == Some(MarkdownViewMode::Split));
                    ensure!(editor.vim().doc.hidden_rows().is_empty());

                    for key in ["j", "j", "z", "a"] {
                        press(workspace, key, window, cx)?;
                    }
                    let editor = &workspace.projects.active().editor;
                    let cursor = editor.vim().doc.cursor;
                    ensure!(editor.vim().doc.location(cursor).0 == 2);
                    ensure!(editor.vim().doc.folded_end_row(2) == Some(6));
                    ensure!(editor.vim().doc.collapsed_body_at(8).is_none());
                    let area = editor.text_area(workspace.active_editor_area());
                    let arrow = workspace
                        .metrics
                        .at(area.x as f32 - 1.5, area.y as f32 + 2.5);
                    let preview = workspace
                        .prepare_markdown_preview(window, cx)
                        .context("Markdown preview")?;
                    ensure!(preview.anchor_top("preview-target").is_some());

                    workspace.mouse_down(
                        &MouseDownEvent {
                            position: arrow,
                            button: MouseButton::Left,
                            click_count: 1,
                            ..Default::default()
                        },
                        window,
                        cx,
                    );
                    let doc = &workspace.projects.active().editor.vim().doc;
                    ensure!(doc.hidden_rows().is_empty());
                    ensure!(doc.cursor == cursor && doc.text == source && !doc.modified());

                    for key in ["z", "shift-m"] {
                        press(workspace, key, window, cx)?;
                    }
                    let editor = &workspace.projects.active().editor;
                    ensure!(editor.vim().doc.folded_at(0) && editor.vim().doc.folded_at(8));
                    let area = editor.text_area(workspace.active_editor_area());
                    let right = (area.x + area.w) as f32;
                    let device =
                        |value: Pixels| (f32::from(value) * window.scale_factor()).round() as u32;
                    // Sample the gutter, text, inner divider padding and outer divider padding.
                    let columns = [area.x as f32 - 0.4, right - 1.5, right + 0.25, right + 0.75]
                        .map(|column| device(workspace.metrics.at(column, 0.).x));
                    let rows = [0., 1., 2., 3.]
                        .map(|row| device(workspace.metrics.at(0., area.y as f32 + row + 0.5).y));
                    Ok::<_, anyhow::Error>((columns, rows))
                })?
            })?;
            let frame = app.update_window(handle.into(), |_, window, cx| {
                window.draw(cx).clear(cx);
                window.render_to_image()
            })??;
            ensure!(columns[3] < frame.width() && rows[3] < frame.height());
            for [folded, blank] in [[rows[0], rows[1]], [rows[2], rows[3]]] {
                for (index, x) in columns.into_iter().enumerate() {
                    ensure!(
                        (frame.get_pixel(x, folded) != frame.get_pixel(x, blank)) == (index < 3),
                        "unexpected fold background at column {index} on scanline {folded}"
                    );
                }
            }
            ensure!(frame.get_pixel(columns[1], rows[0]) != frame.get_pixel(columns[1], rows[2]));
            Ok(())
        },
    )
}
