//! Exercise confirmed input, rejected complete scenes and resource lifetime through the worker.

use super::{install, key, project, session, stop_runtime, wait};
use nanobug::plugins::{
    drawing::Asset,
    package::UI_CONTRACT,
    session::{Action, CallResult, Key, Session, View},
};
use nanobug_plugin_sdk::{DocumentSnapshot, PluginEvent};
use serde_json::{Value, json};
use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use zip::{ZipWriter, write::SimpleFileOptions};

const TINY_PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 2, 0, 0, 0, 2, 8, 6, 0,
    0, 0, 114, 182, 13, 36, 0, 0, 0, 21, 73, 68, 65, 84, 120, 156, 99, 188, 211, 16, 250, 159, 129,
    129, 129, 129, 9, 68, 128, 48, 0, 41, 191, 2, 180, 233, 1, 215, 194, 0, 0, 0, 0, 73, 69, 78,
    68, 174, 66, 96, 130,
];
const DRAFT_CRASH_DIR: &str = "NANOBUG_TEST_CANVAS_DRAFT_CRASH_DIR";

#[test]
#[ignore = "requires the release wasm32-unknown-unknown canvas-delta SDK example"]
fn canvas_delta_resynchronizes_after_gaps_relayout_and_restart() -> anyhow::Result<()> {
    use nanobug_plugin_sdk::canvas::{Draw, Fill, Sync, Viewport};

    let temp = tempfile::tempdir()?;
    let root = temp.path();
    let wasm = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/wasm32-unknown-unknown/release/examples/canvas_delta.wasm");
    let archive = root.join("incremental.zip");
    let manifest = format!(
        "name='Canvas delta'\nid='canvas-delta'\nversion='1.0.0'\n[engines]\nnanobug='*'\n\
         [experiments]\nui={UI_CONTRACT:?}\n[[modules]]\nid='workspace'\nwasm='main.wasm'\ninteractive=true\n"
    );
    let mut zip = ZipWriter::new(File::create(&archive)?);
    for (name, data) in [
        ("nanobug-plugin.toml", manifest.into_bytes()),
        ("main.wasm", fs::read(wasm)?),
    ] {
        zip.start_file(name, SimpleFileOptions::default())?;
        zip.write_all(&data)?;
    }
    zip.finish()?;

    let editor = session(&root.join("profile"), true);
    let namespace = install(&editor, archive, "1.0.0");
    let project = project(root, 1);
    let owner = key(&namespace, project.id);
    editor.projects(Some(project.id), vec![project]);
    wait(&editor, |view| view.surfaces.contains_key(&owner));
    let request = |action: &str, argument| invoke_argument(&editor, &owner, action, argument);
    let sync = || -> anyhow::Result<Sync> {
        Ok(serde_json::from_str(
            &request("inspect", Value::Null)
                .map_err(anyhow::Error::msg)?
                .output
                .message,
        )?)
    };
    let pixel = |view: &View, sequence, color| {
        view.surfaces.get(&owner).is_some_and(|surface| {
        surface.frame.canvas.as_ref().is_some_and(|canvas| canvas.sequence == Some(sequence)
            && matches!(&canvas.draw[0], Draw::Quad { fill: Fill::Solid { color: found }, .. } if *found == color))
    })
    };
    let resize = |id| {
        editor.send(Action::Resize(
            owner.clone(),
            Viewport {
                id,
                width: 320.,
                height: 160.,
                scale: 1.5,
                apple_surface: false,
                shaped_line: true,
                appearance: None,
            },
        ))
    };
    resize(7)?;
    let view = wait(&editor, |view| {
        view.surfaces.get(&owner).is_some_and(|surface| {
            surface
                .frame
                .canvas
                .as_ref()
                .and_then(|canvas| canvas.viewport)
                == Some(7)
        })
    });
    let worker = view.workers[&namespace];
    let base = sync()?;
    assert_eq!((base.viewport, base.sequence), (Some(7), Some(1)));
    let confirmed = "增量期间的输入 👩‍💻";
    editor.draft(owner.clone(), confirmed.into());
    wait(&editor, |view| view.surfaces[&owner].draft == confirmed);

    request("delta", Value::Null).map_err(anyhow::Error::msg)?;
    let updated = wait(&editor, |view| pixel(view, 2, 0x799ccdff));
    let revision = updated.surfaces[&owner].revision;
    let second = sync()?;
    assert_eq!((second.sequence, second.epoch), (Some(2), base.epoch));
    request("delta", json!({"base": 1})).map_err(anyhow::Error::msg)?;
    assert_eq!(sync()?, second);
    assert_eq!(editor.view().surfaces[&owner].revision, revision);

    request("gap", Value::Null).map_err(anyhow::Error::msg)?;
    let recovered = wait(&editor, |view| pixel(view, 1, 0x7e5d98ff));
    let revision = recovered.surfaces[&owner].revision;
    let after_gap = sync()?;
    assert_ne!(after_gap.epoch, base.epoch);
    assert_eq!(recovered.surfaces[&owner].draft, confirmed);
    request("delta", json!({"base": 1, "epoch": base.epoch})).map_err(anyhow::Error::msg)?;
    assert_eq!(sync()?, after_gap);
    assert_eq!(editor.view().surfaces[&owner].revision, revision);

    request("bad_asset", Value::Null).map_err(anyhow::Error::msg)?;
    wait(&editor, |view| {
        view.surfaces[&owner].revision > revision && pixel(view, 1, 0x7e5d98ff)
    });
    let after_asset = sync()?;
    assert_ne!(after_asset.epoch, after_gap.epoch);

    resize(8)?;
    let resized = wait(&editor, |view| {
        view.surfaces.get(&owner).is_some_and(|surface| {
            surface
                .frame
                .canvas
                .as_ref()
                .and_then(|canvas| canvas.viewport)
                == Some(8)
        })
    });
    let new_layout = sync()?;
    assert_ne!(new_layout.epoch, after_asset.epoch);
    let revision = resized.surfaces[&owner].revision;
    request("delta", json!({"viewport": 7, "epoch": after_asset.epoch}))
        .map_err(anyhow::Error::msg)?;
    assert_eq!(sync()?, new_layout);
    assert_eq!(editor.view().surfaces[&owner].revision, revision);

    stop_runtime(worker);
    let reopened = wait(&editor, |view| {
        view.surfaces.get(&owner).is_some_and(|surface| {
            surface.generation != base.generation && surface.draft == confirmed
        })
    });
    let new_worker = sync()?;
    assert_ne!(new_worker.generation, base.generation);
    let revision = reopened.surfaces[&owner].revision;
    request(
        "delta",
        json!({"generation": base.generation, "epoch": new_layout.epoch}),
    )
    .map_err(anyhow::Error::msg)?;
    assert_eq!(sync()?, new_worker);
    assert_eq!(editor.view().surfaces[&owner].revision, revision);
    request("delta", Value::Null).map_err(anyhow::Error::msg)?;
    let final_view = wait(&editor, |view| pixel(view, 2, 0x799ccdff));
    assert_eq!(final_view.surfaces[&owner].draft, confirmed);
    Ok(())
}

