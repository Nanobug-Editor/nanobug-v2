//! Exercises the installed WASM and JavaScript workers through the editor session.

#[cfg(windows)]
#[path = "session/acceptance.rs"]
mod acceptance;
#[path = "session/canvas_cases.rs"]
mod canvas_cases;
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
#[path = "session/resource_lifecycle.rs"]
mod resource_lifecycle;
#[path = "session/rule_paths.rs"]
mod rule_paths;

use nanobug::plugins::{
    package::{InstallStore, UI_CONTRACT},
    session::{Action, ConfigScope, Key, Options, Project, Rule, Session, State, View},
};
use nanobug_plugin_sdk::{DocumentSnapshot, Instance, Permission, PluginEvent};
use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use zip::{ZipWriter, write::SimpleFileOptions};

fn bundle(path: &Path, engine: &str, rule: &str, interactive: bool, version: &str, broken: bool) {
    bundle_with_permissions(
        path,
        engine,
        rule,
        interactive,
        version,
        broken,
        &[Permission::ReadProject],
    );
}

fn bundle_with_permissions(
    path: &Path,
    engine: &str,
    rule: &str,
    interactive: bool,
    version: &str,
    broken: bool,
    permissions: &[Permission],
) {
    let extra = if interactive {
        format!("\n[experiments]\nui = {UI_CONTRACT:?}\n")
    } else {
        String::new()
    };
    let permissions = serde_json::to_string(permissions).unwrap();
    let manifest = format!(
        "name = 'Workspace Fixture'\nid = 'workspace-test'\nversion = {version:?}\n\
         permissions = {permissions}\n[engines]\nnanobug = {engine:?}\n{extra}\n\
         [[modules]]\nid = 'workspace'\nwasm = 'main.wasm'\nrule = 'detect.js'\ninteractive = {interactive}\n"
    );
    let result = r#"{"message":"guest ran","surface":{"title":"Guest UI","lines":["Loaded"],"input":"default","canvas":{"draw":[{"op":"path","segments":[{"segment":"move","to":[4,4]},{"segment":"line","to":[80,20]},{"segment":"line","to":[20,40]},{"segment":"close"}],"fill":{"fill":"solid","color":1907107583}}]}}}"#;
    bundle_guest(path, &manifest, rule, result, broken);
}

/// Package a deterministic real WASM guest for startup and rule acceptance cases.
fn bundle_guest(path: &Path, manifest: &str, rule: &str, result: &str, broken: bool) {
    let contents = result
        .bytes()
        .map(|byte| format!("\\{byte:02x}"))
        .collect::<String>();
    let pointer = (16_u64 << 32) | result.len() as u64;
    let body = if broken {
        "unreachable".into()
    } else {
        format!("i64.const {pointer}")
    };
    let wasm = wat::parse_str(format!(
        r#"
        (module
            (memory (export "memory") 20)
            (data (i32.const 16) "{contents}")
            (func (export "nb_alloc") (param i32) (result i32) i32.const 1024)
            (func (export "nb_run") (param i32 i32) (result i64) {body})
            (func (export "nb_free") (param i32 i32)))
    "#
    ))
    .unwrap();
    let mut zip = ZipWriter::new(File::create(path).unwrap());
    for (name, bytes) in [
        ("nanobug-plugin.toml", manifest.as_bytes()),
        ("main.wasm", &wasm),
        ("detect.js", rule.as_bytes()),
    ] {
        zip.start_file(name, SimpleFileOptions::default()).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap();
}

fn session(profile: &Path, development: bool) -> Session {
    let flag = if development {
        "--plugin-dev-profile"
    } else {
        "--plugin-profile"
    };
    let opts = Options::take(
        &mut vec![flag.into(), profile.into()],
        profile.parent().unwrap(),
    )
    .unwrap();
    Session::start(opts).0
}

fn wait(session: &Session, predicate: impl Fn(&View) -> bool) -> Arc<View> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let view = session.view();
        if predicate(&view) {
            return view;
        }
        assert!(
            Instant::now() < deadline,
            "message={:?}; modules={:?}; logs={:?}",
            view.message,
            view.modules,
            view.logs
        );
        std::thread::sleep(Duration::from_millis(30));
    }
}

/// Only a local picker selection updates the location, including across profile restarts.
#[test]
fn local_zip_location_is_saved_per_profile_on_selection() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let profile = root.join("profile");
    let directory = root.join("本地 ZIP");
    fs::create_dir(&directory).unwrap();
    let archive = directory.join("package.ZIP");
    bundle(&archive, ">=0.1.0", "true", false, "1.0.0", false);

    let editor = session(&profile, false);
    editor.send(Action::InspectChosenZip(archive)).unwrap();
    let review = wait(&editor, |view| view.review.is_some());
    assert_eq!(
        review.local_zip_directory.as_deref(),
        Some(directory.as_path())
    );
    editor.send(Action::CancelInstall).unwrap();
    wait(&editor, |view| view.review.is_none());
    drop(editor);

    let editor = session(&profile, false);
    let reopened = wait(&editor, |view| view.operation_root.is_some());
    assert_eq!(
        reopened.local_zip_directory.as_deref(),
        Some(directory.as_path())
    );
    let other_archive = root.join("other.ZIP");
    bundle(&other_archive, ">=0.1.0", "true", false, "1.0.1", false);
    editor.send(Action::InspectZip(other_archive)).unwrap();
    let review = wait(&editor, |view| view.review.is_some());
    assert_eq!(
        review.local_zip_directory.as_deref(),
        Some(directory.as_path())
    );
    editor.send(Action::CancelInstall).unwrap();
    wait(&editor, |view| view.review.is_none());
    let invalid_archive = root.join("invalid.ZIP");
    fs::write(&invalid_archive, "not a ZIP").unwrap();
    editor
        .send(Action::InspectChosenZip(invalid_archive))
        .unwrap();
    wait(&editor, |view| {
        view.local_zip_directory.as_deref() == Some(root.as_path()) && !view.installing
    });
    let reopened = session(&profile, false);
    assert_eq!(
        wait(&reopened, |view| view.operation_root.is_some())
            .local_zip_directory
            .as_deref(),
        Some(root.as_path())
    );

    let isolated = session(&root.join("isolated"), true);
    assert!(
        wait(&isolated, |view| view.operation_root.is_some())
            .local_zip_directory
            .is_none()
    );
}

fn project(root: &Path, id: u64) -> Project {
    let root = root.join(format!("project-{id}"));
    fs::create_dir_all(&root).unwrap();
    Project {
        id,
        root,
        document: None,
        open_documents: Vec::new(),
    }
}

/// Approve a package without assuming a rejected candidate becomes observable.
fn approve_zip(session: &Session, path: PathBuf, version: &str) -> String {
    session.send(Action::InspectZip(path)).unwrap();
    let review = wait(session, |view| view.review.is_some())
        .review
        .clone()
        .unwrap();
    assert_eq!(review.manifest.version.to_string(), version);
    session.send(Action::Approve(review.token)).unwrap();
    review.manifest.identity().namespace()
}

fn install(session: &Session, path: PathBuf, version: &str) -> String {
    let namespace = approve_zip(session, path, version);
    wait(session, |view| {
        view.installed.iter().any(|record| {
            record.release.manifest.identity().namespace() == namespace
                && record.release.manifest.version.to_string() == version
        })
    });
    namespace
}

fn key(namespace: &str, project: u64) -> Key {
    Key {
        namespace: namespace.into(),
        instance: Instance {
            project,
            module: "workspace".into(),
        },
    }
}

/// Return the last automatic event verbatim when queried with Diagnose.
fn document_echo_guest() -> Vec<u8> {
    let prefix = r#"{"service_value":"#;
    let escaped = prefix
        .bytes()
        .map(|byte| format!("\\{byte:02x}"))
        .collect::<String>();
    let response = 32_768_u64;
    let pointer = response << 32;
    let payload = response as usize + prefix.len();
    let overhead = prefix.len() + 1;
    let event_offset = r#"{"event":{"event":""#.len();
    let diagnose = i32::from_le_bytes(*b"diag");
    wat::parse_str(format!(
        r#"
        (module
            (memory (export "memory") 2)
            (data (i32.const {response}) "{escaped}")
            (global $length (mut i32) (i32.const 0))
            (func (export "nb_alloc") (param i32) (result i32) i32.const 1024)
            (func (export "nb_run") (param $input i32) (param $size i32) (result i64)
                local.get $input
                i32.load offset={event_offset}
                i32.const {diagnose}
                i32.ne
                if
                    i32.const {payload}
                    local.get $input
                    local.get $size
                    memory.copy
                    i32.const {payload}
                    local.get $size
                    i32.add
                    i32.const 125
                    i32.store8
                    local.get $size
                    i32.const {overhead}
                    i32.add
                    global.set $length
                end
                i64.const {pointer}
                global.get $length
                i64.extend_i32_u
                i64.or)
            (func (export "nb_free") (param i32 i32)))
    "#
    ))
    .unwrap()
}

