//! End-to-end tests for the sibling Rust plugin ZIP in the native Windows editor.

use super::{
    Workspace,
    native_editing_tests::{options, step, until, use_session},
    rows::Target,
};
use crate::grid::workspace::{layout::Focus, native_test};
use anyhow::{Context, Result, ensure};
use gpui::{AsyncApp, Context as GpuiContext, KeyDownEvent, Keystroke, Window, WindowHandle};
use nanobug::plugins::{
    package::InstallStore,
    session::{Action, ConfigEdit, ConfigScope, Session, State},
};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Install the external Rust plugin ZIP into `profile` and start the session for `project`.
fn rust_session(project: &Path, profile: &Path) -> Result<Session> {
    let archive = std::env::var_os("NANOBUG_RUST_PLUGIN_ZIP")
        .map(PathBuf::from)
        .context("Set NANOBUG_RUST_PLUGIN_ZIP to the built Rust plugin ZIP")?;
    let store = InstallStore::new(profile.to_owned());
    let prepared = store.prepare(&archive)?;
    let grants = prepared.release.manifest.permissions.clone();
    store.install(prepared, &grants)?;
    Ok(Session::start(options(project, profile)?).0)
}

/// Submit `command` through the workspace's ordinary Vim edit and command dispatch.
fn command(
    workspace: &mut Workspace,
    command: &str,
    window: &mut Window,
    cx: &mut GpuiContext<Workspace>,
) {
    let edit = workspace
        .projects
        .active_mut()
        .editor
        .edit(|vim, register| {
            vim.text(command, register);
            vim.key("Enter", register)
        });
    workspace.apply_edit(edit, window, cx);
    workspace.sync(window, cx);
}

