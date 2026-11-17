//! Exercise a real rust-analyzer through the Windows workspace and the installed Rust plugin.

use super::{
    Workspace,
    native_editing_tests::{options, step, until, use_session},
};
use crate::grid::workspace::{editor_close::CloseTarget, native_test};
use anyhow::{Context as _, Result, ensure};
use gpui::{
    AppContext, AsyncApp, Bounds, Context, KeyDownEvent, Keystroke, Pixels, Point, ScrollDelta,
    ScrollEvent, ScrollSource, TouchPhase, Window, WindowHandle, point, px,
};
use nanobug::plugins::{
    package::{InstallStore, Provider},
    session::{Action, Key, LanguageServerPhase, Session, State},
};
use nanobug_plugin_sdk::{Instance, PluginEvent};
use std::{
    fs,
    path::PathBuf,
    time::{Duration, Instant},
};

/// Feed the same native events that are used for Vim prefixes and the completion picker.
fn keys(
    workspace: &mut Workspace,
    keys: &[&'static str],
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    for key in keys {
        workspace.key_down(
            &KeyDownEvent {
                keystroke: Keystroke::parse(key).expect("valid test key"),
                is_held: false,
                prefer_character_input: false,
            },
            window,
            cx,
        );
    }
}

/// Send a Windows-style wheel delta at the native window-local `position`. Positive `rows` scroll up.
fn wheel_rows(
    workspace: &mut Workspace,
    position: Point<Pixels>,
    rows: f32,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    workspace.scroll(
        &ScrollEvent {
            position,
            delta: ScrollDelta::Lines(point(0., rows)),
            modifiers: Default::default(),
            touch_phase: TouchPhase::Moved,
            source: ScrollSource::Pointer,
        },
        window,
        cx,
    );
}

/// Move from the current selection to the first candidate matching `label_prefix`.
fn select_candidate(
    workspace: &mut Workspace,
    label_prefix: &str,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Result<()> {
    let (labels, selected) = workspace.completion_items().context("Completion popup")?;
    let index = labels
        .iter()
        .position(|label| label.starts_with(label_prefix))
        .with_context(|| format!("Completion matching {label_prefix}"))?;
    for _ in selected..index {
        keys(workspace, &["down"], window, cx);
    }
    for _ in index..selected {
        keys(workspace, &["up"], window, cx);
    }
    Ok(())
}

/// Draw the current native scene and capture the DirectX target after updating the window.
fn capture_frame(app: &AsyncApp, handle: WindowHandle<Workspace>) -> Result<image::RgbaImage> {
    app.update(|app| {
        app.update_window(handle.into(), |_, window, cx| {
            window.draw(cx).clear(cx);
            window.render_to_image()
        })?
    })
}

/// Sample only the Markdown body of the hover in `handle` through `app`.
/// Convert the logical `clip` to device pixels using the window's `scale`.
fn capture_hover_body(
    app: &AsyncApp,
    handle: WindowHandle<Workspace>,
    clip: Bounds<Pixels>,
    scale: f32,
) -> Result<image::RgbaImage> {
    let frame = capture_frame(app, handle)?;
    let device = |value: Pixels| (f32::from(value) * scale).round() as u32;
    let (left, right) = (device(clip.left()), device(clip.right()));
    let (top, bottom) = (device(clip.top()), device(clip.bottom()));
    ensure!(right <= frame.width() && bottom <= frame.height());
    Ok(image::imageops::crop_imm(&frame, left, top, right - left, bottom - top).to_image())
}

/// Wait for the live subscriber to publish `phase` to `window`, then capture its footer.
/// An optional `sample` name requests a full-size PNG in the configured artifact directory.
async fn capture_rust_status(
    app: &AsyncApp,
    window: WindowHandle<Workspace>,
    phase: Option<LanguageServerPhase>,
    sample: Option<&str>,
) -> Result<image::RgbaImage> {
    let expected = phase.map(|phase| ("rust-analyzer", phase));
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let (actual, bounds, scale) = app.update(|app| {
            window.update(app, |workspace, native, _| {
                (
                    workspace.active_language_server_status(),
                    workspace.footer_bounds(),
                    native.scale_factor(),
                )
            })
        })?;
        if actual == expected {
            // Explicit capture can occur before GPUI invalidates this window's cached scene.
            app.update(|app| window.update(app, |_, native, _| native.refresh()))?;
            let frame = capture_frame(app, window)?;
            if let Some((sample, directory)) =
                sample.zip(std::env::var_os("NANOBUG_RUST_STATUS_SCREENSHOTS"))
            {
                let path = PathBuf::from(directory);
                fs::create_dir_all(&path)?;
                frame.save(path.join(format!("{sample}.png")))?;
            }
            let top = (f32::from(bounds.top()) * scale).floor() as u32;
            let bottom = (f32::from(bounds.bottom()) * scale).ceil() as u32;
            ensure!(
                top < bottom && bottom <= frame.height(),
                "Footer outside native frame"
            );
            return Ok(
                image::imageops::crop_imm(&frame, 0, top, frame.width(), bottom - top).to_image(),
            );
        }
        ensure!(
            Instant::now() < deadline,
            "Expected LSP status {expected:?}; last status was {actual:?}"
        );
        app.background_executor()
            .timer(Duration::from_millis(5))
            .await;
    }
}

/// Retry a native action until it succeeds with the server still reporting indexing.
async fn while_rust_indexing(
    app: &AsyncApp,
    window: WindowHandle<Workspace>,
    action: &str,
    mut try_action: impl FnMut(&mut Workspace, &mut Window, &mut Context<Workspace>) -> Result<bool>,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let complete = step(app, window, |workspace, native, cx| {
            let status = workspace.active_language_server_status();
            ensure!(
                status == Some(("rust-analyzer", LanguageServerPhase::Indexing)),
                "{action} must work during indexing, current server status {status:?}"
            );
            ensure!(
                !workspace.plugins.open,
                "{action} must leave the editor active"
            );
            try_action(workspace, native, cx)
        })?;
        if complete {
            return Ok(());
        }
        ensure!(Instant::now() < deadline, "Timed out waiting for {action}");
        app.background_executor()
            .timer(Duration::from_millis(150))
            .await;
    }
}

