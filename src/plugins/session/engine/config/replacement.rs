//! Retain the exact file displaced by a project configuration replacement.

use super::{LIMIT, hash_bytes, read_optional};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

const PREFIX: &str = ".nanobug-apply-";
const CANDIDATE: &str = "candidate.json";
const BACKUP: &str = "displaced.json";

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Replacement {
    directory: String,
    project: RecordedPath,
    #[cfg(not(windows))]
    candidate_identity: (u64, u64),
}

/// Journal the native path without discarding filenames that are not valid Unicode.
#[cfg(windows)]
type RecordedPath = Vec<u16>;
#[cfg(unix)]
type RecordedPath = Vec<u8>;
#[cfg(not(any(windows, unix)))]
type RecordedPath = Vec<u8>;

fn record_path(path: &Path) -> RecordedPath {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        path.as_os_str().encode_wide().collect()
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    }
    #[cfg(not(any(windows, unix)))]
    path.to_string_lossy().as_bytes().to_vec()
}

fn restore_path(path: &RecordedPath) -> PathBuf {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        std::ffi::OsString::from_wide(path).into()
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        std::ffi::OsStr::from_bytes(path).into()
    }
    #[cfg(not(any(windows, unix)))]
    String::from_utf8_lossy(path).into_owned().into()
}

pub(super) enum Displaced {
    None,
    Expected,
    Conflict,
    #[cfg(windows)]
    RestoreRequired,
}

impl Displaced {
    pub(super) fn must_preserve(&self) -> bool {
        match self {
            Self::Conflict => true,
            #[cfg(windows)]
            Self::RestoreRequired => true,
            _ => false,
        }
    }
}

/// Flush a rename's parent directory on systems exposing a non-privileged directory sync.
pub(super) fn sync_parent(path: &Path) -> io::Result<()> {
    sync_directory(path.parent().ok_or(io::ErrorKind::InvalidInput)?)
}

impl Replacement {
    /// Stage and sync a file on the same volume before adding the transaction to the journal.
    pub(super) fn stage(project: &Path, bytes: &[u8]) -> Result<Self> {
        let parent = project
            .parent()
            .context("Missing project configuration directory")?;
        let directory = tempfile::Builder::new().prefix(PREFIX).tempdir_in(parent)?;
        let mut candidate = tempfile::NamedTempFile::new_in(directory.path())?;
        candidate.write_all(bytes)?;
        candidate.as_file().sync_all()?;
        #[cfg(not(windows))]
        let candidate_identity = identity(&candidate.as_file().metadata()?);
        candidate.persist_noclobber(directory.path().join(CANDIDATE))?;
        sync_directory(directory.path())?;
        sync_directory(parent)?;
        let name = directory
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .context("Invalid Apply transaction directory")?
            .to_owned();
        let project = record_path(project);
        ensure!(
            project.len() <= 32 * 1024,
            "Project configuration path is too long to journal"
        );
        let _ = directory.keep();
        Ok(Self {
            directory: name,
            project,
            #[cfg(not(windows))]
            candidate_identity,
        })
    }

    fn directory(&self, project: &Path) -> Result<PathBuf> {
        ensure!(
            self.directory
                .strip_prefix(PREFIX)
                .is_some_and(|suffix| !suffix.is_empty()
                    && suffix.len() <= 32
                    && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())),
            "Invalid Apply transaction directory"
        );
        let parent = project
            .parent()
            .context("Missing project configuration directory")?;
        ensure!(
            project == restore_path(&self.project),
            "The project configuration path changed during Apply"
        );
        let directory = parent.join(&self.directory);
        ensure!(
            directory.canonicalize()? == directory,
            "Apply transaction directory was replaced"
        );
        Ok(directory)
    }

    /// Recover a recorded sidecar even if the project file has since been removed or replaced.
    pub(super) fn recorded_project(&self, root: &Path) -> Result<PathBuf> {
        let root = root.canonicalize()?;
        let project = restore_path(&self.project);
        let expected = project
            .parent()
            .context("Missing recorded Apply directory")?;
        let parent = expected.canonicalize()?;
        ensure!(
            parent == expected && parent.starts_with(root),
            "The saved Apply directory is outside the project"
        );
        Ok(project)
    }

    pub(super) fn publish(&self, project: &Path) -> Result<()> {
        let directory = self.directory(project)?;
        let candidate = directory.join(CANDIDATE);
        ensure!(
            fs::symlink_metadata(project)?.is_file(),
            "The project configuration is no longer a regular file"
        );
        #[cfg(windows)]
        let backup = directory.join(BACKUP);
        #[cfg(windows)]
        let result = replace_windows(project, &candidate, &backup);
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let result = rustix::fs::renameat_with(
            rustix::fs::CWD,
            &candidate,
            rustix::fs::CWD,
            project,
            rustix::fs::RenameFlags::EXCHANGE,
        )
        .map_err(io::Error::from);
        #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
        let result: io::Result<()> = Err(io::Error::from(io::ErrorKind::Unsupported));
        result.context("Could not replace project configuration while retaining the original")?;
        // POSIX exchange leaves the original at the candidate's name until recovery records it.
        #[cfg(not(windows))]
        fs::rename(candidate, directory.join(BACKUP))?;
        sync_directory(&directory)?;
        sync_parent(project)?;
        Ok(())
    }

    /// Recover an exchanged candidate and keep any unexpected backup exactly as it was displaced.
    pub(super) fn recover(&self, project: &Path, before: &str) -> Result<Displaced> {
        let directory = self.directory(project)?;
        let backup = directory.join(BACKUP);
        #[cfg(not(windows))]
        {
            let candidate = directory.join(CANDIDATE);
            if absent(&backup)?
                && !absent(&candidate)?
                && identity(&fs::symlink_metadata(&candidate)?) != self.candidate_identity
            {
                fs::rename(&candidate, &backup)?;
                sync_directory(&directory)?;
            }
        }
        if absent(&backup)? {
            return Ok(Displaced::None);
        }
        // ReplaceFileW can fail after moving the original. A missing path may be restored, but a
        // path re-created by another writer must never be replaced by automatic recovery.
        #[cfg(windows)]
        if !absent(&directory.join(CANDIDATE))?
            && absent(project)?
            && fs::symlink_metadata(&backup)?.is_file()
        {
            // The original's backup is a separate file, never a second link to an editable path.
            // If it cannot be read within the cap, recovery retains it without publishing bytes.
            let mut expected_restored = false;
            if let Ok(Some(bytes)) = read_optional(&backup, LIMIT) {
                expected_restored =
                    restore_missing(project, &bytes).is_ok() && hash_bytes(&bytes) == before;
            }
            return Ok(if expected_restored {
                Displaced::Expected
            } else {
                Displaced::RestoreRequired
            });
        }
        Ok(if file_digest(&backup)?.as_deref() == Some(before) {
            Displaced::Expected
        } else {
            Displaced::Conflict
        })
    }

    pub(super) fn backup(&self, project: &Path) -> Result<PathBuf> {
        Ok(self.directory(project)?.join(BACKUP))
    }

    /// Remove only recognized transaction files once the final journal record is published.
    pub(super) fn cleanup(&self, project: &Path) {
        if let Ok(directory) = self.directory(project) {
            let _ = fs::remove_file(directory.join(CANDIDATE));
            let _ = fs::remove_file(directory.join(BACKUP));
            let _ = fs::remove_dir(directory);
        }
    }
}

