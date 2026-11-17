//! Verify Session lifetime and persistence against an actual Windows pseudoconsole.

use super::Workspace;
use crate::grid::workspace::{editor_close::CloseTarget, native_test};
use anyhow::{Result, ensure};
use nanobug::plugins::session::Options;
use std::{
    fs, io,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT},
    System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
};

/// Hold a kernel handle so the wait refers to this process even if its PID is later reused.
fn process_handle(pid: u32) -> io::Result<OwnedHandle> {
    let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

/// Poll the process without blocking the native UI thread.
fn process_exited(process: &OwnedHandle) -> io::Result<bool> {
    match unsafe { WaitForSingleObject(process.as_raw_handle(), 0) } {
        WAIT_OBJECT_0 => Ok(true),
        WAIT_TIMEOUT => Ok(false),
        _ => Err(io::Error::last_os_error()),
    }
}

/// A project close releases a hidden PTY, and a snapshot restores only the project views.
#[test]
#[ignore = "requires the Windows DirectWrite and DirectX renderer and ConPTY"]
fn native_terminal_stops_when_its_project_closes_without_replaying_on_restore() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().join("project");
    fs::create_dir(&root)?;
    fs::write(root.join("README.md"), "initial view\n")?;
    fs::write(root.join("notes.txt"), "restored view\n")?;
    let root = root.canonicalize()?;
    let notes = root.join("notes.txt");
    let runs = root.join("pty-runs.txt");
    let script = root.join("pty-run.ps1");
    fs::write(
        &script,
        "Add-Content -LiteralPath (Join-Path $PSScriptRoot 'pty-runs.txt') -Value $PID\nStart-Sleep -Seconds 60\n",
    )?;
    let script = script.to_str().expect("temporary script path");
    let script = script.strip_prefix(r"\\?\").unwrap_or(script);
    let command = format!(
        "powershell.exe -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File \"{script}\""
    );
    let sessions = temporary.path().join("sessions");
    let options = Options::take(&mut vec!["--no-plugins".into()], temporary.path())?;
    native_test::run_async(
        root.clone(),
        options,
        move |workspace: &mut Workspace| workspace.projects.use_project_session_store(sessions),
        async move |handle, app| {
            let _temporary = temporary;
            let number = app.update(|app| {
                handle.update(app, |workspace, window, cx| -> Result<u64> {
                    workspace.open_terminal(Some(command), window, cx);
                    let project = workspace.projects.active_mut();
                    let buffer = project.editor.buffer();
                    let number = buffer.vim.doc.buffer_number();
                    let id = buffer.id;
                    ensure!(project.editor.remove_buffer_tab(id, 0.));
                    ensure!(project.terminal(number).is_some());
                    project.open_page(Some(notes.clone()), 0.)?;
                    ensure!(project.page_count() == 2);
                    ensure!(project.editors().all(|(_, editor)| {
                        editor
                            .buffers()
                            .iter()
                            .all(|buffer| buffer.terminal().is_none())
                    }));
                    Ok(number)
                })
            })??;

            let deadline = Instant::now() + Duration::from_secs(10);
            let pid = loop {
                if let Ok(text) = fs::read_to_string(&runs)
                    && let Ok(pid) = text.trim().parse::<u32>()
                {
                    break pid;
                }
                ensure!(
                    Instant::now() < deadline,
                    "PTY child did not report its PID"
                );
                app.background_executor()
                    .timer(Duration::from_millis(20))
                    .await;
            };
            let process = process_handle(pid)?;
            ensure!(
                !process_exited(&process)?,
                "fixture child exited before project close"
            );

            app.update(|app| {
                handle.update(app, |workspace, window, cx| -> Result<()> {
                    workspace.request_close(CloseTarget::Tab, window, cx);
                    ensure!(workspace.projects.active_project().is_none());
                    Ok(())
                })
            })??;
            let deadline = Instant::now() + Duration::from_secs(10);
            while !process_exited(&process)? {
                ensure!(
                    Instant::now() < deadline,
                    "a hidden PTY outlived its project"
                );
                app.background_executor()
                    .timer(Duration::from_millis(20))
                    .await;
            }

            app.update(|app| {
                handle.update(app, |workspace, window, cx| -> Result<()> {
                    let (tab, added) = workspace.projects.add(root, None);
                    ensure!(added && workspace.select_tab(tab, window, cx));
                    let project = workspace.projects.active();
                    ensure!(
                        project.page_count() == 2,
                        "saved Tabpages were not restored"
                    );
                    ensure!(project.editor.vim().doc.path.as_deref() == Some(notes.as_path()));
                    ensure!(project.terminal(number).is_none());
                    ensure!(
                        project
                            .session_buffer_entries()
                            .values()
                            .all(|buffer| buffer.terminal().is_none())
                    );
                    Ok(())
                })
            })??;
            app.background_executor()
                .timer(Duration::from_secs(1))
                .await;
            ensure!(
                fs::read_to_string(&runs)?.trim() == pid.to_string(),
                "restoring replayed the PTY command"
            );
            Ok(())
        },
    )
}