/// Verify background Rust indexing, early editor responses, and launch failure in the footer.
#[test]
#[ignore = "requires Windows GPUI, matching runtime, rust-analyzer on PATH, and NANOBUG_RUST_PLUGIN_ZIP"]
fn native_rust_analyzer_status_changes_in_footer() -> Result<()> {
    use LanguageServerPhase::{Failed, Indexing, Ready, Starting};

    let temp = tempfile::tempdir()?;
    let project = temp.path().join("workspace");
    let profile = temp.path().join("profile");
    let source = project.join("src/lib.rs");
    let helpers = project.join("src/helpers.rs");
    let notes = project.join("notes.txt");
    let config = project.join(".nb/plugin-setting/rust.toml");
    fs::create_dir_all(project.join("src"))?;
    fs::create_dir_all(config.parent().context("Rust configuration directory")?)?;
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = 'ra_status_fixture'\nversion = '0.1.0'\nedition = '2024'\n",
    )?;
    // Keep the genuine Cargo loading phase long enough to capture a frame on a warm machine.
    fs::write(
        project.join("build.rs"),
        "fn main() { std::thread::sleep(std::time::Duration::from_secs(1)); }\n",
    )?;
    fs::write(
        &source,
        concat!(
            "mod helpers;\n",
            "pub fn answer() -> u32 {\n",
            "    let _wrong: u32 = 0;\n",
            "    let _value = helpers::silent_index_;\n",
            "    helpers::definition_anchor()\n",
            "}\n",
        ),
    )?;
    // The completion and definition targets are only present in the unopened helper file.
    fs::write(
        &helpers,
        "pub const silent_index_value: u32 = 2;\npub fn definition_anchor() -> u32 { 40 }\n",
    )?;
    let helpers = helpers.canonicalize()?;
    fs::write(&notes, "A buffer without a language server.\n")?;

    let archive = std::env::var_os("NANOBUG_RUST_PLUGIN_ZIP")
        .map(PathBuf::from)
        .context("Set NANOBUG_RUST_PLUGIN_ZIP to the built Rust plugin ZIP")?;
    let store = InstallStore::new(profile.clone());
    let prepared = store.prepare(&archive)?;
    let grants = prepared.release.manifest.permissions.clone();
    store.install(prepared, &grants)?;

    native_test::run_async_visible(
        project.clone(),
        options(&project, &profile)?,
        |_| {},
        async move |window, app| {
            until(
                app,
                window,
                "native client and plugin session",
                |workspace| {
                    Ok(workspace.layout.visible_cols >= 60
                        && workspace.layout.visible_rows >= 16
                        && workspace.plugins.session.is_some())
                },
            )
            .await?;
            let scope = step(app, window, |workspace, native, cx| {
                workspace.open(source.clone(), native, cx);
                workspace.sync(native, cx);
                Ok(workspace.projects.active().plugin_scope)
            })?;
            let disabled = capture_rust_status(app, window, None, None).await?;
            step(app, window, |workspace, _, _| {
                workspace
                    .plugins
                    .session
                    .as_ref()
                    .context("Plugin session")?
                    .send(Action::SetProjectExecution(scope, true))?;
                Ok(())
            })?;
            let starting =
                capture_rust_status(app, window, Some(Starting), Some("rust-analyzer-starting"))
                    .await?;
            let indexing =
                capture_rust_status(app, window, Some(Indexing), Some("rust-analyzer-indexing"))
                    .await?;
            let indexing_started = Instant::now();
            step(app, window, |workspace, native, cx| {
                let vim = &mut workspace.projects.active_mut().editor.buffer_mut().vim;
                let wrong = vim.doc.text.find("0;").context("Disk-only value")?;
                vim.doc.replace(wrong..wrong + 1, "\"unsaved\"", true);
                let prefix = "silent_index_";
                let cursor = vim
                    .doc
                    .text
                    .find(prefix)
                    .context("Rust cross-file prefix")?;
                vim.set_cursor(cursor + prefix.len());
                keys(workspace, &["i"], native, cx);
                workspace.commit_text("v", native, cx);
                let vim = workspace.projects.active().editor.vim();
                ensure!(vim.mode == nanobug::vim::Mode::Insert);
                ensure!(vim.doc.text.contains("helpers::silent_index_v;"));
                ensure!(vim.doc.modified());
                Ok(())
            })?;
            let edit_seen = indexing_started.elapsed();
            // Give automatic completion its normal debounce, then retry if the crate graph is loading.
            app.background_executor()
                .timer(Duration::from_millis(150))
                .await;
            while_rust_indexing(
                app,
                window,
                "cross-file semantic completion",
                |workspace, native, cx| {
                    if workspace.completion_items().is_some_and(|(labels, _)| {
                        labels
                            .iter()
                            .any(|label| label.starts_with("silent_index_value"))
                    }) {
                        select_candidate(workspace, "silent_index_value", native, cx)?;
                        keys(workspace, &["tab"], native, cx);
                        ensure!(
                            workspace
                                .projects
                                .active()
                                .editor
                                .vim()
                                .doc
                                .text
                                .contains("helpers::silent_index_value;")
                        );
                        return Ok(true);
                    }
                    keys(workspace, &["ctrl-x", "ctrl-o"], native, cx);
                    Ok(false)
                },
            )
            .await?;
            let completion_seen = indexing_started.elapsed();
            step(app, window, |workspace, native, cx| {
                keys(workspace, &["escape"], native, cx);
                let vim = &mut workspace.projects.active_mut().editor.buffer_mut().vim;
                let cursor = vim
                    .doc
                    .text
                    .find("definition_anchor")
                    .context("Rust cross-file reference")?;
                vim.set_cursor(cursor);
                Ok(())
            })?;
            while_rust_indexing(app, window, "cross-file gd", |workspace, native, cx| {
                let doc = &workspace.projects.active().editor.vim().doc;
                if doc.path.as_ref() == Some(&helpers) {
                    ensure!(doc.text[doc.cursor..].starts_with("definition_anchor"));
                    return Ok(true);
                }
                if workspace.plugins.pending_definition.is_none() {
                    keys(workspace, &["g", "d"], native, cx);
                    ensure!(
                        !workspace.plugins.open,
                        "Pending gd must leave the editor active"
                    );
                }
                Ok(false)
            })
            .await?;
            let definition_seen = indexing_started.elapsed();
            let ready =
                capture_rust_status(app, window, Some(Ready), Some("rust-analyzer-ready")).await?;
            eprintln!(
                "Rust footer indexing to edit {:.0?}, completion {:.0?}, gd {:.0?}, ready {:.0?}",
                edit_seen,
                completion_seen,
                definition_seen,
                indexing_started.elapsed()
            );

            step(app, window, |workspace, native, cx| {
                workspace.open(notes, native, cx);
                workspace.sync(native, cx);
                Ok(())
            })?;
            capture_rust_status(app, window, Some(Ready), None).await?;
            step(app, window, |workspace, native, cx| {
                workspace.open(source.clone(), native, cx);
                workspace.sync(native, cx);
                Ok(())
            })?;
            capture_rust_status(app, window, Some(Ready), None).await?;
            until(
                app,
                window,
                "unsaved diagnostics after indexing",
                |workspace| {
                    let doc = &workspace.projects.active().editor.vim().doc;
                    let wrong = "\"unsaved\"";
                    let start = doc.text.find(wrong).context("Retained editor overlay")?;
                    ensure!(fs::read_to_string(&source)?.contains("let _wrong: u32 = 0;"));
                    Ok(workspace
                        .plugins
                        .view
                        .document_diagnostics
                        .iter()
                        .any(|(key, data)| {
                            key.instance.project == scope
                                && data.version == (doc.opening, doc.revision)
                                && data.diagnostics.iter().any(|diagnostic| {
                                    diagnostic.start == start
                                        && diagnostic.end == start + wrong.len()
                                        && diagnostic.message.contains("expected u32")
                                })
                        }))
                },
            )
            .await?;

            step(app, window, |workspace, _, _| {
                workspace
                    .plugins
                    .session
                    .as_ref()
                    .context("Plugin session")?
                    .send(Action::SetProjectExecution(scope, false))?;
                Ok(())
            })?;
            capture_rust_status(app, window, None, None).await?;
            fs::write(&config, "excludeDirs = [")?;
            step(app, window, |workspace, _, _| {
                workspace
                    .plugins
                    .session
                    .as_ref()
                    .context("Plugin session")?
                    .send(Action::SetProjectExecution(scope, true))?;
                Ok(())
            })?;
            let failed =
                capture_rust_status(app, window, Some(Failed), Some("rust-analyzer-failed"))
                    .await?;
            step(app, window, |workspace, _, _| {
                ensure!(
                    workspace
                        .plugins
                        .view
                        .analyzer_errors
                        .iter()
                        .any(|(key, error)| {
                            key.instance.project == scope && error.contains("rust.toml")
                        }),
                    "Expected the launch error to identify the malformed project config"
                );
                Ok(())
            })?;

            let rendered = [disabled, starting, indexing, ready, failed];
            for (index, frame) in rendered.iter().enumerate() {
                ensure!(
                    rendered[..index].iter().all(|other| other != frame),
                    "The native footer did not repaint for status frame {index}"
                );
            }
            Ok(())
        },
    )
}

