//! Run diagnostic mappings, Ex commands and result activation against a real guest.

use super::*;
use crate::grid::windows::editor::ResultList;

/// Route each discrete key through the workspace's normal keyboard and IME handlers.
pub(super) fn press(
    workspace: &mut Workspace,
    keys: &[&'static str],
    native: &mut Window,
    cx: &mut Context<Workspace>,
) -> Result<()> {
    for key in keys {
        workspace.key_down(
            &KeyDownEvent {
                keystroke: Keystroke::parse(key)?,
                is_held: false,
                prefer_character_input: false,
            },
            native,
            cx,
        );
    }
    Ok(())
}

/// Keep live project diagnostics, the saved Quickfix list and window-local entries independent.
#[test]
#[ignore = "requires Windows GPUI and the bundled plugin runtime"]
fn native_diagnostic_lists_scope_jump_and_reopen() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("workspace");
    let profile = temp.path().join("profile");
    let nested = root.join("a-generated-sources-".repeat(7)).join("子目录");
    fs::create_dir_all(&nested)?;
    let first = nested.join("a.txt");
    let second = root.join("b.txt");
    for path in [&first, &second] {
        fs::write(path, "前 warn\nerr\n")?;
    }
    install(
        temp.path(),
        &profile,
        Fixture {
            id: "diagnostic-list",
            event: "diagnose",
            selected: None,
            iterations: 0,
            active: true,
            change_files: false,
            reply: Some(json!({"diagnostics":[
                {"start":4,"end":8,"message":"style warning","severity":"warning"},
                {"start":9,"end":12,"message":"type error","severity":"error"}
            ]})),
        },
    )?;
    native_test::run_async_visible(
        root.clone(),
        options(&root, &profile)?,
        |_| {},
        async move |handle, app| {
            for (path, total) in [(&first, 2), (&second, 4)] {
                step(app, handle, |workspace, native, cx| {
                    workspace.open(path.clone(), native, cx);
                    workspace.sync(native, cx);
                    Ok(())
                })?;
                until(
                    app,
                    handle,
                    "current and retained diagnostics",
                    |workspace| Ok(workspace.diagnostic_entries(None).len() == total),
                )
                .await?;
            }
            step(app, handle, |workspace, native, cx| {
                let ex = |ws: &mut Workspace,
                          line: &str,
                          win: &mut Window,
                          ctx: &mut Context<Workspace>| {
                    ws.execute_ex_input(line, "diagnostic test", 1, false, win, ctx);
                };
                ex(
                    workspace,
                    "lua vim.diagnostic.setqflist({open=false, severity=vim.diagnostic.severity.ERROR})",
                    native,
                    cx,
                );
                ex(
                    workspace,
                    "lua vim.diagnostic.setloclist({open=false})",
                    native,
                    cx,
                );
                press(workspace, &["space", "x", "x"], native, cx)?;
                let editor = &workspace.projects.active().editor;
                ensure!(
                    editor
                        .buffer()
                        .list_owner()
                        .is_some_and(|(list, _)| list == ResultList::Diagnostics),
                    "workspace diagnostics window"
                );
                ensure!(
                    editor.vim().doc.text.contains("a.txt")
                        && editor.vim().doc.text.contains("b.txt"),
                    "both sources"
                );
                press(workspace, &["q"], native, cx)?;
                press(workspace, &["space", "x", "q"], native, cx)?;
                let editor = &workspace.projects.active().editor;
                ensure!(
                    editor
                        .buffer()
                        .list_owner()
                        .is_some_and(|(list, _)| list == ResultList::Quickfix),
                    "quickfix window"
                );
                ensure!(
                    editor.vim().doc.text.contains("type error")
                        && !editor.vim().doc.text.contains("style warning"),
                    "error-only Quickfix snapshot"
                );
                ex(workspace, r"Cfilter /^b\.txt$/", native, cx);
                let text = &workspace.projects.active().editor.vim().doc.text;
                ensure!(
                    text.contains("b.txt") && !text.contains("a.txt"),
                    "filter by file name"
                );
                ex(workspace, "colder", native, cx);
                press(workspace, &["enter"], native, cx)?;
                let doc = &workspace.projects.active().editor.vim().doc;
                ensure!(
                    doc.path.as_ref().is_some_and(|p| p.ends_with("a.txt")) && doc.cursor == 9,
                    "quickfix activates first file"
                );
                let labels = workspace.footer_panel.read(cx).row_labels(0);
                let suffix = Path::new("子目录").join("a.txt");
                ensure!(
                    labels.iter().any(|label| label.starts_with('…')
                        && label.ends_with(suffix.to_string_lossy().as_ref()))
                        && !labels.iter().any(|label| label.contains("type error")),
                    "footer after jumping to an error in a deep path: {labels:?}"
                );
                ex(workspace, "cclose", native, cx);
                press(workspace, &["]", "q"], native, cx)?;
                let doc = &workspace.projects.active().editor.vim().doc;
                ensure!(
                    doc.path.as_ref().is_some_and(|p| p.ends_with("b.txt")) && doc.cursor == 9,
                    "next quickfix target"
                );
                ex(workspace, "lopen", native, cx);
                let editor = &workspace.projects.active().editor;
                ensure!(
                    editor.vim().doc.text.contains("b.txt")
                        && !editor.vim().doc.text.contains("a.txt"),
                    "local to source window"
                );
                ex(workspace, "Lfilter! /style warning/", native, cx);
                let text = &workspace.projects.active().editor.vim().doc.text;
                ensure!(
                    !text.contains("style warning") && text.contains("type error"),
                    "exclude matching location list messages"
                );
                ex(workspace, "lolder", native, cx);
                press(workspace, &["enter"], native, cx)?;
                ensure!(
                    workspace.projects.active().editor.vim().doc.cursor == 4,
                    "UTF-8 column after wide character"
                );
                let labels = workspace.footer_panel.read(cx).row_labels(0);
                ensure!(
                    labels.iter().any(|label| label == "b.txt")
                        && !labels.iter().any(|label| label.contains("style warning")),
                    "footer after jumping to a warning: {labels:?}"
                );
                ex(workspace, "lclose", native, cx);
                press(workspace, &["]", "e"], native, cx)?;
                ensure!(
                    workspace.projects.active().editor.vim().doc.cursor == 9,
                    "next error"
                );
                // A named export is a separate list. Updating the default diagnostics while
                // editing must leave the named list selected, with its own jump position.
                ex(
                    workspace,
                    "lua vim.diagnostic.setqflist({title='Warnings', open=false, severity=2})",
                    native,
                    cx,
                );
                ex(
                    workspace,
                    "lua vim.diagnostic.setqflist({open=false, severity=1})",
                    native,
                    cx,
                );
                ex(workspace, "copen", native, cx);
                ensure!(
                    workspace
                        .projects
                        .active()
                        .editor
                        .vim()
                        .doc
                        .text
                        .contains("style warning"),
                    "named Quickfix selection survived background export"
                );
                ex(workspace, "colder", native, cx);
                let editor = &workspace.projects.active().editor;
                ensure!(
                    !editor.vim().doc.text.contains("style warning")
                        && editor.vim().doc.text.contains("type error"),
                    "older list restored in the open window"
                );
                ex(workspace, "cclose", native, cx);
                // A new source split inherits the parent's local list and can replace its
                // copy without changing the parent, even when both files are already open.
                let parent = workspace.projects.active().active_editor_id();
                ex(workspace, "split", native, cx);
                ensure!(
                    workspace.projects.active().active_editor_id() != parent,
                    "new split"
                );
                workspace.open(first.clone(), native, cx);
                ex(workspace, "lua vim.diagnostic.setloclist()", native, cx);
                let editor = &workspace.projects.active().editor;
                ensure!(
                    editor.vim().doc.text.contains("a.txt")
                        && !editor.vim().doc.text.contains("b.txt"),
                    "child location list replaced"
                );
                ex(workspace, "lclose", native, cx);
                workspace.focus_editor_pane(parent, native, cx);
                ex(workspace, "lopen", native, cx);
                let editor = &workspace.projects.active().editor;
                ensure!(
                    editor.vim().doc.text.contains("b.txt")
                        && !editor.vim().doc.text.contains("a.txt"),
                    "parent location list preserved"
                );
                ex(workspace, "redir > jump-output.txt", native, cx);
                ex(workspace, "ll 1", native, cx);
                ex(workspace, "redir END", native, cx);
                ensure!(
                    fs::read_to_string(root.join("jump-output.txt"))?.contains("style warning"),
                    "explicit Ex redirection receives the diagnostic"
                );
                Ok(())
            })
        },
    )
}
