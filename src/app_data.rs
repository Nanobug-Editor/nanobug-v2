//! Data stored beside the editor executable.

pub mod settings;

use anyhow::{Context, Result, ensure};
use std::{
    env,
    fs::{self, File},
    io::{self, ErrorKind},
    path::{Path, PathBuf},
};

/// Locate the `data` directory beside the editor executable.
pub(crate) fn directory_for(editor: &Path) -> Result<PathBuf> {
    Ok(editor
        .parent()
        .context("Editor installation directory is unavailable")?
        .join("data"))
}

/// Open history beside the running editor, importing per-user history on first use.
pub fn recent_projects_file() -> Result<PathBuf> {
    let legacy = legacy_history_file();
    import_history(&env::current_exe()?, legacy.as_deref())
}

/// Return the history path beside `editor`, seeding it from `legacy` on first use.
fn import_history(editor: &Path, legacy: Option<&Path>) -> Result<PathBuf> {
    let path = directory_for(editor)?.join("recent-projects.json");
    import_history_snapshot(legacy, &path)
        .with_context(|| format!("Import recent projects into {}", path.display()))?;
    Ok(path)
}

/// Seed `path` once from `legacy`, coordinating with normal history writers.
fn import_history_snapshot(legacy: Option<&Path>, path: &Path) -> Result<()> {
    let imported = path.with_extension("imported");
    if imported.try_exists()? {
        return Ok(());
    }
    let _lock = lock_recent_projects(path)?;
    if imported.try_exists()? {
        return Ok(());
    }
    if let Some(legacy) = legacy {
        copy_missing_file(legacy, path)?;
    }
    fs::File::create_new(imported)?;
    Ok(())
}

/// Lock recent-project updates at `path` across the snapshot's atomic replacements.
pub fn lock_recent_projects(path: &Path) -> Result<File> {
    lock_data_file(path)
}

/// Lock the sidecar of `path` across atomic replacements of the data file.
pub(crate) fn lock_data_file(path: &Path) -> Result<File> {
    let dir = path.parent().context("Data file has no parent")?;
    fs::create_dir_all(dir)?;
    let lock = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.with_extension("lock"))?;
    lock.lock()?;
    Ok(lock)
}

/// Locate the history written by older builds to the per-user directory.
fn legacy_history_file() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/Application Support"))
    } else {
        env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
    };
    base.map(|base| base.join("Nanobug").join("recent-projects.json"))
}

/// Copy a saved `source` to `destination` atomically when the destination is absent.
/// A missing source or a destination published by another editor needs no import.
pub(crate) fn copy_missing_file(source: &Path, destination: &Path) -> Result<()> {
    if destination.try_exists()? {
        return Ok(());
    }
    let mut input = match File::open(source) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    ensure!(
        input.metadata()?.is_file(),
        "{} is not a file",
        source.display()
    );
    let parent = destination.parent().context("Data file has no parent")?;
    fs::create_dir_all(parent)?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    io::copy(&mut input, staged.as_file_mut())?;
    staged.as_file().sync_all()?;
    match staged.persist_noclobber(destination) {
        Ok(_) => Ok(()),
        Err(error) if error.error.kind() == ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Import the exact project order and keep subsequent edits to the portable history.
    #[test]
    fn recent_history_keeps_the_portable_snapshot_after_import() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let legacy = temp.path().join("recent-projects.json");
        let editor = temp.path().join("release/Nanobug.exe");
        let saved = br#"["most-recent", "older"]"#;
        fs::write(&legacy, saved)?;
        let path = import_history(&editor, Some(&legacy))?;
        assert_eq!(path, temp.path().join("release/data/recent-projects.json"));
        assert_eq!(fs::read(&path)?, saved);

        fs::write(&path, b"[]")?;
        import_history(&editor, Some(&legacy))?;
        assert_eq!(fs::read(&path)?, b"[]");
        fs::remove_file(&path)?;
        import_history(&editor, Some(&legacy))?;
        assert!(!path.exists());
        assert_eq!(fs::read(&legacy)?, saved);
        Ok(())
    }
}
