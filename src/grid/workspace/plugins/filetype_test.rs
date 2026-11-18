//! Verify that an Ex-assigned filetype reaches the installed Python plugin.

use super::native_editing_tests::{options, step, until, use_session};
use crate::grid::workspace::native_test;
use anyhow::{Result, ensure};
use nanobug::plugins::{package::InstallStore, session::Session};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Activate the Python pilot for an extensionless file and return to path-based detection.
#[test]
#[ignore = "requires a built Python plugin ZIP and the Windows plugin runtime"]
fn native_ex_python_filetype_override_uses_the_installed_plugin() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    let profile = temp.path().join("profile");
    fs::create_dir(&project)?;
    let path = project.join("snippet");
    let ordinary = project.join("normal.py");
    let text = "def title():\n    return '中文'\n";
    fs::write(&path, text)?;
    fs::write(&ordinary, text)?;
    let path = path.canonicalize()?;
    let ordinary = ordinary.canonicalize()?;
    let archives = std::env::var_os("NANOBUG_LANGUAGE_ARCHIVES")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("dist/language-plugins"));
    let store = InstallStore::new(profile.clone());
    let package = store.prepare(&archives.join("nanobug-python-plugin.zip"))?;
    let grants = package.release.manifest.permissions.clone();
    let installed = store.install(package, &grants)?;
    let namespace = installed.release.manifest.identity().namespace();
    let startup = options(temp.path(), &profile)?;
    let (session, _updates) = Session::start(options(temp.path(), &profile)?);
    native_test::run_async(
        project,
        startup,
        move |workspace| use_session(workspace, session),
        async move |handle, app| {
            let _temp = temp;
            let syntax_ready = |workspace: &mut super::Workspace| {
                let doc = &workspace.projects.active().editor.vim().doc;
                Ok(workspace.plugins.view.document_presentations.iter().any(
                    |(key, presentation)| {
                        key.namespace == namespace
                            && key.instance.module == "document"
                            && presentation.version == (doc.opening, doc.revision)
                            && !presentation.features.highlights.is_empty()
                    },
                ))
            };
            step(app, handle, |workspace, window, cx| {
                workspace.execute_ex_input(
                    "edit snippet | setlocal filetype=python",
                    "test",
                    1,
                    false,
                    window,
                    cx,
                );
                workspace.sync(window, cx);
                let vim = workspace.projects.active().editor.vim();
                ensure!(
                    vim.doc.path.as_ref() == Some(&path),
                    "{}",
                    workspace.projects.active().status
                );
                ensure!(vim.filetype_override().as_deref() == Some("python"));
                ensure!(vim.doc.text == text && !vim.doc.modified());
                Ok(())
            })?;
            until(
                app,
                handle,
                "Python syntax for a manually typed extensionless file",
                syntax_ready,
            )
            .await?;
            step(app, handle, |workspace, window, cx| {
                workspace.execute_ex_input("setlocal filetype=", "test", 1, false, window, cx);
                workspace.sync(window, cx);
                ensure!(
                    workspace
                        .projects
                        .active()
                        .editor
                        .vim()
                        .filetype_override()
                        .is_none()
                );
                Ok(())
            })?;
            until(
                app,
                handle,
                "old Python presentation removed",
                |workspace| Ok(!syntax_ready(workspace)?),
            )
            .await?;
            step(app, handle, |workspace, window, cx| {
                workspace.execute_ex_input("edit normal.py", "test", 1, false, window, cx);
                workspace.sync(window, cx);
                ensure!(
                    workspace.projects.active().editor.vim().doc.path.as_ref() == Some(&ordinary)
                );
                Ok(())
            })?;
            until(
                app,
                handle,
                "ordinary Python extension matching",
                syntax_ready,
            )
            .await?;
            ensure!(fs::read_to_string(&path)? == text);
            Ok(())
        },
    )
}