/// Verify both configured exclusions against Rust modules that resolve in a control project.
#[test]
#[ignore = "requires Windows GPUI, matching runtime, rust-analyzer on PATH, and NANOBUG_RUST_PLUGIN_ZIP"]
fn native_rust_analyzer_combines_plugin_and_user_scan_exclusions() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let control = temp.path().join("control");
    let excluded = temp.path().join("excluded");
    let profile = temp.path().join("profile");
    let targets = [
        ("fuzz/artifacts/probe.rs", "artifact_probe"),
        ("manual-artifacts/probe.rs", "manual_probe"),
        ("src/control.rs", "visible_probe"),
    ];
    for (root, apply_exclusions) in [(&control, false), (&excluded, true)] {
        for directory in ["fuzz/artifacts", "fuzz/src", "manual-artifacts", "src"] {
            fs::create_dir_all(root.join(directory))?;
        }
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = 'rust_scan_probe'\nversion = '0.1.0'\nedition = '2024'\n",
        )?;
        for (path, name) in targets {
            fs::write(
                root.join(path),
                format!("pub fn {name}() -> u32 {{ 42 }}\n"),
            )?;
        }
        fs::write(
            root.join("src/lib.rs"),
            concat!(
                "#[path = \"../fuzz/artifacts/probe.rs\"] mod auto_artifacts;\n",
                "#[path = \"../manual-artifacts/probe.rs\"] mod manual_artifacts;\n",
                "mod control;\n",
                "pub fn exercise() -> u32 {\n",
                "    auto_artifacts::artifact_probe() + manual_artifacts::manual_probe() + control::visible_probe()\n",
                "}\n",
            ),
        )?;
        fs::write(
            root.join("fuzz/Cargo.toml"),
            format!(
                "[package]\nname = 'rust_scan_probe_fuzz'\nversion = '0.1.0'\nedition = '2024'\n\
                 [package.metadata]\ncargo-fuzz = {apply_exclusions}\n\n[workspace]\n"
            ),
        )?;
        fs::write(root.join("fuzz/src/lib.rs"), "// Fuzz fixture package.\n")?;
        if apply_exclusions {
            fs::create_dir_all(root.join(".nb/plugin-setting"))?;
            fs::write(
                root.join(".nb/plugin-setting/rust.toml"),
                "excludeDirs = [\"manual-artifacts\"]\n",
            )?;
        }
    }

    let archive = std::env::var_os("NANOBUG_RUST_PLUGIN_ZIP")
        .map(PathBuf::from)
        .context("Set NANOBUG_RUST_PLUGIN_ZIP to the built Rust plugin ZIP")?;
    let store = InstallStore::new(profile.clone());
    let prepared = store.prepare(&archive)?;
    let namespace = prepared.release.manifest.identity().namespace();
    let module = prepared
        .release
        .manifest
        .modules
        .iter()
        .find(|module| module.provides.contains(&Provider::Definition))
        .context("Rust definition module")?
        .id
        .clone();
    let grants = prepared.release.manifest.permissions.clone();
    store.install(prepared, &grants)?;

    native_test::run_async_visible(
        control.clone(),
        options(&control, &profile)?,
        |_| {},
        async move |window, app| {
            until(app, window, "plugin session for scan test", |workspace| {
                Ok(workspace.plugins.session.is_some())
            })
            .await?;
            for (root, apply_exclusions) in [(&control, false), (&excluded, true)] {
                let scope = step(app, window, |workspace, native, cx| {
                    workspace.open_project(root, native, cx);
                    workspace.open(root.join("src/lib.rs"), native, cx);
                    workspace.sync(native, cx);
                    let scope = workspace.projects.active().plugin_scope;
                    workspace
                        .plugins
                        .session
                        .as_ref()
                        .context("Plugin session for scan test")?
                        .send(Action::SetProjectExecution(scope, true))?;
                    Ok(scope)
                })?;
                until(app, window, "Rust scanner ready", |workspace| {
                    Ok(workspace.active_language_server_status()
                        == Some(("rust-analyzer", LanguageServerPhase::Ready)))
                })
                .await?;

                for (index, (path, name)) in targets.into_iter().enumerate() {
                    let reply = step(app, window, |workspace, _, _| {
                        let doc = &workspace.projects.active().editor.vim().doc;
                        let offset = doc.text.find(name).context("Probe reference")? + 1;
                        let (send, reply) = async_channel::bounded(1);
                        workspace
                            .plugins
                            .session
                            .as_ref()
                            .context("Plugin session for definition request")?
                            .send(Action::Invoke {
                                key: Key {
                                    namespace: namespace.clone(),
                                    instance: Instance {
                                        project: scope,
                                        module: module.clone(),
                                    },
                                },
                                event: PluginEvent::Definition { offset },
                                reply: Some(send),
                                timeout: Duration::from_secs(4),
                            })?;
                        Ok(reply)
                    })?;
                    let definitions = reply
                        .recv()
                        .await?
                        .map_err(anyhow::Error::msg)?
                        .output
                        .definitions;
                    let destination = root.join(path).canonicalize()?;
                    ensure!(
                        if apply_exclusions && index < 2 {
                            definitions.is_empty()
                        } else {
                            definitions.iter().any(|item| {
                                item.path
                                    .canonicalize()
                                    .is_ok_and(|path| path == destination)
                            })
                        },
                        "Unexpected definitions for {name} with apply_exclusions={apply_exclusions}: {definitions:?}"
                    );
                }
            }
            Ok(())
        },
    )
}

