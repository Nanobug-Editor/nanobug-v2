//! Disk retention for owned, immutable native tool cache objects.

use crate::plugins::package::{portable_path, read_json, validate_digest, write_json};
use anyhow::{Context, Result, ensure};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions, TryLockError},
    io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant, SystemTime},
};

const CACHE_BYTES: u64 = 512 * 1024 * 1024;
const CACHE_ENTRIES: usize = 32;
const INDEX_LIMIT: u64 = 2 * 1024 * 1024;
const LEASE_WAIT: Duration = Duration::from_secs(5);

/// The shared lock outlives the process using the executable and all its dependencies.
#[derive(Debug)]
pub(in crate::plugins::tools) struct Lease {
    file: Option<File>,
    cache: PathBuf,
}

impl Lease {
    pub(super) fn acquire(cache: &Path, digest: &str, closed: &AtomicBool) -> Result<Self> {
        let file = open_lock(cache, bucket(digest)?)?;
        let deadline = Instant::now() + LEASE_WAIT;
        loop {
            ensure!(
                !closed.load(Ordering::Acquire),
                "Project closed during tool cache reservation"
            );
            match file.try_lock_shared() {
                Ok(()) => {
                    return Ok(Self {
                        file: Some(file),
                        cache: cache.to_path_buf(),
                    });
                }
                Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(error) => return Err(error).context("Reserve the native tool cache"),
            }
        }
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        drop(self.file.take());
        schedule_trim(&self.cache);
    }
}

/// A fixed number of stable lock files avoids deleting a lock while another editor opens it.
fn bucket(digest: &str) -> Result<&str> {
    validate_digest(digest)?;
    Ok(&digest[..2])
}

fn open_lock(cache: &Path, name: &str) -> Result<File> {
    let locks = cache.join("leases");
    fs::create_dir_all(&locks)?;
    ensure!(
        locks.canonicalize()? == locks,
        "Native tool locks escaped the cache"
    );
    let path = locks.join(format!("{name}.lock"));
    match fs::symlink_metadata(&path) {
        Ok(metadata) => ensure!(
            metadata.file_type().is_file() && path.canonicalize()? == path,
            "Invalid native tool lock"
        ),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?)
}

/// Store measured logical bytes and use the verified file index's timestamp for recency.
pub(super) fn mark_used(object: &Path, bytes: u64) {
    let index = object.join("files.json");
    if regular_file(&index).is_ok()
        && let Ok(file) = OpenOptions::new().write(true).open(&index)
    {
        let _ = file.set_modified(SystemTime::now());
    }
    if !object.join("size.json").is_file() {
        let _ = write_json(&object.join("size.json"), &bytes);
    }
}

pub(super) fn trim(cache: &Path) {
    let _ = trim_to(cache, CACHE_BYTES, CACHE_ENTRIES);
}

/// Cleanup after release runs off the supervisor and is serialized across editors.
fn schedule_trim(cache: &Path) {
    struct Cleaner {
        pending: Arc<Mutex<BTreeSet<PathBuf>>>,
        wake: mpsc::SyncSender<()>,
    }
    static CLEANER: OnceLock<Cleaner> = OnceLock::new();
    let cleaner = CLEANER.get_or_init(|| {
        let pending = Arc::new(Mutex::new(BTreeSet::<PathBuf>::new()));
        let directories = Arc::clone(&pending);
        let (wake, work) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            while work.recv().is_ok() {
                let caches = std::mem::take(&mut *directories.lock().unwrap());
                for cache in caches {
                    trim(&cache);
                }
            }
        });
        Cleaner { pending, wake }
    });
    let mut pending = cleaner.pending.lock().unwrap();
    if pending.len() < 32 && pending.insert(cache.to_path_buf()) {
        let _ = cleaner.wake.try_send(());
    }
}

struct Entry {
    digest: String,
    bytes: u64,
    used: SystemTime,
}

fn regular_file(path: &Path) -> Result<fs::Metadata> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file() && path.canonicalize()? == path,
        "Invalid native tool cache file"
    );
    Ok(metadata)
}

