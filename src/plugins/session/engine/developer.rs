//! Local Cargo builds use the selected development profile and immutable install snapshots.

use crate::plugins::{
    package::{InstallStore, Installed, Prepared, Release, hash_bytes, read_limited, write_bytes},
    tools::{self, Run},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, UNIX_EPOCH},
};

/// Per-plugin development options saved only in the active development profile.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct DeveloperSettings {
    pub source: Option<PathBuf>,
    pub automatic: bool,
}

pub(super) struct Build {
    pub settings: DeveloperSettings,
    pub prior: Option<String>,
    pub force: bool,
    pub cancelled: Arc<AtomicBool>,
}

pub(super) struct BuildOutcome {
    pub source: PathBuf,
    pub stamp: String,
    pub prepared: Option<Result<Prepared>>,
}

/// The exact source and installed base a prepared development package was built from.
pub(super) struct Candidate {
    pub source: PathBuf,
    pub stamp: String,
    pub base: String,
    pub cancelled: Arc<AtomicBool>,
    /// True only after the user reviews this prepared build's additional authority.
    pub permissions_reviewed: bool,
}

impl Candidate {
    pub(super) fn verify(&self, installed: Option<&Installed>) -> Result<()> {
        ensure!(
            installed.is_some_and(
                |record| record.release.digest == self.base && record.release.development
            ),
            "The installed development version changed before reload"
        );
        ensure!(
            !self.cancelled.load(Ordering::Acquire),
            "Development build was cancelled"
        );
        ensure!(
            fingerprint(&self.source)? == self.stamp,
            "Source changed before reload. The active version was kept."
        );
        ensure!(
            !self.cancelled.load(Ordering::Acquire),
            "Development build was cancelled"
        );
        Ok(())
    }
}

pub(super) fn run(
    store: InstallStore,
    release: Arc<Release>,
    build: Build,
) -> Result<BuildOutcome> {
    let source = build
        .settings
        .source
        .as_ref()
        .context("Choose a Cargo source directory")?
        .canonicalize()?;
    let stamp = fingerprint(&source)?;
    let rebuild = build.force || build.prior.as_ref().is_some_and(|before| before != &stamp);
    let prepared =
        rebuild.then(|| build_package(&store, &release, &source, &stamp, build.cancelled));
    Ok(BuildOutcome {
        source,
        stamp,
        prepared,
    })
}

fn build_package(
    store: &InstallStore,
    release: &Release,
    source: &Path,
    stamp: &str,
    cancelled: Arc<AtomicBool>,
) -> Result<Prepared> {
    ensure!(
        !cancelled.load(Ordering::Acquire),
        "Development build was cancelled"
    );
    let cargo = std::env::var_os("PATH")
        .and_then(|paths| {
            let name = format!("cargo{}", std::env::consts::EXE_SUFFIX);
            std::env::split_paths(&paths)
                .map(|path| path.join(&name))
                .find(|path| path.is_file())
        })
        .context("Cargo is not available on this development computer")?
        .canonicalize()?;
    let namespace = release.manifest.identity().namespace();
    let target = store
        .root()
        .canonicalize()?
        .join("dev-target")
        .join(&namespace);
    let (status, reported) = tools::build(
        Run {
            profile: store.root().to_path_buf(),
            namespace,
            tool: "cargo".into(),
            project: source.to_path_buf(),
            executable: cargo,
            args: vec![
                "build".into(),
                "--release".into(),
                "--offline".into(),
                "--target".into(),
                "wasm32-unknown-unknown".into(),
                "--target-dir".into(),
                target.to_string_lossy().into_owned(),
                "--message-format=json".into(),
            ],
            closed: Arc::clone(&cancelled),
            timeout: Duration::from_secs(120),
        },
        cargo_artifacts,
    )?;
    ensure!(status == Some(0), "Cargo build did not finish successfully");
    ensure!(
        !cancelled.load(Ordering::Acquire),
        "Development build was cancelled"
    );
    ensure!(
        fingerprint(source)? == stamp,
        "Source changed during build. The current plugin was kept."
    );
    let reported = reported.context("Cargo did not report its built artifacts")?;
    let artifacts = target
        .join("wasm32-unknown-unknown")
        .join("release")
        .canonicalize()?;
    let source_assets = tempfile::tempdir_in(store.root())?;
    for name in release.files.keys() {
        let path = if release
            .manifest
            .modules
            .iter()
            .any(|module| module.wasm.as_ref() == Some(name))
        {
            let path = artifacts
                .join(
                    Path::new(name)
                        .file_name()
                        .context("Invalid WASM module name")?,
                )
                .canonicalize()
                .context("Cargo did not build a declared WASM module")?;
            ensure!(
                path.parent() == Some(artifacts.as_path()) && reported.contains(&path),
                "Cargo did not report a declared WASM module in this build"
            );
            path
        } else {
            store.verified_asset(release, name)?
        };
        let bytes = read_limited(&path, 64 * 1024 * 1024)?;
        write_bytes(&source_assets.path().join(name), &bytes)?;
    }
    let mut prepared = store.prepare_directory(source_assets.path())?;
    prepared.release.source.clone_from(&release.source);
    Ok(prepared)
}

/// Use Cargo's artifact messages so a stale WASM file cannot satisfy a new build.
fn cargo_artifacts(stdout: impl Read) -> Result<BTreeSet<PathBuf>> {
    const OUTPUT_LIMIT: u64 = 16 * 1024 * 1024;
    #[derive(Deserialize)]
    struct Message {
        reason: String,
        #[serde(default)]
        filenames: Vec<PathBuf>,
    }

    let mut reader = BufReader::new(stdout.take(OUTPUT_LIMIT + 1));
    let mut line = Vec::new();
    let mut total = 0;
    let mut paths = BTreeSet::new();
    loop {
        line.clear();
        let length = reader.read_until(b'\n', &mut line)?;
        if length == 0 {
            return Ok(paths);
        }
        total += length as u64;
        ensure!(
            total <= OUTPUT_LIMIT,
            "Cargo artifact output exceeded its budget"
        );
        if let Ok(message) = serde_json::from_slice::<Message>(&line)
            && message.reason == "compiler-artifact"
        {
            paths.extend(message.filenames.into_iter().filter_map(|path| {
                (path
                    .extension()
                    .is_some_and(|extension| extension == "wasm"))
                .then(|| path.canonicalize().ok())
                .flatten()
            }));
        }
    }
}

fn fingerprint(source: &Path) -> Result<String> {
    ensure!(
        source.join("Cargo.toml").is_file(),
        "Development source must contain Cargo.toml"
    );
    let mut pending = vec![source.join("Cargo.toml"), source.join("src")];
    if source.join("build.rs").is_file() {
        pending.push(source.join("build.rs"));
    }
    let mut files = BTreeMap::new();
    let mut count = 0;
    while let Some(path) = pending.pop() {
        count += 1;
        ensure!(
            count <= 4096,
            "Development source tree is too large for automatic checks"
        );
        let metadata = fs::symlink_metadata(&path)?;
        ensure!(
            !metadata.file_type().is_symlink(),
            "Development source must stay in the selected directory"
        );
        if metadata.is_dir() {
            for entry in fs::read_dir(&path)? {
                pending.push(entry?.path());
            }
        } else if metadata.is_file() {
            let time = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |time| time.as_nanos() as u64);
            files.insert(
                path.strip_prefix(source)?.to_string_lossy().into_owned(),
                (metadata.len(), time),
            );
        }
    }
    Ok(hash_bytes(&serde_json::to_vec(&files)?))
}