#[test]
fn confirmed_draft_recovers_within_a_second_after_abrupt_host_exit() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path();
    let mut child = Command::new(std::env::current_exe()?)
        .args(["--exact", "canvas_cases::draft_crash_writer", "--ignored"])
        .env(DRAFT_CRASH_DIR, root)
        .stdout(Stdio::null())
        .spawn()?;
    let started = Instant::now();
    let ledger = root.join("confirmations.txt");
    let enough_input = loop {
        if confirmed_drafts(&ledger)
            .last()
            .is_some_and(|(revision, _)| *revision >= 23)
        {
            break true;
        }
        if started.elapsed() >= Duration::from_secs(15) || child.try_wait()?.is_some() {
            break false;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let terminated = unix_millis()?;
    child.kill()?;
    anyhow::ensure!(
        !child.wait()?.success() && enough_input,
        "the isolated editor must exit during input"
    );
    let confirmed = confirmed_drafts(&ledger);
    let profile = root.join("profile");
    let persisted = fs::read_to_string(profile.join("drafts.json"))?;
    let revision: u32 = persisted
        .split_once("crash-draft-")
        .and_then(|(_, value)| value.split('"').next())
        .ok_or_else(|| anyhow::anyhow!("no confirmed draft was persisted"))?
        .parse()?;
    let persisted_at = confirmed
        .iter()
        .find(|(candidate, _)| *candidate == revision)
        .ok_or_else(|| anyhow::anyhow!("the recovered draft had no confirmation"))?
        .1;
    let latest = confirmed.last().expect("confirmed input").0;
    let loss = terminated.saturating_sub(persisted_at);
    anyhow::ensure!(
        loss <= 1_000,
        "draft revision {revision} was {loss} ms old when the host exited"
    );

    let reopened = session(&profile, true);
    let project = project(root, 1);
    reopened.projects(Some(project.id), vec![project]);
    let view = wait(&reopened, |view| view.surfaces.len() == 1);
    assert_eq!(
        view.surfaces.values().next().unwrap().draft,
        format!("crash-draft-{revision}")
    );
    println!(
        "recovered draft revision {revision} of {latest}, confirmed {loss} ms before the forced exit"
    );
    Ok(())
}

#[test]
#[ignore = "child invoked by the isolated draft crash test"]
fn draft_crash_writer() -> anyhow::Result<()> {
    let Some(root) = std::env::var_os(DRAFT_CRASH_DIR).map(PathBuf::from) else {
        return Ok(());
    };
    let archive = root.join("draft-crash.zip");
    canvas_package(&archive, false)?;
    let editor = session(&root.join("profile"), true);
    let namespace = install(&editor, archive, "1.0.0");
    let project = project(&root, 1);
    let key = key(&namespace, project.id);
    editor.projects(Some(project.id), vec![project]);
    wait(&editor, |view| view.surfaces.contains_key(&key));
    let mut ledger = File::create(root.join("confirmations.txt"))?;
    for revision in 0..96 {
        let draft = format!("crash-draft-{revision}");
        editor.draft(key.clone(), draft.clone());
        wait(&editor, |view| {
            view.surfaces
                .get(&key)
                .is_some_and(|surface| surface.draft == draft)
        });
        writeln!(ledger, "{revision} {}", unix_millis()?)?;
        ledger.flush()?;
        std::thread::sleep(Duration::from_millis(55));
    }
    Ok(())
}

#[test]
fn draft_write_failure_retains_input_and_can_be_retried() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let archive = temp.path().join("draft-write.zip");
    canvas_package(&archive, false)?;
    let profile = temp.path().join("profile");
    let editor = session(&profile, true);
    let namespace = install(&editor, archive, "1.0.0");
    let project = project(temp.path(), 1);
    let key = key(&namespace, project.id);
    editor.projects(Some(project.id), vec![project]);
    wait(&editor, |view| view.surfaces.contains_key(&key));
    editor.draft(key.clone(), "previous draft".into());
    wait(&editor, |view| {
        view.surfaces[&key].draft == "previous draft"
    });
    flush(&editor, &key).map_err(anyhow::Error::msg)?;
    let file = profile.join("drafts.json");
    let previous = profile.join("previous-drafts.json");
    fs::rename(&file, &previous)?;
    fs::create_dir(&file)?;

    editor.draft(key.clone(), "写失败仍保留 👩‍💻".into());
    wait(&editor, |view| {
        view.surfaces[&key].draft == "写失败仍保留 👩‍💻"
    });
    assert!(flush(&editor, &key).is_err());
    let pending = wait(&editor, |view| {
        view.draft_pending && view.message.contains("Could not save plugin input")
    });
    assert_eq!(pending.surfaces[&key].draft, "写失败仍保留 👩‍💻");
    assert!(fs::read_to_string(&previous)?.contains("previous draft"));

    fs::remove_dir(&file)?;
    fs::rename(previous, &file)?;
    flush(&editor, &key).map_err(anyhow::Error::msg)?;
    wait(&editor, |view| !view.draft_pending);
    assert!(fs::read_to_string(file)?.contains("写失败仍保留 👩‍💻"));
    Ok(())
}

