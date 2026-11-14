//! Exercise a real rust-analyzer through the Windows workspace and the installed Rust plugin.

use super::{
    Workspace,
    native_editing_tests::{options, step, until, use_session},
    rows::Target,
};
use crate::grid::workspace::native_test;
use anyhow::{Context as _, Result, ensure};
use gpui::{Context, Window};
use nanobug::plugins::{
    package::InstallStore,
    session::{Action, Session, State},
};
use std::{fs, path::PathBuf};

/// Send `text` through the editor's normal Ex command parser and workspace dispatch.
fn command(
    workspace: &mut Workspace,
    text: &str,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let edit = workspace
        .projects
        .active_mut()
        .editor
        .edit(|vim, register| {
            vim.text(text, register);
            vim.key("Enter", register)
        });
    workspace.apply_edit(edit, window, cx);
    workspace.sync(window, cx);
}

/// Check completion and live diagnostics against an unsaved overlay, then jump into another file.
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

    native_test::run_async(
        project.clone(),
        options(&project, &profile)?,
        move |workspace| use_session(workspace, session),
        async move |window, app| {
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

            let prefix = "editor_only_fi";
            let field = "editor_only_field";
            let unsaved = concat!(
                "mod helpers;\n",
                "struct Widget { editor_only_field: u32 }\n",
                "pub fn example() -> u32 {\n",
                "    let widget = Widget { editor_only_field: 40 };\n",
                "    let _marker = \"😀\"; let _wrong: u32 = \"bad\";\n",
                "    let _marker = \"😀\"; widget.editor_only_fi + helpers::next_value()\n",
                "}\n",
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
                buffer.vim.set_cursor(field_end);
                command(workspace, ":complete", native, cx);
                Ok(())
            })?;
            until(app, window, "semantic field completion", |workspace| {
                Ok(workspace.plugins.rows().iter().any(|row| {
                    row.label.contains(field) && matches!(&row.target, Target::CompletionEdit(_))
                }))
            })
            .await?;
            step(app, window, |workspace, native, cx| {
                let index = workspace
                    .plugins
                    .rows()
                    .iter()
                    .position(|row| {
                        row.label.contains(field)
                            && matches!(&row.target, Target::CompletionEdit(_))
                    })
                    .context("Semantic field completion")?;
                workspace.plugins.manager.selection = index;
                workspace.plugin_activate(native, cx);
                let editor = &mut workspace.projects.active_mut().editor;
                let document = &mut editor.buffer_mut().vim.doc;
                ensure!(document.text.contains("widget.editor_only_field +"));
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
                let editor = &mut workspace.projects.active_mut().editor;
                let vim = &mut editor.buffer_mut().vim;
                vim.set_cursor(
                    vim.doc
                        .text
                        .find("next_value")
                        .context("Definition reference")?,
                );
                command(workspace, ":definition", native, cx);
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
            .await
        },
    )
}