#[test]
fn document_refresh_rebuilds_unicode_and_resends_full_after_a_base_is_evicted() {
    use nanobug::document::Document;
    use nanobug_plugin_sdk::document_wire::DocumentDelta;

    let temp = tempfile::tempdir().unwrap();
    let archive = temp.path().join("documents.zip");
    let manifest = format!(
        r#"
name = 'Document Echo'
id = 'document-echo'
version = '1.0.0'
permissions = ['read_document']
[engines]
nanobug = '^0.1.0'
[experiments]
ui = '{UI_CONTRACT}'
[[modules]]
id = 'workspace'
wasm = 'main.wasm'
interactive = true
"#
    );
    let mut zip = ZipWriter::new(File::create(&archive).unwrap());
    for (name, bytes) in [
        ("nanobug-plugin.toml", manifest.as_bytes()),
        ("main.wasm", &document_echo_guest()),
    ] {
        zip.start_file(name, SimpleFileOptions::default()).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap();

    let session = session(&temp.path().join("profile"), true);
    let namespace = install(&session, archive, "1.0.0");
    let owner = key(&namespace, 1);
    session
        .send(Action::SampleRuntime(namespace.clone(), true))
        .unwrap();
    let first = project(temp.path(), 1);
    let path = first.root.join("source.txt");
    let original = format!(
        "{}\r\n\t中文👩‍💻e\u{301}\r\n{}",
        "a".repeat(230),
        "b".repeat(230)
    );
    fs::write(&path, original).unwrap();
    let mut document = Document::open(&path).unwrap();
    let snapshot = |doc: &Document| {
        Arc::new(DocumentSnapshot {
            path: doc.path.clone(),
            opening: doc.opening,
            revision: doc.revision,
            text: doc.text.clone(),
        })
    };
    let original = snapshot(&document);
    let mut projects = vec![Project {
        document: Some(Arc::clone(&original)),
        ..first
    }];
    session.projects(Some(1), projects.clone());
    wait(&session, |view| {
        view.modules.get(&owner) == Some(&State::Ready)
    });

    let publish = |projects: &[Project]| {
        let completed = session.view().runtime_status[&namespace].completed;
        session.projects(Some(1), projects.to_vec());
        wait(&session, |view| {
            view.runtime_status
                .get(&namespace)
                .is_some_and(|status| status.completed > completed && status.pending == 0)
        });
    };
    let verify = |snapshot: &DocumentSnapshot| {
        let completed = session.view().runtime_status[&namespace].completed;
        let (reply, answer) = async_channel::bounded(1);
        session
            .send(Action::Invoke {
                key: owner.clone(),
                event: PluginEvent::Diagnose,
                reply: Some(reply),
                timeout: Duration::from_secs(3),
            })
            .unwrap();
        let input = answer
            .recv_blocking()
            .unwrap()
            .unwrap()
            .output
            .service_value
            .unwrap();
        assert_eq!(input["event"]["event"], "document_changed");
        let guest: DocumentSnapshot = serde_json::from_value(input["document"].clone()).unwrap();
        assert_eq!(&guest, snapshot);
        wait(&session, |view| {
            view.runtime_status[&namespace].completed > completed
        });
    };

    let start = document.text.find("中文").unwrap();
    document.replace(start..start + "中文".len(), "文字", true);
    let start = document.text.find('💻').unwrap();
    document.replace(start..start + '💻'.len_utf8(), "🚀", true);
    let second = snapshot(&document);
    let delta = DocumentDelta::between(&original, &second).unwrap();
    assert_eq!((delta.base_revision, delta.revision), (0, 2));
    assert_eq!(delta.apply(&original).unwrap(), *second);
    projects[0].document = Some(Arc::clone(&second));
    publish(&projects);
    verify(&second);
    assert!(second.text.contains("\n\t文字👩‍🚀e\u{301}\n"));

    // Additional projects fill the same worker lane's bounded base cache.
    for id in 2..=9 {
        let mut extra = project(temp.path(), id);
        extra.document = Some(snapshot(&Document::new("other project")));
        projects.push(extra);
    }
    session.projects(Some(1), projects.clone());
    wait(&session, |view| {
        view.runtime_status
            .get(&namespace)
            .is_some_and(|status| status.ready_modules == projects.len())
    });

    let start = document.text.find("e\u{301}").unwrap();
    document.replace(start..start + "e\u{301}".len(), "o\u{302}", true);
    let third = snapshot(&document);
    projects[0].document = Some(Arc::clone(&third));
    publish(&projects);
    let resyncs = |view: &View| {
        view.logs[&namespace]
            .iter()
            .filter(|line| line.starts_with("Document base missing"))
            .count()
    };
    assert_eq!(resyncs(&session.view()), 1);
    verify(&third);

    document.save(None).unwrap();
    assert!(
        fs::read_to_string(&path)
            .unwrap()
            .contains("\r\n\t文字👩‍🚀o\u{302}\r\n")
    );
    let reopened = snapshot(&Document::open(&path).unwrap());
    assert_eq!(reopened.path, third.path);
    assert_ne!(reopened.opening, third.opening);
    assert!(DocumentDelta::between(&third, &reopened).is_none());
    projects[0].document = Some(Arc::clone(&reopened));
    publish(&projects);
    verify(&reopened);
    assert_eq!(resyncs(&session.view()), 1);
}

#[test]
fn settings_apply_drains_affected_project_writes_and_notifies_background_modules() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("settings.zip");
    let manifest = br#"
name = 'Settings Fixture'
id = 'settings-test'
version = '1.0.0'
permissions = ['change_files']
[engines]
nanobug = '^0.1.0'
[settings.flag]
title = 'Flag'
default = false
project = true
[[modules]]
id = 'workspace'
wasm = 'main.wasm'
"#;
    let open = r#"{"message":"no settings event"}"#;
    let changed = r#"{"message":"settings observed"}"#;
    let escape = |value: &str| {
        value
            .bytes()
            .map(|byte| format!("\\{byte:02x}"))
            .collect::<String>()
    };
    let event_offset = r#"{"event":{"event":""#.len();
    let event = i64::from_le_bytes(*b"settings");
    let open_result = (16_u64 << 32) | open.len() as u64;
    let changed_result = (128_u64 << 32) | changed.len() as u64;
    let wasm = wat::parse_str(format!(
        r#"
        (module
            (memory (export "memory") 20)
            (data (i32.const 16) "{}")
            (data (i32.const 128) "{}")
            (global $changed (mut i32) (i32.const 0))
            (func (export "nb_alloc") (param i32) (result i32) i32.const 1024)
            (func (export "nb_run") (param $input i32) (param i32) (result i64)
                local.get $input
                i64.load offset={event_offset}
                i64.const {event}
                i64.eq
                if
                    i32.const 1
                    global.set $changed
                end
                global.get $changed
                if (result i64)
                    i64.const {changed_result}
                else
                    i64.const {open_result}
                end)
            (func (export "nb_free") (param i32 i32)))
    "#,
        escape(open),
        escape(changed)
    ))
    .unwrap();
    let mut zip = ZipWriter::new(File::create(&path).unwrap());
    for (name, bytes) in [
        ("nanobug-plugin.toml", manifest.as_slice()),
        ("main.wasm", &wasm),
    ] {
        zip.start_file(name, SimpleFileOptions::default()).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap();

    let session = session(&temp.path().join("profile"), false);
    let namespace = install(&session, path, "1.0.0");
    let first = key(&namespace, 1);
    let second = key(&namespace, 2);
    session.projects(
        Some(1),
        vec![project(temp.path(), 1), project(temp.path(), 2)],
    );
    let view = wait(&session, |view| {
        view.modules.get(&first) == Some(&State::Ready)
            && view.modules.get(&second) == Some(&State::Ready)
            && view
                .runtime_status
                .get(&namespace)
                .is_some_and(|status| status.ready_modules == 2)
    });
    let installed = &view.installed[0].release;
    let (lease, ended) = async_channel::bounded(1);
    let (reply, answer) = async_channel::bounded(1);
    session
        .send(Action::BeginFileWrite {
            owner: first.clone(),
            digest: installed.digest.clone(),
            generation: view.runtime_status[&namespace].generation,
            ended,
            reply,
        })
        .unwrap();
    answer.recv_blocking().unwrap().unwrap();

    let stage = |scope| {
        let mut config = nanobug::plugins::session::Config::default();
        config
            .plugin_mut(installed.manifest.identity())
            .settings
            .insert("flag".into(), true.into());
        session.send(Action::StageConfig(scope, config)).unwrap();
        session.send(Action::ApplyConfig(scope)).unwrap();
    };
    let applied = |view: &View, scope| {
        view.configurations[&scope]
            .applied
            .plugin(&namespace)
            .is_some()
    };
    let guest = |key: Key| {
        // Apply publishes the saved configuration before the guest finishes adopting it.
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let (reply, answer) = async_channel::bounded(1);
            session
                .send(Action::Invoke {
                    key: key.clone(),
                    event: PluginEvent::Diagnose,
                    reply: Some(reply),
                    timeout: Duration::from_secs(3),
                })
                .unwrap();
            match answer.recv_blocking().unwrap() {
                Ok(result) => break result.output.message,
                Err(error)
                    if error == "Module is adopting updated settings"
                        && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(error) => panic!("{error}"),
            }
        }
    };
    let first_scope = ConfigScope::Project(1);
    let second_scope = ConfigScope::Project(2);
    stage(first_scope);
    wait(&session, |view| {
        view.message.starts_with("Apply is waiting")
    });
    stage(second_scope);
    let view = wait(&session, |view| applied(view, second_scope));
    assert!(!applied(&view, first_scope));
    assert_eq!(guest(first.clone()), "no settings event");
    assert_eq!(guest(second), "settings observed");
    drop(lease);
    wait(&session, |view| applied(view, first_scope));
    assert_eq!(guest(first), "settings observed");
}

#[test]
fn project_reopen_keeps_a_rejected_draft_and_the_last_applied_rule() -> anyhow::Result<()> {
    use nanobug::plugins::session::Config;

    let temp = tempfile::tempdir()?;
    let profile = temp.path().join("profile");
    let zip = temp.path().join("config.zip");
    bundle(&zip, "^0.1.0", "return false;", false, "1.0.0", false);
    let first = session(&profile, false);
    let namespace = install(&first, zip, "1.0.0");
    let identity = first.view().installed[0].release.manifest.identity();
    let config = |rule| {
        let mut value = Config::default();
        value
            .plugin_mut(identity.clone())
            .rules
            .insert("workspace".into(), rule);
        value
    };
    let mut workspace = project(temp.path(), 1);
    let path = workspace.root.join(".nb/plugins.json");
    fs::create_dir_all(path.parent().unwrap())?;
    let initial = config(Rule::Always);
    fs::write(&path, serde_json::to_vec(&initial)?)?;
    first.projects(Some(1), vec![workspace.clone()]);
    let owner = key(&namespace, 1);
    let first_scope = ConfigScope::Project(1);
    wait(&first, |view| {
        view.modules.get(&owner) == Some(&State::Ready)
    });

    let invalid_draft = config(Rule::Script("return (;".into()));
    first.send(Action::StageConfig(first_scope, invalid_draft.clone()))?;
    first.send(Action::ApplyConfig(first_scope))?;
    let rejected = wait(&first, |view| {
        view.configurations[&first_scope].apply_issue.is_some()
    });
    assert_eq!(rejected.configurations[&first_scope].applied, initial);
    assert_eq!(rejected.modules.get(&owner), Some(&State::Ready));
    assert_eq!(
        serde_json::from_slice::<Config>(&fs::read(&path)?)?,
        initial
    );
    drop(first);

    let second = session(&profile, false);
    workspace.id = 7;
    let second_scope = ConfigScope::Project(7);
    let owner = key(&namespace, 7);
    second.projects(Some(7), vec![workspace.clone()]);
    let reopened = wait(&second, |view| {
        view.modules.get(&owner) == Some(&State::Ready)
    });
    let reopened = &reopened.configurations[&second_scope];
    assert_eq!(reopened.applied, initial);
    assert_eq!(reopened.draft.as_ref(), Some(&invalid_draft));
    assert!(reopened.apply_issue.is_some());

    let runtime_failure = config(Rule::Script("throw new Error('rule fixture');".into()));
    second.send(Action::StageConfig(second_scope, runtime_failure.clone()))?;
    second.send(Action::ApplyConfig(second_scope))?;
    let failed = wait(&second, |view| {
        view.configurations[&second_scope].applied == runtime_failure
            && matches!(view.modules.get(&owner), Some(State::Failed(_)))
    });
    assert!(failed.configurations[&second_scope].apply_issue.is_none());
    let saved = fs::read(&path)?;
    assert_eq!(serde_json::from_slice::<Config>(&saved)?, runtime_failure);
    let draft = config(Rule::Never);
    second.send(Action::StageConfig(second_scope, draft.clone()))?;
    wait(&second, |view| {
        view.configurations[&second_scope].draft.as_ref() == Some(&draft)
    });
    fs::write(&path, serde_json::to_vec(&initial)?)?;
    drop(second);

    let third = session(&profile, false);
    workspace.id = 9;
    let third_scope = ConfigScope::Project(9);
    let owner = key(&namespace, 9);
    third.projects(Some(9), vec![workspace]);
    let view = wait(&third, |view| {
        matches!(view.modules.get(&owner), Some(State::Failed(_)))
    });
    let config = &view.configurations[&third_scope];
    assert_eq!(config.applied, runtime_failure);
    assert_eq!(config.draft, Some(draft));
    assert_eq!(config.external, Some(initial));
    Ok(())
}