#[cfg(windows)]
fn restore_missing(project: &Path, bytes: &[u8]) -> Result<()> {
    let parent = project
        .parent()
        .context("Missing project configuration directory")?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    staged.write_all(bytes)?;
    staged.as_file().sync_all()?;
    staged.persist_noclobber(project)?;
    Ok(())
}

fn absent(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error),
    }
}

fn file_digest(path: &Path) -> Result<Option<String>> {
    let info = fs::symlink_metadata(path)?;
    if !info.is_file() || info.len() > LIMIT {
        return Ok(None);
    }
    Ok(read_optional(path, LIMIT)
        .ok()
        .flatten()
        .map(|bytes| hash_bytes(&bytes)))
}

#[cfg(unix)]
fn identity(metadata: &fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (metadata.dev(), metadata.ino())
}

#[cfg(not(any(unix, windows)))]
fn identity(_metadata: &fs::Metadata) -> (u64, u64) {
    (0, 0)
}

fn sync_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    return fs::File::open(path)?.sync_all();
    // Windows does not expose a non-privileged directory sync with these semantics.
    #[cfg(not(unix))]
    let _ = path;
    #[cfg(not(unix))]
    Ok(())
}

#[cfg(windows)]
fn replace_windows(project: &Path, candidate: &Path, backup: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::ReplaceFileW;
    let wide = |path: &Path| {
        path.as_os_str()
            .encode_wide()
            .chain([0])
            .collect::<Vec<_>>()
    };
    let project = wide(project);
    let candidate = wide(candidate);
    let backup = wide(backup);
    // All strings are owned, terminated Windows paths; no optional reserved values are provided.
    let result = unsafe {
        ReplaceFileW(
            project.as_ptr(),
            candidate.as_ptr(),
            backup.as_ptr(),
            0,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn windows_partial_replacement_only_restores_a_missing_path() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let project = temp.path().canonicalize()?.join("plugins.json");
        let transaction = Replacement::stage(&project, b"draft")?;
        let displaced = transaction.backup(&project)?;
        fs::write(&displaced, b"external one")?;
        assert!(matches!(
            transaction.recover(&project, &hash_bytes(b"old"))?,
            Displaced::RestoreRequired
        ));
        assert_eq!(fs::read(&project)?, b"external one");
        fs::write(&project, b"external two")?;
        assert!(restore_missing(&project, b"external one").is_err());
        assert_eq!(fs::read(&project)?, b"external two");
        assert_eq!(fs::read(&displaced)?, b"external one");
        Ok(())
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn exchange_interruption_recognizes_the_displaced_inode_even_when_its_bytes_match() -> Result<()>
    {
        let temp = tempfile::tempdir()?;
        let project = temp.path().canonicalize()?.join("plugins.json");
        fs::write(&project, b"draft")?;
        let transaction = Replacement::stage(&project, b"draft")?;
        let candidate = transaction.directory(&project)?.join(CANDIDATE);
        rustix::fs::renameat_with(
            rustix::fs::CWD,
            candidate,
            rustix::fs::CWD,
            &project,
            rustix::fs::RenameFlags::EXCHANGE,
        )?;
        assert!(matches!(
            transaction.recover(&project, &hash_bytes(b"old"))?,
            Displaced::Conflict
        ));
        assert_eq!(fs::read(transaction.backup(&project)?)?, b"draft");
        Ok(())
    }
}