fn entry(cache: &Path, digest: String) -> Result<Entry> {
    validate_digest(&digest)?;
    let object = cache.join(&digest);
    ensure!(
        fs::symlink_metadata(&object)?.file_type().is_dir() && object.canonicalize()? == object,
        "Native tool cache entry escaped its directory"
    );
    let content = object.join("files");
    ensure!(
        fs::symlink_metadata(&content)?.file_type().is_dir() && content.canonicalize()? == content,
        "Native tool content escaped its cache entry"
    );
    let index = object.join("files.json");
    let used = regular_file(&index)?.modified()?;
    let size = object.join("size.json");
    let bytes = if regular_file(&size).is_ok() {
        read_json(&size, 128)?.context("Missing native tool cache size")?
    } else {
        // Profiles created before cache sizes were stored remain eligible for retention.
        let files: BTreeMap<String, String> =
            read_json(&index, INDEX_LIMIT)?.context("Missing native tool cache file list")?;
        ensure!(files.len() <= 4096, "Too many native tool cache files");
        let mut bytes = 0u64;
        for path in files.keys() {
            bytes = bytes.saturating_add(regular_file(&content.join(portable_path(path)?))?.len());
        }
        bytes
    };
    Ok(Entry {
        digest,
        bytes,
        used,
    })
}

fn trim_to(cache: &Path, max_bytes: u64, max_entries: usize) -> Result<()> {
    // The stable guard prevents multiple editors from pruning against the same old totals.
    if !cache.is_dir() || cache.canonicalize()? != cache {
        return Ok(());
    }
    let guard = open_lock(cache, "retention")?;
    if guard.try_lock().is_err() {
        return Ok(());
    }
    let mut entries: Vec<_> = fs::read_dir(cache)?
        .filter_map(|item| {
            let name = item.ok()?.file_name().into_string().ok()?;
            entry(cache, name).ok()
        })
        .collect();
    let mut total = entries
        .iter()
        .fold(0u64, |total, entry| total.saturating_add(entry.bytes));
    let mut count = entries.len();
    entries.sort_unstable_by(|a, b| a.used.cmp(&b.used).then_with(|| a.digest.cmp(&b.digest)));
    for candidate in entries {
        if total <= max_bytes && count <= max_entries {
            break;
        }
        let lease = open_lock(cache, bucket(&candidate.digest)?)?;
        if lease.try_lock().is_err() {
            continue;
        }
        // Recheck the exact managed path and its recency under exclusive ownership.
        let Ok(current) = entry(cache, candidate.digest) else {
            continue;
        };
        if current.used != candidate.used {
            continue;
        }
        let object = cache.join(&current.digest);
        if fs::remove_dir_all(&object).is_ok() {
            total = total.saturating_sub(current.bytes);
            count -= 1;
        }
    }
    Ok(())
}

