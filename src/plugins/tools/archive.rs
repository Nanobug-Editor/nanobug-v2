//! Shared, checksum-verified tool archives staged outside project files.

mod cache;

use super::{PreparedExecutable, Process, command, existing};
use crate::plugins::package::{
    MAX_FILE, ToolDownload, extract_zip, hash_bytes, portable_path, read_json, read_limited,
    validate_digest, write_json,
};
use anyhow::{Context, Result, ensure};
pub(super) use cache::{Lease, lease_existing};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Read, Seek, Write},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use tempfile::NamedTempFile;

const DOWNLOAD_LIMIT: u64 = 128 * 1024 * 1024;
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30);

/// A cache key identifies the verified ZIP independently of the requesting plugin.
pub(super) fn prepare(
    profile: &Path,
    download: &ToolDownload,
    closed: &AtomicBool,
) -> Result<PreparedExecutable> {
    prepare_with(profile, download, closed, |directory| {
        fetch(directory, &download.url, closed)
    })
}

fn prepare_with(
    profile: &Path,
    download: &ToolDownload,
    closed: &AtomicBool,
    fetch: impl FnOnce(&Path) -> Result<NamedTempFile>,
) -> Result<PreparedExecutable> {
    download.validate()?;
    ensure!(
        !closed.load(Ordering::Acquire),
        "Project closed during tool preparation"
    );
    fs::create_dir_all(profile)?;
    let cache = profile.canonicalize()?.join("tools");
    fs::create_dir_all(&cache)?;
    ensure!(
        cache.canonicalize()? == cache,
        "Native tool cache escaped the editor profile"
    );
    let object = cache.join(&download.sha256);
    let lease = Lease::acquire(&cache, &download.sha256, closed)?;
    if object.try_exists()? {
        return cached_executable(&cache, download, lease);
    }
    let mut archive = fetch(&cache)?;
    let size = archive.as_file().metadata()?.len();
    ensure!(
        size <= DOWNLOAD_LIMIT,
        "Native tool download exceeds its size limit"
    );
    archive.as_file_mut().rewind()?;
    let mut hasher = Sha256::new();
    io::copy(
        &mut archive.as_file_mut().take(DOWNLOAD_LIMIT + 1),
        &mut hasher,
    )?;
    ensure!(
        format!("{:x}", hasher.finalize()) == download.sha256,
        "Native tool archive checksum mismatch"
    );
    archive.as_file_mut().rewind()?;
    let staging = tempfile::Builder::new()
        .prefix("tool-stage-")
        .tempdir_in(&cache)?;
    let content = staging.path().join("files");
    fs::create_dir(&content)?;
    let files = extract_zip(archive.as_file_mut(), &content)?;
    ensure!(
        files.contains_key(&download.executable),
        "Native tool executable is missing from the archive"
    );
    let executable = content.join(&download.executable);
    existing(&content, &executable)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(executable, fs::Permissions::from_mode(0o700))?;
    }
    write_json(&staging.path().join("files.json"), &files)?;
    ensure!(
        !closed.load(Ordering::Acquire),
        "Project closed during tool preparation"
    );
    // Another session may publish the same immutable ZIP before this one finishes.
    if let Err(error) = fs::rename(staging.path(), &object)
        && !object.is_dir()
    {
        return Err(error).context("Publish downloaded native tool");
    }
    cached_executable(&cache, download, lease)
}

fn cached_executable(
    cache: &Path,
    download: &ToolDownload,
    lease: Lease,
) -> Result<PreparedExecutable> {
    let object = cache.join(&download.sha256);
    ensure!(
        object.canonicalize()? == object,
        "Native tool cache escaped its directory"
    );
    let content = object.join("files");
    ensure!(
        content.canonicalize()? == content,
        "Native tool files escaped the cache"
    );
    let files: BTreeMap<String, String> = read_json(&object.join("files.json"), 2 * 1024 * 1024)?
        .context("Native tool cache has no integrity record")?;
    ensure!(
        files.len() <= 4096 && files.contains_key(&download.executable),
        "Invalid native tool cache contents"
    );
    let mut bytes = 0u64;
    for (relative, digest) in files {
        validate_digest(&digest)?;
        let path = content.join(portable_path(&relative)?);
        ensure!(
            path.canonicalize()? == path,
            "Native tool file escaped its cache"
        );
        let payload = read_limited(&path, MAX_FILE)?;
        ensure!(
            hash_bytes(&payload) == digest,
            "Native tool cache contents changed after verification"
        );
        bytes += payload.len() as u64;
    }
    let executable = existing(&content, &content.join(&download.executable))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // The same verified archive can declare different executables for different plugins.
        if fs::metadata(&executable)?.permissions().mode() & 0o100 == 0 {
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))?;
        }
    }
    cache::mark_used(&object, bytes);
    cache::trim(cache);
    Ok(PreparedExecutable {
        path: executable,
        _lease: Some(lease),
    })
}