#[test]
fn file_refresh_progresses_during_a_burst_without_applying_external_config() -> anyhow::Result<()> {
    use nanobug::plugins::session::Config;

    let fixture = tempfile::tempdir()?;
    let zip = fixture.path().join("demo.zip");
    bundle(
        &zip,
        "^0.1.0",
        "return await host.exists('enabled');",
        false,
        "1.0.0",
        false,
    );
    let editor = session(&fixture.path().join("profile"), false);
    let namespace = install(&editor, zip, "1.0.0");
    let workspace = project(fixture.path(), 1);
    let root = workspace.root.clone();
    let owner = key(&namespace, 1);
    let scope = ConfigScope::Project(1);
    let flag = root.join("enabled");
    editor.projects(Some(1), vec![workspace]);
    let initial = wait(&editor, |view| {
        view.modules.get(&owner) == Some(&State::Unmatched)
    });
    let identity = initial.installed[0].release.manifest.identity();
    let mut external = Config::default();
    external
        .plugin_mut(identity)
        .rules
        .insert("workspace".into(), Rule::Always);
    let path = root.join(".nb/plugins.json");
    fs::create_dir_all(path.parent().unwrap())?;
    fs::write(&path, serde_json::to_vec(&external)?)?;
    let candidate = wait(&editor, |view| {
        view.configurations[&scope].external.is_some()
    });
    assert_eq!(candidate.modules[&owner], State::Unmatched);

    let noise = root.join("unrelated");
    fs::create_dir(&noise)?;
    std::thread::scope(|thread| -> anyhow::Result<()> {
        let (finished, stop) = std::sync::mpsc::channel();
        let (started, observed) = std::sync::mpsc::channel();
        let producer = thread.spawn(move || -> std::io::Result<u32> {
            for sequence in 1u32..=6_000 {
                let path = noise.join(format!("{}.tmp", sequence % 16));
                fs::write(&path, sequence.to_le_bytes())?;
                fs::remove_file(path)?;
                if sequence == 32 {
                    let _ = started.send(());
                }
                match stop.recv_timeout(Duration::from_millis(2)) {
                    Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        return Ok(sequence);
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                }
            }
            Ok(6_000)
        });
        observed.recv_timeout(Duration::from_secs(2))?;
        fs::write(&flag, "")?;
        let ready = wait(&editor, |view| {
            view.modules.get(&owner) == Some(&State::Ready)
        });
        let pid = ready.workers[&namespace];
        fs::remove_file(&flag)?;
        let off = wait(&editor, |view| {
            view.modules.get(&owner) == Some(&State::Unmatched)
        });
        assert_eq!(
            off.configurations[&scope].external.as_ref(),
            Some(&external)
        );
        assert_eq!(off.configurations[&scope].applied, Config::default());
        fs::write(&flag, "")?;
        let again = wait(&editor, |view| {
            view.modules.get(&owner) == Some(&State::Ready)
        });
        assert_eq!(again.workers[&namespace], pid);
        finished.send(())?;
        assert!(producer.join().unwrap()? > 32);
        Ok(())
    })?;
    Ok(())
}

#[test]
fn rules_use_document_revisions_and_saved_files_as_separate_sources() {
    let temp = tempfile::tempdir().unwrap();
    let zip = temp.path().join("demo.zip");
    let profile = temp.path().join("profile");
    bundle_with_permissions(
        &zip,
        "^0.1.0",
        r#"const doc = await host.readDocument('source.txt');
           if (doc?.text !== '未保存👋') return false;
           const file = await host.readFile('source.txt');
           return file?.text === 'saved';"#,
        false,
        "1.0.0",
        false,
        &[Permission::ReadProject, Permission::ReadDocument],
    );
    let session = session(&profile, false);
    let namespace = install(&session, zip, "1.0.0");
    let owner = key(&namespace, 1);
    let mut project = project(temp.path(), 1);
    let path = project.root.join("source.txt");
    fs::write(&path, "saved").unwrap();
    let doc = |revision, text: &str| {
        Arc::new(DocumentSnapshot {
            path: Some(path.clone()),
            opening: 12,
            revision,
            text: text.into(),
        })
    };
    project.document = Some(doc(0, "saved"));
    session.projects(Some(1), vec![project.clone()]);
    wait(&session, |view| {
        view.modules.get(&owner) == Some(&State::Unmatched)
    });
    project.document = Some(doc(1, "未保存👋"));
    session.projects(Some(1), vec![project.clone()]);
    wait(&session, |view| {
        view.modules.get(&owner) == Some(&State::Ready)
    });
    fs::remove_file(&path).unwrap();
    wait(&session, |view| {
        view.modules.get(&owner) == Some(&State::Unmatched)
    });
    fs::write(&path, "saved").unwrap();
    wait(&session, |view| {
        view.modules.get(&owner) == Some(&State::Ready)
    });
    project.document = Some(doc(2, "latest"));
    session.projects(Some(1), vec![project]);
    wait(&session, |view| {
        view.modules.get(&owner) == Some(&State::Unmatched)
    });
}

#[test]
fn rule_query_missing_denied_failed_and_false_stay_distinct() {
    let temp = tempfile::tempdir().unwrap();
    let archive = temp.path().join("outcomes.zip");
    bundle(
        &archive,
        "^0.1.0",
        r#"
            if (await host.exists('uncaught')) return (await host.readDocument()) === null;
            if (await host.exists('missing')) throw new Error('missing file exists');
            if (await host.readFile('missing') !== null) throw new Error('missing file returned data');
            const denied = await host.readDocument().then(() => false,
                error => error.message.includes('no document-read grant'));
            const failed = await host.readFile('directory').then(() => false,
                error => error.message.includes('Expected a regular file'));
            if (!denied || !failed) throw new Error('query outcomes were conflated');
            return await host.exists('activate');
        "#,
        false,
        "1.0.0",
        false,
    );
    let session = session(&temp.path().join("profile"), false);
    let namespace = install(&session, archive, "1.0.0");
    let first = project(temp.path(), 1);
    let second = project(temp.path(), 2);
    fs::create_dir(first.root.join("directory")).unwrap();
    fs::write(second.root.join("uncaught"), "").unwrap();
    let first_owner = key(&namespace, first.id);
    let second_owner = key(&namespace, second.id);
    session.projects(Some(first.id), vec![first.clone(), second]);
    wait(&session, |view| {
        view.modules.get(&first_owner) == Some(&State::Unmatched)
            && matches!(view.modules.get(&second_owner), Some(State::Failed(reason)) if reason.contains("no document-read grant"))
    });
    fs::write(first.root.join("activate"), "").unwrap();
    wait(&session, |view| {
        view.modules.get(&first_owner) == Some(&State::Ready)
    });
}

#[test]
fn a_bundled_rule_parses_unknown_text_and_reports_bad_data() {
    let temp = tempfile::tempdir().unwrap();
    let archive = temp.path().join("parser.zip");
    bundle(
        &archive,
        "^0.1.0",
        r#"
            function parse(text) {
                const pair = text.trim().split('=>');
                if (pair.length !== 2 || pair[0] !== 'language') throw new Error('Invalid fixture syntax');
                return pair[1];
            }
            const input = await host.readFile('project.custom');
            return input !== null && input.complete && parse(input.text) === '中文👋';
        "#,
        false,
        "1.0.0",
        false,
    );
    let session = session(&temp.path().join("profile"), false);
    let namespace = install(&session, archive, "1.0.0");
    let project = project(temp.path(), 1);
    let owner = key(&namespace, project.id);
    let path = project.root.join("project.custom");
    fs::write(&path, "bad input").unwrap();
    session.projects(Some(project.id), vec![project]);
    wait(
        &session,
        |view| matches!(view.modules.get(&owner), Some(State::Failed(reason)) if reason.contains("Invalid fixture syntax")),
    );
    fs::write(&path, "language=>中文👋").unwrap();
    wait(&session, |view| {
        view.modules.get(&owner) == Some(&State::Ready)
    });
}

#[test]
fn repeated_compute_limits_pause_only_the_affected_project_until_retry() {
    let temp = tempfile::tempdir().unwrap();
    let zip = temp.path().join("demo.zip");
    let profile = temp.path().join("profile");
    bundle(
        &zip,
        "^0.1.0",
        "if (await host.exists('spin')) { while (true) {} } return true;",
        false,
        "1.0.0",
        false,
    );
    let session = session(&profile, false);
    let namespace = install(&session, zip, "1.0.0");
    let first = project(temp.path(), 1);
    let second = project(temp.path(), 2);
    fs::write(first.root.join("spin"), "").unwrap();
    let first_owner = key(&namespace, 1);
    let second_owner = key(&namespace, 2);
    session.projects(Some(1), vec![first.clone(), second]);
    let paused = |view: &View| {
        matches!(
            view.modules.get(&first_owner), Some(State::Failed(reason))
                if reason.contains("three computation limits")
        ) && view.modules.get(&second_owner) == Some(&State::Ready)
    };
    wait(&session, paused);
    fs::remove_file(first.root.join("spin")).unwrap();
    std::thread::sleep(Duration::from_millis(450));
    assert!(paused(&session.view()));
    session.send(Action::Retry(namespace)).unwrap();
    wait(&session, |view| {
        view.modules.get(&first_owner) == Some(&State::Ready)
            && view.modules.get(&second_owner) == Some(&State::Ready)
    });
}