#[test]
#[ignore = "requires Windows GPUI, the matching runtime and NANOBUG_RUST_PLUGIN_ZIP"]
fn native_rust_plugin_installs_and_edits_current_rust_buffer() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let profile = temp.path().join("profile");
    let project = temp.path().join("workspace");
    let source = project.join("crates/demo/src/lib.rs");
    fs::create_dir_all(source.parent().unwrap())?;
    fs::write(
        project.join("Cargo.toml"),
        "[workspace]\nmembers=['crates/*']\n",
    )?;
    fs::write(
        project.join("rust-toolchain.toml"),
        "[toolchain]\nchannel='stable'\n",
    )?;
    fs::write(
        project.join("crates/demo/Cargo.toml"),
        "[package]\nname='demo'\n",
    )?;
    fs::write(project.join("README.md"), "Workspace\n")?;
    let rust =
        "fn first() {\r\n\tlet 名 = r#\"你好👩‍💻\"#;\r\n}\r\n\r\nfn second() {}\r\n\r\ntes\r\n";
    fs::write(&source, rust)?;
    let source = source.canonicalize()?;
    let session = rust_session(&project, &profile)?;

    native_test::run_async(
        project.clone(),
        options(&project, &profile)?,
        move |workspace| use_session(workspace, session),
        async move |window, app| {
            until(app, window, "Rust project activation", |workspace| {
                Ok(workspace
                    .plugins
                    .view
                    .modules
                    .values()
                    .any(|state| *state == State::Ready))
            })
            .await?;
            step(app, window, |workspace, native, cx| {
                ensure!(workspace.plugins.view.document_presentations.is_empty());
                workspace.sync(native, cx);
                let project = workspace.projects.active_mut();
                let manager = project.manager.as_mut().context("Project Explorer")?;
                ensure!(manager.reveal_path(&source)?);
                project.focus = Focus::Manager;
                workspace.key_down(
                    &KeyDownEvent {
                        keystroke: Keystroke::parse("enter")?,
                        is_held: false,
                        prefer_character_input: false,
                    },
                    native,
                    cx,
                );
                let project = workspace.projects.active();
                let doc = &project.editor.vim().doc;
                ensure!(doc.path.as_ref() == Some(&source));
                // Check before the test helper can publish the document on the next poll.
                ensure!(
                    workspace.plugins.projects.iter().any(|snapshot| {
                        snapshot.id == project.plugin_scope
                            && snapshot.document.as_ref().is_some_and(|published| {
                                published.opening == doc.opening && published.path == doc.path
                            })
                    }),
                    "Explorer opened the Rust file without publishing it to language plugins"
                );
                Ok(())
            })?;
            until(app, window, "Rust syntax features", |workspace| {
                let Some(presentation) = workspace
                    .plugins
                    .view
                    .document_presentations
                    .values()
                    .next()
                else {
                    return Ok(false);
                };
                let doc = &workspace.projects.active().editor.vim().doc;
                let features = &presentation.features;
                ensure!(
                    features.highlights.iter().any(|span| {
                        &doc.text[span.range.start..span.range.end] == "r#\"你好👩‍💻\"#"
                    }),
                    "The raw string was not one highlighted span"
                );
                ensure!(features.symbols.iter().any(|symbol| symbol.name == "first"));
                ensure!(
                    features
                        .symbols
                        .iter()
                        .any(|symbol| symbol.name == "second")
                );
                ensure!(!features.folds.is_empty());
                Ok(true)
            })
            .await?;
            step(app, window, |workspace, native, cx| {
                workspace.refresh_document_presentation();
                command(workspace, ":fold", native, cx);
                let editor = &workspace.projects.active().editor;
                ensure!(editor.vim().doc.folded_at(0));
                ensure_eq(editor.vim().doc.document_row(1), 3)?;
                let edit = workspace
                    .projects
                    .active_mut()
                    .editor
                    .edit(|vim, register| vim.key("j", register));
                workspace.apply_edit(edit, native, cx);
                ensure_eq(
                    workspace.projects.active().editor.vim().doc.display_row(3),
                    1,
                )?;
                ensure_eq(
                    workspace
                        .projects
                        .active()
                        .editor
                        .vim()
                        .doc
                        .location(workspace.projects.active().editor.vim().doc.cursor)
                        .0,
                    3,
                )?;
                command(workspace, ":outline", native, cx);
                let selection = workspace
                    .plugins
                    .rows()
                    .iter()
                    .position(|row| row.label == "second")
                    .context("Outline item")?;
                workspace.plugins.manager.selection = selection;
                workspace.plugin_activate(native, cx);
                let doc = &workspace.projects.active().editor.vim().doc;
                ensure_eq(doc.cursor, doc.text.find("second").unwrap())?;
                Ok(())
            })?;
            step(app, window, |workspace, native, cx| {
                command(workspace, ":plugin rust-project", native, cx);
                Ok(())
            })?;
            until(app, window, "Cargo project information", |workspace| {
                let status = &workspace.projects.active().status;
                Ok(status.contains("Crate demo")
                    && status.contains("Workspace Cargo.toml")
                    && status.contains("Toolchain stable"))
            })
            .await?;
            step(app, window, |workspace, native, cx| {
                let editor = &mut workspace.projects.active_mut().editor;
                let vim = &mut editor.buffer_mut().vim;
                vim.set_cursor(vim.doc.text.rfind("tes").unwrap() + 2);
                command(workspace, ":complete", native, cx);
                Ok(())
            })?;
            until(app, window, "Rust completion", |workspace| {
                Ok(workspace
                    .plugins
                    .rows()
                    .iter()
                    .any(|row| row.label.starts_with("#[test]")))
            })
            .await?;
            step(app, window, |workspace, native, cx| {
                let index = workspace
                    .plugins
                    .rows()
                    .iter()
                    .position(|row| row.label.starts_with("#[test]"))
                    .unwrap();
                workspace.plugins.manager.selection = index;
                workspace.plugin_activate(native, cx);
                let doc = &workspace.projects.active().editor.vim().doc;
                ensure!(
                    doc.text
                        .ends_with("#[test]\nfn test_name() {\n    todo!()\n}\n")
                );
                ensure!(doc.modified() && fs::read_to_string(&source)? == rust);
                let editor = &mut workspace.projects.active_mut().editor;
                let doc = &mut editor.buffer_mut().vim.doc;
                let at = doc.text.find("todo!()").unwrap() + "todo!(".len();
                doc.replace(at..at + 1, "", true);
                workspace.sync(native, cx);
                Ok(())
            })?;
            until(app, window, "automatic Rust syntax error", |workspace| {
                let doc = &workspace.projects.active().editor.vim().doc;
                Ok(workspace
                    .plugins
                    .view
                    .guest_diagnostics
                    .values()
                    .any(|data| {
                        data.version == (doc.opening, doc.revision)
                            && data.diagnostics.iter().any(|item| {
                                item.severity == nanobug_plugin_sdk::DiagnosticSeverity::Error
                            })
                    }))
            })
            .await?;
            step(app, window, |workspace, native, cx| {
                command(workspace, ":diagnose", native, cx);
                Ok(())
            })?;
            until(
                app,
                window,
                "diagnostics for the unsaved edit",
                |workspace| {
                    Ok(workspace
                        .plugins
                        .rows()
                        .iter()
                        .any(|row| matches!(row.target, Target::Diagnostic(_))))
                },
            )
            .await?;
            step(app, window, |workspace, native, cx| {
                let rows = workspace.plugins.rows();
                let (index, expected) = rows
                    .iter()
                    .enumerate()
                    .find_map(|(index, row)| {
                        if let Target::Diagnostic(offset) = row.target {
                            Some((index, offset))
                        } else {
                            None
                        }
                    })
                    .context("Syntax diagnostic")?;
                workspace.plugins.manager.selection = index;
                workspace.plugin_activate(native, cx);
                let doc = &workspace.projects.active().editor.vim().doc;
                let expected_row = doc.location(expected).0;
                ensure_eq(doc.location(doc.cursor).0, expected_row)?;
                ensure!(fs::read_to_string(source)? == rust);
                ensure!(!workspace.plugins.open);
                Ok(())
            })
        },
    )
}