/// Render and scroll real Rust Markdown documentation, then restore editor paging and jump.
#[test]
#[ignore = "requires Windows GPUI, matching runtime, rust-analyzer on PATH, and NANOBUG_RUST_PLUGIN_ZIP"]
fn native_rust_analyzer_shows_definition_details_beside_caret() -> Result<()> {
    /// The link's visible label and destination are intentionally different.
    const DOC_LINK: &str = "https://example.com/nanobug-hover";
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("workspace");
    let profile = temp.path().join("profile");
    fs::create_dir_all(project.join("src"))?;
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = 'rust_hover_fixture'\nversion = '0.1.0'\nedition = '2024'\n",
    )?;
    let source = project.join("src/lib.rs");
    let target = project.join("src/helpers.rs");
    fs::write(
        &source,
        format!(
            "mod helpers;\n{}pub fn run() -> u64 {{\n    helpers::documented_total(4, 2)\n}}\n{}",
            "// Before the call.\n".repeat(128),
            "// After the call.\n".repeat(128),
        ),
    )?;
    let mut docs = format!("/// [Combines both inputs without truncating the sum.]({DOC_LINK})\n");
    for line in 1..=40 {
        docs.push_str(&format!("/// # Documentation heading {line}\n"));
    }
    docs.push_str(concat!(
        "pub fn documented_total(left: u32, right: u32) -> u64 {\n",
        "    u64::from(left) + u64::from(right)\n",
        "}\n",
    ));
    fs::write(&target, docs)?;
    let source = source.canonicalize()?;
    let target = target.canonicalize()?;
    let archive = std::env::var_os("NANOBUG_RUST_PLUGIN_ZIP")
        .map(PathBuf::from)
        .context("Set NANOBUG_RUST_PLUGIN_ZIP to the built Rust plugin ZIP")?;
    let store = InstallStore::new(profile.clone());
    let prepared = store.prepare(&archive)?;
    ensure!(
        prepared
            .release
            .manifest
            .modules
            .iter()
            .any(|module| module.provides.contains(&Provider::Hover)),
        "The Rust package must advertise hover"
    );
    let grants = prepared.release.manifest.permissions.clone();
    store.install(prepared, &grants)?;

    native_test::run_async_visible(
        project.clone(),
        options(&project, &profile)?,
        |_| {},
        async move |window, app| {
            until(
                app,
                window,
                "Rust plugin session for definition details",
                |workspace| {
                    Ok(workspace.plugins.session.is_some()
                        && workspace.layout.visible_cols >= 60
                        && workspace.layout.visible_rows >= 16)
                },
            )
            .await?;
            let cursor = step(app, window, |workspace, native, cx| {
                workspace.open(source.clone(), native, cx);
                let scope = workspace.projects.active().plugin_scope;
                workspace
                    .plugins
                    .session
                    .as_ref()
                    .context("Plugin session")?
                    .send(Action::SetProjectExecution(scope, true))?;
                let vim = &mut workspace.projects.active_mut().editor.buffer_mut().vim;
                let cursor = vim.doc.text.find("documented_total").context("Call site")? + 2;
                vim.set_cursor(cursor);
                workspace.sync(native, cx);
                Ok(cursor)
            })?;
            until(
                app,
                window,
                "Rust server ready and call site visible",
                |workspace| {
                    let editor = &workspace.projects.active().editor;
                    let (_, _, _, _, visible) =
                        editor.body_caret(workspace.layout.editor, &workspace.spacing);
                    Ok(workspace.active_language_server_status()
                        == Some(("rust-analyzer", LanguageServerPhase::Ready))
                        && visible)
                },
            )
            .await?;
            step(app, window, |workspace, native, cx| {
                keys(workspace, &["shift-k"], native, cx);
                Ok(())
            })?;
            until(
                app,
                window,
                "Rust signature, types and documentation",
                |workspace| {
                    let Some(text) = workspace.definition_hover_text() else {
                        return Ok(false);
                    };
                    ensure!(
                        text.contains("fn documented_total"),
                        "Missing signature: {text}"
                    );
                    ensure!(
                        text.contains("left: u32") && text.contains("-> u64"),
                        "Missing types: {text}"
                    );
                    ensure!(
                        text.contains("Combines both inputs without truncating the sum."),
                        "Missing docs: {text}"
                    );
                    let vim = workspace.projects.active().editor.vim();
                    ensure!(vim.doc.path.as_ref() == Some(&source) && vim.doc.cursor == cursor);
                    ensure!(vim.mode == nanobug::vim::Mode::Normal);
                    Ok(true)
                },
            )
            .await?;
            let (body_clip, scale) = step(app, window, |workspace, native, cx| {
                let popup = workspace
                    .definition_hover_popup(native, cx)
                    .context("Visible popup")?;
                let area = workspace.layout.editor;
                let metrics = workspace.metrics;
                let editor = &workspace.projects.active().editor;
                let (x, y, _, _, visible) = editor.body_caret(area, &workspace.spacing);
                ensure!(visible);
                let caret = metrics.at(x, y);
                let body = editor.text_area(area).pixels(metrics);
                let bounds = popup.bounds();
                ensure!(bounds.left() >= body.left() && bounds.right() <= body.right());
                ensure!(bounds.top() >= body.top() && bounds.bottom() <= body.bottom());
                ensure!(bounds.left() <= caret.x && caret.x < bounds.right());
                let above = f32::from(bounds.bottom() - caret.y).abs() < 0.05;
                let below = f32::from(bounds.top() - caret.y - px(metrics.lh)).abs() < 0.05;
                ensure!(
                    above || below,
                    "Popup must touch the caret's row without covering it"
                );
                let clip = popup.body_bounds();
                let width = f32::from(clip.size.width) as usize;
                let height = f32::from(clip.size.height) as usize;
                let hyperlink = (0..height).step_by(4).any(|y| {
                    (0..width).step_by(4).any(|x| {
                        let at = clip.origin + point(px(x as f32), px(y as f32));
                        popup.link_at(at, metrics.lh) == Some(DOC_LINK)
                    })
                });
                ensure!(
                    hyperlink,
                    "The popup must shape the Rust Markdown doc's link"
                );
                Ok((clip, native.scale_factor()))
            })?;
            let initial_body = capture_hover_body(app, window, body_clip, scale)?;
            if let Some(path) = std::env::var_os("NANOBUG_RUST_HOVER_SCREENSHOT") {
                app.update(|app| window.update(app, |_, native, _| native.refresh()))?;
                capture_frame(app, window)?.save(path)?;
            }
            let (editor_top, last) = step(app, window, |workspace, native, cx| {
                let popup = workspace
                    .definition_hover_popup(native, cx)
                    .context("Visible popup at the start of the documentation")?;
                ensure!(popup.scroll_target() == 0 && popup.scroll_position() == 0.);
                let last = popup.last_first_row();
                ensure!(last > 12, "Fixture must support multiple four-row steps");
                let row_height = workspace.metrics.lh;
                ensure!(
                    f32::from(body_clip.top() - popup.bounds().top()) < row_height
                        && f32::from(popup.bounds().bottom() - body_clip.bottom())
                            < 2. * row_height,
                    "The popup must reserve less vertical room for chrome than before"
                );
                let editor_top = workspace.projects.active().editor.buffer().scroll.target();
                ensure!(
                    editor_top > 0,
                    "Editor must be able to page up at the initial position"
                );
                let top_inset = body_clip.top() - popup.bounds().top();
                let top_chrome =
                    point(body_clip.center().x, popup.bounds().top() + top_inset * 0.5);
                keys(workspace, &["pageup"], native, cx);
                wheel_rows(workspace, top_chrome, 2., native, cx);
                let popup = workspace
                    .definition_hover_popup(native, cx)
                    .context("Paging and wheeling at the top must keep the popup open")?;
                ensure!(popup.scroll_target() == 0 && popup.scroll_position() == 0.);
                let buffer = workspace.projects.active().editor.buffer();
                ensure!(buffer.vim.doc.cursor == cursor && buffer.scroll.target() == editor_top);

                wheel_rows(workspace, body_clip.center(), -0.5, native, cx);
                ensure!(
                    workspace
                        .definition_hover_popup(native, cx)
                        .is_some_and(|popup| popup.scroll_target() == 0),
                    "A partial wheel step must stay with the popup until a whole row accrues"
                );
                wheel_rows(workspace, body_clip.center(), -1.5, native, cx);
                let popup = workspace
                    .definition_hover_popup(native, cx)
                    .context("Wheeling down inside must keep the popup open")?;
                ensure!(
                    popup.scroll_target() == 2 && popup.scroll_position() < 2.,
                    "The wheel must request two rows before the body reaches its destination"
                );
                Ok((editor_top, last))
            })?;
            // Let real GPUI frame callbacks move the popup while sampling its visual position.
            let wait_for_position = async |target: usize,
                                           matches: &dyn Fn(f64) -> bool|
                   -> Result<()> {
                let deadline = Instant::now() + Duration::from_secs(3);
                loop {
                    let position = step(app, window, |workspace, native, cx| {
                        let popup = workspace
                            .definition_hover_popup(native, cx)
                            .context("Popup must stay open during its scroll animation")?;
                        ensure!(popup.scroll_target() == target);
                        let buffer = workspace.projects.active().editor.buffer();
                        ensure!(
                            buffer.vim.doc.cursor == cursor && buffer.scroll.target() == editor_top,
                            "The editor moved during the hover animation"
                        );
                        let position = popup.scroll_position();
                        let row_height = workspace.metrics.lh;
                        let offset = f64::from(f32::from(popup.body_offset(row_height)));
                        let row_height_px = f64::from(row_height);
                        let aligned = position * row_height_px;
                        let last_full_row = (last - 1) as f64;
                        let expected_offset = if position <= last_full_row {
                            (offset - aligned).abs() < 0.01
                        } else {
                            // The Markdown layout can finish on a partial grid row.
                            offset >= last_full_row * row_height_px - 0.01
                                && offset <= aligned + 0.01
                        };
                        ensure!(
                            popup.body_bounds() == body_clip && expected_offset,
                            "The Markdown body must move by fractional rows within its fixed clip"
                        );
                        Ok(position)
                    })?;
                    if matches(position) {
                        return Ok(());
                    }
                    ensure!(
                        Instant::now() < deadline,
                        "Hover position {position:.3} did not reach the expected state for target {target}"
                    );
                    app.background_executor()
                        .timer(Duration::from_millis(5))
                        .await;
                }
            };
            wait_for_position(2, &|row| row > 0. && row < 2. && row.fract() != 0.).await?;
            wait_for_position(2, &|row| row == 2.).await?;
            step(app, window, |workspace, native, cx| {
                wheel_rows(workspace, body_clip.center(), 2., native, cx);
                Ok(())
            })?;
            wait_for_position(0, &|row| row == 0.).await?;
            step(app, window, |workspace, native, cx| {
                keys(workspace, &["pagedown"], native, cx);
                let popup = workspace
                    .definition_hover_popup(native, cx)
                    .context("PageDown must keep the popup open")?;
                ensure!(
                    popup.scroll_target() == 4 && popup.scroll_position() < 4.,
                    "PageDown must request four rows before the body reaches its destination"
                );
                Ok(())
            })?;
            wait_for_position(4, &|row| row > 0. && row < 4. && row.fract() != 0.).await?;
            wait_for_position(4, &|row| row == 4.).await?;
            ensure!(
                capture_hover_body(app, window, body_clip, scale)? != initial_body,
                "The settled popup must paint the scrolled Markdown"
            );
            step(app, window, |workspace, native, cx| {
                keys(workspace, &["pagedown", "pagedown"], native, cx);
                Ok(())
            })?;
            // Reverse after the body passes the new target, while it is still moving down.
            wait_for_position(12, &|row| row > 8. && row < 12.).await?;
            step(app, window, |workspace, native, cx| {
                keys(workspace, &["pageup"], native, cx);
                let popup = workspace
                    .definition_hover_popup(native, cx)
                    .context("Reversed popup")?;
                ensure!(popup.scroll_target() == 8 && popup.scroll_position() > 8.);
                Ok(())
            })?;
            wait_for_position(8, &|row| row == 8.).await?;
            step(app, window, |workspace, native, cx| {
                for expected in (12..last).step_by(4).chain([last]) {
                    keys(workspace, &["pagedown"], native, cx);
                    let popup = workspace
                        .definition_hover_popup(native, cx)
                        .context("Paging must keep the popup open")?;
                    ensure!(
                        popup.scroll_target() == expected,
                        "PageDown requested row {}, expected {expected}",
                        popup.scroll_target(),
                    );
                }
                Ok(())
            })?;
            wait_for_position(last, &|row| row == last as f64).await?;
            step(app, window, |workspace, native, cx| {
                wheel_rows(workspace, body_clip.center(), -2., native, cx);
                keys(workspace, &["pagedown"], native, cx);
                let popup = workspace
                    .definition_hover_popup(native, cx)
                    .context("Paging and wheeling at the bottom must keep the popup open")?;
                ensure!(popup.scroll_target() == last && popup.scroll_position() == last as f64);
                let buffer = workspace.projects.active().editor.buffer();
                ensure!(buffer.vim.doc.cursor == cursor && buffer.scroll.target() == editor_top);

                keys(workspace, &["pageup"], native, cx);
                let popup = workspace
                    .definition_hover_popup(native, cx)
                    .context("Popup must still be animating before Esc")?;
                ensure!(
                    popup.scroll_target() == last - 4
                        && popup.scroll_position() > (last - 4) as f64
                );
                keys(workspace, &["escape"], native, cx);
                ensure!(workspace.definition_hover_text().is_none());
                let vim = workspace.projects.active().editor.vim();
                ensure!(vim.doc.path.as_ref() == Some(&source) && vim.doc.cursor == cursor);
                keys(workspace, &["pagedown"], native, cx);
                ensure!(
                    workspace.projects.active().editor.buffer().scroll.target() > editor_top,
                    "PageDown must page the editor after closing the popup",
                );
                keys(workspace, &["pageup"], native, cx);
                ensure!(
                    workspace.projects.active().editor.buffer().scroll.target() == editor_top,
                    "PageUp must page the editor after closing the popup",
                );
                workspace
                    .projects
                    .active_mut()
                    .editor
                    .buffer_mut()
                    .vim
                    .set_cursor(cursor);
                workspace.sync(native, cx);
                keys(workspace, &["shift-k", "escape"], native, cx);
                Ok(())
            })?;
            // Dismissing an in-flight request must prevent a later reply from reopening the popup.
            app.background_executor()
                .timer(Duration::from_millis(200))
                .await;
            step(app, window, |workspace, native, cx| {
                ensure!(workspace.definition_hover_text().is_none());
                let vim = workspace.projects.active().editor.vim();
                ensure!(vim.doc.path.as_ref() == Some(&source) && vim.doc.cursor == cursor);
                keys(workspace, &["shift-k"], native, cx);
                Ok(())
            })?;
            until(app, window, "Rust documentation reopened", |workspace| {
                Ok(workspace.definition_hover_text().is_some())
            })
            .await?;
            step(app, window, |workspace, native, cx| {
                let popup = workspace
                    .definition_hover_popup(native, cx)
                    .context("Reopened popup")?;
                ensure!(popup.scroll_target() == 0 && popup.scroll_position() == 0.);
                let editor = &workspace.projects.active().editor;
                let area = workspace.layout.editor;
                let metrics = workspace.metrics;
                let (x, y, _, _, visible) = editor.body_caret(area, &workspace.spacing);
                ensure!(visible);
                let outside = metrics.at(x, y) + point(px(1.), px(metrics.lh / 2.));
                ensure!(
                    editor.text_area(area).pixels(metrics).contains(&outside)
                        && !popup.bounds().contains(&outside),
                    "The outside wheel point must hit editor text beyond the popup"
                );
                let before = editor.buffer().scroll.target();
                wheel_rows(workspace, outside, -3., native, cx);
                ensure!(
                    workspace.projects.active().editor.buffer().scroll.target() > before,
                    "Wheeling over the editor outside the popup must scroll the editor"
                );
                ensure!(
                    workspace
                        .definition_hover_popup(native, cx)
                        .is_none_or(|popup| {
                            popup.scroll_target() == 0 && popup.scroll_position() == 0.
                        })
                );
                workspace
                    .projects
                    .active_mut()
                    .editor
                    .buffer_mut()
                    .vim
                    .set_cursor(cursor);
                workspace.sync(native, cx);
                keys(workspace, &["g", "d"], native, cx);
                Ok(())
            })?;
            until(
                app,
                window,
                "definition jump after closing details",
                |workspace| {
                    let doc = &workspace.projects.active().editor.vim().doc;
                    if doc.path.as_ref() != Some(&target) {
                        return Ok(false);
                    }
                    ensure!(doc.text[doc.cursor..].starts_with("documented_total"));
                    ensure!(workspace.definition_hover_text().is_none());
                    Ok(true)
                },
            )
            .await
        },
    )
}

