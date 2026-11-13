//! Headless Windows acceptance for editor document updates under real plugin load.

use super::*;
use nanobug::document::Document;
use nanobug_plugin_sdk::canvas::Viewport;
use serde_json::json;
use std::{
    collections::BTreeSet,
    os::windows::{fs::OpenOptionsExt, process::CommandExt},
    process::Command,
};

const PATHS: usize = 72;
const EDITS: usize = 144;
const INTERRUPTED_PROFILE: &str = "NANOBUG_TEST_INTERRUPTED_PROFILE";

#[test]
#[ignore = "requires NANOBUG_TEST_SMB_ROOT pointing to a writable isolated UNC test directory"]
fn rule_queries_use_real_smb_paths_with_sharing_recovery() -> anyhow::Result<()> {
    let root = std::env::var_os("NANOBUG_TEST_SMB_ROOT")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("Set NANOBUG_TEST_SMB_ROOT to an isolated UNC directory"))?;
    anyhow::ensure!(
        matches!(root.components().next(), Some(std::path::Component::Prefix(prefix))
        if matches!(prefix.kind(), std::path::Prefix::UNC(..) | std::path::Prefix::VerbatimUNC(..))),
        "The fixture must use a UNC path"
    );
    let share = tempfile::Builder::new()
        .prefix("nanobug-rule-")
        .tempdir_in(&root)?;
    let remote = share.path().canonicalize()?;
    let file = remote.join("fixture.txt");
    fs::write(&file, "approved UNC 中文")?;
    fs::write(remote.join("mode"), "remote")?;
    let held = fs::OpenOptions::new()
        .write(true)
        .share_mode(0)
        .open(&file)?;
    let local = tempfile::tempdir()?;
    let archive = local.path().join("smb-rule.zip");
    let rule = r#"
        const mode = await host.readFile('mode');
        if (mode?.text === 'local') return true;
        const entries = await host.list('.');
        if (!entries?.complete || !entries.entries.includes('fixture.txt')) return false;
        const base = host.context.project;
        const path = `${base}\\fixture.txt`;
        const data = await host.readFile(path);
        return data?.text === 'approved UNC 中文';
    "#;
    bundle(&archive, "^0.1.0", rule, false, "1.0.0", false);
    let editor = session(&local.path().join("profile"), false);
    let namespace = install(&editor, archive, "1.0.0");
    let local_project = project(local.path(), 2);
    fs::write(local_project.root.join("mode"), "local")?;
    let remote_project = Project {
        id: 1,
        root: remote,
        document: None,
        open_documents: Vec::new(),
    };
    let remote_key = key(&namespace, 1);
    let local_key = key(&namespace, 2);
    let started = Instant::now();
    editor.projects(Some(1), vec![remote_project, local_project]);
    let first = wait(&editor, |view| {
        view.modules.get(&local_key) == Some(&State::Ready)
    });
    assert_ne!(first.modules.get(&remote_key), Some(&State::Ready));
    let rejected = wait(&editor, |view| {
        matches!(view.modules.get(&remote_key), Some(State::Failed(_)))
    });
    let rejection = rejected.modules[&remote_key].label();
    assert!(
        rejection.contains("(os error 32)"),
        "unexpected UNC failure {rejection}"
    );
    let elapsed = started.elapsed();
    assert!(elapsed >= Duration::from_millis(400) && elapsed < Duration::from_secs(5));
    let local_pid = rejected.workers[&namespace];
    drop(held);
    let recovered = wait(&editor, |view| {
        view.modules.get(&remote_key) == Some(&State::Ready)
    });
    assert_eq!(recovered.workers[&namespace], local_pid);
    assert_eq!(recovered.modules.get(&local_key), Some(&State::Ready));
    eprintln!(
        "P2-07 SMB file conflict reported after {elapsed:?}; granted UNC directory and absolute path recovered after {:?}",
        started.elapsed()
    );
    Ok(())
}

pub(super) fn interrupt_startup_twice(profile: &Path) {
    for attempts in [1, 2] {
        let status = Command::new(std::env::current_exe().unwrap())
            .creation_flags(0x0800_0000)
            .args([
                "--exact",
                "acceptance::interrupt_startup_child",
                "--ignored",
            ])
            .env(INTERRUPTED_PROFILE, profile)
            .output()
            .unwrap();
        assert_eq!(
            status.status.code(),
            Some(13),
            "{}",
            String::from_utf8_lossy(&status.stdout)
        );
        let record: serde_json::Value =
            serde_json::from_slice(&fs::read(profile.join("startup.json")).unwrap()).unwrap();
        assert_eq!(record, json!({"attempts":attempts, "running":true}));
    }
}