fn curl() -> Result<PathBuf> {
    #[cfg(windows)]
    let paths = [PathBuf::from(
        std::env::var_os("SystemRoot").context("Windows system directory unavailable")?,
    )
    .join("System32/curl.exe")];
    #[cfg(not(windows))]
    let paths = [PathBuf::from("/usr/bin/curl"), PathBuf::from("/bin/curl")];
    paths
        .into_iter()
        .find(|path| path.is_absolute() && path.is_file())
        .context("System HTTPS downloader is unavailable. Configure an existing tool path.")
}

fn fetch(directory: &Path, url: &str, closed: &AtomicBool) -> Result<NamedTempFile> {
    let mut archive = NamedTempFile::new_in(directory)?;
    let mut launch = command(&curl()?, directory);
    // Disable curl configuration files. Accept only HTTPS, including redirects, with OS trust.
    launch
        .args([
            "--disable",
            "--globoff",
            "--silent",
            "--fail",
            "--location",
            "--max-redirs",
            "4",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--connect-timeout",
            "5",
            "--max-time",
            "30",
            "--max-filesize",
            &DOWNLOAD_LIMIT.to_string(),
            "--url",
            url,
        ])
        .stdin(Stdio::null());
    ensure!(
        !closed.load(Ordering::Acquire),
        "Project closed during tool preparation"
    );
    let mut process = Process::own(launch.spawn().context("Start system HTTPS downloader")?)?;
    let stdout = process
        .child
        .stdout
        .take()
        .context("Tool download pipe unavailable")?;
    let (written, bytes) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let result = io::copy(&mut stdout.take(DOWNLOAD_LIMIT + 1), &mut archive);
        let _ = written.send(result.map(|count| (archive, count)));
    });
    let deadline = Instant::now() + DOWNLOAD_TIMEOUT;
    let mut body = None;
    loop {
        if body.is_none()
            && let Ok(result) = bytes.try_recv()
        {
            let (file, size) = result?;
            ensure!(
                size <= DOWNLOAD_LIMIT,
                "Native tool download exceeds its size limit"
            );
            body = Some(file);
        }
        if let Some(exit) = process.child.try_wait()? {
            ensure!(exit.success(), "HTTPS tool download failed ({exit})");
            let file = match body {
                Some(file) => file,
                None => {
                    let (file, size) = bytes.recv_timeout(Duration::from_secs(1))??;
                    ensure!(
                        size <= DOWNLOAD_LIMIT,
                        "Native tool download exceeds its size limit"
                    );
                    file
                }
            };
            let mut file = file;
            file.flush()?;
            file.as_file().sync_all()?;
            return Ok(file);
        }
        ensure!(
            Instant::now() < deadline && !closed.load(Ordering::Acquire),
            "Native tool download cancelled or timed out"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_is_verified_before_publishing_and_reused_by_checksum() -> Result<()> {
        let profile = tempfile::tempdir()?;
        let mut zip = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        zip.start_file("bin/tool.exe", zip::write::SimpleFileOptions::default())?;
        zip.write_all(b"test executable bytes")?;
        let archive = zip.finish()?.into_inner();
        let closed = AtomicBool::new(false);
        let mut download = ToolDownload {
            url: "https://example.test/tool.zip".into(),
            sha256: "0".repeat(64),
            executable: "bin/tool.exe".into(),
        };
        let fetch = |directory: &Path| {
            let mut file = NamedTempFile::new_in(directory)?;
            file.write_all(&archive)?;
            Ok(file)
        };
        assert!(
            prepare_with(profile.path(), &download, &closed, fetch)
                .unwrap_err()
                .to_string()
                .contains("checksum mismatch")
        );
        assert!(!profile.path().join("tools").join(&download.sha256).exists());

        download.sha256 = hash_bytes(&archive);
        let installed = prepare_with(profile.path(), &download, &closed, fetch)?;
        let cached = prepare_with(profile.path(), &download, &closed, |_| {
            panic!("a verified archive should be shared")
        })?;
        assert_eq!(cached.path(), installed.path());
        assert_eq!(fs::read(installed.path())?, b"test executable bytes");
        fs::write(installed.path(), b"corrupted")?;
        assert!(prepare_with(profile.path(), &download, &closed, fetch).is_err());
        Ok(())
    }

    #[test]
    #[ignore = "requires a controlled HTTPS fixture with a known archive checksum"]
    fn system_https_fetcher_prepares_and_reuses_a_verified_archive() -> Result<()> {
        let profile = tempfile::tempdir()?;
        let closed = AtomicBool::new(false);
        let download = ToolDownload {
            url: std::env::var("NANOBUG_TEST_TOOL_URL")?,
            sha256: std::env::var("NANOBUG_TEST_TOOL_SHA256")?,
            executable: std::env::var("NANOBUG_TEST_TOOL_EXECUTABLE")?,
        };
        let executable = prepare(profile.path(), &download, &closed)?;
        assert!(executable.path().is_file());
        let mut offline = download;
        offline.url = "https://example.invalid/not-used.zip".into();
        assert_eq!(
            prepare(profile.path(), &offline, &closed)?.path(),
            executable.path()
        );
        Ok(())
    }
}