/// Follow two real Rust definitions into a sibling path dependency using the original project.
#[test]
#[ignore = "requires Windows GPUI, matching runtime, rust-analyzer on PATH, and NANOBUG_RUST_PLUGIN_ZIP"]
fn native_rust_analyzer_jumps_again_from_an_external_definition() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("workspace");
    let dependency = temp.path().join("dependency");
    let profile = temp.path().join("profile");
    fs::create_dir_all(project.join("src"))?;
    fs::create_dir_all(dependency.join("src"))?;
    fs::write(
        project.join("Cargo.toml"),
        concat!(
            "[package]\nname = 'external_gd_fixture'\nversion = '0.1.0'\nedition = '2024'\n",
            "[dependencies]\nexternal_gd_dependency = { path = '../dependency' }\n",
        ),
    )?;
    fs::write(
        dependency.join("Cargo.toml"),
        "[package]\nname = 'external_gd_dependency'\nversion = '0.1.0'\nedition = '2024'\n",
    )?;
    let source = project.join("src/lib.rs");
    let first = dependency.join("src/lib.rs");
    let second = dependency.join("src/detail.rs");
    fs::write(
        &source,
        "pub fn run() -> u32 { external_gd_dependency::first_hop(42) }\n",
    )?;
    fs::write(
        &first,
        "mod detail;\npub fn first_hop(value: u32) -> u32 { /* 🦀 */ detail::second_hop(value) }\n",
    )?;
    fs::write(
        &second,
        "pub(crate) fn second_hop(value: u32) -> u32 { value }\n",
    )?;
    let source = source.canonicalize()?;
    let first = first.canonicalize()?;
    let second = second.canonicalize()?;
    let root = project.canonicalize()?;
    let archive = std::env::var_os("NANOBUG_RUST_PLUGIN_ZIP")
        .map(PathBuf::from)
        .context("Set NANOBUG_RUST_PLUGIN_ZIP to the built Rust plugin ZIP")?;
    let store = InstallStore::new(profile.clone());
    let prepared = store.prepare(&archive)?;
    let grants = prepared.release.manifest.permissions.clone();
    store.install(prepared, &grants)?;

    native_test::run_async_visible(
        project.clone(),
        options(&project, &profile)?,
        |_| {},
        async move |window, app| {
            until(
                app,
                window,
                "plugin session for external definitions",
                |workspace| Ok(workspace.plugins.session.is_some()),
            )
            .await?;
            let scope = step(app, window, |workspace, native, cx| {
                workspace.open(source.clone(), native, cx);
                let scope = workspace.projects.active().plugin_scope;
                workspace
                    .plugins
                    .session
                    .as_ref()
                    .context("Plugin session")?
                    .send(Action::SetProjectExecution(scope, true))?;
                let vim = &mut workspace.projects.active_mut().editor.buffer_mut().vim;
                let cursor = vim
                    .doc
                    .text
                    .find("first_hop")
                    .context("Application reference")?;
                vim.set_cursor(cursor + 2);
                workspace.sync(native, cx);
                Ok(scope)
            })?;
            until(app, window, "Rust server ready", |workspace| {
                Ok(workspace.active_language_server_status()
                    == Some(("rust-analyzer", LanguageServerPhase::Ready)))
            })
            .await?;
            step(app, window, |workspace, native, cx| {
                keys(workspace, &["g", "d"], native, cx);
                Ok(())
            })?;
            until(
                app,
                window,
                "definition in the sibling dependency",
                |workspace| {
                    let project = workspace.projects.active();
                    let doc = &project.editor.vim().doc;
                    if doc.path.as_ref() != Some(&first) {
                        return Ok(false);
                    }
                    ensure!(project.root == root && project.plugin_scope == scope);
                    ensure!(doc.text[doc.cursor..].starts_with("first_hop"));
                    Ok(true)
                },
            )
            .await?;
            step(app, window, |workspace, native, cx| {
                let vim = &mut workspace.projects.active_mut().editor.buffer_mut().vim;
                let cursor = vim
                    .doc
                    .text
                    .find("second_hop")
                    .context("Dependency reference")?;
                vim.set_cursor(cursor + 2);
                keys(workspace, &["g", "d"], native, cx);
                Ok(())
            })?;
            until(
                app,
                window,
                "definition from the external dependency",
                |workspace| {
                    let project = workspace.projects.active();
                    ensure!(project.root == root && project.plugin_scope == scope);
                    ensure!(
                        project.status != "The language server needs an open file in this project.",
                        "{}",
                        project.status
                    );
                    let doc = &project.editor.vim().doc;
                    if doc.path.as_ref() != Some(&second) {
                        return Ok(false);
                    }
                    ensure!(doc.text[doc.cursor..].starts_with("second_hop"));
                    Ok(true)
                },
            )
            .await?;
            step(app, window, |workspace, native, cx| {
                for (key, path, reference, offset) in [
                    ("ctrl-o", &first, "second_hop", 2),
                    ("ctrl-o", &source, "first_hop", 2),
                    ("ctrl-i", &first, "second_hop", 2),
                    ("tab", &second, "second_hop", 0),
                ] {
                    keys(workspace, &[key], native, cx);
                    let doc = &workspace.projects.active().editor.vim().doc;
                    ensure!(doc.path.as_ref() == Some(path), "{key}: {:?}", doc.path);
                    ensure!(doc.cursor == doc.text.find(reference).context(reference)? + offset);
                }
                Ok(())
            })
        },
    )
}