#[test]
fn install_rule_scope_and_draft_recovery() {
    let temp = tempfile::tempdir().unwrap();
    let zip = temp.path().join("demo.zip");
    let profile = temp.path().join("profile");
    bundle(
        &zip,
        "^0.1.0",
        "return await host.exists('activate');",
        true,
        "1.0.0",
        false,
    );
    let session = session(&profile, true);
    session.send(Action::InspectZip(zip.clone())).unwrap();
    let view = wait(&session, |view| view.review.is_some());
    assert!(view.installed.is_empty());
    assert_eq!(
        view.review
            .as_ref()
            .unwrap()
            .additional
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        [Permission::ReadProject]
    );
    session.send(Action::CancelInstall).unwrap();
    wait(&session, |view| view.message == "Cancelled");
    let namespace = install(&session, zip, "1.0.0");
    assert!(session.view().workers.is_empty());
    let one = project(temp.path(), 1);
    let two = project(temp.path(), 2);
    let k1 = key(&namespace, 1);
    let k2 = key(&namespace, 2);
    session.projects(Some(1), vec![one.clone(), two.clone()]);
    wait(&session, |view| {
        view.modules.get(&k1) == Some(&State::Unmatched)
            && view.modules.get(&k2) == Some(&State::Unmatched)
    });
    fs::write(one.root.join("activate"), "").unwrap();
    let view = wait(&session, |view| view.surfaces.contains_key(&k1));
    assert_eq!(view.surfaces[&k1].frame.title, "Guest UI");
    assert_eq!(view.surfaces[&k1].draft, "default");
    assert!(
        view.surfaces[&k1]
            .drawing
            .as_ref()
            .is_some_and(|drawing| drawing.paths.len() == 1)
    );
    let pid = view.workers[&namespace];
    session
        .send(Action::SetDraft(k1.clone(), "已确认👋".into()))
        .unwrap();
    wait(&session, |view| {
        view.surfaces
            .get(&k1)
            .is_some_and(|surface| surface.draft == "已确认👋")
    });
    stop_runtime(pid);
    let view = wait(&session, |view| {
        view.workers
            .get(&namespace)
            .is_some_and(|current| *current != pid)
            && view
                .surfaces
                .get(&k1)
                .is_some_and(|surface| surface.draft == "已确认👋")
    });
    let pid = view.workers[&namespace];
    let scope = ConfigScope::Project(2);
    let view = wait(&session, |view| view.configurations.contains_key(&scope));
    let mut config = view.configurations[&scope].applied.clone();
    config
        .plugin_mut(view.installed[0].release.manifest.identity())
        .rules
        .insert("workspace".into(), Rule::Always);
    session.send(Action::StageConfig(scope, config)).unwrap();
    wait(&session, |view| view.configurations[&scope].draft.is_some());
    assert_eq!(session.view().modules.get(&k2), Some(&State::Unmatched));
    session.send(Action::ApplyConfig(scope)).unwrap();
    let view = wait(&session, |view| view.surfaces.contains_key(&k2));
    assert_eq!(view.workers[&namespace], pid);
    assert!(two.root.join(".nb/plugins.json").is_file());

    session.projects(Some(2), vec![two.clone()]);
    wait(&session, |view| {
        !view.modules.contains_key(&k1) && view.surfaces.contains_key(&k2)
    });
    assert_eq!(session.view().workers[&namespace], pid);
    session.projects(None, Vec::new());
    wait(&session, |view| view.workers.is_empty());
    session.projects(Some(1), vec![Project { id: 3, ..one }]);
    let k3 = key(&namespace, 3);
    let view = wait(&session, |view| view.surfaces.contains_key(&k3));
    assert_eq!(view.surfaces[&k3].draft, "已确认👋");
}

#[test]
fn unmatched_open_project_retains_its_instance_until_it_closes() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let zip = temp.path().join("idle.zip");
    bundle(
        &zip,
        "^0.1.0",
        "return await host.exists('enabled');",
        true,
        "1.0.0",
        false,
    );
    let editor = session(&temp.path().join("profile"), true);
    let namespace = install(&editor, zip, "1.0.0");
    let rust = project(temp.path(), 1);
    let vue = project(temp.path(), 2);
    let rust_owner = key(&namespace, rust.id);
    let vue_owner = key(&namespace, vue.id);
    let marker = vue.root.join("enabled");
    fs::write(rust.root.join("enabled"), "")?;
    fs::write(&marker, "")?;
    editor.projects(Some(vue.id), vec![rust.clone(), vue.clone()]);
    let ready = wait(&editor, |view| {
        view.surfaces.contains_key(&rust_owner) && view.surfaces.contains_key(&vue_owner)
    });
    let pid = ready.workers[&namespace];
    let generation = ready.runtime_status[&namespace].generation;
    editor.draft(vue_owner.clone(), "confirmed workspace text".into());
    wait(&editor, |view| {
        view.surfaces[&vue_owner].draft == "confirmed workspace text"
    });

    fs::remove_file(&marker)?;
    editor.projects(Some(rust.id), vec![rust.clone(), vue.clone()]);
    let inactive = |view: &View| {
        view.modules.get(&vue_owner) == Some(&State::Unmatched)
            && !view.surfaces.contains_key(&vue_owner)
            && view.surfaces.contains_key(&rust_owner)
            && view.workers.get(&namespace) == Some(&pid)
            && view.runtime_status[&namespace].ready_modules == 2
    };
    wait(&editor, inactive);
    std::thread::sleep(Duration::from_millis(1200));
    assert!(inactive(&editor.view()));
    fs::write(&marker, "")?;
    let resumed = wait(&editor, |view| {
        view.surfaces
            .get(&vue_owner)
            .is_some_and(|surface| surface.draft == "confirmed workspace text")
    });
    assert_eq!(resumed.workers[&namespace], pid);
    assert_eq!(resumed.runtime_status[&namespace].generation, generation);

    editor.projects(Some(rust.id), vec![rust]);
    wait(&editor, |view| {
        !view.modules.contains_key(&vue_owner)
            && !view.surfaces.contains_key(&vue_owner)
            && view.surfaces.contains_key(&rust_owner)
            && view.workers.get(&namespace) == Some(&pid)
            && view.runtime_status[&namespace].ready_modules == 1
    });
    editor.projects(None, Vec::new());
    wait(&editor, |view| {
        view.modules.is_empty() && view.surfaces.is_empty() && view.workers.is_empty()
    });
    Ok(())
}

fn stop_runtime(pid: u32) {
    let pid = pid.to_string();
    #[cfg(windows)]
    let result = std::process::Command::new("taskkill")
        .args(["/PID", &pid, "/F"])
        .output();
    #[cfg(unix)]
    let result = std::process::Command::new("kill")
        .args(["-KILL", &pid])
        .output();
    assert!(result.unwrap().status.success());
}

#[test]
fn broken_update_restores_previous_package_before_use() {
    let temp = tempfile::tempdir().unwrap();
    let zip = temp.path().join("demo.zip");
    let profile = temp.path().join("profile");
    bundle(&zip, "^0.1.0", "return true;", true, "1.0.0", false);
    let session = session(&profile, true);
    let namespace = install(&session, zip.clone(), "1.0.0");
    let project = project(temp.path(), 1);
    let key = key(&namespace, 1);
    session.projects(Some(1), vec![project]);
    wait(&session, |view| {
        view.modules.get(&key) == Some(&State::Ready) && view.surfaces.contains_key(&key)
    });
    session.draft(key.clone(), "last confirmed draft 👋".into());
    wait(&session, |view| {
        view.surfaces[&key].draft == "last confirmed draft 👋"
    });
    bundle(&zip, "^0.1.0", "return true;", true, "1.1.0", true);
    approve_zip(&session, zip, "1.1.0");
    let restored = wait(&session, |view| {
        view.message.contains("Previous version restored")
            && view.modules.get(&key) == Some(&State::Ready)
    });
    assert_eq!(
        InstallStore::new(profile).list().unwrap()[0]
            .release
            .manifest
            .version
            .to_string(),
        "1.0.0"
    );
    wait(&session, |view| {
        view.surfaces
            .get(&key)
            .is_some_and(|surface| surface.draft == "last confirmed draft 👋")
    });
    assert_eq!(
        restored.installed[0].release.manifest.version.to_string(),
        "1.0.0"
    );
    let (reply, response) = async_channel::bounded(1);
    session
        .send(Action::Invoke {
            key,
            event: PluginEvent::Diagnose,
            reply: Some(reply),
            timeout: Duration::from_secs(3),
        })
        .unwrap();
    assert_eq!(
        response.recv_blocking().unwrap().unwrap().output.message,
        "guest ran"
    );
}

#[test]
fn interrupted_update_reopens_the_last_confirmed_version_after_candidate_failure() {
    let temp = tempfile::tempdir().unwrap();
    let zip = temp.path().join("candidate.zip");
    let profile = temp.path().join("profile");
    let store = InstallStore::new(profile.clone());
    let grants = [Permission::ReadProject].into();
    let install = |version, broken| {
        bundle(&zip, "^0.1.0", "return true;", false, version, broken);
        store
            .install(store.prepare(&zip).unwrap(), &grants)
            .unwrap()
    };
    let first = install("1.0.0", false);
    let namespace = first.release.manifest.identity().namespace();
    let workspace = project(temp.path(), 1);
    let owner = key(&namespace, workspace.id);
    {
        let old = session(&profile, false);
        old.projects(Some(workspace.id), vec![workspace.clone()]);
        wait(&old, |view| view.modules.get(&owner) == Some(&State::Ready));
    }

    // The registry changed before the editor recorded a successful runtime takeover.
    install("1.1.0", true);
    install("1.2.0", true);
    let reopened = session(&profile, false);
    reopened.projects(Some(workspace.id), vec![workspace]);
    let view = wait(&reopened, |view| {
        view.message.contains("Previous version restored")
            || matches!(view.modules.get(&owner), Some(State::Failed(_)))
    });
    assert!(view.message.contains("Previous version restored"));
    assert_eq!(
        store.list().unwrap()[0].release.digest,
        first.release.digest
    );
    wait(&reopened, |view| {
        view.modules.get(&owner) == Some(&State::Ready)
    });
}

