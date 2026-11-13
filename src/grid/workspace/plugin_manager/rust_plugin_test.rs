//! End-to-end test for the sibling Rust plugin ZIP in the native Windows editor.

use super::{
    Workspace,
    native_editing_tests::{options, step, until, use_session},
    rows::Target,
};
use crate::grid::workspace::native_test;
use anyhow::{Context, Result, ensure};
use gpui::{Context as GpuiContext, Window};
use nanobug::plugins::{
    package::InstallStore,
    session::{Session, State},
};
use std::{fs, path::PathBuf};

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
    let archive = std::env::var_os("NANOBUG_RUST_PLUGIN_ZIP")
        .map(PathBuf::from)
        .context("Set NANOBUG_RUST_PLUGIN_ZIP to the built Rust plugin ZIP")?;
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
    let store = InstallStore::new(profile.clone());
    let prepared = store.prepare(&archive)?;
    let grants = prepared.release.manifest.permissions.clone();
    store.install(prepared, &grants)?;
    let (session, _) = Session::start(options(&project, &profile)?);

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
                workspace.open(source.clone(), native, cx);
                workspace.sync(native, cx);
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
                let vim = &mut editor.buffers[editor.active].vim;
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
                let doc = &mut editor.buffers[editor.active].vim.doc;
                let at = doc.text.find("todo!()").unwrap() + "todo!(".len();
                doc.replace(at..at + 1, "", true);
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

/// Compare native editor coordinates without unwinding its event loop on failure.
fn ensure_eq(actual: usize, expected: usize) -> Result<()> {
    ensure!(actual == expected, "expected {expected}, got {actual}");
    Ok(())
}
