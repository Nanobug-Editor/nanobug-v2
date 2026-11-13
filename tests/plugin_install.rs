//! Installation is tested through package preparation and the persisted registry.

use nanobug::plugins::package::{HostCompatibility, Identity, InstallStore, UI_CONTRACT};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use zip::{ZipWriter, write::SimpleFileOptions};

fn write_zip(path: &Path, name: &str, id: &str, version: &str) {
    let manifest = format!(
        "name = {name:?}\nid = {id:?}\nversion = {version:?}\n[engines]\nnanobug = '^0.1.0'\n"
    );
    write_manifest(path, &manifest);
}

fn write_manifest(path: &Path, manifest: &str) {
    let mut zip = ZipWriter::new(File::create(path).unwrap());
    zip.start_file("nanobug-plugin.toml", SimpleFileOptions::default())
        .unwrap();
    zip.write_all(manifest.as_bytes()).unwrap();
    zip.finish().unwrap();
}

#[test]
fn abandoned_and_invalid_packages_preserve_the_installed_release() {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("input.zip");
    let store = InstallStore::new(root.path().join("profile"));
    write_zip(&archive, "Fixture", "fixture", "1.0.0");
    let original = fs::read(&archive).unwrap();
    let installed = store
        .install(store.prepare(&archive).unwrap(), &Default::default())
        .unwrap();
    assert_eq!(fs::read(&archive).unwrap(), original);
    let index = store.root().join("installed.json");
    let prior = fs::read(&index).unwrap();

    write_zip(&archive, "Fixture", "fixture", "1.1.0");
    let candidate = fs::read(&archive).unwrap();
    drop(store.prepare(&archive).unwrap());
    assert_eq!(fs::read(&archive).unwrap(), candidate);
    fs::write(&archive, b"incomplete archive").unwrap();
    assert!(store.prepare(&archive).is_err());
    write_manifest(&archive, "incomplete = true");
    assert!(store.prepare(&archive).is_err());
    let mut zip = ZipWriter::new(File::create(&archive).unwrap());
    zip.start_file("assets/CON.txt", SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"portable fixture").unwrap();
    zip.finish().unwrap();
    let Err(error) = store.prepare(&archive) else {
        panic!("Reserved device name was accepted")
    };
    assert!(error.to_string().contains("Reserved device name"));
    assert_eq!(fs::read(&index).unwrap(), prior);
    assert_eq!(
        store.list().unwrap()[0].release.digest,
        installed.release.digest
    );
    assert_eq!(
        fs::read_dir(store.root().join("packages")).unwrap().count(),
        1
    );
}

