//! Exercise Cargo reloads using a harmless, temporary local source directory.

use super::*;
use nanobug::plugins::session::DeveloperSettings;

fn source(message: &str) -> String {
    let output = json!({ "message": message });
    format!(
        r###"#![no_std]
        #[panic_handler]
        fn panic(_: &core::panic::PanicInfo) -> ! {{ loop {{}} }}
        static mut INPUT: [u8; 262144] = [0; 262144];
        #[unsafe(no_mangle)]
        pub extern "C" fn nb_alloc(_: u32) -> u32 {{ (&raw mut INPUT) as u32 }}
        #[unsafe(no_mangle)]
        pub extern "C" fn nb_run(_: u32, _: u32) -> u64 {{
            let result = br#"{output}"#;
            ((result.as_ptr() as u64) << 32) | result.len() as u64
        }}
        #[unsafe(no_mangle)]
        pub extern "C" fn nb_free(_: u32, _: u32) {{}}
        "###
    )
}

fn hold_write(session: &Session, owner: &Key) -> async_channel::Sender<()> {
    let view = session.view();
    let (lease, ended) = async_channel::bounded(1);
    let (reply, result) = async_channel::bounded(1);
    session
        .send(Action::BeginFileWrite {
            owner: owner.clone(),
            digest: view.installed[0].release.digest.clone(),
            generation: view.runtime_status[&owner.namespace].generation,
            ended,
            reply,
        })
        .unwrap();
    answer(&result).unwrap();
    lease
}

