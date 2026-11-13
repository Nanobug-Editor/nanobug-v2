//! Native command and background recovery coverage using only test-owned files on Unix desktops.

#![cfg(any(target_os = "linux", target_os = "macos"))]

use nanobug::plugins::session::{
    Action, BackgroundToolPhase, Key, Options, Project, Session, State, ToolOperationState, View,
};
use nanobug_plugin_sdk::{DocumentSnapshot, Instance, PluginEvent};
use serde_json::{Value, json};
use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use zip::{ZipWriter, write::SimpleFileOptions};

fn wait(editor: &Session, condition: impl Fn(&View) -> bool) -> Arc<View> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let view = editor.view();
        if condition(&view) {
            return view;
        }
        assert!(
            Instant::now() < deadline,
            "Native tool did not settle. {} {:?}",
            view.message,
            view.logs
        );
        std::thread::sleep(Duration::from_millis(15));
    }
}

fn open(profile: &Path) -> Session {
    Session::start(
        Options::take(
            &mut vec!["--plugin-profile".into(), profile.as_os_str().into()],
            profile.parent().unwrap(),
        )
        .unwrap(),
    )
    .0
}

fn project(parent: &Path, id: u64) -> Project {
    let root = parent.join(format!("project-{id}"));
    fs::create_dir(&root).unwrap();
    let file = root.join("input.txt");
    fs::write(&file, "saved fixture\n").unwrap();
    Project {
        id,
        root,
        document: Some(Arc::new(DocumentSnapshot {
            path: Some(file),
            opening: id,
            revision: 1,
            text: "first unsaved fixture".into(),
        })),
        open_documents: Vec::new(),
    }
}

fn install(editor: &Session, path: PathBuf, tools: &str, operation: Value) -> String {
    let manifest = format!(
        "name='Native Tool Fixture'\nid='native-tool-fixture'\nversion='1.0.0'\n\
         permissions=['read_document','run_tool']\n[engines]\nnanobug='^0.1.0'\n\
         [[modules]]\nid='main'\nwasm='main.wasm'\n{tools}"
    );
    let output = json!({"services":[{"token":1,"operation":operation}]}).to_string();
    let bytes = output
        .bytes()
        .map(|byte| format!("\\{byte:02x}"))
        .collect::<String>();
    let reply = (16_u64 << 32) | output.len() as u64;
    let wasm = wat::parse_str(format!(
        r#"(module
        (memory (export "memory") 20)
        (data (i32.const 16) "{bytes}")
        (func (export "nb_alloc") (param i32) (result i32) i32.const 4096)
        (func (export "nb_run") (param i32 i32) (result i64) i64.const {reply})
        (func (export "nb_free") (param i32 i32)))"#
    ))
    .unwrap();
    let mut zip = ZipWriter::new(File::create(&path).unwrap());
    for (name, bytes) in [
        ("nanobug-plugin.toml", manifest.as_bytes()),
        ("main.wasm", &wasm),
    ] {
        zip.start_file(name, SimpleFileOptions::default()).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap();
    editor.send(Action::InspectZip(path)).unwrap();
    let review = wait(editor, |view| view.review.is_some());
    let review = review.review.as_ref().unwrap();
    let owner = review.manifest.identity().namespace();
    editor.send(Action::Approve(review.token)).unwrap();
    wait(editor, |view| view.installed.len() == 1);
    owner
}

fn key(owner: &str, project: u64) -> Key {
    Key {
        namespace: owner.into(),
        instance: Instance {
            project,
            module: "main".into(),
        },
    }
}