#[test]
#[ignore = "child invoked by the isolated interrupted startup test"]
fn interrupt_startup_child() {
    let Some(profile) = std::env::var_os(INTERRUPTED_PROFILE).map(PathBuf::from) else {
        return;
    };
    let editor = session(&profile, false);
    wait(&editor, |view| view.startup_pending);
    std::process::exit(13);
}

fn pressure_bundle(path: &Path) {
    let manifest = format!(
        "name='Drawing Pressure'\nid='drawing-pressure'\nversion='1.0.0'\n\
         permissions=['read_project','read_document']\n[engines]\nnanobug='^0.1.0'\n\
         [experiments]\nui={UI_CONTRACT:?}\n[[modules]]\nid='workspace'\n\
         wasm='main.wasm'\nrule='activate.js'\ninteractive=true\n"
    );
    let draw = (0..PATHS)
        .map(|index| {
            let x = (index % 12) * 20;
            let y = (index / 12) * 20;
            json!({"op":"path", "segments":[
            {"segment":"move","to":[x,y]},
            {"segment":"line","to":[x+18,y+3]},
            {"segment":"line","to":[x+13,y+17]},
            {"segment":"close"}],
            "fill":{"fill":"solid","color":1907107583}})
        })
        .collect::<Vec<_>>();
    let response = json!({"surface":{"title":"Background drawing", "lines":[], "input":null,
        "canvas":{"draw": draw, "interaction":{"next_frame":"redraw"}}}})
    .to_string();
    let bytes = response
        .bytes()
        .map(|byte| format!("\\{byte:02x}"))
        .collect::<String>();
    let result = (16_u64 << 32) | response.len() as u64;
    let wasm = wat::parse_str(format!(
        r#"(module
        (memory (export "memory") 20)
        (data (i32.const 16) "{bytes}")
        (func (export "nb_alloc") (param i32) (result i32) i32.const 1048576)
        (func (export "nb_run") (param i32 i32) (result i64) i64.const {result})
        (func (export "nb_free") (param i32 i32)))"#
    ))
    .unwrap();
    let rule = br#"
        const gate = await host.readFile('gate.txt');
        const scan = await host.scanText('fixture.data', 'fixture-target');
        return gate?.text === 'ready' && scan?.complete && scan.offsets.length === 1;
    "#;
    let mut zip = ZipWriter::new(File::create(path).unwrap());
    for (name, bytes) in [
        ("nanobug-plugin.toml", manifest.as_bytes()),
        ("main.wasm", &wasm),
        ("activate.js", rule.as_slice()),
    ] {
        zip.start_file(name, SimpleFileOptions::default()).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap();
}

/// Create the same large-query and drawing fixture for the foreground Windows timing driver.
#[test]
#[ignore = "writes a native timing fixture to NANOBUG_PLUGIN_TEST_OUTPUT"]
fn prepare_foreground_pressure_profile() -> anyhow::Result<()> {
    use nanobug::plugins::package::HostCompatibility;

    let root = PathBuf::from(
        std::env::var_os("NANOBUG_PLUGIN_TEST_OUTPUT").expect("set fixture output directory"),
    );
    fs::create_dir_all(&root)?;
    let archive = root.join("drawing.zip");
    pressure_bundle(&archive);
    let store =
        InstallStore::with_compatibility(root.join("profile"), HostCompatibility::current(true));
    let prepared = store.prepare(&archive)?;
    let grants = prepared.release.manifest.permissions.clone();
    store.install(prepared, &grants)?;
    for (id, filename) in [("rust", "README.md"), ("vue", "App.vue")] {
        let project = root.join(id);
        fs::create_dir_all(&project)?;
        fs::write(project.join(filename), "\n")?;
        fs::write(project.join("gate.txt"), "ready")?;
        let mut data = File::create(project.join("fixture.data"))?;
        for _ in 0..384 {
            data.write_all(&[b'x'; 32 * 1024])?;
        }
        data.write_all(b"fixture-target")?;
    }
    Ok(())
}

fn snapshot(doc: &Document) -> Arc<DocumentSnapshot> {
    Arc::new(DocumentSnapshot {
        path: doc.path.clone(),
        opening: doc.opening,
        revision: doc.revision,
        text: doc.text.clone(),
    })
}