#[test]
#[ignore = "requires Cargo and the wasm32-unknown-unknown target"]
fn only_current_opted_in_development_builds_can_reload() {
    let fixture = tempfile::tempdir().unwrap();
    let path = fixture.path().join("fixture.zip");
    let output = fixture.path().join("initial-build");
    fs::create_dir(&output).unwrap();
    bundle(
        &path,
        "Development Fixture",
        &["change_files"],
        false,
        "",
        json!({}),
    );
    zip::ZipArchive::new(File::open(&path).unwrap())
        .unwrap()
        .extract(&output)
        .unwrap();
    let source_dir = fixture.path().join("source");
    fs::create_dir_all(source_dir.join("src")).unwrap();
    let manifest = source_dir.join("Cargo.toml");
    let cargo = |name| {
        format!(
            "[package]\nname='{name}'\nversion='0.1.0'\nedition='2024'\n\
             [lib]\ncrate-type=['cdylib']\n"
        )
    };
    fs::write(&manifest, cargo("main")).unwrap();
    let rust = source_dir.join("src/lib.rs");
    fs::write(&rust, source("initial manual build")).unwrap();

    let profile = fixture.path().join("development");
    let regular_profile = fixture.path().join("regular");
    let ordinary = session(&regular_profile, false);
    let identity = install(&ordinary, &path);
    let owner = key(&identity, 1);
    let original = ordinary.view().installed[0].release.digest.clone();
    let settings = |automatic| DeveloperSettings {
        source: Some(source_dir.clone()),
        automatic,
    };
    ordinary
        .send(Action::SetDeveloperSettings(
            owner.namespace.clone(),
            settings(true),
        ))
        .unwrap();
    ordinary
        .send(Action::BuildDevelopment(owner.namespace.clone()))
        .unwrap();
    configure(&ordinary, &[(&identity, "regular setting")]);
    assert!(ordinary.view().developer_settings.is_empty());
    assert!(!regular_profile.join("developer.json").exists());

    let development = session(&profile, true);
    development
        .send(Action::InspectDevelopment(output.clone()))
        .unwrap();
    let review = wait(&development, |view| view.review.is_some());
    let review = review.review.as_ref().unwrap();
    assert_eq!(review.manifest.identity(), identity);
    development.send(Action::Approve(review.token)).unwrap();
    wait(&development, |view| view.installed.len() == 1);
    configure(&development, &[(&identity, "development setting")]);
    let regular_project = project(fixture.path(), 2);
    let dev_project = project(fixture.path(), 1);
    let regular_owner = key(&identity, 2);
    ordinary.projects(Some(2), vec![regular_project]);
    development.projects(Some(1), vec![dev_project]);
    wait(&ordinary, |view| {
        view.modules.get(&regular_owner) == Some(&State::Ready)
    });
    assert_eq!(label(&ordinary, &regular_owner), "regular setting");
    wait(&development, |view| {
        view.modules.get(&owner) == Some(&State::Ready)
    });
    assert_eq!(label(&development, &owner), "development setting");
    let version = development.view().installed[0].release.digest.clone();
    let configure_build = |automatic| {
        development
            .send(Action::SetDeveloperSettings(
                owner.namespace.clone(),
                settings(automatic),
            ))
            .unwrap();
        wait(&development, |view| {
            view.developer_settings.get(&owner.namespace) == Some(&settings(automatic))
        });
    };
    configure_build(false);
    std::thread::sleep(Duration::from_millis(850));
    assert!(!profile.join("dev-target").exists());
    assert_eq!(development.view().installed[0].release.digest, version);
    configure_build(true);
    development
        .send(Action::BuildDevelopment(owner.namespace.clone()))
        .unwrap();
    let manual = wait(&development, |view| {
        view.installed[0].release.digest != version
            && view.modules.get(&owner) == Some(&State::Ready)
    });
    assert_eq!(
        call(&development, &owner).unwrap().output.message,
        "initial manual build"
    );
    let active = manual.installed[0].release.digest.clone();

    // Change the source after the first build is complete, but before its confirmed write can end.
    let lease = hold_write(&development, &owner);
    fs::write(&rust, source("build that became stale")).unwrap();
    wait(&development, |view| view.waiting_for_operations);
    assert_eq!(development.view().installed[0].release.digest, active);
    fs::write(&rust, source("latest complete build")).unwrap();
    drop(lease);
    let latest = wait(&development, |view| {
        view.installed[0].release.digest != active
            && view.modules.get(&owner) == Some(&State::Ready)
    });
    assert_eq!(
        latest.installed[0]
            .previous
            .as_ref()
            .unwrap()
            .release
            .digest,
        active
    );
    assert_eq!(
        call(&development, &owner).unwrap().output.message,
        "latest complete build"
    );
    let active = latest.installed[0].release.digest.clone();

    let lease = hold_write(&development, &owner);
    fs::write(&rust, source("queued before switching off")).unwrap();
    wait(&development, |view| view.waiting_for_operations);
    configure_build(false);
    drop(lease);
    std::thread::sleep(Duration::from_millis(850));
    assert_eq!(development.view().installed[0].release.digest, active);
    assert!(!development.view().waiting_for_operations);

    // Cargo succeeds with a different artifact name. It must not adopt an old main.wasm.
    fs::write(&manifest, cargo("different")).unwrap();
    development
        .send(Action::BuildDevelopment(owner.namespace.clone()))
        .unwrap();
    wait(&development, |view| {
        view.logs.get(&owner.namespace).is_some_and(|logs| {
            logs.iter()
                .any(|log| log.contains("Cargo did not") && log.contains("declared WASM"))
        })
    });
    assert_eq!(development.view().installed[0].release.digest, active);
    assert_eq!(
        call(&development, &owner).unwrap().output.message,
        "latest complete build"
    );
    let view = development.view();
    assert_eq!(
        view.configurations[&ConfigScope::AllProjects]
            .applied
            .plugin(&owner.namespace)
            .unwrap()
            .settings["label"],
        "development setting"
    );
    assert_eq!(ordinary.view().installed[0].release.digest, original);
    assert_eq!(label(&ordinary, &regular_owner), "regular setting");
    assert!(!regular_profile.join("dev-target").exists());
    assert!(!output.join("src").exists());

    #[cfg(windows)]
    {
        // The compiler child of a real Cargo invocation stays active until the session closes.
        let marker = source_dir.join("build.pid");
        fs::write(&manifest, cargo("main")).unwrap();
        let compiler_source = fixture.path().join("hold-rustc.rs");
        let compiler = fixture.path().join("hold-rustc.exe");
        fs::write(
            &compiler_source,
            format!(
                r##"fn main() {{
                std::fs::write(r#"{}"#, std::process::id().to_string()).unwrap();
                std::thread::sleep(std::time::Duration::from_secs(30));
            }}"##,
                marker.display()
            ),
        )
        .unwrap();
        let status = std::process::Command::new("rustc")
            .arg(&compiler_source)
            .arg("-o")
            .arg(&compiler)
            .status()
            .unwrap();
        assert!(status.success());
        fs::create_dir(source_dir.join(".cargo")).unwrap();
        fs::write(
            source_dir.join(".cargo/config.toml"),
            format!("[build]\nrustc-wrapper = '{}'\n", compiler.display()),
        )
        .unwrap();
        development
            .send(Action::BuildDevelopment(owner.namespace.clone()))
            .unwrap();
        let build_pid = || fs::read_to_string(&marker).ok()?.parse::<u32>().ok();
        wait(&development, |_| build_pid().is_some());
        let pid = build_pid().unwrap();
        let running = || {
            let list = std::process::Command::new("tasklist.exe")
                .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
                .output()
                .unwrap();
            assert!(list.status.success());
            String::from_utf8_lossy(&list.stdout).contains("hold-rustc.exe")
        };
        assert!(running());
        drop(development);
        let deadline = Instant::now() + Duration::from_secs(6);
        while running() {
            assert!(
                Instant::now() < deadline,
                "Development build child {pid} survived close"
            );
            std::thread::sleep(Duration::from_millis(30));
        }
        let stored = nanobug::plugins::package::InstallStore::new(profile)
            .list()
            .unwrap();
        assert_eq!(stored[0].release.digest, active);
    }
}
