//! Exercise recovery through the Ex dispatcher with an isolated recovery directory.

use super::{Draft, Recovery, Store, Workspace};
use crate::grid::workspace::native_test;
use anyhow::{Result, ensure};
use gpui::{AppContext, AsyncApp, Context, Window, WindowHandle};
use nanobug::{document::Document, i18n, plugins::session::Options, vim::Action};
use std::{fs, path::Path, time::Duration};

/// Save a file's draft and release the former owner's lock as a terminated process would.
fn abandon(root: &Path, original: &Path, text: &str) -> Result<()> {
    let mut doc = Document::open(original)?;
    doc.replace(0..doc.text.len(), text, true);
    let parent = original.parent().unwrap();
    let store = Store::new(root.to_owned())?;
    store.write(
        doc.buffer_number(),
        &Draft {
            scope: parent.to_owned(),
            buffer: doc.recovery_draft(),
        },
    )?;
    Ok(())
}

/// Execute typed or sourced Ex on the workspace's active editor and rebuild its visible state.
fn ex(workspace: &mut Workspace, line: &str, window: &mut Window, cx: &mut Context<Workspace>) {
    workspace.execute_ex_input(line, "test", 1, false, window, cx);
    workspace.sync(window, cx);
}

/// Keep native tests away from the real plugin profile and production recovery directory.
fn isolated(root: &Path) -> Result<Options> {
    let profile = root.join("profile");
    Options::take(
        &mut vec![
            "--no-plugins".into(),
            "--plugin-profile".into(),
            profile.into(),
        ],
        root,
    )
}

/// Give the offscreen editor enough rows and columns for the same splits as the real app.
async fn size_editor(handle: WindowHandle<Workspace>, app: &mut AsyncApp) -> Result<()> {
    app.update_window(handle.into(), |_, window, _| {
        window.resize(gpui::size(gpui::px(1200.), gpui::px(880.)));
    })?;
    app.background_executor()
        .timer(Duration::from_millis(60))
        .await;
    Ok(())
}

/// Recover a shared draft after a crash and require an explicit save to update the file.
#[test]
#[ignore = "requires Windows DirectWrite and DirectX"]
fn native_ex_recovers_shared_drafts_without_writing_and_guards_disk_changes() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    fs::create_dir(&project)?;
    let recovery = temp.path().join("recovery");
    let file = project.join("main.txt");
    let other = project.join("other.txt");
    let private = project.join("private.txt");
    fs::write(&file, "original\n")?;
    fs::write(&other, "original\n")?;
    fs::write(&private, "original\n")?;
    abandon(&recovery, &file, "unsaved 中文\n")?;
    abandon(&recovery, &other, "other draft\n")?;
    abandon(&recovery, &private, "private draft\n")?;
    fs::write(&other, "external change\n")?;
    let init = recovery.clone();
    native_test::run_async(
        project,
        isolated(temp.path())?,
        move |workspace| {
            workspace.recovery = Recovery::new(Some(Ok(init)));
        },
        async move |handle, app| {
            size_editor(handle, app).await?;
            app.update(|app| {
                handle.update(app, |workspace, window, cx| {
                    let _keep_temp = temp;
                    workspace.sync(window, cx);
                    ex(workspace, "edit main.txt | split | recover", window, cx);
                    let project = workspace.projects.active();
                    let doc = &project.editor.vim().doc;
                    let id = doc.buffer_number();
                    ensure!(
                        doc.text == "unsaved 中文\n" && doc.modified(),
                        "{}",
                        project.status
                    );
                    ensure!(fs::read_to_string(&file)? == "original\n");
                    ensure!(
                        project
                            .editors()
                            .filter(|(_, editor)| {
                                editor.vim().doc.buffer_number() == id
                                    && editor.vim().doc.text == "unsaved 中文\n"
                            })
                            .count()
                            == 2
                    );

                    ex(workspace, "swapname", window, cx);
                    let path = workspace.projects.active().editor.vim().message.clone();
                    ensure!(Path::new(&path).is_file(), "{path}");
                    ex(workspace, "write", window, cx);
                    ensure!(fs::read_to_string(&file)? == "unsaved 中文\n");
                    ensure!(!Path::new(&path).exists());

                    ex(workspace, "recover other.txt", window, cx);
                    ensure!(workspace.projects.active().editor.vim().doc.text == "other draft\n");
                    ensure!(
                        workspace.projects.active().editor.vim().message
                            == i18n::text("ex.recovery_conflict")
                    );
                    ex(workspace, "write", window, cx);
                    ensure!(fs::read_to_string(&other)? == "external change\n");
                    ex(workspace, "write saved-copy.txt", window, cx);
                    ensure!(
                        fs::read_to_string(other.with_file_name("saved-copy.txt"))?
                            == "other draft\n"
                    );
                    ex(workspace, "noswapfile recover private.txt", window, cx);
                    let vim = workspace.projects.active().editor.vim();
                    ensure!(vim.doc.text == "private draft\n" && !vim.swapfile_enabled());
                    {
                        let writer = workspace.recovery.writer.as_ref().unwrap().lock().unwrap();
                        ensure!(!writer.store.path(vim.doc.buffer_number()).exists());
                        let path = private.canonicalize()?;
                        ensure!(
                            writer
                                .store
                                .find(&workspace.projects.active().root, Some(&path))?
                                .is_some()
                        );
                    }
                    workspace.recovery.finish();
                    ensure!(!Path::new(&path).exists());
                    Ok(())
                })
            })?
        },
    )
}