/// Exercise the Cargo rule, project permission, and real server without any Rust buffer open.
#[test]
#[ignore = "requires Windows GPUI, matching runtime, rust-analyzer on PATH, and NANOBUG_RUST_PLUGIN_ZIP"]
fn native_rust_analyzer_starts_on_authorized_cargo_project_open() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("workspace");
    let profile = temp.path().join("profile");
    fs::create_dir_all(project.join("src"))?;
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = 'ra_prelaunch_fixture'\nversion = '0.1.0'\nedition = '2024'\n",
    )?;
    fs::write(
        project.join("README.md"),
        "Cargo project before opening Rust.\n",
    )?;
    fs::write(
        project.join("src/lib.rs"),
        "pub fn answer() -> u32 { 42 }\n",
    )?;
    let standalone = temp.path().join("standalone");
    fs::create_dir(&standalone)?;
    fs::write(standalone.join("README.md"), "Non-Cargo project.\n")?;
    let standalone_source = standalone.join("main.rs");
    fs::write(&standalone_source, "fn main() {}\n")?;

    let archive = std::env::var_os("NANOBUG_RUST_PLUGIN_ZIP")
        .map(PathBuf::from)
        .context("Set NANOBUG_RUST_PLUGIN_ZIP to the built Rust plugin ZIP")?;
    let store = InstallStore::new(profile.clone());
    let prepared = store.prepare(&archive)?;
    let namespace = prepared.release.manifest.identity().namespace();
    let grants = prepared.release.manifest.permissions.clone();
    store.install(prepared, &grants)?;

    native_test::run_async_visible(
        project.clone(),
        options(&project, &profile)?,
        |_| {},
        async move |window, app| {
            let scope = step(app, window, |workspace, _, _| {
                Ok(workspace.projects.active().plugin_scope)
            })?;
            until(
                app,
                window,
                "only the semantic Rust module in a Cargo project before opening source",
                |workspace| {
                    let modules = &workspace.plugins.view.modules;
                    Ok([
                        ("complete", State::Ready),
                        ("document", State::Unmatched),
                        ("format", State::Unmatched),
                    ]
                    .into_iter()
                    .all(|(id, expected)| {
                        modules.iter().any(|(key, state)| {
                            key.namespace == namespace
                                && key.instance.project == scope
                                && key.instance.module == id
                                && *state == expected
                        })
                    }))
                },
            )
            .await?;
            step(app, window, |workspace, _, _| {
                ensure!(workspace.plugins.view.project_execution.get(&scope) != Some(&true));
                ensure!(!workspace.plugins.view.language_servers.contains_key(&scope));
                workspace
                    .plugins
                    .session
                    .as_ref()
                    .context("Plugin session")?
                    .send(Action::SetProjectExecution(scope, true))?;
                Ok(())
            })?;
            until(
                app,
                window,
                "rust-analyzer ready from the Cargo rule",
                |workspace| {
                    Ok(workspace
                        .plugins
                        .view
                        .language_servers
                        .get(&scope)
                        .is_some_and(|status| {
                            status.name == "rust-analyzer"
                                && status.phase == LanguageServerPhase::Ready
                                && status.document.is_none()
                        }))
                },
            )
            .await?;
            step(app, window, |workspace, native, cx| {
                let editor = &workspace.projects.active().editor;
                ensure!(
                    editor
                        .vim()
                        .doc
                        .text
                        .starts_with("Cargo project before opening Rust.")
                );
                ensure!(editor.buffers().len() == 1);
                workspace.request_close(CloseTarget::Tab, native, cx);
                Ok(())
            })?;
            until(
                app,
                window,
                "server status removed after closing the project",
                |workspace| {
                    Ok(workspace.projects.active_project().is_none()
                        && !workspace.plugins.view.language_servers.contains_key(&scope)
                        && !workspace
                            .plugins
                            .view
                            .modules
                            .keys()
                            .any(|key| key.instance.project == scope))
                },
            )
            .await?;
            let reopened = step(app, window, |workspace, native, cx| {
                workspace.open_project(&project, native, cx);
                let reopened = workspace.projects.active().plugin_scope;
                ensure!(scope != reopened);
                Ok(reopened)
            })?;
            until(
                app,
                window,
                "prelaunch when opening an already-authorized Cargo project",
                |workspace| {
                    let view = &workspace.plugins.view;
                    Ok(view.project_execution.get(&reopened) == Some(&true)
                        && view.language_servers.get(&reopened).is_some_and(|status| {
                            status.name == "rust-analyzer"
                                && status.phase == LanguageServerPhase::Ready
                                && status.document.is_none()
                        }))
                },
            )
            .await?;
            let independent = step(app, window, |workspace, native, cx| {
                workspace.open_project(&standalone, native, cx);
                Ok(workspace.projects.active().plugin_scope)
            })?;
            until(
                app,
                window,
                "Rust rule inactive in the unrelated project",
                |workspace| {
                    Ok(workspace.plugins.view.modules.iter().any(|(key, state)| {
                        key.namespace == namespace
                            && key.instance.project == independent
                            && *state == State::Unmatched
                    }))
                },
            )
            .await?;
            step(app, window, |workspace, _, _| {
                workspace
                    .plugins
                    .session
                    .as_ref()
                    .context("Plugin session")?
                    .send(Action::SetProjectExecution(independent, true))?;
                Ok(())
            })?;
            until(
                app,
                window,
                "authorized unrelated project without a language server",
                |workspace| {
                    let view = &workspace.plugins.view;
                    Ok(view.project_execution.get(&independent) == Some(&true)
                        && !view.language_servers.contains_key(&independent))
                },
            )
            .await?;
            step(app, window, |workspace, native, cx| {
                workspace.open(standalone_source, native, cx);
                workspace.sync(native, cx);
                Ok(())
            })?;
            until(
                app,
                window,
                "ordinary Rust file enables all modules and starts its language server",
                |workspace| {
                    let view = &workspace.plugins.view;
                    let ready = ["document", "complete", "format"].into_iter().all(|id| {
                        view.modules.iter().any(|(key, state)| {
                            key.namespace == namespace
                                && key.instance.project == independent
                                && key.instance.module == id
                                && *state == State::Ready
                        })
                    });
                    Ok(ready
                        && view
                            .language_servers
                            .get(&independent)
                            .is_some_and(|status| {
                                status.name == "rust-analyzer"
                                    && status.phase != LanguageServerPhase::Failed
                                    && status
                                        .document
                                        .as_ref()
                                        .is_some_and(|(_, path)| path.ends_with("main.rs"))
                            }))
                },
            )
            .await
        },
    )
}

/// Confirm that opening or closing the menu does not alter unrelated editor text pixels.
fn assert_stable_editor(
    before: &image::RgbaImage,
    after: &image::RgbaImage,
    body: Bounds<Pixels>,
    popup: Bounds<Pixels>,
    caret_y: Pixels,
    row_height: Pixels,
    scale: f32,
) -> Result<()> {
    ensure!(before.dimensions() == after.dimensions());
    let margin = px(2. / scale);
    let mut compared = 0;
    let mut popup_changed = false;
    for (x, y, pixel) in before.enumerate_pixels() {
        let position = point(px(x as f32 / scale), px(y as f32 / scale));
        let after = after.get_pixel(x, y);
        if popup.contains(&position) {
            popup_changed |= pixel != after;
            continue;
        }
        let near_popup = position.x >= popup.left() - margin
            && position.x <= popup.right() + margin
            && position.y >= popup.top() - margin
            && position.y <= popup.bottom() + margin;
        let caret_row =
            position.y >= caret_y - margin && position.y <= caret_y + row_height + margin;
        if body.contains(&position) && !near_popup && !caret_row {
            compared += 1;
            ensure!(
                pixel == after,
                "Editor changed outside menu at {position:?}: {pixel:?} → {after:?}; caret y {caret_y:?}, menu {popup:?}"
            );
        }
    }
    ensure!(
        compared > 0 && popup_changed,
        "The two frames must exercise the menu"
    );
    Ok(())
}

