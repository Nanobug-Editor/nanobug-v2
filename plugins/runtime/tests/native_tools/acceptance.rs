//! Windows resource and combined failure acceptance using only temporary projects.

use super::*;
use nanobug::{
    document::Document,
    plugins::session::{Config, ConfigScope, Rule},
};
use std::os::windows::fs::OpenOptionsExt;

/// Sample exact fixture processes without summing working sets that contain shared pages.
pub(super) fn resources(
    profile: &Path,
    phase: &str,
    runtime: Option<u32>,
    tool: Option<u32>,
    instances: usize,
) -> Value {
    let host = std::process::id();
    let pids = std::iter::once(host)
        .chain(runtime)
        .chain(tool)
        .map(|pid| pid.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let script = format!(
        "Get-Process -Id {pids} -ErrorAction Stop | ForEach-Object {{ '{{0}},{{1}},{{2}}' -f $_.Id, $_.PrivateMemorySize64, $_.WorkingSet64 }}"
    );
    let output = Command::new(system32().join("WindowsPowerShell/v1.0/powershell.exe"))
        .creation_flags(0x0800_0000)
        .args(powershell(&script))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "could not sample fixture process memory"
    );
    let processes = String::from_utf8_lossy(&output.stdout).lines().map(|line| {
        let values = line.trim().split(',').map(|value| value.parse::<u64>().unwrap()).collect::<Vec<_>>();
        assert_eq!(values.len(), 3);
        let pid = values[0] as u32;
        let role = if pid == host { "session_harness" } else if Some(pid) == runtime { "wasm_worker" } else {
            assert_eq!(Some(pid), tool);
            "native_tool"
        };
        json!({"role":role, "pid":pid, "private_bytes":values[1], "working_set_bytes":values[2]})
    }).collect::<Vec<_>>();
    assert_eq!(
        processes.len(),
        1 + usize::from(runtime.is_some()) + usize::from(tool.is_some())
    );
    json!({"phase":phase, "wasm_instances":instances, "processes":processes, "compiled_cache_bytes": cache_bytes(&profile.join("compiled"))})
}

fn cache_bytes(directory: &Path) -> u64 {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return 0,
        Err(error) => panic!("fixture cache could not be read: {error}"),
    };
    entries
        .map(|entry| {
            let entry = entry.unwrap();
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                cache_bytes(&entry.path())
            } else if kind.is_file() {
                entry.metadata().unwrap().len()
            } else {
                0
            }
        })
        .sum()
}