/// Publish one automatic draft for a hidden shared buffer and respect the Ex modifier's scope.
#[test]
#[ignore = "requires Windows DirectWrite and DirectX"]
fn native_ex_checkpoints_hidden_edits_and_scopes_noswapfile() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    let recovery = temp.path().join("recovery");
    fs::create_dir(&project)?;
    for name in ["first.txt", "second.txt", "third.txt"] {
        fs::write(project.join(name), "original\n")?;
    }
    let init = recovery.clone();
    native_test::run_async(
        project,
        isolated(temp.path())?,
        move |workspace| {
            workspace.recovery = Recovery::new(Some(Ok(init)));
        },
        async move |handle, app| {
            size_editor(handle, app).await?;
            let (hidden, disabled) = app.update(|app| {
                handle.update(app, |workspace, window, cx| {
                    workspace.sync(window, cx);
                    workspace.start_recovery(window, cx);
                    ex(workspace, "edit first.txt", window, cx);
                    workspace.run_vim(
                        |vim, _| {
                            vim.doc.replace(0..0, "hidden ", true);
                            Action::None
                        },
                        window,
                        cx,
                    );
                    let hidden = workspace.projects.active().editor.vim().doc.buffer_number();
                    ex(
                        workspace,
                        "hide edit second.txt | noswapfile edit first.txt",
                        window,
                        cx,
                    );
                    ensure!(workspace.projects.active().editor.vim().swapfile_enabled());
                    ex(
                        workspace,
                        "hide edit second.txt | noswapfile vertical split third.txt",
                        window,
                        cx,
                    );
                    let vim = workspace.projects.active().editor.vim();
                    ensure!(!vim.swapfile_enabled());
                    let disabled = vim.doc.buffer_number();
                    workspace.run_vim(
                        |vim, _| {
                            vim.doc.replace(0..0, "private ", true);
                            Action::None
                        },
                        window,
                        cx,
                    );
                    ex(workspace, "preserve", window, cx);
                    ensure!(
                        workspace
                            .projects
                            .active()
                            .status
                            .contains(i18n::text("ex.recovery_disabled"))
                    );
                    Ok::<_, anyhow::Error>((hidden, disabled))
                })
            })??;
            app.background_executor()
                .timer(Duration::from_millis(1700))
                .await;
            app.update(|app| {
                handle.update(app, |workspace, window, cx| {
                    let _keep_temp = temp;
                    let writer = workspace.recovery.writer.as_ref().unwrap().clone();
                    let (hidden_path, disabled_path) = {
                        let writer = writer.lock().unwrap();
                        let hidden_path = writer.store.path(hidden);
                        let saved = writer
                            .store
                            .open(&hidden_path)?
                            .expect("automatic hidden draft");
                        ensure!(saved.draft.buffer.text == "hidden original\n");
                        (hidden_path, writer.store.path(disabled))
                    };
                    ensure!(!disabled_path.exists());
                    ex(workspace, "setlocal swapfile | preserve", window, cx);
                    ensure!(disabled_path.exists());
                    workspace.recovery.finish();
                    ensure!(!hidden_path.exists() && !disabled_path.exists());
                    Ok(())
                })
            })?
        },
    )
}
