//! Verify host-bound ownership and grants using ordinary SDK calls and temporary profiles.

use nanobug::plugins::{
    package::{Identity, UI_CONTRACT},
    session::{
        Action, CallResult, Config, ConfigEdit, ConfigScope, Key, Options, Project, Session, State,
        View,
    },
};
use nanobug_plugin_sdk::{DocumentSnapshot, Instance, Permission, PluginEvent};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    ffi::OsString,
    fs::{self, File},
    io::Write,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};
use zip::{ZipWriter, write::SimpleFileOptions};

#[path = "authorization/cooperation.rs"]
mod cooperation;
#[path = "authorization/developer.rs"]
mod developer;

const ID: &str = "authorization-fixture";

/// Report the input of an ordinary SDK callback alongside harmless fixture contributions.
fn guest(output: Value) -> Vec<u8> {
    let mut prefix = output.to_string();
    assert_eq!(prefix.pop(), Some('}'));
    if prefix.len() > 1 {
        prefix.push(',');
    }
    prefix.push_str("\"service_value\":");
    let bytes = prefix
        .bytes()
        .map(|byte| format!("\\{byte:02x}"))
        .collect::<String>();
    let length = prefix.len();
    wat::parse_str(format!(
        r#"(module
            (memory (export "memory") 32)
            (data (i32.const 16) "{bytes}")
            (func (export "nb_alloc") (param i32) (result i32) i32.const 4096)
            (func (export "nb_run") (param $input i32) (param $size i32) (result i64)
                (memory.copy (i32.const 1048576) (i32.const 16) (i32.const {length}))
                (memory.copy (i32.const {data}) (local.get $input) (local.get $size))
                (i32.store8 (i32.add (i32.const {data}) (local.get $size)) (i32.const 125))
                (i64.or (i64.const {pointer})
                    (i64.extend_i32_u (i32.add (local.get $size) (i32.const {overhead})))))
            (func (export "nb_free") (param i32 i32)))"#,
        data = 1048576 + length,
        pointer = 1048576_u64 << 32,
        overhead = length + 1
    ))
    .unwrap()
}

fn bundle(
    path: &Path,
    name: &str,
    permissions: &[&str],
    interactive: bool,
    extra: &str,
    output: Value,
) {
    let manifest = format!(
        "name={name:?}\nid='{ID}'\nversion='1.0.0'\npermissions={}\n\
         [engines]\nnanobug='^0.1.0'\n{}\n\
         [[modules]]\nid='main'\nwasm='main.wasm'\ninteractive={interactive}\n\
         [settings.label]\ntitle='Label'\ndefault='fixture-default'\n{extra}",
        serde_json::to_string(permissions).unwrap(),
        if interactive {
            format!("[experiments]\nui={UI_CONTRACT:?}\n")
        } else {
            String::new()
        }
    );
    write_bundle(path, &manifest, &guest(output));
}

fn write_bundle(path: &Path, manifest: &str, wasm: &[u8]) {
    let mut zip = ZipWriter::new(File::create(path).unwrap());
    for (name, contents) in [
        ("nanobug-plugin.toml", manifest.as_bytes()),
        ("main.wasm", wasm),
    ] {
        zip.start_file(name, SimpleFileOptions::default()).unwrap();
        zip.write_all(contents).unwrap();
    }
    zip.finish().unwrap();
}

fn options(profile: &Path, development: bool) -> Options {
    let flag = if development {
        "--plugin-dev-profile"
    } else {
        "--plugin-profile"
    };
    let mut args = vec![OsString::from(flag), profile.as_os_str().to_os_string()];
    Options::take(&mut args, profile.parent().unwrap()).unwrap()
}

fn session(profile: &Path, development: bool) -> Session {
    Session::start(options(profile, development)).0
}

fn wait(session: &Session, predicate: impl Fn(&View) -> bool) -> Arc<View> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let view = session.view();
        if predicate(&view) {
            return view;
        }
        assert!(
            Instant::now() < deadline,
            "Timed out. Message={:?}, modules={:?}, logs={:?}",
            view.message,
            view.modules,
            view.logs
        );
        std::thread::sleep(Duration::from_millis(15));
    }
}