#[test]
fn update_that_rejects_saved_settings_never_takes_over() {
    let temp = tempfile::tempdir().unwrap();
    let zip = temp.path().join("demo.zip");
    let profile = temp.path().join("profile");
    bundle(&zip, "^0.1.0", "return true;", true, "1.0.0", false);
    let session = session(&profile, true);
    let namespace = install(&session, zip.clone(), "1.0.0");
    let owner = key(&namespace, 1);
    session.projects(Some(1), vec![project(temp.path(), 1)]);
    wait(&session, |view| view.surfaces.contains_key(&owner));
    session.draft(owner.clone(), "confirmed by user".into());
    wait(&session, |view| {
        view.surfaces[&owner].draft == "confirmed by user"
    });
    let manifest = format!(
        "name = 'Workspace Fixture'\nid = 'workspace-test'\nversion = '1.1.0'\n\
         permissions = ['read_project']\n[engines]\nnanobug = '^0.1.0'\n\
         [experiments]\nui = {UI_CONTRACT:?}\n[settings.label]\ntitle = 'Label'\ndefault = 'old value'\n\
         [[modules]]\nid = 'workspace'\nwasm = 'main.wasm'\nrule = 'detect.js'\ninteractive = true\nsettings_ui = true\n"
    );
    bundle_guest(
        &zip,
        &manifest,
        "return true;",
        r#"{"settings_error":"Cannot migrate old label","surface":{"title":"Rejected candidate","lines":[],"input":"wrong draft"}}"#,
        false,
    );
    approve_zip(&session, zip, "1.1.0");
    let view = wait(&session, |view| {
        view.message.contains("Previous version restored")
            && view.modules.get(&owner) == Some(&State::Ready)
            && view.surfaces.contains_key(&owner)
    });
    assert_eq!(view.surfaces[&owner].frame.title, "Guest UI");
    assert_eq!(view.surfaces[&owner].draft, "confirmed by user");
    assert_eq!(
        view.installed[0].release.manifest.version.to_string(),
        "1.0.0"
    );
}

#[test]
fn an_update_requiring_new_permissions_keeps_the_running_version_on_cancel() {
    let temp = tempfile::tempdir().unwrap();
    let zip = temp.path().join("demo.zip");
    let profile = temp.path().join("profile");
    bundle(&zip, "^0.1.0", "return true;", false, "1.0.0", false);
    let session = session(&profile, false);
    let namespace = install(&session, zip.clone(), "1.0.0");
    let owner = key(&namespace, 1);
    session.projects(Some(1), vec![project(temp.path(), 1)]);
    let view = wait(&session, |view| {
        view.modules.get(&owner) == Some(&State::Ready)
    });
    let pid = view.workers[&namespace];
    bundle_with_permissions(
        &zip,
        "^0.1.0",
        "return true;",
        false,
        "1.1.0",
        false,
        &[Permission::ReadProject, Permission::ReadDocument],
    );
    session.send(Action::InspectZip(zip)).unwrap();
    let view = wait(&session, |view| view.review.is_some());
    assert_eq!(
        view.review.as_ref().unwrap().additional,
        [Permission::ReadDocument].into()
    );
    assert_eq!(view.workers[&namespace], pid);
    session.send(Action::CancelInstall).unwrap();
    let view = wait(&session, |view| {
        view.review.is_none() && view.message == "Cancelled"
    });
    assert_eq!(view.workers[&namespace], pid);
    assert_eq!(
        view.installed[0].release.manifest.version.to_string(),
        "1.0.0"
    );
    assert_eq!(view.modules.get(&owner), Some(&State::Ready));
}

#[test]
fn update_waits_for_a_confirmed_file_plan_and_can_be_deferred() {
    let temp = tempfile::tempdir().unwrap();
    let zip = temp.path().join("demo.zip");
    let profile = temp.path().join("profile");
    let write_bundle = |version| {
        bundle_with_permissions(
            &zip,
            "^0.1.0",
            "return true;",
            false,
            version,
            false,
            &[Permission::ChangeFiles],
        )
    };
    write_bundle("1.0.0");
    let session = session(&profile, false);
    let namespace = install(&session, zip.clone(), "1.0.0");
    let owner = key(&namespace, 1);
    session.projects(Some(1), vec![project(temp.path(), 1)]);
    let view = wait(&session, |view| {
        view.modules.get(&owner) == Some(&State::Ready)
    });
    let previous = view.installed[0].release.digest.clone();
    let generation = view.runtime_status[&namespace].generation;
    let (lease, ended) = async_channel::bounded(1);
    let (reply, response) = async_channel::bounded(1);
    session
        .send(Action::BeginFileWrite {
            owner: owner.clone(),
            digest: previous.clone(),
            generation,
            ended,
            reply,
        })
        .unwrap();
    response.recv_blocking().unwrap().unwrap();

    write_bundle("1.1.0");
    let approve_update = || {
        session.send(Action::InspectZip(zip.clone())).unwrap();
        let view = wait(&session, |view| view.review.is_some());
        session
            .send(Action::Approve(view.review.as_ref().unwrap().token))
            .unwrap();
        wait(&session, |view| view.waiting_for_operations)
    };
    let view = approve_update();
    assert_eq!(view.installed[0].release.digest, previous);
    session.send(Action::CancelInstall).unwrap();
    wait(&session, |view| {
        !view.waiting_for_operations && view.message == "Cancelled"
    });
    assert_eq!(
        session.view().runtime_status[&namespace].generation,
        generation
    );
    let view = approve_update();
    assert_eq!(view.installed[0].release.digest, previous);
    drop(lease);
    wait(&session, |view| {
        view.installed[0].release.manifest.version.to_string() == "1.1.0"
            && view.modules.get(&owner) == Some(&State::Ready)
    });
}

#[test]
fn compatibility_is_checked_before_worker_start() {
    let temp = tempfile::tempdir().unwrap();
    let zip = temp.path().join("demo.zip");
    let store = InstallStore::new(temp.path().join("profile"));
    bundle(&zip, ">=9.0.0", "return true;", false, "1.0.0", true);
    let error = store.prepare(&zip).err().unwrap().to_string();
    assert!(error.contains("Editor version too old"), "{error}");
    bundle(&zip, "^0.1.0", "return true;", true, "1.0.0", true);
    let error = store.prepare(&zip).err().unwrap().to_string();
    assert!(
        error.contains("Experimental interface ui is unavailable"),
        "{error}"
    );
    assert!(store.list().unwrap().is_empty());
}

#[test]
fn a_broken_compilation_cache_is_rebuilt_only_when_the_plugin_activates() {
    let temp = tempfile::tempdir().unwrap();
    let zip = temp.path().join("demo.zip");
    let profile = temp.path().join("profile");
    bundle(&zip, "^0.1.0", "return true;", false, "1.0.0", false);
    let session = session(&profile, false);
    let namespace = install(&session, zip, "1.0.0");
    let mut directories = vec![profile.join("compiled")];
    let mut damaged = 0;
    while let Some(directory) = directories.pop() {
        for item in fs::read_dir(directory).unwrap() {
            let item = item.unwrap();
            let kind = item.file_type().unwrap();
            if kind.is_dir() {
                directories.push(item.path());
            } else if kind.is_file() {
                fs::write(item.path(), b"incomplete test cache").unwrap();
                damaged += 1;
            }
        }
    }
    assert!(damaged > 0);
    assert!(session.view().workers.is_empty());
    let owner = key(&namespace, 1);
    session.projects(Some(1), vec![project(temp.path(), 1)]);
    wait(&session, |view| {
        view.modules.get(&owner) == Some(&State::Ready)
    });
}

#[test]
fn install_and_activation_do_not_depend_on_a_writable_compile_cache() {
    let temp = tempfile::tempdir().unwrap();
    let zip = temp.path().join("demo.zip");
    let profile = temp.path().join("profile");
    fs::create_dir(&profile).unwrap();
    fs::write(profile.join("compiled"), "unavailable cache directory").unwrap();
    bundle(&zip, "^0.1.0", "return true;", false, "1.0.0", false);
    let store = InstallStore::new(profile.clone());
    let installed = store
        .install(
            store.prepare(&zip).unwrap(),
            &[Permission::ReadProject].into(),
        )
        .unwrap();
    let owner = key(&installed.release.manifest.identity().namespace(), 1);
    let session = session(&profile, false);
    session.projects(Some(1), vec![project(temp.path(), 1)]);
    let view = wait(&session, |view| {
        matches!(
            view.modules.get(&owner),
            Some(State::Ready | State::Recovering | State::Failed(_))
        )
    });
    assert_eq!(view.modules.get(&owner), Some(&State::Ready));
}

#[test]
fn marketplace_falls_back_to_compatible_release_and_reopens_on_demand() {
    let temp = tempfile::tempdir().unwrap();
    let profile = temp.path().join("market");
    fs::create_dir_all(&profile).unwrap();
    let session = session(&profile, false);
    let compatible = profile.join("compatible.zip");
    let future = profile.join("future.zip");
    bundle(&compatible, "^0.1.0", "return true;", false, "1.0.0", false);
    bundle(&future, ">=10.0.0", "return true;", false, "2.0.0", false);
    let catalog = serde_json::json!([
        {"identity": {"name": "Workspace Fixture", "id": "workspace-test"}, "version": "2.0.0", "zip": "future.zip"},
        {"identity": {"name": "Workspace Fixture", "id": "workspace-test"}, "version": "1.0.0", "zip": "compatible.zip"}
    ]);
    fs::write(
        profile.join("marketplace.json"),
        serde_json::to_vec(&catalog).unwrap(),
    )
    .unwrap();
    session.send(Action::OpenMarketplace).unwrap();
    let view = wait(&session, |view| view.catalog.len() == 2);
    session
        .send(Action::InspectCatalog(view.catalog[0].clone()))
        .unwrap();
    let view = wait(&session, |view| view.review.is_some());
    let review = view.review.as_ref().unwrap();
    assert_eq!(review.manifest.version.to_string(), "1.0.0");
    session.send(Action::Approve(review.token)).unwrap();
    wait(&session, |view| view.installed.len() == 1);
    assert!(session.view().workers.is_empty());
}

#[test]
fn marketplace_keeps_its_snapshot_until_the_next_explicit_open() {
    let temp = tempfile::tempdir().unwrap();
    let profile = temp.path().join("market");
    fs::create_dir_all(&profile).unwrap();
    let catalog = profile.join("marketplace.json");
    let set_catalog = |version: &str| {
        let value = serde_json::json!([
            {"identity":{"name":"Fixture","id":"fixture"}, "version":version, "zip":"local.zip"}
        ]);
        fs::write(&catalog, serde_json::to_vec(&value).unwrap()).unwrap();
    };
    set_catalog("1.0.0");
    let session = session(&profile, false);
    let version_is = |view: &View, expected: &str| {
        view.catalog
            .first()
            .is_some_and(|item| item.version.to_string() == expected)
    };
    wait(&session, |view| version_is(view, "1.0.0"));
    set_catalog("1.1.0");
    session.projects(Some(1), vec![project(temp.path(), 1)]);
    wait(&session, |view| {
        view.configurations.contains_key(&ConfigScope::Project(1))
    });
    assert!(version_is(&session.view(), "1.0.0"));
    session.send(Action::OpenMarketplace).unwrap();
    wait(&session, |view| version_is(view, "1.1.0"));
    fs::write(&catalog, b"broken catalog").unwrap();
    session.send(Action::OpenMarketplace).unwrap();
    let view = wait(&session, |view| {
        view.message.contains("Could not read Marketplace")
    });
    assert!(version_is(&view, "1.1.0"));
    assert!(view.workers.is_empty());
}