#[test]
fn editing_apply_update_and_tool_result_survive_the_same_worker_failure() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let mut workspace = project(temp.path(), 1);
    let document_path = workspace.root.join("source.txt");
    let mut document = Document::open(&document_path)?;
    let publish_document = |workspace: &mut Project, document: &Document| {
        workspace.document = Some(Arc::new(DocumentSnapshot {
            path: document.path.clone(),
            opening: document.opening,
            revision: document.revision,
            text: document.text.clone(),
        }));
    };
    publish_document(&mut workspace, &document);
    let settings = workspace.root.join(".nb/plugins.json");
    fs::create_dir(settings.parent().unwrap())?;
    let original = Config::default();
    let original_bytes = serde_json::to_vec(&original)?;
    fs::write(&settings, &original_bytes)?;

    let first = temp.path().join("first.zip");
    let second = temp.path().join("second.zip");
    let args = powershell(
        r#"
        $ErrorActionPreference = 'Stop'
        [IO.File]::WriteAllText((Join-Path (Get-Location) 'operation.pid'), [string]$PID)
        [IO.File]::AppendAllText((Join-Path (Get-Location) 'started'), 'once')
        while (!(Test-Path -LiteralPath 'finish')) { Start-Sleep -Milliseconds 20 }
        [IO.File]::AppendAllText((Join-Path (Get-Location) 'completed'), 'once')
        exit 7
    "#,
    );
    let service = json!({"service":"run_tool","tool":"test-tool","args":args});
    tool_bundle(&first, service.clone(), None);
    versioned_tool_bundle(&second, service, None, "1.1.0");

    let profile = temp.path().join("profile");
    let editor = session(&profile);
    let namespace = install(&editor, first);
    let owner = key(&namespace, &workspace);
    let scope = ConfigScope::Project(workspace.id);
    editor.projects(Some(workspace.id), vec![workspace.clone()]);
    editor.send(Action::SetProjectExecution(workspace.id, true))?;
    let active = wait(&editor, |view| {
        view.modules.get(&owner) == Some(&State::Ready)
            && view.project_execution.get(&workspace.id) == Some(&true)
    });
    let identity = active.installed[0].release.manifest.identity();
    let wasm_pid = active.workers[&namespace];
    let started = workspace.root.join("started");
    let completed = workspace.root.join("completed");
    invoke(&editor, &owner);
    wait(&editor, |_| {
        fs::read_to_string(&started).is_ok_and(|text| text == "once")
    });
    let tool_pid = fs::read_to_string(workspace.root.join("operation.pid"))?.parse()?;
    assert!(process_is_running(tool_pid, "powershell.exe"));
    editor.send(Action::ReviewToolOperations(namespace.clone()))?;
    let pending = wait(&editor, |view| {
        view.tool_operations
            .as_ref()
            .is_some_and(|(owner, receipts)| {
                owner == &namespace
                    && receipts.len() == 1
                    && receipts[0].state == ToolOperationState::Pending
            })
    });
    let operation = pending.tool_operations.as_ref().unwrap().1[0].id.clone();

    let mut proposed = Config::default();
    proposed
        .plugin_mut(identity)
        .rules
        .insert("main".into(), Rule::Always);
    editor.send(Action::StageConfig(scope, proposed.clone()))?;
    editor.send(Action::ApplyConfig(scope))?;
    let waiting = wait(&editor, |view| view.message.starts_with("Apply is waiting"));
    assert_eq!(waiting.configurations[&scope].applied, original);
    editor.send(Action::InspectZip(second))?;
    let reviewed = wait(&editor, |view| view.review.is_some());
    editor.send(Action::Approve(reviewed.review.as_ref().unwrap().token))?;
    let waiting = wait(&editor, |view| view.waiting_for_operations);
    assert_eq!(
        waiting.installed[0].release.manifest.version.to_string(),
        "1.0.0"
    );

    // Keep shared configuration readable while denying replacement, then lose the old guest callback.
    let locked = fs::OpenOptions::new()
        .read(true)
        .share_mode(0x01 | 0x02)
        .open(&settings)?;
    let killed = Command::new(system32().join("taskkill.exe"))
        .creation_flags(0x0800_0000)
        .args(["/PID", &wasm_pid.to_string(), "/F"])
        .output()?;
    assert!(killed.status.success());
    assert!(process_is_running(tool_pid, "powershell.exe"));
    let end = document.text.len();
    document.replace(end..end, "\n已确认👋", true);
    document.save(None)?;
    publish_document(&mut workspace, &document);
    editor.projects(Some(workspace.id), vec![workspace.clone()]);
    fs::write(workspace.root.join("finish"), "")?;

    let recovered = wait(&editor, |view| {
        view.installed[0].release.manifest.version.to_string() == "1.1.0"
            && view.modules.get(&owner) == Some(&State::Ready)
            && view.configurations[&scope].apply_issue.is_some()
    });
    assert_ne!(recovered.workers[&namespace], wasm_pid);
    assert_eq!(recovered.configurations[&scope].applied, original);
    assert_eq!(
        recovered.configurations[&scope].draft.as_ref(),
        Some(&proposed)
    );
    assert_eq!(fs::read(&settings)?, original_bytes);
    assert_eq!(fs::read_to_string(&document_path)?, "saved\n已确认👋");
    assert!(!process_is_running(tool_pid, "powershell.exe"));

    let receipt = |view: &View| {
        view.tool_operations
            .as_ref()
            .is_some_and(|(owner, receipts)| {
                owner == &namespace
                    && receipts.len() == 1
                    && receipts[0].id == operation
                    && receipts[0].state == ToolOperationState::Exited
                    && receipts[0].code == Some(7)
            })
    };
    editor.send(Action::ReviewToolOperations(namespace.clone()))?;
    wait(&editor, receipt);
    drop(locked);
    editor.send(Action::ApplyConfig(scope))?;
    wait(&editor, |view| {
        view.configurations[&scope].applied == proposed
            && view.configurations[&scope].draft.is_none()
            && view.configurations[&scope].apply_issue.is_none()
    });
    assert_eq!(
        serde_json::from_slice::<Config>(&fs::read(settings)?)?,
        proposed
    );
    assert_eq!(fs::read_to_string(&started)?, "once");
    assert_eq!(fs::read_to_string(&completed)?, "once");

    editor.projects(None, Vec::new());
    wait(&editor, |view| view.workers.is_empty());
    drop(editor);
    let reopened = session(&profile);
    reopened.projects(Some(workspace.id), vec![workspace]);
    wait(&reopened, |view| {
        view.modules.get(&owner) == Some(&State::Ready)
    });
    reopened.send(Action::ReviewToolOperations(namespace.clone()))?;
    let final_view = wait(&reopened, receipt);
    assert_eq!(final_view.configurations[&scope].applied, proposed);
    assert_eq!(fs::read_to_string(&started)?, "once");
    assert_eq!(fs::read_to_string(&completed)?, "once");
    Ok(())
}