fn confirmed_drafts(path: &Path) -> Vec<(u32, u128)> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let (revision, timestamp) = line.split_once(' ')?;
            Some((revision.parse().ok()?, timestamp.parse().ok()?))
        })
        .collect()
}

fn unix_millis() -> anyhow::Result<u128> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())
}

fn flush(editor: &Session, key: &Key) -> Result<(), String> {
    let (reply, result) = async_channel::bounded(1);
    editor
        .send(Action::FlushDrafts {
            project: key.instance.project,
            reply,
        })
        .unwrap();
    result.recv_blocking().unwrap()
}

#[test]
fn canvas_faults_keep_confirmed_input_and_drop_old_actions() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let archive = temp.path().join("canvas-faults.zip");
    canvas_package(&archive, false)?;
    let editor = session(&temp.path().join("profile"), true);
    let namespace = install(&editor, archive, "1.0.0");
    let mut project = project(temp.path(), 1);
    let file = project.root.join("README.md");
    fs::write(&file, "previously saved")?;
    project.document = Some(Arc::new(DocumentSnapshot {
        path: Some(file.clone()),
        opening: 41,
        revision: 7,
        text: "body0001 unsaved editor buffer".into(),
    }));
    let key = key(&namespace, project.id);
    editor.projects(Some(project.id), vec![project]);
    wait(&editor, |view| view.surfaces.contains_key(&key));
    let draft = "draft001 已确认👩‍💻";
    editor.draft(key.clone(), draft.into());
    wait(&editor, |view| {
        view.surfaces
            .get(&key)
            .is_some_and(|surface| surface.draft == draft)
    });

    assert_eq!(
        invoke(&editor, &key, "probe001").unwrap().output.message,
        "confirmed input intact"
    );
    for action in ["stall001", "trap0001"] {
        assert!(
            invoke(&editor, &key, action).is_err(),
            "{action} must fail inside the worker"
        );
        let view = wait(&editor, |view| {
            view.surfaces
                .get(&key)
                .is_some_and(|surface| surface.draft == draft)
        });
        assert_eq!(view.surfaces[&key].frame.title, "Stable Canvas");
    }

    let view = editor.view();
    let previous_revision = view.surfaces[&key].revision;
    let previous_generation = view.surfaces[&key].generation;
    stop_runtime(view.workers[&namespace]);
    drop(view);
    wait(&editor, |view| {
        view.surfaces.get(&key).is_some_and(|surface| {
            surface.generation != previous_generation && surface.draft == draft
        })
    });
    let settled = wait(&editor, |view| view.runtime_status[&namespace].pending == 0);
    let calls = settled.runtime_status[&namespace].completed;
    editor.send(Action::SurfaceAction {
        key: key.clone(),
        revision: previous_revision,
        action: "echo0001".into(),
        argument: Value::Null,
    })?;
    assert_eq!(
        invoke(&editor, &key, "probe001").unwrap().output.message,
        "confirmed input intact"
    );
    let observed = wait(&editor, |view| {
        let status = view.runtime_status[&namespace];
        status.pending == 0 && status.completed > calls
    });
    assert_eq!(observed.runtime_status[&namespace].completed, calls + 1);
    assert_eq!(observed.surfaces[&key].draft, draft);
    assert_eq!(fs::read_to_string(file)?, "previously saved");
    Ok(())
}