#[test]
fn two_projects_keep_progress_during_large_queries_and_canvas_frame_pressure() -> anyhow::Result<()>
{
    let temp = tempfile::tempdir()?;
    let archive = temp.path().join("drawing.zip");
    pressure_bundle(&archive);
    let editor = session(&temp.path().join("profile"), true);
    let namespace = install(&editor, archive, "1.0.0");
    let mut rust = project(temp.path(), 1);
    let mut vue = project(temp.path(), 2);
    let rust_file = rust.root.join("main.rs");
    let vue_file = vue.root.join("App.vue");
    fs::write(&rust_file, "fn main() {}\r\n")?;
    fs::write(&vue_file, "<template>Vue</template>\n")?;
    let mut document = Document::open(&rust_file)?;
    rust.document = Some(snapshot(&document));
    vue.document = Some(snapshot(&Document::open(&vue_file)?));
    let active = key(&namespace, rust.id);
    let other = key(&namespace, vue.id);
    for project in [&rust, &vue] {
        fs::write(project.root.join("gate.txt"), "ready")?;
        let mut data = File::create(project.root.join("fixture.data"))?;
        for _ in 0..384 {
            data.write_all(&[b'x'; 32 * 1024])?;
        }
        data.write_all(b"fixture-target")?;
    }
    let mut held = Some(
        fs::OpenOptions::new()
            .write(true)
            .share_mode(0)
            .open(rust.root.join("gate.txt"))?,
    );
    editor.projects(Some(rust.id), vec![rust.clone(), vue.clone()]);
    let mut durations = Vec::with_capacity(EDITS);
    let mut other_frames = BTreeSet::new();
    let mut release_at = None;
    let pressure_started = Instant::now();
    // Keep applying pressure while the first cold compilation competes with the
    // other integration tests, without using the edit count as its deadline.
    while durations.len() < EDITS
        || (other_frames.len() < 2 && pressure_started.elapsed() < Duration::from_secs(6))
    {
        let index = durations.len();
        let started = Instant::now();
        let end = document.text.len();
        document.replace(end..end, "\t中e\u{301}👋\n", false);
        rust.document = Some(snapshot(&document));
        editor.projects(Some(rust.id), vec![rust.clone(), vue.clone()]);
        durations.push(started.elapsed().as_micros() as u64);
        for step in 0..4 {
            for owner in [&active, &other] {
                editor.send(Action::Resize(
                    owner.clone(),
                    Viewport {
                        id: (index * 4 + step + 1) as u64,
                        width: 300.0 + step as f32,
                        height: 160.,
                        scale: 1.,
                        apple_surface: false,
                        shaped_line: true,
                        appearance: None,
                    },
                ))?;
            }
        }
        let view = editor.view();
        if let Some(surface) = view.surfaces.get(&other).filter(|surface| {
            surface
                .drawing
                .as_ref()
                .is_some_and(|drawing| drawing.paths.len() == PATHS)
        }) {
            other_frames.insert(surface.revision);
            if held.is_some() {
                assert_ne!(view.modules.get(&active), Some(&State::Ready));
                release_at = Some((index + 1, pressure_started.elapsed().as_millis()));
                drop(held.take());
            }
        }
        std::thread::sleep(Duration::from_millis(12));
    }
    assert!(
        release_at.is_some(),
        "the second project never prepared a frame while the first dependency was unavailable; states={:?}",
        editor.view().modules
    );
    assert!(
        other_frames.len() >= 2,
        "the second project did not keep preparing frames"
    );
    wait(&editor, |view| {
        [&active, &other].into_iter().all(|owner| {
            view.modules.get(owner) == Some(&State::Ready)
                && view
                    .surfaces
                    .get(owner)
                    .and_then(|surface| surface.drawing.as_ref())
                    .is_some_and(|drawing| drawing.paths.len() == PATHS)
        })
    });
    let edits = durations.len();
    assert_eq!(document.revision, edits as u64);
    assert_eq!(document.cursor, document.text.len());
    document.save(None)?;
    assert_eq!(
        fs::read_to_string(&rust_file)?,
        format!("fn main() {{}}\r\n{}", "\t中e\u{301}👋\r\n".repeat(edits))
    );

    let mut sorted = durations.clone();
    sorted.sort_unstable();
    let percentile = |rank: usize| sorted[(rank * edits).div_ceil(100) - 1];
    eprintln!(
        "P7-02 document-model and snapshot publish µs P50={} P95={} P99={} max={}; second scene appeared at edit/ms={release_at:?}; observed prepared scenes={}; samples={durations:?}",
        percentile(50),
        percentile(95),
        percentile(99),
        sorted[edits - 1],
        other_frames.len()
    );
    Ok(())
}
