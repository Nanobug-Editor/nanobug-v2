//! Per-user project history shared by start pages and persisted across launches.

use std::{
    env, fs,
    io::{BufReader, ErrorKind, Write},
    path::{Path, PathBuf},
};

/// Opened project roots in newest-first order, with an optional backing file.
pub(super) struct RecentProjects {
    /// History location in the user's application state directory.
    path: Option<PathBuf>,
    /// Canonical project roots, deduplicated and ordered by last open.
    pub(super) roots: Vec<PathBuf>,
    /// Latest read or write failure, displayed when viewing the list.
    pub(super) error: Option<String>,
}

impl RecentProjects {
    /// Load history and record `roots` from explicit startup arguments in their display order.
    pub(super) fn new(roots: impl Iterator<Item = PathBuf>) -> Self {
        let mut recent = Self {
            path: state_dir().map(|dir| dir.join("recent-projects.json")),
            roots: Vec::new(),
            error: None,
        };
        recent.refresh();
        let roots: Vec<_> = roots.filter(|root| root.is_dir()).collect();
        if !roots.is_empty() {
            recent.record_roots(&roots);
        }
        recent
    }

    /// Refresh history from disk so openings in another instance appear in the list.
    pub(super) fn refresh(&mut self) {
        if let Some(path) = &self.path {
            match read(path) {
                Ok(mut roots) => {
                    append_missing(&mut roots, &self.roots);
                    self.roots = roots;
                }
                Err(error) => self.error = Some(error.to_string()),
            }
        }
    }

    /// Promote the newly opened canonical `root`.
    pub(super) fn record(&mut self, root: &Path) {
        self.record_roots(&[root.to_path_buf()]);
    }

    /// Promote `opened` in order and merge concurrent writes before replacing the history file.
    fn record_roots(&mut self, opened: &[PathBuf]) {
        if let Some(path) = &self.path {
            match write(path, &self.roots, opened) {
                Ok(roots) => {
                    self.roots = roots;
                    self.error = None;
                    return;
                }
                Err(error) => self.error = Some(error.to_string()),
            }
        }
        promote(&mut self.roots, opened);
    }
}

/// Resolve the platform's per-user state directory without using the project root.
fn state_dir() -> Option<PathBuf> {
    let dir = if cfg!(windows) {
        env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/Application Support"))
    } else {
        env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
    };
    dir.map(|dir| dir.join("Nanobug"))
}

/// Read the snapshot at `path`, treating a first launch as empty history.
fn read(path: &Path) -> anyhow::Result<Vec<PathBuf>> {
    match fs::File::open(path) {
        Ok(file) => Ok(serde_json::from_reader(BufReader::new(file))?),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.into()),
    }
}

/// Merge `known` and promote `opened` in the shared history at `path`, returning the new snapshot.
fn write(path: &Path, known: &[PathBuf], opened: &[PathBuf]) -> anyhow::Result<Vec<PathBuf>> {
    let dir = path.parent().expect("history path has a parent");
    fs::create_dir_all(dir)?;
    // Lock a separate file so the lock survives the atomic replacement of the snapshot.
    let lock = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.with_extension("lock"))?;
    lock.lock()?;
    let mut roots = read(path)?;
    append_missing(&mut roots, known);
    promote(&mut roots, opened);
    let mut staged = tempfile::NamedTempFile::new_in(dir)?;
    // Keep native paths in memory even if they cannot be represented in the JSON history.
    let serializable: Vec<_> = roots.iter().filter_map(|root| root.to_str()).collect();
    serde_json::to_writer_pretty(staged.as_file_mut(), &serializable)?;
    staged.write_all(b"\n")?;
    staged.persist(path)?;
    Ok(roots)
}

/// Append roots from `known` that are missing from the saved list.
fn append_missing(roots: &mut Vec<PathBuf>, known: &[PathBuf]) {
    for root in known {
        if !roots.contains(root) {
            roots.push(root.clone());
        }
    }
}

/// Move `opened` to the front of `roots`, keeping display order and removing duplicates.
fn promote(roots: &mut Vec<PathBuf>, opened: &[PathBuf]) {
    for root in opened.iter().rev() {
        roots.retain(|entry| entry != root);
        roots.insert(0, root.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Successive writers with stale snapshots preserve one another's entries and their order.
    #[test]
    fn history_merges_and_deduplicates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recent-projects.json");
        let first = dir.path().join("first");
        let second = dir.path().join("second");
        assert!(read(&path).unwrap().is_empty());
        write(&path, &[], std::slice::from_ref(&first)).unwrap();
        write(&path, &[], std::slice::from_ref(&second)).unwrap();
        assert_eq!(read(&path).unwrap(), [second.clone(), first.clone()]);
        write(&path, &[], std::slice::from_ref(&first)).unwrap();
        assert_eq!(read(&path).unwrap(), [first, second]);
    }
}