#[test]
fn invalid_complete_canvas_frames_keep_the_last_valid_scene_and_recover() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let archive = temp.path().join("canvas-budget.zip");
    canvas_package(&archive, true)?;
    let editor = session(&temp.path().join("profile"), true);
    let namespace = install(&editor, archive, "1.0.0");
    let project = project(temp.path(), 1);
    let key = key(&namespace, project.id);
    editor.projects(Some(project.id), vec![project]);
    let opened = wait(&editor, |view| view.surfaces.contains_key(&key));
    let revision = opened.surfaces[&key].revision;
    drop(opened);

    for action in ["bigframe", "bigimage"] {
        let rejects = rejected_frames(&editor.view(), &namespace);
        invoke(&editor, &key, action).unwrap();
        let view = wait(&editor, |view| rejected_frames(view, &namespace) > rejects);
        let retained = &view.surfaces[&key];
        assert_eq!(retained.revision, revision);
        assert_eq!(retained.frame.title, "Stable Canvas");
        assert!(retained.drawing.is_some());
    }
    invoke(&editor, &key, "good0001").unwrap();
    let recovered = wait(&editor, |view| {
        view.surfaces
            .get(&key)
            .is_some_and(|surface| surface.frame.title == "Recovered Canvas")
    });
    assert!(recovered.surfaces[&key].revision > revision);
    assert!(matches!(
        recovered.surfaces[&key].drawing.as_ref().unwrap().assets["small.png"],
        Asset::Image(_)
    ));
    Ok(())
}