/// Kill a separate installer with a prepared candidate and verify the on-disk state from scratch.
#[test]
fn interrupted_prepare_and_repeated_install_preserve_previous_release() -> anyhow::Result<()> {
    const FIXTURE: &str = "NANOBUG_TEST_INSTALL_INTERRUPTION";
    if let Some(root) = std::env::var_os(FIXTURE) {
        let root = Path::new(&root);
        let store = InstallStore::new(root.join("profile"));
        let _prepared = store.prepare(&root.join("candidate.zip"))?;
        File::create(root.join("prepared"))?.sync_all()?;
        std::io::stdin().read_exact(&mut [0])?;
        anyhow::bail!("Interruption fixture returned without being killed");
    }

    let temp = tempfile::tempdir()?;
    let root = temp.path();
    let profile = root.join("profile");
    let first_zip = root.join("first.zip");
    let candidate_zip = root.join("candidate.zip");
    write_zip(&first_zip, "Fixture", "fixture", "1.0.0");
    write_zip(&candidate_zip, "Fixture", "fixture", "1.1.0");
    let original = fs::read(&first_zip)?;
    let candidate = fs::read(&candidate_zip)?;
    let store = InstallStore::new(profile.clone());
    let previous = store.install(store.prepare(&first_zip)?, &Default::default())?;
    let registry = profile.join("installed.json");
    let prior_registry = fs::read(&registry)?;

    let mut child = Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "interrupted_prepare_and_repeated_install_preserve_previous_release",
        ])
        .env(FIXTURE, root)
        .stdin(Stdio::piped())
        .spawn()?;
    let until = Instant::now() + Duration::from_secs(10);
    let reached_prepare = loop {
        if root.join("prepared").is_file() {
            break true;
        }
        if child.try_wait()?.is_some() || Instant::now() >= until {
            break false;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let killed = child.kill();
    let status = child.wait()?;
    assert!(
        reached_prepare,
        "The installer never prepared its candidate"
    );
    killed?;
    assert!(!status.success());
    assert_eq!(fs::read(&registry)?, prior_registry);

    let reopened = InstallStore::new(profile);
    let mut current = reopened.list()?;
    assert_eq!(current.len(), 1);
    assert_eq!(current.remove(0).release.digest, previous.release.digest);
    assert!(
        reopened
            .verified_asset(&previous.release, "nanobug-plugin.toml")?
            .is_file()
    );
    let release = reopened.install(reopened.prepare(&candidate_zip)?, &Default::default())?;
    let repeated = reopened.install(reopened.prepare(&candidate_zip)?, &Default::default())?;
    assert_eq!(repeated.release.digest, release.release.digest);
    assert_eq!(
        repeated.previous.unwrap().release.digest,
        previous.release.digest
    );
    assert_eq!(reopened.list()?.len(), 1);
    assert_eq!(fs::read(first_zip)?, original);
    assert_eq!(fs::read(candidate_zip)?, candidate);
    Ok(())
}

#[cfg(windows)]
#[test]
fn failed_index_publication_keeps_the_previous_install_and_cleans_new_bytes() {
    use std::os::windows::fs::OpenOptionsExt;

    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("input.zip");
    let store = InstallStore::new(root.path().join("profile"));
    write_zip(&archive, "Fixture", "fixture", "1.0.0");
    let first = store
        .install(store.prepare(&archive).unwrap(), &Default::default())
        .unwrap();
    write_zip(&archive, "Fixture", "fixture", "1.1.0");
    let prepared = store.prepare(&archive).unwrap();
    // An external reader can hold the old registry open without permitting replacement.
    let reader = fs::OpenOptions::new()
        .read(true)
        .share_mode(0x01 | 0x02)
        .open(store.root().join("installed.json"))
        .unwrap();
    assert!(store.install(prepared, &Default::default()).is_err());
    drop(reader);
    assert_eq!(
        store.list().unwrap()[0].release.digest,
        first.release.digest
    );
    assert_eq!(
        fs::read_dir(store.root().join("packages")).unwrap().count(),
        1
    );
    let next = store
        .install(store.prepare(&archive).unwrap(), &Default::default())
        .unwrap();
    assert_eq!(next.previous.unwrap().release.digest, first.release.digest);
}

#[test]
fn compatibility_errors_identify_the_requirement_before_installing() {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("input.zip");
    let profile = root.path().join("profile");
    let mut host = HostCompatibility::current(true);
    host.platform = "windows-x86_64".into();
    host.version = "0.1.0".parse().unwrap();
    let development = InstallStore::with_compatibility(profile.clone(), host.clone());
    host.experiments.clear();
    let normal = InstallStore::with_compatibility(profile, host);
    for (before_engines, range, experiments, expected) in [
        ("", ">=0.2", "", "Editor version too old"),
        ("", ">0.1", "", "Editor version too old"),
        ("", ">0.1.0-alpha, <0.1.0", "", "does not support Nanobug"),
        ("", "<0.1", "", "does not support Nanobug"),
        (
            "platforms = ['linux']",
            "*",
            "",
            "does not support platform",
        ),
        ("api = '2.0'", "*", "", "Unsupported plugin API"),
        (
            "",
            "*",
            "[experiments]\nui = 'previous'",
            "does not match this build",
        ),
    ] {
        write_manifest(
            &archive,
            &format!(
                "name='Fixture'\nid='fixture'\nversion='1.0.0'\n{before_engines}\n[engines]\nnanobug={range:?}\n{experiments}\n"
            ),
        );
        let message = match development.prepare(&archive) {
            Ok(_) => panic!("Unexpectedly accepted {range:?} {experiments:?}"),
            Err(error) => format!("{error:#}"),
        };
        assert!(message.contains(expected), "{message}");
    }
    write_manifest(
        &archive,
        &format!(
            "name='Fixture'\nid='fixture'\nversion='1.0.0'\n[engines]\nnanobug='*'\n[experiments]\nui={UI_CONTRACT:?}\n"
        ),
    );
    let Err(error) = normal.prepare(&archive) else {
        panic!("Experiment was enabled in a normal profile")
    };
    assert!(error.to_string().contains("unavailable in this profile"));
    assert!(development.prepare(&archive).is_ok());
    assert!(normal.list().unwrap().is_empty());
}

#[test]
fn same_id_with_a_different_name_keeps_separate_installations() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("input.zip");
    let store = InstallStore::new(root.path().join("profile"));
    for name in ["First Plugin", "Second Plugin"] {
        write_zip(&file, name, "3d--view", "1.0.0");
        let package = store.prepare(&file).unwrap();
        store.install(package, &Default::default()).unwrap();
    }
    let installs = store.list().unwrap();
    assert_eq!(installs.len(), 2);
    assert_ne!(
        installs[0].release.manifest.identity().namespace(),
        installs[1].release.manifest.identity().namespace(),
    );
    assert!(Identity::new("Plugin".into(), "abc\n".into()).is_err());
}

#[test]
fn manifest_identity_accepts_the_whole_id_and_ignores_display_translation() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("input.zip");
    let store = InstallStore::new(root.path().join("profile"));
    for id in [
        "git",
        "rust-tools",
        "vue3-helper",
        "a",
        "123",
        "3d-view",
        "a--b",
    ] {
        write_zip(&file, "Same name", id, "1.0.0");
        store
            .install(store.prepare(&file).unwrap(), &Default::default())
            .unwrap();
    }
    for id in [
        "",
        "-git",
        "git-",
        "Git",
        "rust_tools",
        "rust tools",
        "git\n",
        "rust\ttools",
        "中文",
    ] {
        write_zip(&file, "Invalid ID", id, "1.0.0");
        assert!(store.prepare(&file).is_err(), "{id:?}");
    }
    let mut zip = ZipWriter::new(File::create(&file).unwrap());
    zip.start_file("nanobug-plugin.toml", SimpleFileOptions::default())
        .unwrap();
    write!(
        zip,
        "name = 'Same name'\nid = 'git'\nversion = '1.1.0'\n[engines]\nnanobug = '^0.1.0'\n\
        [translations.zh-CN]\nname = '本地化名称'\n"
    )
    .unwrap();
    zip.finish().unwrap();
    let updated = store
        .install(store.prepare(&file).unwrap(), &Default::default())
        .unwrap();
    assert_eq!(updated.release.manifest.display("zh-CN").0, "本地化名称");
    assert_eq!(
        updated
            .previous
            .unwrap()
            .release
            .manifest
            .version
            .to_string(),
        "1.0.0"
    );
    assert_eq!(store.list().unwrap().len(), 7);
}