fn answer<T>(receiver: &async_channel::Receiver<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        match receiver.try_recv() {
            Ok(result) => return result,
            Err(async_channel::TryRecvError::Closed) => panic!("Host response closed"),
            Err(async_channel::TryRecvError::Empty) => {
                assert!(Instant::now() < deadline, "Host response timed out");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

fn install(session: &Session, path: &Path) -> Identity {
    session
        .send(Action::InspectZip(path.to_path_buf()))
        .unwrap();
    let view = wait(session, |view| view.review.is_some());
    let review = view.review.as_ref().unwrap();
    let identity = review.manifest.identity();
    session.send(Action::Approve(review.token)).unwrap();
    wait(session, |view| {
        view.installed
            .iter()
            .any(|plugin| plugin.release.manifest.identity() == identity)
    });
    identity
}

fn project(parent: &Path, id: u64) -> Project {
    let root = parent.join(format!("project-{id}"));
    fs::create_dir_all(&root).unwrap();
    let path = root.join("document.txt");
    fs::write(&path, "saved fixture text").unwrap();
    Project {
        id,
        root,
        document: Some(Arc::new(DocumentSnapshot {
            path: Some(path),
            opening: id,
            revision: 1,
            text: "unsaved fixture text".into(),
        })),
        open_documents: Vec::new(),
    }
}

fn key(identity: &Identity, project: u64) -> Key {
    Key {
        namespace: identity.namespace(),
        instance: Instance {
            project,
            module: "main".into(),
        },
    }
}

fn call(session: &Session, key: &Key) -> Result<CallResult, String> {
    let (reply, result) = async_channel::bounded(1);
    session
        .send(Action::Invoke {
            key: key.clone(),
            event: PluginEvent::Command {
                id: "inspect".into(),
                argument: Value::Null,
            },
            reply: Some(reply),
            timeout: Duration::from_secs(2),
        })
        .unwrap();
    answer(&result)
}

fn configure(session: &Session, values: &[(&Identity, &str)]) {
    let scope = ConfigScope::AllProjects;
    let mut config = Config::default();
    for (identity, label) in values {
        config
            .plugin_mut((*identity).clone())
            .settings
            .insert("label".into(), (*label).into());
    }
    session
        .send(Action::StageConfig(scope, config.clone()))
        .unwrap();
    session.send(Action::ApplyConfig(scope)).unwrap();
    wait(session, |view| {
        view.configurations
            .get(&scope)
            .is_some_and(|value| value.applied == config)
    });
}

fn label(session: &Session, key: &Key) -> Value {
    call(session, key).unwrap().output.service_value.unwrap()["settings"]["label"].clone()
}

/// The settings dialog uses the same running guest, committed scopes, and inherited fallback as editor commands.
#[test]
fn settings_dialog_restores_running_plugin_overrides_and_lists_installs_on_start_page() {
    use nanobug::plugins::session::{SettingPatch, SettingsWrite};
    use std::collections::BTreeMap;
    let fixture = tempfile::tempdir().unwrap();
    let archive = fixture.path().join("settings.zip");
    bundle(
        &archive,
        "Settings fixture",
        &[],
        false,
        "project=true",
        json!({}),
    );
    let session = session(&fixture.path().join("profile"), false);
    let identity = install(&session, &archive);
    let namespace = identity.namespace();
    let (reply, result) = async_channel::bounded(1);
    session
        .send(Action::SettingsCatalog {
            project: None,
            reply,
        })
        .unwrap();
    let welcome = answer(&result).unwrap();
    assert_eq!(welcome.scopes.len(), 1);
    assert!(welcome.scans.is_empty());
    let installed = &welcome.installed[0];
    assert_eq!(installed.release.manifest.identity(), identity);
    let digest = installed.release.digest.clone();
    let write = |request| {
        let (reply, result) = async_channel::bounded(1);
        session.send(Action::WriteSettings(request, reply)).unwrap();
        answer(&result).unwrap();
    };
    for (before, after) in [(true, false), (false, true)] {
        write(SettingsWrite::Enabled {
            namespace: namespace.clone(),
            digest: digest.clone(),
            before,
            after,
        });
    }
    let first = project(fixture.path(), 1);
    let second = project(fixture.path(), 2);
    session.projects(Some(1), vec![first, second]);
    let owner = key(&identity, 1);
    let other = key(&identity, 2);
    wait(&session, |v| {
        v.modules.get(&owner) == Some(&State::Ready) && v.modules.get(&other) == Some(&State::Ready)
    });
    let patch = |scope, before: Option<&str>, after: Option<&str>| {
        let raw = |v: Option<&str>| {
            v.map(|v| BTreeMap::from([("label".to_owned(), json!(v))]))
                .unwrap_or_default()
        };
        write(SettingsWrite::Manifest {
            namespace: namespace.clone(),
            digest: digest.clone(),
            scope,
            owner_was_absent: true,
            fields: vec![SettingPatch {
                restore_snapshot: after.is_none(),
                id: "label".into(),
                before: raw(before),
                after: raw(after),
            }],
        });
    };
    let observed = |key: &Key, wanted: &str| {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            match call(&session, key) {
                Ok(call) => {
                    let current = &call.output.service_value.unwrap()["settings"]["label"];
                    if current == wanted {
                        return;
                    }
                }
                Err(error) if error.contains("adopting updated settings") => {}
                Err(error) => panic!("Unexpected guest error: {error}"),
            }
            assert!(Instant::now() < deadline, "Guest did not adopt {wanted}");
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    patch(ConfigScope::AllProjects, None, Some("all"));
    patch(ConfigScope::Project(1), None, Some("one"));
    observed(&owner, "one");
    observed(&other, "all");
    patch(ConfigScope::Project(1), Some("one"), None);
    observed(&owner, "all");
    patch(ConfigScope::AllProjects, Some("all"), None);
    observed(&owner, "fixture-default");
    observed(&other, "fixture-default");
    let (reply, result) = async_channel::bounded(1);
    session
        .send(Action::SettingsCatalog {
            project: Some(1),
            reply,
        })
        .unwrap();
    let restored = answer(&result).unwrap();
    assert!(
        restored.scopes[&ConfigScope::Project(1)]
            .plugin(&namespace)
            .is_none()
    );
    assert!(
        restored.scopes[&ConfigScope::AllProjects]
            .plugin(&namespace)
            .is_none()
    );
}

fn admit_plan(
    session: &Session,
    owner: Key,
    digest: String,
    generation: u64,
) -> Result<(), String> {
    let (lease, ended) = async_channel::bounded(1);
    let (reply, result) = async_channel::bounded(1);
    session
        .send(Action::BeginFileWrite {
            owner,
            digest,
            generation,
            ended,
            reply,
        })
        .unwrap();
    let result = answer(&result);
    drop(lease);
    result
}

#[test]
fn ordinary_runtimes_keep_their_own_settings_and_reject_stale_plan_authority() {
    let fixture = tempfile::tempdir().unwrap();
    let ordinary = fixture.path().join("regular");
    let development = fixture.path().join("development");
    fs::create_dir(&ordinary).unwrap();
    let registry = ordinary.join("installed.json");
    fs::write(&registry, "{}").unwrap();
    options(&ordinary, false).store().unwrap();
    assert_eq!(fs::read_to_string(&registry).unwrap(), "{}");
    let first_zip = fixture.path().join("alpha.zip");
    let second_zip = fixture.path().join("beta.zip");
    bundle(
        &first_zip,
        "Alpha Fixture",
        &["change_files"],
        false,
        "",
        json!({}),
    );
    bundle(
        &second_zip,
        "Beta Fixture",
        &["change_files"],
        false,
        "",
        json!({}),
    );
    let regular = session(&ordinary, false);
    let alpha = install(&regular, &first_zip);
    let beta = install(&regular, &second_zip);
    assert_eq!(alpha.id, beta.id);
    assert_ne!(alpha.namespace(), beta.namespace());
    let workspace = project(fixture.path(), 1);
    let a = key(&alpha, workspace.id);
    let b = key(&beta, workspace.id);
    regular.projects(Some(workspace.id), vec![workspace.clone()]);
    let started = wait(&regular, |view| {
        view.modules.get(&a) == Some(&State::Ready) && view.modules.get(&b) == Some(&State::Ready)
    });
    assert_ne!(started.workers[&a.namespace], started.workers[&b.namespace]);
    configure(
        &regular,
        &[(&alpha, "ordinary alpha"), (&beta, "ordinary beta")],
    );
    assert_eq!(label(&regular, &a), "ordinary alpha");
    assert_eq!(label(&regular, &b), "ordinary beta");
    let alpha_release = started
        .installed
        .iter()
        .find(|record| record.release.manifest.identity() == alpha)
        .unwrap();
    let digest = alpha_release.release.digest.clone();
    let generation = started.runtime_status[&a.namespace].generation;
    admit_plan(&regular, a.clone(), digest.clone(), generation).unwrap();
    assert!(admit_plan(&regular, b.clone(), digest.clone(), generation).is_err());

    regular.projects(None, Vec::new());
    wait(&regular, |view| view.workers.is_empty());
    assert!(call(&regular, &a).is_err());
    let reopened = Project {
        id: 2,
        ..workspace.clone()
    };
    let new_a = key(&alpha, reopened.id);
    regular.projects(Some(reopened.id), vec![reopened]);
    let current = wait(&regular, |view| {
        view.modules.get(&new_a) == Some(&State::Ready)
    });
    let new_generation = current.runtime_status[&a.namespace].generation;
    assert_ne!(generation, new_generation);
    assert!(admit_plan(&regular, new_a.clone(), digest.clone(), generation).is_err());
    assert!(admit_plan(&regular, a, digest.clone(), new_generation).is_err());
    admit_plan(&regular, new_a.clone(), digest, new_generation).unwrap();

    let dev = session(&development, true);
    let empty = wait(&dev, |view| view.development);
    assert!(empty.installed.is_empty());
    let dev_alpha = install(&dev, &first_zip);
    assert_eq!(dev_alpha, alpha);
    let dev_key = key(&dev_alpha, workspace.id);
    dev.projects(Some(workspace.id), vec![workspace]);
    wait(&dev, |view| {
        view.modules.get(&dev_key) == Some(&State::Ready)
    });
    assert_eq!(label(&dev, &dev_key), "fixture-default");
    configure(&dev, &[(&dev_alpha, "development alpha")]);
    assert_eq!(label(&dev, &dev_key), "development alpha");
    assert_eq!(label(&regular, &new_a), "ordinary alpha");

    let registered = fs::read(&registry).unwrap();
    let alias = ordinary.join("..").join("regular");
    let mistaken = session(&alias, true);
    let rejected = wait(&mistaken, |view| {
        view.message.contains("belongs to another editor mode")
    });
    assert!(rejected.installed.is_empty());
    assert!(rejected.workers.is_empty());
    let mistaken = session(&development, false);
    wait(&mistaken, |view| {
        view.message.contains("belongs to another editor mode")
    });
    assert_eq!(fs::read(registry).unwrap(), registered);
}

#[test]
fn interactive_input_requires_the_active_project_and_current_frame() {
    let fixture = tempfile::tempdir().unwrap();
    let path = fixture.path().join("interactive.zip");
    bundle(
        &path,
        "Interactive Fixture",
        &[],
        true,
        "",
        json!({"surface":{"title":"Fixture","lines":[],"input":""}}),
    );
    let editor = session(&fixture.path().join("development"), true);
    let plugin = install(&editor, &path);
    let first = project(fixture.path(), 1);
    let second = project(fixture.path(), 2);
    let active = key(&plugin, first.id);
    let hidden = key(&plugin, second.id);
    editor.projects(Some(first.id), vec![first, second]);
    let ready = wait(&editor, |view| {
        view.surfaces.contains_key(&active)
            && view.surfaces.contains_key(&hidden)
            && view.runtime_status[&active.namespace].pending == 0
    });
    let completed = ready.runtime_status[&active.namespace].completed;
    let click = |owner: &Key, revision| {
        editor
            .send(Action::SurfaceAction {
                key: owner.clone(),
                revision,
                action: "submit".into(),
                argument: Value::Null,
            })
            .unwrap();
    };
    click(&hidden, ready.surfaces[&hidden].revision);
    call(&editor, &hidden).unwrap();
    let checked = wait(&editor, |view| {
        let status = view.runtime_status[&active.namespace];
        status.pending == 0 && status.completed > completed
    });
    assert_eq!(
        checked.runtime_status[&active.namespace].completed,
        completed + 1
    );

    let old_frame = checked.surfaces[&active].revision;
    call(&editor, &active).unwrap();
    let updated = wait(&editor, |view| view.surfaces[&active].revision != old_frame);
    let completed = updated.runtime_status[&active.namespace].completed;
    click(&active, old_frame);
    call(&editor, &active).unwrap();
    let checked = wait(&editor, |view| {
        let status = view.runtime_status[&active.namespace];
        status.pending == 0 && status.completed > completed
    });
    assert_eq!(
        checked.runtime_status[&active.namespace].completed,
        completed + 1
    );
    click(&active, checked.surfaces[&active].revision);
    wait(&editor, |view| {
        view.runtime_status[&active.namespace].completed == completed + 2
    });
}

#[test]
fn permission_review_cancels_cleanly_and_approved_reads_keep_their_limits_after_reopen() {
    let fixture = tempfile::tempdir().unwrap();
    let path = fixture.path().join("permissions.zip");
    bundle(
        &path,
        "Permission Fixture",
        &["read_document", "read_project", "credentials"],
        false,
        "[settings.private-token]\ntitle='Credential slot'\nsecret=true\n",
        json!({
            "edits": [{"start": 0, "end": 0, "text": "fixture proposal"}],
            "file_operations": [{"operation":"create","path":"preview-only.txt","text":"fixture"}],
            "services": [{"token":1,"operation":{"service":"run_tool","tool":"example-tool","args":[]}}]
        }),
    );
    let archive = fs::read(&path).unwrap();
    let profile = fixture.path().join("regular");
    let workspace = project(fixture.path(), 1);
    let configuration = workspace.root.join(".nb/plugins.json");
    fs::create_dir(configuration.parent().unwrap()).unwrap();
    fs::write(&configuration, "{}\n").unwrap();
    let editor = session(&profile, false);
    editor.projects(Some(workspace.id), vec![workspace.clone()]);
    editor.send(Action::InspectZip(path.clone())).unwrap();
    let first = wait(&editor, |view| view.review.is_some());
    let first = first.review.as_ref().unwrap();
    let permissions = BTreeSet::from([
        Permission::ReadDocument,
        Permission::ReadProject,
        Permission::Credentials,
    ]);
    assert_eq!(first.additional, permissions);
    assert!(first.manifest.settings["private-token"].secret);
    assert!(first.manifest.settings["private-token"].default.is_null());
    let identity = first.manifest.identity();
    let owner = key(&identity, workspace.id);
    assert!(editor.view().installed.is_empty());
    assert!(editor.view().workers.is_empty());

    editor.send(Action::CancelInstall).unwrap();
    let cancelled = wait(&editor, |view| view.message == "Cancelled");
    assert!(cancelled.review.is_none());
    assert!(cancelled.installed.is_empty());
    assert!(cancelled.workers.is_empty());
    assert!(!profile.join("installed.json").exists());
    assert_eq!(fs::read(&path).unwrap(), archive);
    assert_eq!(fs::read_to_string(&configuration).unwrap(), "{}\n");

    editor.send(Action::InspectZip(path.clone())).unwrap();
    let second = wait(&editor, |view| view.review.is_some());
    let second = second.review.as_ref().unwrap();
    assert_ne!(first.token, second.token);
    assert_eq!(second.additional, permissions);
    editor.send(Action::Approve(first.token)).unwrap();
    answer(&editor.drain_for_close(None).unwrap()).unwrap();
    assert_eq!(editor.view().review.as_ref().unwrap().token, second.token);
    assert!(editor.view().installed.is_empty());

    editor.send(Action::Approve(second.token)).unwrap();
    let approved = wait(&editor, |view| {
        view.modules.get(&owner) == Some(&State::Ready)
    });
    assert_eq!(approved.installed[0].grants, permissions);
    assert!(approved.review.is_none());
    let check_call = |session: &Session| {
        let result = call(session, &owner).unwrap();
        let input = result.output.service_value.unwrap();
        assert_eq!(input["document"]["text"], "unsaved fixture text");
        assert!(input["settings"].get("private-token").is_none());
        assert!(result.output.edits.is_empty());
        assert!(result.output.file_operations.is_empty());
        let observed = wait(session, |view| {
            view.logs.get(&owner.namespace).is_some_and(|logs| {
                logs.iter()
                    .any(|line| line.contains("Native tool access is not granted"))
            })
        });
        assert!(observed.review.is_none());
        assert_eq!(observed.installed[0].grants, permissions);
    };
    check_call(&editor);
    check_call(&editor);
    editor.projects(None, Vec::new());
    wait(&editor, |view| view.workers.is_empty());
    drop(editor);

    let reopened = session(&profile, false);
    reopened.projects(Some(workspace.id), vec![workspace.clone()]);
    wait(&reopened, |view| {
        view.modules.get(&owner) == Some(&State::Ready)
    });
    check_call(&reopened);
    assert!(!workspace.root.join("preview-only.txt").exists());
    assert!(!profile.join("operations").exists());
    assert_eq!(fs::read_to_string(&configuration).unwrap(), "{}\n");
    assert_eq!(fs::read(&path).unwrap(), archive);
}

#[test]
fn an_experimental_surface_requires_a_development_profile_and_its_own_review() {
    let fixture = tempfile::tempdir().unwrap();
    let path = fixture.path().join("experimental.zip");
    bundle(
        &path,
        "Experimental Fixture",
        &["read_document"],
        true,
        "",
        json!({"surface":{"title":"Fixture","lines":[],"input":""}}),
    );
    let archive = fs::read(&path).unwrap();
    let ordinary = session(&fixture.path().join("regular"), false);
    ordinary.send(Action::InspectZip(path.clone())).unwrap();
    let rejected = wait(&ordinary, |view| {
        view.message
            .contains("Experimental interface ui is unavailable")
    });
    assert!(rejected.review.is_none());
    assert!(rejected.installed.is_empty());
    assert!(rejected.workers.is_empty());

    let development = session(&fixture.path().join("development"), true);
    development.send(Action::InspectZip(path.clone())).unwrap();
    let review = wait(&development, |view| view.review.is_some());
    let review = review.review.as_ref().unwrap();
    assert_eq!(
        review.additional,
        BTreeSet::from([Permission::ReadDocument])
    );
    assert_eq!(review.manifest.experiments["ui"], UI_CONTRACT);
    let workspace = project(fixture.path(), 1);
    let owner = key(&review.manifest.identity(), workspace.id);
    development.projects(Some(workspace.id), vec![workspace]);
    development.send(Action::Approve(review.token)).unwrap();
    let loaded = wait(&development, |view| view.surfaces.contains_key(&owner));
    assert_eq!(
        loaded.installed[0].grants,
        BTreeSet::from([Permission::ReadDocument])
    );
    assert!(loaded.review.is_none());
    assert!(ordinary.view().installed.is_empty());
    assert_eq!(fs::read(path).unwrap(), archive);
}

#[test]
fn static_and_sdk_settings_share_apply_without_starting_a_disabled_plugin() {
    let fixture = tempfile::tempdir().unwrap();
    let path = fixture.path().join("settings.zip");
    write_bundle(
        &path,
        &format!(
            "name='Settings Surface'\nid='{ID}'\nversion='1.0.0'\n\
             [engines]\nnanobug='^0.1.0'\n[experiments]\nui={UI_CONTRACT:?}\n\
             [[modules]]\nid='main'\nwasm='main.wasm'\ninteractive=true\nsettings_ui=true\n\
             [settings.label]\ntitle='Label'\ndefault='fixture-default'\nproject=true\n"
        ),
        &guest(json!({
            "surface": {"title":"Settings Surface","lines":[],"input":""},
            "services": [{
                "token": 3,
                "operation": {
                    "service": "stage_settings",
                    "scope": "current_project",
                    "values": {"label": "chosen value"}
                }
            }]
        })),
    );
    let workspace = project(fixture.path(), 1);
    let file = workspace.root.join(".nb/plugins.json");
    let session = session(&fixture.path().join("development"), true);
    session.projects(Some(1), vec![workspace]);
    let identity = install(&session, &path);
    let owner = key(&identity, 1);
    let scope = ConfigScope::Project(1);
    let setting = |config: &Config| {
        config
            .plugin(&owner.namespace)
            .and_then(|plugin| plugin.settings.get("label"))
            .cloned()
    };
    let view = wait(&session, |view| view.surfaces.contains_key(&owner));
    assert!(view.configurations[&scope].draft.is_none());
    assert!(!file.exists());

    assert_eq!(label(&session, &owner), "fixture-default");
    let staged = wait(&session, |view| view.configurations[&scope].draft.is_some());
    let sdk_draft = staged.configurations[&scope].draft.clone().unwrap();
    assert_eq!(setting(&sdk_draft), Some(json!("chosen value")));
    assert_eq!(setting(&staged.configurations[&scope].applied), None);
    assert!(!file.exists());

    session
        .send(Action::SetEnabled(owner.namespace.clone(), false))
        .unwrap();
    wait(&session, |view| {
        view.modules.get(&owner) == Some(&State::Disabled) && view.workers.is_empty()
    });
    session
        .send(Action::StageConfig(scope, Config::default()))
        .unwrap();
    session
        .send(Action::EditConfig(
            scope,
            ConfigEdit::Setting {
                owner: identity,
                id: "label".into(),
                renamed_from: Vec::new(),
                original: None,
                value: Some(json!("chosen value")),
            },
        ))
        .unwrap();
    wait(&session, |view| {
        view.configurations[&scope].draft.as_ref() == Some(&sdk_draft)
    });
    session.send(Action::ApplyConfig(scope)).unwrap();
    let saved = wait(&session, |view| {
        view.configurations[&scope].applied == sdk_draft
    });
    assert!(saved.workers.is_empty());
    assert!(!saved.surfaces.contains_key(&owner));
    assert_eq!(
        serde_json::from_slice::<Config>(&fs::read(file).unwrap()).unwrap(),
        sdk_draft
    );
}

#[test]
fn rejected_settings_remain_saved_and_recover_with_explicit_apply() {
    let fixture = tempfile::tempdir().unwrap();
    let path = fixture.path().join("settings.zip");
    let rejected = r#"{"settings_error":"The fixture does not support this label"}"#;
    let needle = i64::from_le_bytes(*b"!reject!");
    let failed_output = (16_u64 << 32) | rejected.len() as u64;
    let event_offset = r#"{"event":{"event":""#.len();
    let settings_event = i64::from_le_bytes(*b"settings");
    let accepted_output = (512_u64 << 32) | 2;
    let wasm = wat::parse_str(format!(
        r#"(module
            (memory (export "memory") 4)
            (data (i32.const 16) "{}")
            (data (i32.const 512) "{{}}")
            (func (export "nb_alloc") (param i32) (result i32) i32.const 4096)
            (func (export "nb_run") (param $input i32) (param $length i32) (result i64)
                (local $offset i32)
                (if (i64.ne (i64.load offset={event_offset} (local.get $input))
                            (i64.const {settings_event}))
                    (then (return (i64.const {accepted_output}))))
                (block $done
                    (loop $scan
                        (br_if $done (i32.gt_u (i32.add (local.get $offset) (i32.const 8))
                                              (local.get $length)))
                        (if (i64.eq (i64.load (i32.add (local.get $input) (local.get $offset)))
                                    (i64.const {needle}))
                            (then (return (i64.const {failed_output}))))
                        (local.set $offset (i32.add (local.get $offset) (i32.const 1)))
                        (br $scan)))
            (i64.const {accepted_output}))
            (func (export "nb_free") (param i32 i32)))"#,
        rejected
            .bytes()
            .map(|byte| format!("\\{byte:02x}"))
            .collect::<String>()
    ))
    .unwrap();
    write_bundle(
        &path,
        &format!(
            "name='Settings Fixture'\nid='{ID}'\nversion='1.0.0'\n\
             [engines]\nnanobug='^0.1.0'\n[[modules]]\nid='main'\nwasm='main.wasm'\n\
             [settings.label]\ntitle='Label'\ndefault='fixture-default'\nproject=true\n"
        ),
        &wasm,
    );
    let profile = fixture.path().join("ordinary");
    let workspace = project(fixture.path(), 1);
    let another = project(fixture.path(), 2);
    let file = workspace.root.join(".nb/plugins.json");
    let first = session(&profile, false);
    first.projects(Some(1), vec![workspace.clone(), another]);
    let identity = install(&first, &path);
    let owner = key(&identity, 1);
    let other = key(&identity, 2);
    let scope = ConfigScope::Project(1);
    let ready = |view: &View| view.modules.get(&owner) == Some(&State::Ready);
    let ready_view = wait(&first, |view| {
        ready(view) && view.modules[&other] == State::Ready
    });
    let generation = ready_view.runtime_status[&owner.namespace].generation;
    let config = |value| {
        let mut config = Config::default();
        config
            .plugin_mut(identity.clone())
            .settings
            .insert("label".into(), value);
        config
    };
    let apply = |host: &Session, value: Config| {
        host.send(Action::StageConfig(scope, value)).unwrap();
        host.send(Action::ApplyConfig(scope)).unwrap();
    };
    let previous = config(json!("old value"));
    apply(&first, previous.clone());
    wait(&first, |view| {
        view.configurations[&scope].applied == previous
    });
    let invalid = config(json!(123));
    apply(&first, invalid.clone());
    let invalid_view = wait(&first, |view| {
        view.configurations[&scope].apply_issue.is_some()
    });
    assert_eq!(invalid_view.configurations[&scope].applied, previous);
    assert_eq!(invalid_view.configurations[&scope].draft, Some(invalid));

    let latest = config(json!("!reject!"));
    apply(&first, latest.clone());
    let cannot_adopt = |view: &View| view.settings_errors.contains_key(&owner);
    let failed = wait(&first, cannot_adopt);
    assert_eq!(failed.configurations[&scope].applied, latest);
    assert!(failed.configurations[&scope].draft.is_none());
    assert_eq!(failed.modules[&other], State::Ready);
    assert!(call(&first, &owner).unwrap_err().contains("saved settings"));
    assert!(call(&first, &other).is_ok());
    assert_eq!(
        serde_json::from_slice::<Config>(&fs::read(&file).unwrap()).unwrap(),
        latest
    );
    let before_retry = first.view().runtime_status[&owner.namespace].completed;
    first.send(Action::RetrySettings(owner.clone())).unwrap();
    let retry = wait(&first, |view| {
        view.runtime_status[&owner.namespace].completed > before_retry
    });
    assert_eq!(
        retry.runtime_status[&owner.namespace].generation,
        generation
    );
    assert!(cannot_adopt(&retry));
    drop(first);

    let reopened = session(&profile, false);
    reopened.projects(Some(1), vec![workspace]);
    let failed = wait(&reopened, cannot_adopt);
    assert_eq!(failed.configurations[&scope].applied, latest);
    assert_eq!(
        failed.settings_errors[&owner],
        "Module previously rejected these saved settings"
    );
    reopened
        .send(Action::EditConfig(
            scope,
            ConfigEdit::Restore {
                owner: identity,
                rules: false,
                revision: failed.configurations[&scope].revision,
            },
        ))
        .unwrap();
    let restoring = wait(&reopened, |view| {
        view.configurations[&scope].draft.as_ref() == Some(&previous)
    });
    assert_eq!(restoring.configurations[&scope].applied, latest);
    assert!(cannot_adopt(&restoring));
    reopened.send(Action::ApplyConfig(scope)).unwrap();
    let recovered = wait(&reopened, |view| {
        ready(view) && view.configurations[&scope].applied == previous
    });
    assert!(!cannot_adopt(&recovered));
    assert!(call(&reopened, &owner).is_ok());
}

#[test]
fn upgrading_settings_keeps_closed_projects_and_inherited_defaults_untouched() {
    let fixture = tempfile::tempdir().unwrap();
    let first = fixture.path().join("settings-1.zip");
    let next = fixture.path().join("settings-2.zip");
    let manifest = |version, settings| {
        format!(
            "name='Upgrade Settings'\nid='{ID}'\nversion='{version}'\n\
             [engines]\nnanobug='^0.1.0'\n[[modules]]\nid='main'\nwasm='main.wasm'\n{settings}"
        )
    };
    let guest = guest(json!({}));
    write_bundle(
        &first,
        &manifest(
            "1.0.0",
            r#"
[settings.old-label]
title='Old label'
default='first default'
project=true
[settings.count]
title='Count'
default='old count'
project=true
[settings.inherited]
title='Inherited'
default='before'
project=true
"#,
        ),
        &guest,
    );
    write_bundle(
        &next,
        &manifest(
            "1.1.0",
            r#"
[settings.label]
title='Label'
default='next default'
project=true
renamed_from=['old-label']
[settings.count]
title='Count'
default=3
project=true
[settings.inherited]
title='Inherited'
default='after'
project=true
"#,
        ),
        &guest,
    );
    let session = session(&fixture.path().join("profile"), false);
    let identity = install(&session, &first);
    let open = project(fixture.path(), 1);
    let closed = project(fixture.path(), 2);
    let write_legacy = |project: &Project, value| {
        let mut config = Config::default();
        let settings = &mut config.plugin_mut(identity.clone()).settings;
        settings.insert("old-label".into(), value);
        settings.insert("count".into(), json!("former setting"));
        settings.insert("removed".into(), json!(false));
        let path = project.root.join(".nb/plugins.json");
        fs::create_dir(path.parent().unwrap()).unwrap();
        let bytes = serde_json::to_vec_pretty(&config).unwrap();
        fs::write(&path, &bytes).unwrap();
        (path, bytes)
    };
    let (open_file, original) = write_legacy(&open, json!("open value"));
    let (closed_file, unopened) = write_legacy(&closed, json!("closed value"));
    let owner = key(&identity, 1);
    session.projects(Some(1), vec![open]);
    wait(&session, |view| {
        view.modules.get(&owner) == Some(&State::Ready)
    });
    let settings = || {
        call(&session, &owner)
            .unwrap()
            .output
            .service_value
            .unwrap()["settings"]
            .clone()
    };
    assert_eq!(settings()["old-label"], "open value");
    assert_eq!(settings()["inherited"], "before");
    install(&session, &next);
    wait(&session, |view| {
        view.installed[0].release.manifest.version.to_string() == "1.1.0"
            && view.modules.get(&owner) == Some(&State::Ready)
            && !view.installed[0].pending_takeover
    });
    let observed = settings();
    assert_eq!(observed["label"], "open value");
    assert_eq!(observed["count"], 3);
    assert_eq!(observed["inherited"], "after");
    assert_eq!(fs::read(&open_file).unwrap(), original);
    assert_eq!(fs::read(&closed_file).unwrap(), unopened);

    let scope = ConfigScope::Project(1);
    session
        .send(Action::EditConfig(
            scope,
            ConfigEdit::Setting {
                owner: identity,
                id: "label".into(),
                renamed_from: vec!["old-label".into()],
                original: Some(json!("open value")),
                value: Some(json!("open value")),
            },
        ))
        .unwrap();
    wait(&session, |view| view.configurations[&scope].draft.is_some());
    assert_eq!(fs::read(&open_file).unwrap(), original);
    session.send(Action::ApplyConfig(scope)).unwrap();
    let applied = wait(&session, |view| view.configurations[&scope].draft.is_none());
    let saved = &applied.configurations[&scope]
        .applied
        .plugin(&owner.namespace)
        .unwrap()
        .settings;
    assert_eq!(saved["label"], "open value");
    assert_eq!(saved["old-label"], "open value");
    assert_eq!(saved["count"], "former setting");
    assert_eq!(saved["removed"], false);
    assert!(!saved.contains_key("inherited"));
    assert_eq!(fs::read(&closed_file).unwrap(), unopened);
}

#[test]
fn a_new_default_preserves_earlier_shortcuts_and_remaps_stay_local() {
    let fixture = tempfile::tempdir().unwrap();
    let first = fixture.path().join("first.zip");
    let later = fixture.path().join("later.zip");
    let update = fixture.path().join("update.zip");
    let command = r#"
[[commands]]
id = 'choose'
title = 'Choose'
module = 'main'
key = 'F10'
contexts = ['editor_normal']
"#;
    bundle(&first, "First Shortcut", &[], false, "", json!({}));
    bundle(&later, "Other Shortcut", &[], false, command, json!({}));
    write_bundle(
        &update,
        &format!(
            "name='First Shortcut'\nid='{ID}'\nversion='1.1.0'\n\
             [engines]\nnanobug='^0.1.0'\n[[modules]]\nid='main'\nwasm='main.wasm'\n{command}"
        ),
        &guest(json!({})),
    );
    let profile = fixture.path().join("profile");
    let host = session(&profile, false);
    let early = install(&host, &first).namespace();
    let claimed = install(&host, &later).namespace();
    let priority = |view: &View, owner: &str| {
        view.installed
            .iter()
            .find(|record| record.release.manifest.identity().namespace() == owner)
            .and_then(|record| record.shortcut_order.get("choose"))
            .copied()
    };
    let original_order = priority(&host.view(), &claimed).unwrap();
    install(&host, &update);
    let updated = wait(&host, |view| priority(view, &early).is_some());
    assert!(priority(&updated, &early).unwrap() > original_order);
    assert_eq!(priority(&updated, &claimed), Some(original_order));
    let a = project(fixture.path(), 1);
    let b = project(fixture.path(), 2);
    let paths = [
        a.root.join(".nb/plugins.json"),
        b.root.join(".nb/plugins.json"),
    ];
    host.projects(Some(1), vec![a, b]);
    let edit = || ConfigEdit::Shortcut {
        namespace: claimed.clone(),
        command: "choose".into(),
        value: Some("F11".into()),
    };
    host.send(Action::EditConfig(ConfigScope::Project(1), edit()))
        .unwrap();
    wait(&host, |view| view.message == "Shortcut settings are global");
    host.send(Action::EditConfig(ConfigScope::AllProjects, edit()))
        .unwrap();
    host.send(Action::ApplyConfig(ConfigScope::AllProjects))
        .unwrap();
    wait(&host, |view| {
        view.configurations[&ConfigScope::AllProjects]
            .applied
            .shortcuts
            .get(&claimed)
            .is_some_and(|mappings| mappings["choose"] == "F11")
    });
    assert!(paths.iter().all(|file| !file.exists()));
    drop(host);
    let reopened = session(&profile, false);
    let reopened = wait(&reopened, |view| priority(view, &claimed).is_some());
    assert_eq!(priority(&reopened, &claimed), Some(original_order));
    assert_eq!(
        reopened.configurations[&ConfigScope::AllProjects]
            .applied
            .shortcuts[&claimed]["choose"],
        "F11"
    );
    let developer = session(&fixture.path().join("development"), true);
    let developer = wait(&developer, |view| {
        view.configurations.contains_key(&ConfigScope::AllProjects)
    });
    assert!(
        developer.configurations[&ConfigScope::AllProjects]
            .applied
            .shortcuts
            .is_empty()
    );
}