#[test]
fn repeated_project_closes_release_only_their_prepared_canvas_assets() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let archive = temp.path().join("canvas-assets.zip");
    canvas_package(&archive, false)?;
    let editor = session(&temp.path().join("profile"), true);
    let namespace = install(&editor, archive, "1.0.0");
    for turn in 0..4 {
        let first = project(temp.path(), turn * 2 + 1);
        let second = project(temp.path(), turn * 2 + 2);
        let first_key = key(&namespace, first.id);
        let second_key = key(&namespace, second.id);
        editor.projects(Some(first.id), vec![first, second.clone()]);
        wait(&editor, |view| {
            view.surfaces.contains_key(&first_key) && view.surfaces.contains_key(&second_key)
        });
        for owner in [&first_key, &second_key] {
            invoke(&editor, owner, "good0001").unwrap();
        }
        let view = wait(&editor, |view| {
            [&first_key, &second_key].into_iter().all(|owner| {
                view.surfaces
                    .get(owner)
                    .is_some_and(|surface| surface.frame.title == "Recovered Canvas")
            })
        });
        let worker = view.workers[&namespace];
        let [first_image, second_image] = [&first_key, &second_key].map(|owner| {
            let drawing = view.surfaces[owner].drawing.as_ref().unwrap();
            assert!(drawing.resource_bytes >= 16);
            let Asset::Image(image) = &drawing.assets["small.png"] else {
                panic!("the runtime must publish a prepared PNG");
            };
            Arc::downgrade(image)
        });
        drop(view);

        editor.projects(Some(second.id), vec![second]);
        let remaining = wait(&editor, |view| {
            !view.surfaces.contains_key(&first_key)
                && view.surfaces.contains_key(&second_key)
                && first_image.upgrade().is_none()
        });
        assert_eq!(remaining.workers[&namespace], worker);
        assert!(second_image.upgrade().is_some());
        drop(remaining);

        editor.projects(None, Vec::new());
        wait(&editor, |view| {
            view.surfaces.is_empty() && view.workers.is_empty() && second_image.upgrade().is_none()
        });
    }
    Ok(())
}

fn rejected_frames(view: &View, namespace: &str) -> usize {
    view.logs
        .get(namespace)
        .into_iter()
        .flatten()
        .filter(|line| line.contains("Canvas frame rejected:"))
        .count()
}

fn invoke(editor: &Session, key: &Key, action: &str) -> Result<CallResult, String> {
    invoke_argument(editor, key, action, Value::Null)
}

fn invoke_argument(
    editor: &Session,
    key: &Key,
    action: &str,
    argument: Value,
) -> Result<CallResult, String> {
    let (reply, answer) = async_channel::bounded(1);
    editor
        .send(Action::Invoke {
            key: key.clone(),
            event: PluginEvent::Ui {
                action: action.into(),
                argument,
            },
            reply: Some(reply),
            timeout: Duration::from_secs(4),
        })
        .unwrap();
    answer.recv_blocking().unwrap()
}