/// Verify rustfmt edits the in-memory document and follows the active project's save setting.
#[test]
#[ignore = "requires Windows GPUI, the matching runtime, rustfmt and NANOBUG_RUST_PLUGIN_ZIP"]
fn native_rust_plugin_formats_unsaved_buffer_and_project_saves() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let profile = temp.path().join("profile");
    let project = temp.path().join("workspace");
    let other_project = temp.path().join("other-workspace");
    let initial = "fn initial() {}\n";
    let unformatted = "async fn example(){let value=1;println!(\"值 {value}\");}\n";
    let formatted = "async fn example() {\n  let value = 1;\n  println!(\"值 {value}\");\n}\n";
    let invalid = "fn incomplete(\n";
    for root in [&project, &other_project] {
        fs::create_dir_all(root.join("src"))?;
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname='demo'\nversion='0.1.0'\nedition='2024'\n",
        )?;
        fs::write(root.join("rustfmt.toml"), "tab_spaces = 2\n")?;
        fs::write(root.join("src/lib.rs"), initial)?;
    }
    let source = project.join("src/lib.rs").canonicalize()?;
    let other_source = other_project.join("src/lib.rs").canonicalize()?;
    let destination = project.join("generated/lib.rs");
    fs::create_dir_all(destination.parent().unwrap())?;
    fs::write(
        project.join("generated/rustfmt.toml"),
        "tab_spaces = 8\nnewline_style = 'Windows'\n",
    )?;
    let session = rust_session(&project, &profile)?;
    native_test::run_async(
        project.clone(),
        options(&project, &profile)?,
        move |workspace| use_session(workspace, session),
        async move |window, app| {
            step(app, window, |workspace, native, cx| {
                workspace.open(source.clone(), native, cx);
                workspace.sync(native, cx);
                Ok(())
            })?;
            let scope = allow_rust_formatting(app, window).await?;
            step(app, window, |workspace, native, cx| {
                let editor = &mut workspace.projects.active_mut().editor;
                let doc = &mut editor.buffer_mut().vim.doc;
                ensure!(doc.path.as_ref() == Some(&source));
                doc.replace(0..doc.text.len(), unformatted, true);
                command(workspace, ":format", native, cx);
                Ok(())
            })?;
            until(
                app,
                window,
                "rustfmt to edit the unsaved buffer",
                |workspace| Ok(workspace.projects.active().editor.vim().doc.text == formatted),
            )
            .await?;
            let config_scope = ConfigScope::Project(scope);
            step(app, window, |workspace, _, _| {
                ensure!(
                    fs::read_to_string(&source)? == initial,
                    ":format wrote to disk"
                );
                let editor = &mut workspace.projects.active_mut().editor;
                let doc = &mut editor.buffer_mut().vim.doc;
                ensure!(doc.modified());
                doc.undo(false);
                ensure!(
                    doc.text == unformatted,
                    "Undo did not restore the unsaved edit"
                );
                let session = workspace
                    .plugins
                    .session
                    .as_ref()
                    .context("Plugin session")?;
                session.send(Action::EditConfig(
                    config_scope,
                    ConfigEdit::ToggleFormatOnSave("rs".into()),
                ))?;
                session.send(Action::ApplyConfig(config_scope))
            })?;
            until(app, window, "Rust project save policy", |workspace| {
                Ok(workspace
                    .plugins
                    .view
                    .configurations
                    .get(&config_scope)
                    .and_then(|view| view.applied.formatters.get("rs"))
                    .is_some_and(|format| format.on_save == Some(true)))
            })
            .await?;
            step(app, window, |workspace, native, cx| {
                command(workspace, ":w", native, cx);
                Ok(())
            })?;
            until(app, window, "formatted Rust source on disk", |workspace| {
                let doc = &workspace.projects.active().editor.vim().doc;
                Ok(!doc.modified() && fs::read_to_string(&source)? == formatted)
            })
            .await?;

            // Save As uses the destination's config, and the buffer retains its original LF policy.
            step(app, window, |workspace, native, cx| {
                command(workspace, ":w generated/lib.rs", native, cx);
                Ok(())
            })?;
            until(
                app,
                window,
                "formatted Rust Save As destination",
                |workspace| {
                    let doc = &workspace.projects.active().editor.vim().doc;
                    if !destination.exists() {
                        return Ok(false);
                    }
                    ensure!(
                        doc.path
                            .as_ref()
                            .context("Saved file path")?
                            .canonicalize()?
                            == destination.canonicalize()?
                    );
                    let saved = fs::read_to_string(&destination)?;
                    ensure!(
                        saved.contains("\n        let value"),
                        "Unexpected formatting: {saved:?}"
                    );
                    ensure!(
                        !saved.contains('\r'),
                        "Save As changed the existing LF policy"
                    );
                    Ok(true)
                },
            )
            .await?;
            step(app, window, |workspace, native, cx| {
                let editor = &mut workspace.projects.active_mut().editor;
                let doc = &mut editor.buffer_mut().vim.doc;
                doc.replace(0..doc.text.len(), invalid, true);
                command(workspace, ":w", native, cx);
                Ok(())
            })?;
            until(
                app,
                window,
                "save fallback after rustfmt error",
                |workspace| {
                    let project = workspace.projects.active();
                    if project.editor.vim().doc.modified() {
                        return Ok(false);
                    }
                    ensure!(fs::read_to_string(&destination)? == invalid);
                    ensure!(
                        project.status.contains("rustfmt failed"),
                        "{}",
                        project.status
                    );
                    Ok(true)
                },
            )
            .await?;

            step(app, window, |workspace, native, cx| {
                workspace.open_project(&other_project, native, cx);
                workspace.open(other_source.clone(), native, cx);
                workspace.sync(native, cx);
                Ok(())
            })?;
            let other_scope = allow_rust_formatting(app, window).await?;
            ensure!(other_scope != scope);
            step(app, window, |workspace, native, cx| {
                let editor = &mut workspace.projects.active_mut().editor;
                let doc = &mut editor.buffer_mut().vim.doc;
                ensure!(doc.path.as_ref() == Some(&other_source));
                doc.replace(0..doc.text.len(), unformatted, true);
                command(workspace, ":w", native, cx);
                Ok(())
            })?;
            until(app, window, "save in the other Rust project", |workspace| {
                let doc = &workspace.projects.active().editor.vim().doc;
                if doc.modified() {
                    return Ok(false);
                }
                ensure!(
                    fs::read_to_string(&other_source)? == unformatted,
                    "Save formatting leaked into another project"
                );
                Ok(true)
            })
            .await
        },
    )
}

/// Approve tool execution and return the active project scope once its formatter is ready.
/// `app` and `window` host the native workspace and drive the session updates.
async fn allow_rust_formatting(app: &AsyncApp, window: WindowHandle<Workspace>) -> Result<u64> {
    let scope = step(app, window, |workspace, _, _| {
        let scope = workspace.projects.active().plugin_scope;
        let session = workspace
            .plugins
            .session
            .as_ref()
            .context("Plugin session")?;
        session.send(Action::SetProjectExecution(scope, true))?;
        Ok(scope)
    })?;
    until(
        app,
        window,
        "Rust formatter and project tool execution",
        |workspace| {
            Ok(
                workspace.plugins.view.project_execution.get(&scope) == Some(&true)
                    && workspace.plugins.view.modules.iter().any(|(key, state)| {
                        key.instance.project == scope
                            && key.instance.module == "format"
                            && *state == State::Ready
                    }),
            )
        },
    )
    .await?;
    Ok(scope)
}

/// Compare native editor coordinates without unwinding its event loop on failure.
fn ensure_eq(actual: usize, expected: usize) -> Result<()> {
    ensure!(actual == expected, "expected {expected}, got {actual}");
    Ok(())
}
