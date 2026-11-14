//! Project history shared by start pages and persisted across launches.

use nanobug::app_data;
use std::{
    fs,
    io::{BufReader, ErrorKind, Write},
    path::{Path, PathBuf},
};

/// Opened project roots in newest-first order, with an optional backing file.
pub(super) struct RecentProjects {
    /// History file for this editor installation.
    path: Option<PathBuf>,
    /// Canonical project roots, deduplicated and ordered by last open.
    pub(super) roots: Vec<PathBuf>,
    /// Local opens awaiting a successful write, including paths that JSON cannot encode.
    pending: Vec<PathBuf>,
    /// Latest read or write failure, displayed when viewing the list.
    pub(super) error: Option<String>,
}

impl RecentProjects {
    /// Keep experimental project paths local to this window.
    pub(super) fn in_memory() -> Self {
        Self {
            path: None,
            roots: Vec::new(),
            pending: Vec::new(),
            error: None,
        }
    }

    /// Load this installation's history and record the startup `roots` in their display order.
    pub(super) fn new(roots: impl Iterator<Item = PathBuf>) -> Self {
        let (path, error) = match app_data::recent_projects_file() {
            Ok(path) => (Some(path), None),
            Err(error) => (None, Some(format!("{error:#}"))),
        };
        let mut recent = Self {
            path,
            roots: Vec::new(),
            pending: Vec::new(),
            error,
        };
        recent.refresh();
        let roots: Vec<_> = roots.filter(|root| root.is_dir()).collect();
        if !roots.is_empty() {
            recent.record_roots(&roots);
        }
        recent
    }

    /// Refresh history from disk, keeping only locally opened paths that still need writing.
    pub(super) fn refresh(&mut self) {
        if let Some(path) = &self.path {
            match read(path) {
                Ok(mut roots) => {
                    append_missing(&mut roots, &self.pending);
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

    /// Remove `root` from history after a successful write, or from memory when no file is configured.
    pub(super) fn remove(&mut self, root: &Path) -> anyhow::Result<()> {
        self.update(|roots| roots.retain(|entry| entry != root))
    }

    /// Promote `opened` in order and merge concurrent writes before replacing the history file.
    fn record_roots(&mut self, opened: &[PathBuf]) {
        if let Err(error) = self.update(|roots| promote(roots, opened)) {
            self.error = Some(error.to_string());
            promote(&mut self.pending, opened);
            promote(&mut self.roots, opened);
        }
    }

    /// Apply `edit` to the latest history, replacing the local snapshot only on success.
    fn update(&mut self, edit: impl FnOnce(&mut Vec<PathBuf>)) -> anyhow::Result<()> {
        if let Some(path) = &self.path {
            let roots = write(path, |roots| {
                append_missing(roots, &self.pending);
                edit(roots);
            })?;
            self.pending = roots
                .iter()
                .filter(|root| root.to_str().is_none())
                .cloned()
                .collect();
            self.roots = roots;
            self.error = None;
        } else {
            edit(&mut self.roots);
        }
        Ok(())
    }
}

/// Read the snapshot at `path`, treating a first launch as empty history.
fn read(path: &Path) -> anyhow::Result<Vec<PathBuf>> {
    match fs::File::open(path) {
        Ok(file) => Ok(serde_json::from_reader(BufReader::new(file))?),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.into()),
    }
}

/// Apply `edit` to the shared history at `path` under its lock and return the saved snapshot.
fn write(path: &Path, edit: impl FnOnce(&mut Vec<PathBuf>)) -> anyhow::Result<Vec<PathBuf>> {
    let _lock = app_data::lock_recent_projects(path)?;
    let dir = path.parent().expect("history path has a parent");
    let mut roots = read(path)?;
    edit(&mut roots);
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

    /// Successive opens promote projects and deduplicate their history entries.
    #[test]
    fn history_merges_and_deduplicates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recent-projects.json");
        let first = dir.path().join("first");
        let second = dir.path().join("second");
        assert!(read(&path).unwrap().is_empty());
        write(&path, |roots| promote(roots, std::slice::from_ref(&first))).unwrap();
        write(&path, |roots| promote(roots, std::slice::from_ref(&second))).unwrap();
        assert_eq!(read(&path).unwrap(), [second.clone(), first.clone()]);
        write(&path, |roots| promote(roots, std::slice::from_ref(&first))).unwrap();
        assert_eq!(read(&path).unwrap(), [first, second]);
    }

    /// A refresh or new open from a stale instance must honor a removal on disk.
    #[test]
    fn removed_root_stays_out_of_history_until_opened_again() {
        let dir = tempfile::tempdir().unwrap();
        let history = || RecentProjects {
            path: Some(dir.path().join("recent-projects.json")),
            roots: Vec::new(),
            pending: Vec::new(),
            error: None,
        };
        let root = dir.path().join("project");
        fs::create_dir(&root).unwrap();
        let mut writer = history();
        writer.record(&root);
        let mut reader = history();
        reader.refresh();
        let mut opener = history();
        opener.refresh();

        writer.remove(&root).unwrap();
        assert!(writer.roots.is_empty());
        assert!(root.is_dir());
        reader.refresh();
        assert!(reader.roots.is_empty());
        let other = dir.path().join("other");
        opener.record(&other);
        reader.refresh();
        assert_eq!(reader.roots, std::slice::from_ref(&other));
        opener.record(&root);
        reader.refresh();
        assert_eq!(reader.roots, [root, other]);
    }
}
