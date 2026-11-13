//! Exercise project execution authority, native service recovery, and durable tool results.

#![cfg(windows)]

#[path = "native_tools/acceptance.rs"]
mod acceptance;

use nanobug::plugins::session::{
    Action, BackgroundToolPhase, Key, Options, Project, Session, State, ToolOperationState, View,
};
use nanobug_plugin_sdk::{DocumentSnapshot, Instance, PluginEvent};
use serde_json::{Value, json};
use std::{
    fs::{self, File},
    io::Write,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::{Duration, Instant},
};
use zip::{ZipWriter, write::SimpleFileOptions};

fn system32() -> PathBuf {
    PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32")
}

fn process_is_running(pid: u32, image: &str) -> bool {
    let processes = Command::new(system32().join("tasklist.exe"))
        .creation_flags(0x0800_0000)
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .output()
        .unwrap();
    assert!(processes.status.success());
    String::from_utf8_lossy(&processes.stdout)
        .to_lowercase()
        .contains(&format!("\"{image}\",\"{pid}\""))
}

fn tool_bundle(path: &Path, service: Value, background: Option<&[String]>) {
    versioned_tool_bundle(path, service, background, "1.0.0");
}

fn versioned_tool_bundle(
    path: &Path,
    service: Value,
    background: Option<&[String]>,
    version: &str,
) {
    let executable = system32().join("WindowsPowerShell/v1.0/powershell.exe");
    let mut manifest = format!(
        "name='Native Tool Fixture'\nid='native-tool-fixture'\nversion={version:?}\n\
         permissions=['read_document','run_tool']\n\
         [engines]\nnanobug='^0.1.0'\n\
         [[modules]]\nid='main'\nwasm='main.wasm'\n\
         [settings.executable]\ntitle='Executable'\ndefault={}\n\
         [tools.test-tool]\npath_setting='executable'\n",
        serde_json::to_string(&executable).unwrap()
    );
    if let Some(args) = background {
        manifest.push_str(&format!(
            "background={{ args = {} }}\n",
            serde_json::to_string(args).unwrap()
        ));
    }
    let output = json!({"services":[{"token":1,"operation":service}]}).to_string();
    let bytes = output
        .bytes()
        .map(|byte| format!("\\{byte:02x}"))
        .collect::<String>();
    let pointer = (16_u64 << 32) | output.len() as u64;
    let wasm = wat::parse_str(format!(
        "(module
            (memory (export \"memory\") 20)
            (data (i32.const 16) \"{bytes}\")
            (func (export \"nb_alloc\") (param i32) (result i32) i32.const 2048)
            (func (export \"nb_run\") (param i32 i32) (result i64) i64.const {pointer})
            (func (export \"nb_free\") (param i32 i32)))"
    ))
    .unwrap();
    let mut archive = ZipWriter::new(File::create(path).unwrap());
    for (name, contents) in [
        ("nanobug-plugin.toml", manifest.as_bytes()),
        ("main.wasm", &wasm),
    ] {
        archive
            .start_file(name, SimpleFileOptions::default())
            .unwrap();
        archive.write_all(contents).unwrap();
    }
    archive.finish().unwrap();
}

fn session(profile: &Path) -> Session {
    let mut arguments = vec!["--plugin-profile".into(), profile.into()];
    Session::start(Options::take(&mut arguments, profile.parent().unwrap()).unwrap()).0
}

fn wait(session: &Session, predicate: impl Fn(&View) -> bool) -> Arc<View> {
    let end = Instant::now() + Duration::from_secs(15);
    loop {
        let view = session.view();
        if predicate(&view) {
            return view;
        }
        assert!(
            Instant::now() < end,
            "message={}; modules={:?}; tools={:?}; logs={:?}",
            view.message,
            view.modules,
            view.background_tools,
            view.logs
        );
        std::thread::sleep(Duration::from_millis(15));
    }
}

fn install(session: &Session, bundle: PathBuf) -> String {
    session.send(Action::InspectZip(bundle)).unwrap();
    let view = wait(session, |view| view.review.is_some());
    let review = view.review.as_ref().unwrap();
    session.send(Action::Approve(review.token)).unwrap();
    wait(session, |view| !view.installed.is_empty());
    review.manifest.identity().namespace()
}

fn project(root: &Path, id: u64) -> Project {
    let root = root.join(format!("project-{id}"));
    fs::create_dir(&root).unwrap();
    let path = root.join("source.txt");
    fs::write(&path, "saved").unwrap();
    Project {
        id,
        root,
        document: Some(Arc::new(DocumentSnapshot {
            path: Some(path),
            opening: id,
            revision: 1,
            text: "initial unsaved".into(),
        })),
        open_documents: Vec::new(),
    }
}

fn key(namespace: &str, project: &Project) -> Key {
    Key {
        namespace: namespace.into(),
        instance: Instance {
            project: project.id,
            module: "main".into(),
        },
    }
}

fn invoke(session: &Session, key: &Key) {
    session
        .send(Action::Invoke {
            key: key.clone(),
            event: PluginEvent::Command {
                id: "tool".into(),
                argument: Value::Null,
            },
            reply: None,
            timeout: Duration::from_secs(3),
        })
        .unwrap();
}

fn powershell(script: &str) -> Vec<String> {
    [
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        script,
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

fn update(project: &mut Project, text: &str) {
    let snapshot = Arc::make_mut(project.document.as_mut().unwrap());
    snapshot.revision += 1;
    snapshot.text = text.into();
}

#[test]
fn a_background_tool_recovers_with_new_unsaved_text_and_closes_with_its_project() {
    let directory = tempfile::tempdir().unwrap();
    let mut first = project(directory.path(), 1);
    let second = project(directory.path(), 2);
    let bundle = directory.path().join("fixture.zip");
    let args = powershell(
        r#"
        $ErrorActionPreference = 'Stop'
        while ($null -ne ($line = [Console]::ReadLine())) {
            $sync = ConvertFrom-Json -InputObject $line
            if ($sync.protocol -ne 1) { exit 9 }
            $text = $sync.documents[0].text
            if ($text -eq 'crash once') { exit 8 }
            [IO.File]::WriteAllText((Join-Path (Get-Location) 'seen.txt'), $text)
            [Console]::WriteLine(('{0}"revision":{1}{2}' -f '{', $sync.revision, '}'))
        }
        "#,
    );
    tool_bundle(
        &bundle,
        json!({"service":"start_background_tool","tool":"test-tool"}),
        Some(&args),
    );
    let editor = session(&directory.path().join("profile"));
    let namespace = install(&editor, bundle);
    let a = key(&namespace, &first);
    let b = key(&namespace, &second);
    editor.projects(Some(first.id), vec![first.clone(), second.clone()]);
    let initial = wait(&editor, |view| {
        view.modules.get(&a) == Some(&State::Ready) && view.modules.get(&b) == Some(&State::Ready)
    });
    let worker = initial.workers[&namespace];
    invoke(&editor, &a);
    let blocked = wait(&editor, |view| {
        view.logs.get(&namespace).is_some_and(|logs| {
            logs.iter()
                .any(|line| line.contains("Project execution is blocked"))
        })
    });
    assert!(blocked.background_tools.is_empty());
    for id in [first.id, second.id] {
        editor.send(Action::SetProjectExecution(id, true)).unwrap();
    }
    wait(&editor, |view| {
        view.project_execution.values().filter(|&&v| v).count() == 2
    });
    invoke(&editor, &a);
    invoke(&editor, &b);
    let ready = |view: &View, key: &Key| {
        view.background_tools
            .get(key)
            .and_then(|tools| tools.get("test-tool"))
            .filter(|status| status.phase == BackgroundToolPhase::Ready)
            .and_then(|status| status.pid)
    };
    let initial = wait(&editor, |view| {
        ready(view, &a).is_some() && ready(view, &b).is_some()
    });
    let original_a = ready(&initial, &a).unwrap();
    let original_b = ready(&initial, &b).unwrap();
    assert_ne!(original_a, original_b);
    assert_eq!(
        fs::read_to_string(first.root.join("seen.txt")).unwrap(),
        "initial unsaved"
    );

    update(&mut first, "crash once");
    editor.projects(Some(first.id), vec![first.clone(), second.clone()]);
    let recovered = wait(&editor, |view| {
        view.background_tools
            .get(&a)
            .is_some_and(|tools| tools["test-tool"].phase == BackgroundToolPhase::Recovering)
    });
    assert_eq!(ready(&recovered, &b), Some(original_b));
    assert_eq!(recovered.workers[&namespace], worker);
    update(&mut first, "latest unsaved after crash");
    editor.projects(Some(second.id), vec![first.clone(), second.clone()]);
    let recovered = wait(&editor, |view| {
        ready(view, &a).is_some_and(|pid| pid != original_a)
    });
    let restarted = ready(&recovered, &a).unwrap();
    assert_eq!(ready(&recovered, &b), Some(original_b));
    assert_eq!(
        fs::read_to_string(first.root.join("seen.txt")).unwrap(),
        "latest unsaved after crash"
    );
    assert_eq!(
        fs::read_to_string(first.root.join("source.txt")).unwrap(),
        "saved"
    );

    editor.projects(Some(second.id), vec![second]);
    let closed = wait(&editor, |view| !view.background_tools.contains_key(&a));
    assert_eq!(ready(&closed, &b), Some(original_b));
    assert!(!process_is_running(restarted, "powershell.exe"));
}

#[test]
fn a_one_time_tool_result_is_reviewable_after_disable_reopen_and_uninstall() {
    let directory = tempfile::tempdir().unwrap();
    let project = project(directory.path(), 1);
    let bundle = directory.path().join("fixture.zip");
    let args = powershell(
        "$ErrorActionPreference='Stop'; [IO.File]::AppendAllText((Join-Path (Get-Location) 'count.txt'), 'once'); Start-Sleep -Milliseconds 700; exit 7",
    );
    tool_bundle(
        &bundle,
        json!({"service":"run_tool","tool":"test-tool","args":args}),
        None,
    );
    let profile = directory.path().join("profile");
    let editor = session(&profile);
    let namespace = install(&editor, bundle);
    let key = key(&namespace, &project);
    editor.projects(Some(project.id), vec![project.clone()]);
    editor
        .send(Action::SetProjectExecution(project.id, true))
        .unwrap();
    wait(&editor, |view| {
        view.modules.get(&key) == Some(&State::Ready)
            && view.project_execution.get(&project.id) == Some(&true)
    });
    invoke(&editor, &key);
    let marker = project.root.join("count.txt");
    wait(&editor, |_| marker.is_file());
    // The result can outlive the calling runtime and the editor session.
    editor
        .send(Action::SetEnabled(namespace.clone(), false))
        .unwrap();
    wait(&editor, |view| !view.workers.contains_key(&namespace));
    let exit = |view: &View| {
        view.tool_operations
            .as_ref()
            .is_some_and(|(owner, receipts)| {
                owner == &namespace
                    && receipts.len() == 1
                    && receipts[0].state == ToolOperationState::Exited
                    && receipts[0].code == Some(7)
            })
    };
    editor
        .send(Action::ReviewToolOperations(namespace.clone()))
        .unwrap();
    wait(&editor, exit);
    drop(editor);

    let reopened = session(&profile);
    wait(&reopened, |view| !view.installed.is_empty());
    reopened.send(Action::Uninstall(namespace.clone())).unwrap();
    wait(&reopened, |view| {
        view.retained.contains_key(&namespace) && view.installed.is_empty()
    });
    reopened
        .send(Action::ReviewToolOperations(namespace.clone()))
        .unwrap();
    wait(&reopened, exit);
    assert_eq!(fs::read_to_string(marker).unwrap(), "once");
}

#[test]
fn closing_a_project_drains_its_dispatched_writes_and_seals_new_commands() {
    let directory = tempfile::tempdir().unwrap();
    let first = project(directory.path(), 1);
    let second = project(directory.path(), 2);
    let bundle = directory.path().join("fixture.zip");
    let args = powershell(
        r#"
        $ErrorActionPreference = 'Stop'
        [IO.File]::AppendAllText((Join-Path (Get-Location) 'started.txt'), 'once')
        while (!(Test-Path -LiteralPath 'release.txt')) { Start-Sleep -Milliseconds 15 }
        [IO.File]::WriteAllText((Join-Path (Get-Location) 'finished.txt'), 'done')
        "#,
    );
    tool_bundle(
        &bundle,
        json!({"service":"run_tool","tool":"test-tool","args":args}),
        None,
    );
    let editor = session(&directory.path().join("profile"));
    let namespace = install(&editor, bundle);
    let a = key(&namespace, &first);
    let b = key(&namespace, &second);
    editor.projects(Some(first.id), vec![first.clone(), second.clone()]);
    for id in [first.id, second.id] {
        editor.send(Action::SetProjectExecution(id, true)).unwrap();
    }
    wait(&editor, |view| {
        view.modules.get(&a) == Some(&State::Ready)
            && view.modules.get(&b) == Some(&State::Ready)
            && view
                .project_execution
                .values()
                .filter(|&&allowed| allowed)
                .count()
                == 2
    });

    invoke(&editor, &a);
    invoke(&editor, &b);
    let drained = editor.drain_for_close(Some(first.id)).unwrap();
    invoke(&editor, &a);
    wait(&editor, |view| {
        view.message == "Project is waiting for confirmed plugin commands to finish"
            && first.root.join("started.txt").is_file()
            && second.root.join("started.txt").is_file()
    });
    assert!(drained.is_empty());
    fs::write(first.root.join("release.txt"), "").unwrap();
    wait(&editor, |_| !drained.is_empty());
    drained.try_recv().unwrap().unwrap();
    assert_eq!(
        fs::read_to_string(first.root.join("finished.txt")).unwrap(),
        "done"
    );
    assert_eq!(
        fs::read_to_string(first.root.join("started.txt")).unwrap(),
        "once"
    );
    assert!(!second.root.join("finished.txt").exists());

    editor.projects(Some(second.id), vec![second.clone()]);
    let exit = editor.drain_for_close(None).unwrap();
    fs::write(second.root.join("release.txt"), "").unwrap();
    wait(&editor, |_| !exit.is_empty());
    exit.try_recv().unwrap().unwrap();
    assert_eq!(
        fs::read_to_string(second.root.join("finished.txt")).unwrap(),
        "done"
    );
}

#[test]
fn reopening_vue_scopes_releases_processes_and_preserves_a_real_unknown_operation() {
    let directory = tempfile::tempdir().unwrap();
    let rust = project(directory.path(), 1);
    let vue = project(directory.path(), 2);
    fs::write(
        rust.root.join("Cargo.toml"),
        "[package]\nname='fixture'\nversion='0.0.0'\n",
    )
    .unwrap();
    fs::write(
        vue.root.join("package.json"),
        r#"{"name":"fixture","dependencies":{"vue":"3.5.0"}}"#,
    )
    .unwrap();
    let bundle = directory.path().join("fixture.zip");
    let args = powershell(
        r#"
        $ErrorActionPreference = 'Stop'
        [IO.File]::WriteAllText((Join-Path (Get-Location) 'tool.pid'), [string]$PID)
        [IO.File]::AppendAllText((Join-Path (Get-Location) 'started.txt'), 'once')
        while (!(Test-Path -LiteralPath 'release.txt')) { Start-Sleep -Milliseconds 25 }
        [IO.File]::AppendAllText((Join-Path (Get-Location) 'finished.txt'), 'done')
        "#,
    );
    tool_bundle(
        &bundle,
        json!({"service":"run_tool","tool":"test-tool","args":args}),
        None,
    );
    let profile = directory.path().join("profile");
    let editor = session(&profile);
    let namespace = install(&editor, bundle);
    let rust_key = key(&namespace, &rust);
    editor.projects(Some(rust.id), vec![rust.clone()]);
    let first = wait(&editor, |view| {
        view.modules.get(&rust_key) == Some(&State::Ready)
    });
    let runtime = first.workers[&namespace];
    assert!(process_is_running(runtime, "nanobug-plugin-runtime.exe"));
    let mut samples = vec![acceptance::resources(
        &profile,
        "rust-only",
        Some(runtime),
        None,
        1,
    )];
    let started = vue.root.join("started.txt");
    let finished = vue.root.join("finished.txt");
    let release = vue.root.join("release.txt");

    for cycle in 0..3 {
        let mut scope = Project {
            id: 10 + cycle as u64,
            ..vue.clone()
        };
        Arc::make_mut(scope.document.as_mut().unwrap()).opening = scope.id;
        let vue_key = key(&namespace, &scope);
        editor.projects(Some(scope.id), vec![rust.clone(), scope.clone()]);
        if cycle == 0 {
            editor
                .send(Action::SetProjectExecution(scope.id, true))
                .unwrap();
        }
        let open = wait(&editor, |view| {
            view.project_execution.get(&scope.id) == Some(&true)
                && view.modules.get(&vue_key) == Some(&State::Ready)
                && view
                    .runtime_status
                    .get(&namespace)
                    .is_some_and(|status| status.ready_modules == 2)
        });
        assert_eq!(open.workers[&namespace], runtime);
        invoke(&editor, &vue_key);
        let launches = "once".repeat(cycle + 1);
        wait(&editor, |_| {
            fs::read_to_string(&started).is_ok_and(|text| text == launches)
        });
        let native_pid = fs::read_to_string(vue.root.join("tool.pid"))
            .unwrap()
            .parse()
            .unwrap();
        assert!(process_is_running(native_pid, "powershell.exe"));
        samples.push(acceptance::resources(
            &profile,
            &format!("vue-open-{cycle}"),
            Some(runtime),
            Some(native_pid),
            2,
        ));

        if cycle == 2 {
            // Revoke execution after the tool has acted but before it can publish an exit.
            editor
                .send(Action::SetProjectExecution(scope.id, false))
                .unwrap();
        }
        let drained = editor.drain_for_close(Some(scope.id)).unwrap();
        if cycle < 2 {
            fs::write(&release, "").unwrap();
        }
        wait(&editor, |_| !drained.is_empty());
        drained.try_recv().unwrap().unwrap();
        assert!(!process_is_running(native_pid, "powershell.exe"));
        if cycle < 2 {
            fs::remove_file(&release).unwrap();
        }

        editor.projects(Some(rust.id), vec![rust.clone()]);
        let closed = wait(&editor, |view| {
            !view.modules.contains_key(&vue_key)
                && view
                    .runtime_status
                    .get(&namespace)
                    .is_some_and(|status| status.ready_modules == 1)
        });
        assert_eq!(closed.modules.get(&rust_key), Some(&State::Ready));
        assert_eq!(closed.workers[&namespace], runtime);
        samples.push(acceptance::resources(
            &profile,
            &format!("vue-closed-{cycle}"),
            Some(runtime),
            None,
            1,
        ));
    }
    assert_eq!(fs::read_to_string(&finished).unwrap(), "donedone");
    editor.projects(None, Vec::new());
    wait(&editor, |view| view.workers.is_empty());
    assert!(!process_is_running(runtime, "nanobug-plugin-runtime.exe"));
    samples.push(acceptance::resources(&profile, "all-closed", None, None, 0));
    eprintln!(
        "P7-03 Windows process usage and compiled cache bytes: {}",
        json!(samples)
    );
    drop(editor);

    let reopened = session(&profile);
    let vue_key = key(&namespace, &vue);
    reopened.projects(Some(vue.id), vec![rust, vue.clone()]);
    wait(&reopened, |view| {
        view.modules.get(&vue_key) == Some(&State::Ready)
            && view.project_execution.get(&vue.id) == Some(&false)
    });
    reopened
        .send(Action::SetProjectExecution(vue.id, true))
        .unwrap();
    wait(&reopened, |view| {
        view.project_execution.get(&vue.id) == Some(&true)
    });
    reopened
        .send(Action::ReviewToolOperations(namespace.clone()))
        .unwrap();
    let review = wait(&reopened, |view| view.tool_operations.is_some());
    let (owner, operations) = review.tool_operations.as_ref().unwrap();
    assert_eq!(owner, &namespace);
    assert_eq!(operations.len(), 3);
    assert_eq!(
        operations
            .iter()
            .filter(|record| record.state == ToolOperationState::Exited && record.code == Some(0))
            .count(),
        2
    );
    assert_eq!(
        operations
            .iter()
            .filter(|record| record.state == ToolOperationState::Pending && record.code.is_none())
            .count(),
        1
    );
    assert_eq!(fs::read_to_string(started).unwrap(), "onceonceonce");
    assert_eq!(fs::read_to_string(finished).unwrap(), "donedone");
}

#[test]
fn a_native_service_requests_cache_release_then_pauses_on_persistent_memory_pressure() {
    let directory = tempfile::tempdir().unwrap();
    let project = project(directory.path(), 1);
    let bundle = directory.path().join("fixture.zip");
    let args = powershell(
        r#"
        $ErrorActionPreference = 'Stop'
        $buffer = [byte[]]::new(400 * 1024 * 1024)
        while ($null -ne ($line = [Console]::ReadLine())) {
            $sync = ConvertFrom-Json -InputObject $line
            if ($sync.discard_rebuildable_cache) {
                [IO.File]::WriteAllText((Join-Path (Get-Location) 'reclaim.txt'), 'asked')
            }
            [Console]::WriteLine((ConvertTo-Json -Compress -InputObject @{ revision = $sync.revision }))
            [GC]::KeepAlive($buffer)
        }
        "#,
    );
    tool_bundle(
        &bundle,
        json!({"service":"start_background_tool","tool":"test-tool"}),
        Some(&args),
    );
    let editor = session(&directory.path().join("profile"));
    let namespace = install(&editor, bundle);
    let key = key(&namespace, &project);
    editor.projects(Some(project.id), vec![project.clone()]);
    editor
        .send(Action::SetProjectExecution(project.id, true))
        .unwrap();
    let initial = wait(&editor, |view| {
        view.modules.get(&key) == Some(&State::Ready)
            && view.project_execution.get(&project.id) == Some(&true)
    });
    let pid = initial.workers[&namespace];
    invoke(&editor, &key);
    let paused = wait(&editor, |view| {
        view.background_tools
            .get(&key)
            .is_some_and(|tools| tools["test-tool"].phase == BackgroundToolPhase::Paused)
    });
    let status = paused.background_tools[&key]["test-tool"];
    assert!(status.memory_paused);
    assert!(
        status
            .private_bytes
            .is_some_and(|bytes| bytes > 384 * 1024 * 1024)
    );
    assert_eq!(status.pid, None);
    assert_eq!(paused.modules.get(&key), Some(&State::Ready));
    assert_eq!(paused.workers[&namespace], pid);
    assert_eq!(
        fs::read_to_string(project.root.join("reclaim.txt")).unwrap(),
        "asked"
    );
}

#[test]
fn a_program_that_never_acknowledges_the_background_protocol_is_not_replayed() {
    let directory = tempfile::tempdir().unwrap();
    let project = project(directory.path(), 1);
    let bundle = directory.path().join("fixture.zip");
    let args = powershell(
        r#"
        $ErrorActionPreference = 'Stop'
        [IO.File]::WriteAllText((Join-Path (Get-Location) 'hung.pid'), [string]$PID)
        [IO.File]::AppendAllText((Join-Path (Get-Location) 'launches.txt'), 'once')
        Start-Sleep -Seconds 20
    "#,
    );
    tool_bundle(
        &bundle,
        json!({"service":"start_background_tool","tool":"test-tool"}),
        Some(&args),
    );
    let editor = session(&directory.path().join("profile"));
    let namespace = install(&editor, bundle);
    let key = key(&namespace, &project);
    editor.projects(Some(project.id), vec![project.clone()]);
    editor
        .send(Action::SetProjectExecution(project.id, true))
        .unwrap();
    wait(&editor, |view| {
        view.modules.get(&key) == Some(&State::Ready)
            && view.project_execution.get(&project.id) == Some(&true)
    });
    let start = Instant::now();
    invoke(&editor, &key);
    let paused = wait(&editor, |view| {
        view.background_tools
            .get(&key)
            .is_some_and(|tools| tools["test-tool"].phase == BackgroundToolPhase::Paused)
    });
    let elapsed = start.elapsed();
    assert!(
        paused.logs[&namespace]
            .iter()
            .any(|line| line.contains("synchronization timed out"))
    );
    let hung_pid = fs::read_to_string(project.root.join("hung.pid"))
        .unwrap()
        .parse()
        .unwrap();
    assert!(!process_is_running(hung_pid, "powershell.exe"));
    eprintln!("P7-04 unresponsive background child paused after {elapsed:?}");
    std::thread::sleep(Duration::from_millis(1150));
    assert_eq!(
        fs::read_to_string(project.root.join("launches.txt")).unwrap(),
        "once"
    );
    assert_eq!(
        editor.view().background_tools[&key]["test-tool"].phase,
        BackgroundToolPhase::Paused
    );
}