/// A path manually selected from this profile's cache still protects its managed object.
pub(in crate::plugins::tools) fn lease_existing(
    profile: &Path,
    path: &Path,
    closed: &AtomicBool,
) -> Result<Option<Lease>> {
    let cache = match profile.canonicalize() {
        Ok(profile) => profile.join("tools"),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let Some(digest) = path
        .strip_prefix(&cache)
        .ok()
        .and_then(|relative| relative.iter().next())
        .and_then(|name| name.to_str())
        .filter(|digest| validate_digest(digest).is_ok())
    else {
        return Ok(None);
    };
    let lease = Lease::acquire(&cache, digest, closed)?;
    ensure!(path.is_file(), "Native tool cache entry was reclaimed");
    Ok(Some(lease))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::{
        package::{ToolDownload, hash_bytes},
        tools::{PreparedExecutable, archive::prepare_with},
    };
    use std::io::Write;

    fn fixture(profile: &Path, body: &[u8]) -> Result<PreparedExecutable> {
        let mut zip = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        for path in ["bin/tool.exe", "bin/dependency.dat"] {
            zip.start_file(path, zip::write::SimpleFileOptions::default())?;
            zip.write_all(body)?;
        }
        let archive = zip.finish()?.into_inner();
        let download = ToolDownload {
            url: "https://example.test/tool.zip".into(),
            sha256: hash_bytes(&archive),
            executable: "bin/tool.exe".into(),
        };
        prepare_with(profile, &download, &AtomicBool::new(false), |directory| {
            let mut staged = tempfile::NamedTempFile::new_in(directory)?;
            staged.write_all(&archive)?;
            Ok(staged)
        })
    }

    fn prune_until(
        cache: &Path,
        bytes: u64,
        count: usize,
        complete: impl Fn() -> bool,
    ) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !complete() {
            trim_to(cache, bytes, count)?;
            ensure!(Instant::now() < deadline, "Cache cleanup did not finish");
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }

    #[test]
    fn retention_keeps_shared_executables_and_dependencies_until_the_last_release() -> Result<()> {
        let profile = tempfile::tempdir()?;
        let first = fixture(profile.path(), b"shared tool fixture")?;
        let second = fixture(profile.path(), b"shared tool fixture")?;
        let candidate = fixture(profile.path(), b"unreferenced tool fixture")?;
        let executable = first.path().to_path_buf();
        let dependency = executable.with_file_name("dependency.dat");
        let discarded = candidate.path().to_path_buf();
        let cache = profile.path().join("tools").canonicalize()?;
        let user_tool = profile.path().join("user-tool.exe");
        fs::write(&user_tool, b"user-managed tool")?;
        let foreign = cache.join("f".repeat(64));
        fs::create_dir(&foreign)?;

        drop(candidate);
        prune_until(&cache, 0, 0, || !discarded.exists())?;
        assert_eq!(fs::read(&executable)?, b"shared tool fixture");
        assert_eq!(fs::read(&dependency)?, b"shared tool fixture");
        drop(first);
        trim_to(&cache, 0, 0)?;
        assert!(second.path().is_file());
        assert!(dependency.is_file());
        drop(second);
        prune_until(&cache, 0, 0, || !executable.exists())?;
        assert!(!dependency.exists());
        assert_eq!(fs::read(user_tool)?, b"user-managed tool");
        assert!(foreign.is_dir());
        Ok(())
    }

    #[test]
    fn retention_evicts_the_least_recent_unreserved_archive_first() -> Result<()> {
        let profile = tempfile::tempdir()?;
        let earlier = fixture(profile.path(), b"earlier fixture")?;
        let later = fixture(profile.path(), b"later fixture")?;
        let first = earlier.path().to_path_buf();
        let second = later.path().to_path_buf();
        // An archive published by an earlier version has no stored size.
        fs::remove_file(first.ancestors().nth(3).unwrap().join("size.json"))?;
        for (offset, executable) in [first.as_path(), second.as_path()].into_iter().enumerate() {
            let index = executable.ancestors().nth(3).unwrap().join("files.json");
            OpenOptions::new().write(true).open(index)?.set_modified(
                SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000 + offset as u64),
            )?;
        }
        drop(earlier);
        drop(later);
        let cache = profile.path().join("tools").canonicalize()?;
        prune_until(&cache, u64::MAX, 1, || !first.exists())?;
        assert_eq!(fs::read(second)?, b"later fixture");
        Ok(())
    }

    #[test]
    fn releasing_a_tool_rechecks_the_budget_off_thread() -> Result<()> {
        let profile = tempfile::tempdir()?;
        let tool = fixture(profile.path(), b"active budget fixture")?;
        let path = tool.path().to_path_buf();
        let object = path.ancestors().nth(3).unwrap();
        write_json(&object.join("size.json"), &(CACHE_BYTES + 1))?;
        let cache = profile.path().join("tools").canonicalize()?;
        trim(&cache);
        assert!(path.is_file());

        drop(tool);
        let deadline = Instant::now() + Duration::from_secs(3);
        while path.exists() {
            ensure!(
                Instant::now() < deadline,
                "Final release did not schedule cache cleanup"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }
}