#[test]
fn uninstall_and_cleanup_leave_project_configuration_and_source_zip_intact() {
    let temp = tempfile::tempdir().unwrap();
    let zip = temp.path().join("demo.zip");
    let profile = temp.path().join("profile");
    bundle(&zip, "^0.1.0", "return true;", false, "1.0.0", false);
    let source = fs::read(&zip).unwrap();
    let session = session(&profile, false);
    let namespace = install(&session, zip.clone(), "1.0.0");
    let project = project(temp.path(), 1);
    let path = project.root.join(".nb/plugins.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let project_config = br#"{"plugins":[{"identity":{"name":"Workspace Fixture","id":"workspace-test"},"rules":{"workspace":false}}]}"#;
    fs::write(&path, project_config).unwrap();
    session.projects(Some(1), vec![project]);
    let scope = ConfigScope::AllProjects;
    let view = wait(&session, |view| view.configurations.contains_key(&scope));
    let mut config = view.configurations[&scope].applied.clone();
    let values = config.plugin_mut(view.installed[0].release.manifest.identity());
    values.rules.insert("workspace".into(), Rule::Always);
    values
        .settings
        .insert("old-setting".into(), serde_json::json!(true));
    session.send(Action::StageConfig(scope, config)).unwrap();
    wait(&session, |view| view.configurations[&scope].draft.is_some());
    session.send(Action::ApplyConfig(scope)).unwrap();
    wait(&session, |view| {
        view.configurations[&scope]
            .applied
            .plugin(&namespace)
            .is_some()
    });
    session.send(Action::Uninstall(namespace.clone())).unwrap();
    let view = wait(&session, |view| {
        view.installed.is_empty() && view.retained.contains_key(&namespace)
    });
    assert_eq!(
        view.configurations[&scope]
            .applied
            .plugin(&namespace)
            .unwrap()
            .settings
            .len(),
        1
    );
    session
        .send(Action::CleanLocalData {
            namespace: namespace.clone(),
            local: true,
            credentials: false,
        })
        .unwrap();
    let view = wait(&session, |view| {
        view.configurations[&scope]
            .applied
            .plugin(&namespace)
            .unwrap()
            .settings
            .is_empty()
    });
    assert_eq!(
        view.configurations[&scope]
            .applied
            .plugin(&namespace)
            .unwrap()
            .rules["workspace"],
        Rule::Always
    );
    assert_eq!(fs::read(&path).unwrap(), project_config);
    assert_eq!(fs::read(&zip).unwrap(), source);
}

#[test]
fn uninstall_can_be_cancelled_and_offline_cleanup_removes_only_selected_local_data() {
    let temp = tempfile::tempdir().unwrap();
    let zip = temp.path().join("removable.zip");
    let profile = temp.path().join("profile");
    bundle_with_permissions(
        &zip,
        "^0.1.0",
        "return true;",
        true,
        "1.0.0",
        false,
        &[Permission::ReadProject, Permission::ChangeFiles],
    );
    let original = fs::read(&zip).unwrap();
    let editor = session(&profile, true);
    let namespace = install(&editor, zip.clone(), "1.0.0");
    let project = project(temp.path(), 1);
    let owner = key(&namespace, project.id);
    editor.projects(Some(project.id), vec![project.clone()]);
    let view = wait(&editor, |view| view.surfaces.contains_key(&owner));
    let pid = view.workers[&namespace];
    let generation = view.runtime_status[&namespace].generation;
    editor.draft(owner.clone(), "retained draft".into());
    let (reply, saved) = async_channel::bounded(1);
    editor
        .send(Action::FlushDrafts {
            project: project.id,
            reply,
        })
        .unwrap();
    saved.recv_blocking().unwrap().unwrap();
    let (lease, ended) = async_channel::bounded(1);
    let (reply, admitted) = async_channel::bounded(1);
    editor
        .send(Action::BeginFileWrite {
            owner: owner.clone(),
            digest: view.installed[0].release.digest.clone(),
            generation,
            ended,
            reply,
        })
        .unwrap();
    admitted.recv_blocking().unwrap().unwrap();
    editor.send(Action::Uninstall(namespace.clone())).unwrap();
    wait(&editor, |view| view.waiting_for_operations);
    editor.send(Action::CancelInstall).unwrap();
    let current = wait(&editor, |view| view.message == "Cancelled");
    assert_eq!(current.workers[&namespace], pid);
    assert_eq!(current.runtime_status[&namespace].generation, generation);
    drop(lease);
    editor.send(Action::Uninstall(namespace.clone())).unwrap();
    wait(&editor, |view| {
        view.installed.is_empty() && view.retained.contains_key(&namespace)
    });
    assert!(
        fs::read_to_string(profile.join("drafts.json"))
            .unwrap()
            .contains("retained draft")
    );
    assert_eq!(install(&editor, zip.clone(), "1.0.0"), namespace);
    wait(&editor, |view| {
        view.surfaces
            .get(&owner)
            .is_some_and(|surface| surface.draft == "retained draft")
    });
    editor.send(Action::Uninstall(namespace.clone())).unwrap();
    wait(&editor, |view| {
        view.installed.is_empty() && view.retained.contains_key(&namespace)
    });
    drop(editor);

    let opts = Options::take(
        &mut vec![
            "--plugin-dev-profile".into(),
            profile.clone().into_os_string(),
            "--no-plugins".into(),
        ],
        temp.path(),
    )
    .unwrap();
    let offline = Session::start(opts).0;
    wait(&offline, |view| view.retained.contains_key(&namespace));
    offline
        .send(Action::CleanLocalData {
            namespace: namespace.clone(),
            local: true,
            credentials: false,
        })
        .unwrap();
    wait(&offline, |view| view.message.contains("cleared"));
    assert!(offline.view().workers.is_empty());
    assert!(
        !fs::read_to_string(profile.join("drafts.json"))
            .unwrap()
            .contains("retained draft")
    );
    assert_eq!(fs::read(zip).unwrap(), original);
}

#[test]
fn interrupted_startups_pause_activation_until_user_resumes() {
    let temp = tempfile::tempdir().unwrap();
    let zip = temp.path().join("demo.zip");
    let profile = temp.path().join("profile");
    let store = InstallStore::new(profile.clone());
    bundle(&zip, "^0.1.0", "return true;", false, "1.0.0", false);
    let record = store
        .install(
            store.prepare(&zip).unwrap(),
            &[Permission::ReadProject].into(),
        )
        .unwrap();
    let namespace = record.release.manifest.identity().namespace();
    #[cfg(windows)]
    acceptance::interrupt_startup_twice(&profile);
    #[cfg(not(windows))]
    fs::write(
        profile.join("startup.json"),
        r#"{"attempts":2,"running":true}"#,
    )
    .unwrap();
    let session = session(&profile, false);
    session.projects(Some(1), vec![project(temp.path(), 1)]);
    let key = key(&namespace, 1);
    wait(&session, |view| {
        view.core_only && view.modules.get(&key) == Some(&State::Disabled)
    });
    assert!(session.view().workers.is_empty());
    session.send(Action::ResumePlugins).unwrap();
    wait(&session, |view| {
        !view.core_only && view.modules.get(&key) == Some(&State::Ready)
    });
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
#[test]
fn an_over_budget_runtime_releases_cache_then_pauses() {
    let temp = tempfile::tempdir().unwrap();
    let profile = temp.path().join("profile");
    let zip = temp.path().join("pressure.zip");
    let wasm = wat::parse_str(
        r#"
        (module
            (memory (export "memory") 1024)
            (data (i32.const 16) "{}")
            (func (export "nb_alloc") (param i32) (result i32) i32.const 1024)
            (func (export "nb_run") (param i32 i32) (result i64)
                i32.const 4096 i32.const 1 i32.const 52428800 memory.fill
                i64.const 68719476738)
            (func (export "nb_free") (param i32 i32)))
    "#,
    )
    .unwrap();
    let manifest = "name = 'Memory Fixture'\nid = 'memory-fixture'\nversion = '1.0.0'\n\
        [engines]\nnanobug = '^0.1.0'\n[[modules]]\nid = 'workspace'\nwasm = 'main.wasm'\n";
    let mut archive = ZipWriter::new(File::create(&zip).unwrap());
    for (name, content) in [
        ("nanobug-plugin.toml", manifest.as_bytes()),
        ("main.wasm", wasm.as_slice()),
    ] {
        archive
            .start_file(name, SimpleFileOptions::default())
            .unwrap();
        archive.write_all(content).unwrap();
    }
    archive.finish().unwrap();
    let session = session(&profile, false);
    let namespace = install(&session, zip, "1.0.0");
    let healthy = temp.path().join("healthy.zip");
    bundle(&healthy, "^0.1.0", "return true;", false, "1.0.0", false);
    let other = install(&session, healthy, "1.0.0");
    let open = (1..=8).map(|id| project(temp.path(), id)).collect();
    session.projects(Some(1), open);
    let survivor = key(&other, 2);
    let pid = wait(&session, |view| {
        view.modules.get(&survivor) == Some(&State::Ready)
    })
    .workers[&other];
    let pressured = key(&namespace, 1);
    let view = wait(&session, |view| {
        matches!(view.modules.get(&pressured), Some(State::Failed(reason)) if reason.contains("memory budget"))
            && !view.workers.contains_key(&namespace)
    });
    let logs = &view.logs[&namespace];
    assert!(
        logs.iter()
            .any(|line| line.contains("Releasing rebuildable runtime cache"))
    );
    assert!(logs.iter().any(|line| line.contains("above the budget")));
    assert_eq!(view.workers[&other], pid);
    session
        .send(Action::Invoke {
            key: survivor,
            event: PluginEvent::Diagnose,
            reply: None,
            timeout: Duration::from_secs(3),
        })
        .unwrap();
    wait(&session, |view| view.message == "guest ran");
}

#[test]
fn runtime_diagnostics_keep_sampling_optional_and_bounded() {
    let temp = tempfile::tempdir().unwrap();
    let profile = temp.path().join("profile");
    let zip = temp.path().join("drawing.zip");
    bundle_with_permissions(
        &zip,
        "^0.1.0",
        "return true;",
        true,
        "1.0.0",
        false,
        &[Permission::ReadDocument],
    );
    let session = session(&profile, true);
    let namespace = install(&session, zip, "1.0.0");
    let owner = key(&namespace, 1);
    let mut first = project(temp.path(), 1);
    let mut second = project(temp.path(), 2);
    let snapshot = Arc::new(DocumentSnapshot {
        path: None,
        opening: 4,
        revision: 1,
        text: "diagnostics fixture ".repeat(100),
    });
    first.document = Some(Arc::clone(&snapshot));
    second.document = Some(Arc::clone(&snapshot));
    let other = key(&namespace, 2);
    session.projects(Some(1), vec![first.clone(), second]);
    let initial = wait(&session, |view| {
        view.runtime_status.get(&namespace).is_some_and(|stats| {
            stats.canvas_bytes > 0
                && stats.last_worker_timing.is_some()
                && stats.last_canvas_prepare_us.is_some()
        }) && [&owner, &other].into_iter().all(|key| {
            view.host_resources
                .get(key)
                .is_some_and(|usage| usage.model_bytes > 0 && usage.shared_document_bytes > 0)
        })
    });
    let shared = initial.host_resources[&owner].shared_document_bytes;
    assert_eq!(initial.host_resources[&other].shared_document_bytes, shared);
    assert_eq!(initial.shared_plugin_resources, shared);
    let pipe = initial.runtime_status[&namespace];
    assert!(pipe.peak_request_bytes >= snapshot.text.len());
    assert!(pipe.peak_reply_wire_bytes > 0);
    session.projects(Some(1), vec![first]);
    let remaining = wait(&session, |view| !view.host_resources.contains_key(&other));
    assert_eq!(remaining.shared_plugin_resources, shared);
    let invoke = || {
        let (reply, response) = async_channel::bounded(1);
        session
            .send(Action::Invoke {
                key: owner.clone(),
                event: PluginEvent::Diagnose,
                reply: Some(reply),
                timeout: Duration::from_secs(3),
            })
            .unwrap();
        assert_eq!(
            response.recv_blocking().unwrap().unwrap().output.message,
            "guest ran"
        );
    };
    invoke();
    let measured = wait(&session, |view| {
        view.runtime_status[&namespace].completed > initial.runtime_status[&namespace].completed
    });
    assert!(
        measured.logs[&namespace]
            .iter()
            .all(|line| !line.contains("response in"))
    );
    session
        .send(Action::SampleRuntime(namespace.clone(), true))
        .unwrap();
    wait(&session, |view| view.sampling.contains(&namespace));
    for _ in 0..82 {
        invoke();
    }
    let sampled = wait(&session, |view| view.logs[&namespace].len() == 80);
    assert!(sampled.logs[&namespace].iter().all(|line| {
        line.starts_with("Module workspace · response in")
            && line.contains("project 1")
            && line.contains("worker queue")
            && line.contains("compute")
    }));
    session
        .send(Action::SampleRuntime(namespace.clone(), false))
        .unwrap();
    let stopped = wait(&session, |view| !view.sampling.contains(&namespace));
    invoke();
    let after = wait(&session, |view| {
        view.runtime_status[&namespace].completed > stopped.runtime_status[&namespace].completed
    });
    assert_eq!(after.logs[&namespace], stopped.logs[&namespace]);
}

#[test]
#[ignore = "requires Cargo and wasm32-unknown-unknown"]
fn developer_build_reloads_and_keeps_working_plugin_after_compile_failure() {
    use nanobug::plugins::session::DeveloperSettings;
    let temp = tempfile::tempdir().unwrap();
    let profile = temp.path().join("development");
    let zip = temp.path().join("demo.zip");
    let output = temp.path().join("build-output");
    fs::create_dir_all(&output).unwrap();
    bundle(&zip, "^0.1.0", "return true;", false, "1.0.0", false);
    let mut archive = zip::ZipArchive::new(File::open(&zip).unwrap()).unwrap();
    archive.extract(&output).unwrap();
    let session = session(&profile, true);
    session.send(Action::InspectDevelopment(output)).unwrap();
    let view = wait(&session, |view| view.review.is_some());
    let review = view.review.as_ref().unwrap();
    let namespace = review.manifest.identity().namespace();
    session.send(Action::Approve(review.token)).unwrap();
    let installed = wait(&session, |view| !view.installed.is_empty());
    let before = installed.installed[0].release.digest.clone();
    let source = temp.path().join("rust-project");
    fs::create_dir_all(source.join("src")).unwrap();
    fs::write(source.join("Cargo.toml"), "[package]\nname = 'main'\nversion = '0.1.0'\nedition = '2024'\n[lib]\ncrate-type = ['cdylib']\n").unwrap();
    let rust = r###"
        #![no_std]
        #[panic_handler]
        fn panic(_: &core::panic::PanicInfo) -> ! { loop {} }
        static mut INPUT: [u8; 262144] = [0; 262144];
        #[unsafe(no_mangle)]
        pub extern "C" fn nb_alloc(_: u32) -> u32 { (&raw mut INPUT) as u32 }
        #[unsafe(no_mangle)]
        pub extern "C" fn nb_run(_: u32, _: u32) -> u64 {
            let result = br#"{"message":"cargo build is live"}"#;
            ((result.as_ptr() as u64) << 32) | result.len() as u64
        }
        #[unsafe(no_mangle)]
        pub extern "C" fn nb_free(_: u32, _: u32) {}
    "###;
    let source_file = source.join("src/lib.rs");
    fs::write(&source_file, rust).unwrap();
    session
        .send(Action::SetDeveloperSettings(
            namespace.clone(),
            DeveloperSettings {
                source: Some(source),
                automatic: true,
            },
        ))
        .unwrap();
    session
        .send(Action::BuildDevelopment(namespace.clone()))
        .unwrap();
    let built = wait(&session, |view| {
        view.installed
            .first()
            .is_some_and(|record| record.release.digest != before)
    });
    let current = built.installed[0].release.digest.clone();
    session.projects(Some(1), vec![project(temp.path(), 1)]);
    let key = key(&namespace, 1);
    wait(&session, |view| {
        view.modules.get(&key) == Some(&State::Ready)
    });
    session
        .send(Action::Invoke {
            key,
            event: PluginEvent::Diagnose,
            reply: None,
            timeout: Duration::from_secs(3),
        })
        .unwrap();
    wait(&session, |view| view.message == "cargo build is live");
    fs::write(source_file, "deliberate syntax error").unwrap();
    wait(&session, |view| {
        view.logs.get(&namespace).is_some_and(|logs| {
            logs.iter()
                .any(|line| line.contains("Cargo build did not finish"))
        })
    });
    assert_eq!(session.view().installed[0].release.digest, current);
}

#[test]
#[ignore = "requires the release wasm32-unknown-unknown workspace-panel SDK example"]
fn rust_sdk_canvas_runs_in_the_actual_worker() {
    use nanobug_plugin_sdk::canvas::{
        Appearance, Draw, EventKind, Language, ThemeColors, Viewport,
    };
    let temp = tempfile::tempdir().unwrap();
    let profile = temp.path().join("profile");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../sdk/examples");
    let wasm =
        source.join("../../../target/wasm32-unknown-unknown/release/examples/workspace_panel.wasm");
    let build = temp.path().join("example-build");
    fs::create_dir_all(&build).unwrap();
    for name in ["nanobug-plugin.toml", "detect.js"] {
        fs::copy(source.join(name), build.join(name)).unwrap();
    }
    fs::copy(wasm, build.join("workspace_panel.wasm")).unwrap();
    let session = session(&profile, true);
    let project = project(temp.path(), 1);
    fs::write(project.root.join("README.md"), "SDK demo").unwrap();
    session.projects(Some(1), vec![project]);
    session.send(Action::InspectDevelopment(build)).unwrap();
    let view = wait(&session, |view| view.review.is_some());
    let review = view.review.as_ref().unwrap();
    let key = Key {
        namespace: review.manifest.identity().namespace(),
        instance: Instance {
            project: 1,
            module: "notes".into(),
        },
    };
    session.send(Action::Approve(review.token)).unwrap();
    let view = wait(&session, |view| view.surfaces.contains_key(&key));
    assert_eq!(view.surfaces[&key].frame.title, "Workspace panel");
    session
        .send(Action::Resize(
            key.clone(),
            Viewport {
                id: 19,
                width: 320.,
                height: 120.,
                scale: 1.,
                apple_surface: cfg!(any(target_os = "macos", target_os = "ios")),
                shaped_line: true,
                appearance: None,
            },
        ))
        .unwrap();
    let view = wait(&session, |view| {
        view.surfaces.get(&key).is_some_and(|surface| {
            surface
                .frame
                .canvas
                .as_ref()
                .is_some_and(|canvas| canvas.viewport == Some(19))
        })
    });
    assert!(view.surfaces[&key].drawing.is_some());
    let revision = view.surfaces[&key].revision;
    let dispatch = |kind, revision, viewport, action: &str| {
        session
            .send(Action::CanvasAction {
                key: key.clone(),
                revision,
                viewport,
                kind,
                action: action.into(),
                argument: serde_json::json!({"event": kind}),
            })
            .unwrap();
    };
    let barrier = |expected| {
        let (reply, answer) = async_channel::bounded(1);
        session
            .send(Action::Invoke {
                key: key.clone(),
                event: PluginEvent::Diagnose,
                reply: Some(reply),
                timeout: Duration::from_secs(3),
            })
            .unwrap();
        assert_eq!(
            answer.recv_blocking().unwrap().unwrap().output.message,
            "Workspace panel is ready"
        );
        let view = wait(&session, |view| {
            let status = view.runtime_status[&key.namespace];
            status.pending == 0 && status.completed >= expected
        });
        assert_eq!(view.runtime_status[&key.namespace].completed, expected);
    };
    let view = wait(&session, |view| {
        view.runtime_status[&key.namespace].pending == 0
    });
    let completed = view.runtime_status[&key.namespace].completed;
    dispatch(EventKind::NextFrame, revision, 19, "painted");
    dispatch(EventKind::NextFrame, revision, 19, "painted");
    barrier(completed + 2);
    assert_eq!(session.view().surfaces[&key].revision, revision);

    dispatch(EventKind::NextFrame, revision, 19, "painted");
    dispatch(EventKind::Wheel, revision, 19, "panel-key");
    dispatch(EventKind::KeyDown, revision - 1, 19, "panel-key");
    dispatch(EventKind::KeyDown, revision, 18, "panel-key");
    barrier(completed + 3);

    dispatch(EventKind::KeyDown, revision, 19, "panel-key");
    let caption = |view: &View, expected| {
        view.surfaces[&key]
            .frame
            .canvas
            .as_ref()
            .unwrap()
            .draw
            .iter()
            .any(|draw| matches!(draw, Draw::Text { text, .. } if text == expected))
    };
    let view = wait(&session, |view| caption(view, "Canvas key pressed"));
    session
        .send(Action::SurfaceAction {
            key: key.clone(),
            revision: view.surfaces[&key].revision,
            action: "heading".into(),
            argument: serde_json::json!({"x": 20., "y": 24.}),
        })
        .unwrap();
    wait(&session, |view| caption(view, "Heading clicked"));
    session.draft(key.clone(), "中文👋".into());
    wait(&session, |view| {
        view.surfaces
            .get(&key)
            .is_some_and(|surface| surface.draft == "中文👋")
    });
    let pid = session.view().workers[&key.namespace];
    session
        .send(Action::Resize(
            key.clone(),
            Viewport {
                id: 20,
                width: 320.,
                height: 120.,
                scale: 1.5,
                apple_surface: cfg!(any(target_os = "macos", target_os = "ios")),
                shaped_line: true,
                appearance: Some(Appearance {
                    language: Language::SimplifiedChinese,
                    font_size: 19.,
                    base_font_size: None,
                    font: None,
                    user_scale: 1.1875,
                    theme: None,
                    colors: ThemeColors {
                        background: 0x0b1624ff,
                        text: 0xc8d3f5ff,
                        muted: 0x8a9eb3ff,
                        accent: 0x8acf9bff,
                        selection: 0x65baa558,
                        warning: 0xf2d09bff,
                        border: None,
                        error: None,
                    },
                }),
            },
        ))
        .unwrap();
    let updated = wait(&session, |view| {
        view.surfaces[&key]
            .frame
            .canvas
            .as_ref()
            .is_some_and(|canvas| {
                canvas.viewport == Some(20) && canvas.draw.iter().any(|draw| matches!(draw,
            Draw::Text { text, size, .. } if text.starts_with("工作区笔记") && *size == 19.
        ))
            })
    });
    assert_eq!(updated.workers[&key.namespace], pid);
    assert_eq!(updated.surfaces[&key].draft, "中文👋");
    session
        .send(Action::SurfaceAction {
            key: key.clone(),
            revision: updated.surfaces[&key].revision,
            action: "heading".into(),
            argument: serde_json::Value::Null,
        })
        .unwrap();
    let fallback = wait(&session, |view| caption(view, "Heading clicked"));
    assert!(caption(&fallback, "工作区笔记 你好 👋"));
    assert_eq!(fallback.workers[&key.namespace], pid);
}

#[test]
fn a_package_font_is_measured_and_painted_by_the_installed_plugin() {
    let temp = tempfile::tempdir().unwrap();
    let archive = temp.path().join("font.zip");
    let request = r#"{"services":[{"token":1,"operation":{"service":"measure_text",
        "lines":[{"text":"Font test","size":18,"package_font":{"asset":"panel.ttf"}}]}}]}"#;
    let measured = r#"{"surface":{"title":"Measured package font","canvas":{"draw":[
        {"op":"package_text","baseline":[12,28],"text":"Font test",
         "font":{"asset":"panel.ttf"},"size":18,"color":4294967295}]}}}"#;
    let wat_data = |json: &str| {
        json.bytes()
            .map(|byte| format!("\\{byte:02x}"))
            .collect::<String>()
    };
    // There is no viewport or document in this fixture. Only a successful measurement reply
    // contains the width key and allows the guest to return the package-font frame.
    let width_key = i64::from_le_bytes(*b"\"width\":");
    let wasm = wat::parse_str(format!(
        r#"(module
            (memory (export "memory") 20)
            (data (i32.const 32) "{}")
            (data (i32.const 4096) "{}")
            (func (export "nb_alloc") (param i32) (result i32) i32.const 8192)
            (func (export "nb_run") (param $input i32) (param $len i32) (result i64)
                (local $i i32)
                (block $miss
                    (loop $scan
                        (br_if $miss (i32.gt_u (i32.add (local.get $i) (i32.const 8)) (local.get $len)))
                        (if (i64.eq (i64.load (i32.add (local.get $input) (local.get $i))) (i64.const {width_key}))
                            (then (return (i64.const {}))))
                        (local.set $i (i32.add (local.get $i) (i32.const 1)))
                        (br $scan)))
                i64.const {})
            (func (export "nb_free") (param i32 i32)))"#,
        wat_data(request),
        wat_data(measured),
        (4096_u64 << 32) | measured.len() as u64,
        (32_u64 << 32) | request.len() as u64,
    )).unwrap();
    let manifest = format!(
        "name='Package Font'\nid='package-font'\nversion='1.0.0'\n[engines]\nnanobug='*'\n\
         [experiments]\nui={UI_CONTRACT:?}\n[[modules]]\nid='workspace'\nwasm='main.wasm'\ninteractive=true\n"
    );
    let write_archive = |path: &Path, manifest: &str| {
        let mut zip = ZipWriter::new(File::create(path).unwrap());
        for (name, bytes) in [
            ("nanobug-plugin.toml", manifest.as_bytes()),
            ("main.wasm", &wasm),
            (
                "panel.ttf",
                include_bytes!("../../../assets/fonts/RecMonoCasual-Regular-1.085.ttf"),
            ),
        ] {
            zip.start_file(name, SimpleFileOptions::default()).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
    };
    write_archive(&archive, &manifest);

    let profile = temp.path().join("profile");
    let session = session(&profile, true);
    let project = project(temp.path(), 1);
    session.projects(Some(1), vec![project.clone()]);
    let namespace = install(&session, archive, "1.0.0");
    let owner = key(&namespace, 1);
    let view = wait(&session, |view| view.surfaces.contains_key(&owner));
    let surface = &view.surfaces[&owner];
    assert_eq!(surface.frame.title, "Measured package font");
    let drawing = surface.drawing.as_ref().expect("Prepared font outlines");
    assert!(drawing.assets.is_empty());
    assert_eq!(drawing.paths.len(), 1);
    assert!(!drawing.paths[0].vertices.is_empty());
    let released = Arc::downgrade(drawing);
    drop(view);
    session.projects(None, Vec::new());
    wait(&session, |view| !view.surfaces.contains_key(&owner));
    assert!(released.upgrade().is_none());
    wait(&session, |view| view.workers.is_empty());

    // Native measurements needed for startup can finish while the candidate cannot take over.
    let update = temp.path().join("font-update.zip");
    write_archive(&update, &manifest.replace("1.0.0", "1.1.0"));
    assert_eq!(install(&session, update, "1.1.0"), namespace);
    let lease = File::options()
        .read(true)
        .write(true)
        .open(profile.join("installed.lock"))
        .unwrap();
    lease.lock().unwrap();
    session.projects(Some(1), vec![project]);
    let pending = wait(&session, |view| {
        view.runtime_status
            .get(&namespace)
            .is_some_and(|status| status.completed >= 2)
    });
    assert!(pending.installed[0].pending_takeover);
    assert_eq!(pending.modules[&owner], State::Starting);
    assert!(!pending.surfaces.contains_key(&owner));
    drop(lease);
    let view = wait(&session, |view| {
        view.modules.get(&owner) == Some(&State::Ready) && view.surfaces.contains_key(&owner)
    });
    assert_eq!(view.surfaces[&owner].frame.title, "Measured package font");
    assert!(!InstallStore::new(profile).list().unwrap()[0].pending_takeover);
}