fn canvas_package(path: &Path, include_big_image: bool) -> anyhow::Result<()> {
    let quad =
        json!({"op":"quad", "bounds":[4,4,40,26], "fill":{"fill":"solid","color":2624064767_u32}});
    let frame = |title, draw| {
        json!({"surface":{
            "title": title, "input": "",
            "canvas": {"input": [2, 36, 120, 24], "draw": draw,
                "hits":[{"bounds":[2,2,40,20], "action":"echo0001"}]}
        }})
    };
    let image = |asset| json!({"op":"image", "bounds":[4,4,40,26], "asset":asset});
    let replies = [
        frame("Stable Canvas", vec![quad.clone()]),
        json!({"message":"confirmed input intact"}),
        json!({"message":"confirmed input missing"}),
        json!({"message":"unexpected stale Canvas action"}),
        frame("Over draw budget", vec![quad; 513]),
        frame("Over asset budget", vec![image("big.png")]),
        frame("Recovered Canvas", vec![image("small.png")]),
    ];
    let mut data = String::new();
    let mut offset = 16_u64;
    let mut pointers = Vec::new();
    for reply in replies {
        let encoded = serde_json::to_vec(&reply)?;
        let escaped = encoded
            .iter()
            .map(|byte| format!("\\{byte:02x}"))
            .collect::<String>();
        data.push_str(&format!(r#"(data (i32.const {offset}) "{escaped}")"#));
        pointers.push((offset << 32) | encoded.len() as u64);
        offset += encoded.len() as u64;
    }
    let contains = |token: &[u8; 8]| {
        format!(
            "(call $contains (local.get $input) (local.get $length) (i64.const {}))",
            i64::from_le_bytes(*token)
        )
    };
    let jump = |token, index: usize| {
        format!(
            "(if {} (then (return (i64.const {}))))",
            contains(token),
            pointers[index]
        )
    };
    let wasm = wat::parse_str(format!(
        r#"(module
        (memory (export "memory") 20)
        {data}
        (func $contains (param $input i32) (param $length i32) (param $token i64) (result i32)
            (local $i i32)
            (block $end
                (loop $next
                    (br_if $end (i32.gt_u (i32.add (local.get $i) (i32.const 8)) (local.get $length)))
                    (if (i64.eq (i64.load (i32.add (local.get $input) (local.get $i))) (local.get $token))
                        (then (return (i32.const 1))))
                    (local.set $i (i32.add (local.get $i) (i32.const 1)))
                    (br $next)))
            i32.const 0)
        (func (export "nb_alloc") (param i32) (result i32) i32.const 500000)
        (func (export "nb_run") (param $input i32) (param $length i32) (result i64)
            (if {} (then (loop $busy (br $busy))))
            (if {} (then (unreachable)))
            (if {}
                (then (return (if (result i64) (i32.and {} {})
                    (then (i64.const {})) (else (i64.const {}))))))
            {} {} {} {}
            i64.const {})
        (func (export "nb_free") (param i32 i32)))"#,
        contains(b"stall001"),
        contains(b"trap0001"),
        contains(b"probe001"),
        contains(b"body0001"),
        contains(b"draft001"),
        pointers[1],
        pointers[2],
        jump(b"echo0001", 3),
        jump(b"bigframe", 4),
        jump(b"bigimage", 5),
        jump(b"good0001", 6),
        pointers[0],
    ))?;
    let manifest = format!(
        "name='Canvas cases'\nid='canvas-cases'\nversion='1.0.0'\npermissions=['read_document']\n\
         [engines]\nnanobug='*'\n[experiments]\nui={UI_CONTRACT:?}\n\
         [[modules]]\nid='workspace'\nwasm='main.wasm'\ninteractive=true\n"
    );
    let mut zip = ZipWriter::new(File::create(path)?);
    for (name, bytes) in [
        ("nanobug-plugin.toml", manifest.as_bytes()),
        ("main.wasm", &wasm),
        ("small.png", TINY_PNG),
    ] {
        zip.start_file(name, SimpleFileOptions::default())?;
        zip.write_all(bytes)?;
    }
    if include_big_image {
        zip.start_file("big.png", SimpleFileOptions::default())?;
        zip.write_all(&vec![0; 4 * 1024 * 1024 + 1])?;
    }
    zip.finish()?;
    Ok(())
}