/// Check native completion keys, retained candidates, and Rust diagnostics and navigation.
#[test]
#[ignore = "requires Windows GPUI, matching runtime, rust-analyzer on PATH, and NANOBUG_RUST_PLUGIN_ZIP"]
fn native_rust_analyzer_completes_diagnoses_and_navigates_unsaved_rust() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("workspace");
    let profile = temp.path().join("profile");
    let source = project.join("src/lib.rs");
    let helpers = project.join("src/helpers.rs");
    fs::create_dir_all(project.join("src"))?;
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = 'ra_fixture'\nversion = '0.1.0'\nedition = '2024'\n",
    )?;
    fs::write(&helpers, "pub(crate) fn next_value() -> u32 { 2 }\n")?;
    let saved = "mod helpers;\nstruct Widget { disk_field: u32 }\n";
    fs::write(&source, saved)?;

    let archive = std::env::var_os("NANOBUG_RUST_PLUGIN_ZIP")
        .map(PathBuf::from)
        .context("Set NANOBUG_RUST_PLUGIN_ZIP to the built Rust plugin ZIP")?;
    let store = InstallStore::new(profile.clone());
    let prepared = store.prepare(&archive)?;
    let grants = prepared.release.manifest.permissions.clone();
    store.install(prepared, &grants)?;
    let session = Session::start(options(&project, &profile)?).0;
    let helpers = helpers.canonicalize()?;

    native_test::run_async_visible(
        project.clone(),
        options(&project, &profile)?,
        move |workspace| use_session(workspace, session),
        async move |window, app| {
            until(app, window, "native client geometry", |workspace| {
                Ok(workspace.layout.visible_cols >= 60 && workspace.layout.visible_rows >= 16)
            })
            .await?;
            let scope = step(app, window, |workspace, native, cx| {
                workspace.open(source.clone(), native, cx);
                let scope = workspace.projects.active().plugin_scope;
                workspace
                    .plugins
                    .session
                    .as_ref()
                    .context("Plugin session")?
                    .send(Action::SetProjectExecution(scope, true))?;
                workspace.sync(native, cx);
                Ok(scope)
            })?;
            until(
                app,
                window,
                "Rust plugin and project execution",
                |workspace| {
                    Ok(
                        workspace.plugins.view.project_execution.get(&scope) == Some(&true)
                            && workspace.plugins.view.modules.iter().any(|(key, state)| {
                                key.instance.project == scope && *state == State::Ready
                            }),
                    )
                },
            )
            .await?;

            // The footer test exercises requests during indexing. These longer editing checks
            // begin after initial project loading to exercise ready-state editor behavior.
            until(app, window, "Rust language server ready", |workspace| {
                Ok(workspace.active_language_server_status()
                    == Some(("rust-analyzer", LanguageServerPhase::Ready)))
            })
            .await?;

            let prefix = "editor_only_";
            let field = "editor_only_field";
            let unsaved = concat!(
                "mod helpers;\n",
                "struct Widget { editor_only_field: u32, editor_only_fable: u32 }\n",
                "pub fn example() -> u32 {\n",
                "    let widget = Widget { editor_only_field: 40, editor_only_fable: 0 };\n",
                "    let _marker = \"😀\"; let _wrong: u32 = \"bad\";\n",
                "    let _marker = \"😀\"; widget.editor_only_ + helpers::next_value()\n",
                "}\n",
                "pub fn standard_result() -> Result<(), ()> { Ok(()) }\n",
            );
            let wrong = "\"bad\"";
            let wrong_start = unsaved.find(wrong).context("Unsaved type mismatch")?;
            let wrong_end = wrong_start + wrong.len();
            let field_start = unsaved.rfind(prefix).context("Unsaved field prefix")?;
            let field_end = field_start + prefix.len();
            step(app, window, |workspace, native, cx| {
                let editor = &mut workspace.projects.active_mut().editor;
                let doc = &mut editor.buffer_mut().vim.doc;
                doc.replace(0..doc.text.len(), unsaved, true);
                workspace.sync(native, cx);
                Ok(())
            })?;
            let diagnostics =
                until(
                    app,
                    window,
                    "live diagnostics for the unsaved type mismatch",
                    |workspace| {
                        let doc = &workspace.projects.active().editor.vim().doc;
                        Ok(workspace.plugins.view.document_diagnostics.iter().any(
                            |(key, result)| {
                                key.instance.project == scope
                                    && result.version == (doc.opening, doc.revision)
                                    && result.diagnostics.iter().any(|diagnostic| {
                                        diagnostic.start == wrong_start
                                            && diagnostic.end == wrong_end
                                            && diagnostic.message.contains("expected u32")
                                    })
                            },
                        ))
                    },
                )
                .await;
            if let Err(error) = diagnostics {
                return step(app, window, |workspace, _, _| {
                    let project = workspace.projects.active();
                    let doc = &project.editor.vim().doc;
                    let view = &workspace.plugins.view;
                    let snapshots: Vec<_> = view
                        .document_diagnostics
                        .iter()
                        .map(|(key, data)| (key, data.version, &data.path, &data.diagnostics))
                        .collect();
                    Err(error.context(format!(
                        "root {:?}, active path {:?}, version {:?}, expected bytes {:?}, \
                         analyzer errors {:?}, live diagnostics {:?}",
                        project.root,
                        doc.path,
                        (doc.opening, doc.revision),
                        wrong_start..wrong_end,
                        view.analyzer_errors,
                        snapshots,
                    )))
                });
            }

            step(app, window, |workspace, native, cx| {
                workspace.refresh_document_presentation();
                let editor = &mut workspace.projects.active_mut().editor;
                let buffer = editor.buffer_mut();
                buffer.vim.set_cursor(wrong_start);
                ensure!(
                    buffer
                        .caret_diagnostic()
                        .is_some_and(|message| message.contains("expected u32"))
                );
                keys(workspace, &["ctrl-w", "d"], native, cx);
                ensure!(workspace.projects.active().status.contains("expected u32"));
                workspace
                    .projects
                    .active_mut()
                    .editor
                    .buffer_mut()
                    .vim
                    .set_cursor(field_end);
                keys(workspace, &["i"], native, cx);
                workspace.commit_text("f", native, cx);
                Ok(())
            })?;
            until(
                app,
                window,
                "automatic semantic field completion",
                |workspace| {
                    ensure!(
                        !workspace.plugins.open,
                        "Completion must leave editor input active"
                    );
                    let doc = &workspace.projects.active().editor.vim().doc;
                    let colored =
                        workspace
                            .plugins
                            .view
                            .document_presentations
                            .iter()
                            .any(|(key, data)| {
                                key.instance.project == scope
                                    && data.version == (doc.opening, doc.revision)
                                    && !data.features.highlights.is_empty()
                            });
                    Ok(colored
                        && workspace.completion_items().is_some_and(|(labels, _)| {
                            labels.iter().any(|label| label.contains(field))
                                && labels
                                    .iter()
                                    .any(|label| label.contains("editor_only_fable"))
                        }))
                },
            )
            .await?;
            let initial_popup = step(app, window, |workspace, native, _| {
                let metrics = workspace.metrics;
                let (x, y, _, insert, visible) = workspace
                    .projects
                    .active()
                    .editor
                    .body_caret(workspace.layout.editor, &workspace.spacing);
                ensure!(insert && visible);
                let caret = metrics.at(x, y);
                let body = workspace
                    .projects
                    .active()
                    .editor
                    .text_area(workspace.layout.editor);
                let (menu, _, _) = workspace.completion_menu(native).with_context(|| {
                    format!(
                        "Popup does not fit near caret {caret:?} in text region {:?}, viewport {:?}",
                        body.pixels(metrics),
                        native.viewport_size()
                    )
                })?;
                let bounds = menu.bounds();
                ensure!(bounds.left() <= caret.x && caret.x < bounds.right());
                ensure!(bounds.top() == caret.y + px(metrics.lh));
                Ok(bounds)
            })?;
            let before_typing = capture_frame(app, window)?;
            if let Some(path) = std::env::var_os("NANOBUG_RUST_COMPLETION_SCREENSHOT") {
                before_typing.save(path)?;
            }
            step(app, window, |workspace, native, cx| {
                let (_, initial) = workspace.completion_items().context("Popup selection")?;
                keys(workspace, &["down"], native, cx);
                let (_, next) = workspace.completion_items().context("Next selection")?;
                ensure!(next != initial);
                keys(workspace, &["up"], native, cx);
                let (_, previous) = workspace.completion_items().context("Previous selection")?;
                ensure!(previous == initial);

                keys(workspace, &["i"], native, cx);
                workspace.commit_text("i", native, cx);
                let vim = workspace.projects.active().editor.vim();
                ensure!(vim.mode == nanobug::vim::Mode::Insert);
                ensure!(vim.doc.text.contains("widget.editor_only_fi +"));
                ensure!(vim.doc.cursor == field_end + 2);
                let (labels, _) = workspace
                    .completion_items()
                    .context("Matching candidates remain visible while the request refreshes")?;
                ensure!(labels.iter().any(|label| label.starts_with(field)));
                ensure!(
                    !labels
                        .iter()
                        .any(|label| label.starts_with("editor_only_fable"))
                );
                Ok(())
            })?;
            let (body, popup, caret_y, row_height, scale) =
                step(app, window, |workspace, native, _| {
                    let editor = &workspace.projects.active().editor;
                    let area = workspace.layout.editor;
                    let metrics = workspace.metrics;
                    let (x, y, ..) = editor.body_caret(area, &workspace.spacing);
                    let (menu, ..) = workspace.completion_menu(native).context("Visible menu")?;
                    Ok((
                        editor.text_area(area).pixels(metrics),
                        menu.bounds(),
                        metrics.at(x, y).y,
                        px(metrics.lh),
                        native.scale_factor(),
                    ))
                })?;
            let before = capture_frame(app, window)?;
            if let Some(path) = std::env::var_os("NANOBUG_RUST_COMPLETION_SCREENSHOT") {
                before.save(PathBuf::from(path).with_extension("after-typing.png"))?;
            }
            // Compare colored source lines above the typing area. The animated caret can
            // paint into adjacent rows even when those lines have no text changes.
            let stable_body =
                Bounds::from_corners(body.origin, point(body.right(), caret_y - row_height));
            assert_stable_editor(
                &before_typing,
                &before,
                stable_body,
                initial_popup.union(&popup),
                caret_y,
                row_height,
                scale,
            )?;
            step(app, window, |workspace, native, cx| {
                keys(workspace, &["ctrl-e"], native, cx);
                ensure!(workspace.completion_items().is_none());
                let vim = workspace.projects.active().editor.vim();
                ensure!(vim.mode == nanobug::vim::Mode::Insert);
                ensure!(vim.doc.text.contains("widget.editor_only_fi +"));
                Ok(())
            })?;
            let after = capture_frame(app, window)?;
            assert_stable_editor(
                &before,
                &after,
                stable_body,
                popup,
                caret_y,
                row_height,
                scale,
            )?;
            step(app, window, |workspace, native, cx| {
                keys(workspace, &["backspace"], native, cx);
                let vim = workspace.projects.active().editor.vim();
                ensure!(vim.doc.text.contains("widget.editor_only_f +"));
                Ok(())
            })?;
            until(
                app,
                window,
                "semantic field completion after Backspace",
                |workspace| {
                    Ok(workspace.completion_items().is_some_and(|(labels, _)| {
                        labels
                            .iter()
                            .any(|label| label.contains("editor_only_fable"))
                    }))
                },
            )
            .await?;
            step(app, window, |workspace, native, cx| {
                keys(workspace, &["escape"], native, cx);
                ensure!(workspace.completion_items().is_none());
                ensure!(
                    workspace.projects.active().editor.vim().mode == nanobug::vim::Mode::Normal
                );

                workspace
                    .projects
                    .active_mut()
                    .editor
                    .buffer_mut()
                    .vim
                    .set_cursor(field_end + 1);
                keys(workspace, &["i", "ctrl-x", "ctrl-o"], native, cx);
                Ok(())
            })?;
            until(
                app,
                window,
                "manual semantic field completion",
                |workspace| {
                    Ok(workspace.completion_items().is_some_and(|(labels, _)| {
                        labels.iter().any(|label| label.contains(field))
                    }))
                },
            )
            .await?;
            step(app, window, |workspace, native, cx| {
                select_candidate(workspace, field, native, cx)?;
                keys(workspace, &["tab"], native, cx);
                ensure!(workspace.completion_items().is_none());
                let editor = &mut workspace.projects.active_mut().editor;
                ensure!(editor.vim().mode == nanobug::vim::Mode::Insert);
                let document = &mut editor.buffer_mut().vim.doc;
                ensure!(document.text.contains("widget.editor_only_field +"));
                ensure!(document.text[document.cursor..].starts_with(" +"));
                ensure!(document.modified() && fs::read_to_string(&source)? == saved);
                document.replace(wrong_start..wrong_end, "1", true);
                workspace.sync(native, cx);
                Ok(())
            })?;
            until(
                app,
                window,
                "live diagnostics to clear after correcting the type mismatch",
                |workspace| {
                    let doc = &workspace.projects.active().editor.vim().doc;
                    Ok(workspace
                        .plugins
                        .view
                        .document_diagnostics
                        .iter()
                        .any(|(key, result)| {
                            key.instance.project == scope
                                && result.version == (doc.opening, doc.revision)
                                && !result
                                    .diagnostics
                                    .iter()
                                    .any(|diagnostic| diagnostic.message.contains("expected u32"))
                        }))
                },
            )
            .await?;

            step(app, window, |workspace, native, cx| {
                keys(workspace, &["escape"], native, cx);
                let editor = &mut workspace.projects.active_mut().editor;
                let vim = &mut editor.buffer_mut().vim;
                vim.set_cursor(
                    vim.doc
                        .text
                        .find("next_value")
                        .context("Definition reference")?,
                );
                keys(workspace, &["g", "d"], native, cx);
                Ok(())
            })?;
            until(app, window, "cross-file definition jump", |workspace| {
                let document = &workspace.projects.active().editor.vim().doc;
                if document.path.as_ref() != Some(&helpers) {
                    return Ok(false);
                }
                ensure!(document.text[document.cursor..].starts_with("next_value"));
                ensure!(fs::read_to_string(&source)? == saved);
                Ok(true)
            })
            .await?;

            // Complete a prelude type in a return signature, then exercise both keys with no menu.
            let result_start = step(app, window, |workspace, native, cx| {
                workspace.open(source.clone(), native, cx);
                let vim = &mut workspace.projects.active_mut().editor.buffer_mut().vim;
                let signature = "standard_result() -> ";
                let start =
                    vim.doc.text.find(signature).context("Result return type")? + signature.len();
                vim.doc.replace(start + 2..start + "Result".len(), "", true);
                vim.set_cursor(start + 2);
                keys(workspace, &["i"], native, cx);
                workspace.commit_text("s", native, cx);
                Ok(start)
            })?;
            until(
                app,
                window,
                "Result on the first completion page for Res",
                |workspace| {
                    Ok(workspace.completion_items().is_some_and(|(labels, _)| {
                        labels
                            .iter()
                            .take(10)
                            .any(|label| label.starts_with("Result"))
                    }))
                },
            )
            .await?;
            step(app, window, |workspace, native, cx| {
                select_candidate(workspace, "Result", native, cx)?;
                keys(workspace, &["enter"], native, cx);
                ensure!(workspace.completion_items().is_none());
                let vim = workspace.projects.active().editor.vim();
                ensure!(vim.doc.text[result_start..].starts_with("Result<(), ()> { Ok(()) }"));
                let end = result_start + "Result".len();
                ensure!(vim.doc.cursor == end);

                keys(workspace, &["tab", "enter"], native, cx);
                let vim = &mut workspace.projects.active_mut().editor.buffer_mut().vim;
                ensure!(vim.doc.text[end..].starts_with("\t\n<(), ()>"));
                vim.doc.replace(end..end + 2, "", true);
                vim.set_cursor(result_start + "Res".len());
                keys(workspace, &["ctrl-x", "ctrl-o"], native, cx);
                Ok(())
            })?;
            until(
                app,
                window,
                "Result with the caret inside the token",
                |workspace| {
                    Ok(workspace.completion_items().is_some_and(|(labels, _)| {
                        labels
                            .iter()
                            .take(10)
                            .any(|label| label.starts_with("Result"))
                    }))
                },
            )
            .await?;
            step(app, window, |workspace, native, cx| {
                select_candidate(workspace, "Result", native, cx)?;
                keys(workspace, &["tab"], native, cx);
                ensure!(workspace.completion_items().is_none());
                let vim = &mut workspace.projects.active_mut().editor.buffer_mut().vim;
                ensure!(vim.doc.text[result_start..].starts_with("Result<(), ()> { Ok(()) }"));
                vim.doc
                    .replace(result_start..result_start + "Result".len(), "Rs", true);
                workspace.commit_text("l", native, cx);
                Ok(())
            })?;
            until(
                app,
                window,
                "Result for noncontiguous input Rsl",
                |workspace| {
                    Ok(workspace.completion_items().is_some_and(|(labels, _)| {
                        labels.iter().any(|label| label.starts_with("Result"))
                    }))
                },
            )
            .await?;
            step(app, window, |workspace, native, cx| {
                select_candidate(workspace, "Result", native, cx)?;
                keys(workspace, &["tab"], native, cx);
                let document = &workspace.projects.active().editor.vim().doc;
                ensure!(document.text[result_start..].starts_with("Result<(), ()> { Ok(()) }"));
                ensure!(fs::read_to_string(&source)? == saved);
                Ok(())
            })
        },
    )
}