/// Load a preserved release ZIP with its original guest binary.
#[test]
#[ignore = "uses a previously built workspace-panel ZIP chosen through NANOBUG_PLUGIN_TEST_ARCHIVE"]
fn preserved_workspace_panel_runs_without_recompiling_the_guest() {
    let archive = PathBuf::from(
        std::env::var_os("NANOBUG_PLUGIN_TEST_ARCHIVE").expect("select an archived SDK ZIP"),
    );
    let temp = tempfile::tempdir().unwrap();
    let session = session(&temp.path().join("profile"), true);
    let project = project(temp.path(), 1);
    fs::write(project.root.join("README.md"), "Compatibility fixture").unwrap();
    session.projects(Some(1), vec![project]);
    session.send(Action::InspectZip(archive)).unwrap();
    let view = wait(&session, |view| view.review.is_some());
    let review = view.review.as_ref().unwrap();
    assert_eq!(review.manifest.api, "1.0");
    let namespace = review.manifest.identity().namespace();
    session.send(Action::Approve(review.token)).unwrap();
    wait(&session, |view| {
        view.surfaces.iter().any(|(key, surface)| {
            key.namespace == namespace
                && surface.frame.title == "Workspace panel"
                && surface.drawing.is_some()
        })
    });
}

/// Generate actual installed packages for the Windows Release startup measurement.
#[test]
#[ignore = "writes benchmark fixtures to the explicitly selected output directory"]
fn prepare_launch_profiles() {
    let root = PathBuf::from(
        std::env::var_os("NANOBUG_PLUGIN_TEST_OUTPUT").expect("set test output directory"),
    );
    fs::create_dir_all(&root).unwrap();
    let off = root.join("unmatched");
    let on = root.join("active");
    fs::create_dir_all(&off).unwrap();
    fs::create_dir_all(&on).unwrap();
    for project in [&off, &on] {
        fs::write(project.join("README.md"), "release benchmark\n").unwrap();
    }
    fs::write(on.join("activate"), "").unwrap();
    let empty_document = root.join("empty-document");
    fs::create_dir_all(&empty_document).unwrap();
    fs::write(empty_document.join("activate"), "").unwrap();
    let profile = root.join("fifty");
    let store = InstallStore::new(profile);
    for index in 0..50 {
        // Keep the response small while giving every installed package its own cache entry.
        let wasm = wat::parse_str(format!(
            r#"
            (module
                (memory (export "memory") 20)
                (data (i32.const 16) "{{}}")
                (data (i32.const 128) "{index}")
                (func (export "nb_alloc") (param i32) (result i32) i32.const 1024)
                (func (export "nb_run") (param i32 i32) (result i64) i64.const 68719476738)
                (func (export "nb_free") (param i32 i32)))
            "#,
        ))
        .unwrap();
        let path = root.join(format!("bench-{index}.zip"));
        let mut zip = ZipWriter::new(File::create(&path).unwrap());
        let manifest = format!(
            "name = 'Benchmark {index}'\nid = 'bench-{index}'\nversion = '1.0.0'\npermissions = ['read_project']\n\
             [engines]\nnanobug = '^0.1.0'\n[[modules]]\nid = 'test'\nwasm = 'main.wasm'\nrule = 'detect.js'\n"
        );
        let rule = if index < 10 {
            "return await host.exists('activate');"
        } else {
            "return await host.exists('intentionally-absent');"
        };
        for (name, bytes) in [
            ("nanobug-plugin.toml", manifest.as_bytes()),
            ("main.wasm", &wasm),
            ("detect.js", rule.as_bytes()),
        ] {
            zip.start_file(name, SimpleFileOptions::default()).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
        store
            .install(
                store.prepare(&path).unwrap(),
                &[Permission::ReadProject].into(),
            )
            .unwrap();
    }
    assert_eq!(store.list().unwrap().len(), 50);
}