fn invoke(editor: &Session, key: &Key) {
    editor
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

#[test]
fn existing_native_tool_requires_permission_and_keeps_a_durable_result() {
    assert!(Path::new("/usr/bin/grep").is_file(), "requires system grep");
    let fixture = tempfile::tempdir().unwrap();
    let profile = fixture.path().join("profile");
    let editor = open(&profile);
    let project = project(fixture.path(), 1);
    let owner = install(
        &editor,
        fixture.path().join("grep.zip"),
        "[settings.path]\ntitle='Tool path'\ndefault='/usr/bin/grep'\n[tools.fixture]\npath_setting='path'\n",
        json!({"service":"run_tool","tool":"fixture","args":["-F","saved fixture","input.txt"]}),
    );
    let key = key(&owner, 1);
    editor.projects(Some(1), vec![project.clone()]);
    wait(&editor, |view| {
        view.modules.get(&key) == Some(&State::Ready)
    });
    invoke(&editor, &key);
    wait(&editor, |view| {
        view.logs[&owner]
            .iter()
            .any(|line| line.contains("Project execution is blocked"))
    });
    assert!(!profile.join("operations").exists());

    editor.send(Action::SetProjectExecution(1, true)).unwrap();
    wait(&editor, |view| {
        view.project_execution.get(&1) == Some(&true)
    });
    invoke(&editor, &key);
    let exited = |view: &View| {
        view.tool_operations
            .as_ref()
            .is_some_and(|(namespace, operations)| {
                namespace == &owner
                    && operations.len() == 1
                    && operations[0].state == ToolOperationState::Exited
                    && operations[0].code == Some(0)
            })
    };
    let deadline = Instant::now() + Duration::from_secs(8);
    while !exited(&editor.view()) {
        assert!(Instant::now() < deadline, "grep result did not settle");
        editor
            .send(Action::ReviewToolOperations(owner.clone()))
            .unwrap();
        std::thread::sleep(Duration::from_millis(80));
    }
    let receipt = editor.view().tool_operations.as_ref().unwrap().1[0]
        .id
        .clone();
    drop(editor);
    let editor = open(&profile);
    wait(&editor, |view| view.installed.len() == 1);
    editor
        .send(Action::ReviewToolOperations(owner.clone()))
        .unwrap();
    let reopened = wait(&editor, exited);
    assert_eq!(reopened.tool_operations.as_ref().unwrap().1[0].id, receipt);
    assert_eq!(
        fs::read_to_string(project.root.join("input.txt")).unwrap(),
        "saved fixture\n"
    );
}

#[test]
fn native_background_service_restarts_one_scope_and_releases_its_process() {
    let python = std::env::var("NANOBUG_TEST_PYTHON").unwrap_or_else(|_| "/usr/bin/python3".into());
    assert!(
        Path::new(&python).is_absolute() && Path::new(&python).is_file(),
        "requires an absolute Python 3 path"
    );
    let fixture = tempfile::tempdir().unwrap();
    let profile = fixture.path().join("profile");
    let script = fixture.path().join("background.py");
    fs::write(
        &script,
        r#"
import json
import sys
from pathlib import Path

for line in sys.stdin:
    snapshot = json.loads(line)
    if snapshot["protocol"] != 1:
        sys.exit(9)
    text = snapshot["documents"][0]["text"]
    if text == "fixture self-exit":
        sys.exit(8)
    Path("seen.txt").write_text(text, encoding="utf-8")
    print(json.dumps({"revision": snapshot["revision"]}), flush=True)
"#,
    )
    .unwrap();
    let editor = open(&profile);
    let mut first = project(fixture.path(), 1);
    let second = project(fixture.path(), 2);
    let tools = format!(
        "[settings.path]\ntitle='Tool path'\ndefault={}\n\
         [tools.fixture]\npath_setting='path'\nbackground={{ args = ['-I', {}] }}\n",
        serde_json::to_string(&python).unwrap(),
        serde_json::to_string(&script).unwrap(),
    );
    let owner = install(
        &editor,
        fixture.path().join("background.zip"),
        &tools,
        json!({"service":"start_background_tool","tool":"fixture"}),
    );
    let a = key(&owner, 1);
    let b = key(&owner, 2);
    editor.projects(Some(1), vec![first.clone(), second.clone()]);
    wait(&editor, |view| {
        [&a, &b]
            .into_iter()
            .all(|key| view.modules.get(key) == Some(&State::Ready))
    });
    for project in [1, 2] {
        editor
            .send(Action::SetProjectExecution(project, true))
            .unwrap();
    }
    wait(&editor, |view| {
        [1, 2]
            .into_iter()
            .all(|project| view.project_execution.get(&project) == Some(&true))
    });
    invoke(&editor, &a);
    invoke(&editor, &b);
    let status = |view: &View, key: &Key| {
        view.background_tools
            .get(key)
            .and_then(|tools| tools.get("fixture"))
            .copied()
    };
    let ready_pid = |view: &View, key: &Key| {
        status(view, key)
            .filter(|status| status.phase == BackgroundToolPhase::Ready)
            .and_then(|status| status.pid)
    };
    let initial = wait(&editor, |view| {
        ready_pid(view, &a).is_some() && ready_pid(view, &b).is_some()
    });
    let old = ready_pid(&initial, &a).unwrap();
    let other = ready_pid(&initial, &b).unwrap();
    let worker = initial.workers[&owner];
    let update = |project: &mut Project, text: &str| {
        let snapshot = Arc::make_mut(project.document.as_mut().unwrap());
        snapshot.revision += 1;
        snapshot.text = text.into();
        editor.projects(Some(2), vec![project.clone(), second.clone()]);
    };
    update(&mut first, "fixture self-exit");
    let recovering = wait(&editor, |view| {
        status(view, &a).is_some_and(|status| status.phase == BackgroundToolPhase::Recovering)
    });
    assert_eq!(ready_pid(&recovering, &b), Some(other));
    update(&mut first, "latest unsaved fixture");
    let recovered = wait(&editor, |view| {
        ready_pid(view, &a).is_some_and(|pid| pid != old)
    });
    let new = ready_pid(&recovered, &a).unwrap();
    assert_eq!(recovered.workers[&owner], worker);
    assert_eq!(ready_pid(&recovered, &b), Some(other));
    assert_eq!(
        fs::read_to_string(first.root.join("seen.txt")).unwrap(),
        "latest unsaved fixture"
    );
    assert_eq!(
        fs::read_to_string(first.root.join("input.txt")).unwrap(),
        "saved fixture\n"
    );
    editor.projects(Some(2), vec![second]);
    let closed = wait(&editor, |view| !view.background_tools.contains_key(&a));
    assert_eq!(ready_pid(&closed, &b), Some(other));
    for pid in [old, new] {
        assert!(process_gone(pid), "test process was retained");
    }
}

/// Verify that a test-owned process was reaped on the host platform.
#[cfg(target_os = "linux")]
fn process_gone(pid: u32) -> bool {
    !Path::new(&format!("/proc/{pid}")).exists()
}

#[cfg(target_os = "macos")]
fn process_gone(pid: u32) -> bool {
    // Signal zero checks the PID without delivering a signal.
    let present = unsafe { libc::kill(pid as libc::c_int, 0) } == 0;
    !present && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}
