//! Lazily expanded filesystem tree with persistent path-based marks.
use super::EntryIcon;
use anyhow::Result;
use nanobug::filesystem::{DirectoryOptions, read_directory};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

/// One visible file-manager entry.
pub struct Entry {
    /// Full filesystem path.
    pub path: PathBuf,
    /// Zero-based nesting depth below the project root.
    pub depth: usize,
    /// Whether this entry is an actual directory rather than a followed symlink.
    pub directory: bool,
    /// One bit per depth where this entry or its ancestor has a later visible sibling.
    continuing_branches: u64,
}

impl Entry {
    /// Whether the branch at `depth` continues past this row to a later sibling.
    pub fn branch_continues(&self, depth: usize) -> bool {
        self.continuing_branches & (1 << depth) != 0
    }

    /// Choose this entry's Material icon, using `expanded` for directories.
    pub(crate) fn icon(&self, expanded: bool) -> crate::ui::icons::Icon {
        EntryIcon::for_path(&self.path, self.directory, expanded).image()
    }
}

/// Project tree state, retained while focus moves into the editor.
pub struct Explorer {
    /// Current project directory.
    pub root: PathBuf,
    /// Flattened visible tree, sorted with directories first.
    pub entries: Vec<Entry>,
    /// Selected visible entry.
    pub selected: usize,
    /// Files and directories marked for a batch operation, including collapsed descendants.
    pub marked: HashSet<PathBuf>,
    /// Directories whose immediate children have been requested.
    pub expanded: HashSet<PathBuf>,
}

impl Explorer {
    /// Scan the first level of `root`, without recursively indexing the project.
    pub fn new(root: PathBuf) -> Self {
        let mut tree = Self::empty(root);
        let _ = tree.refresh();
        tree
    }

    /// Read the immediate children of `root` in the same order as a new Explorer.
    pub(in crate::grid) fn preview(root: PathBuf) -> Result<Vec<Entry>> {
        Self::directory_preview(root, false, &|_| true)
    }

    /// Read `root` with optional `follow_directory_links` and the `include_file` predicate.
    fn directory_preview(
        root: PathBuf,
        follow_directory_links: bool,
        include_file: &impl Fn(&Path) -> bool,
    ) -> Result<Vec<Entry>> {
        let tree = Self::empty(root);
        let mut entries = Vec::new();
        tree.walk(
            &tree.root,
            0,
            &mut entries,
            follow_directory_links,
            include_file,
        )?;
        Ok(entries)
    }

    /// Prepare a tree at `root` before reading its directory.
    fn empty(root: PathBuf) -> Self {
        Self {
            root,
            entries: Vec::new(),
            selected: 0,
            marked: HashSet::new(),
            expanded: HashSet::new(),
        }
    }

    /// Refresh visible entries and retain the selected path when it still exists.
    pub fn refresh(&mut self) -> Result<()> {
        let selected = self.entries.get(self.selected).map(|e| e.path.clone());
        let mut entries = Vec::new();
        self.walk(&self.root, 0, &mut entries, false, &|_| true)?;
        self.entries = entries;
        self.selected = selected
            .and_then(|p| self.entries.iter().position(|e| e.path == p))
            .unwrap_or(0);
        self.marked.retain(|path| !matches!(fs::symlink_metadata(path), Err(error) if error.kind() == std::io::ErrorKind::NotFound));
        Ok(())
    }

    /// Toggle the current entry's mark, returning the number of marked paths.
    pub fn toggle_mark(&mut self) -> usize {
        if let Some(entry) = self.entries.get(self.selected)
            && !self.marked.remove(&entry.path)
        {
            self.marked.insert(entry.path.clone());
        }
        self.marked.len()
    }

    /// Use the current directory, or the containing directory of a file, as the paste destination.
    pub fn directory(&self) -> &Path {
        match self.entries.get(self.selected) {
            Some(entry) if entry.directory => &entry.path,
            Some(entry) => entry.path.parent().unwrap_or(&self.root),
            None => &self.root,
        }
    }

    /// Append children of `directory` at `depth` to `output`, applying `include_file` before sorting.
    /// `follow_directory_links` treats links to directories as navigable entries.
    fn walk(
        &self,
        directory: &Path,
        depth: usize,
        output: &mut Vec<Entry>,
        follow_directory_links: bool,
        include_file: &impl Fn(&Path) -> bool,
    ) -> Result<()> {
        if depth > 32 || output.len() >= 10_000 {
            return Ok(());
        }
        let children = read_directory(
            directory,
            DirectoryOptions {
                follow_directory_links,
                hide_transfer_staging: directory == self.root,
            },
            include_file,
        )?;
        let mut branch_start = output.len();
        for nanobug::filesystem::DirectoryEntry { path, directory } in children {
            if output.len() >= 10_000 {
                break;
            }
            // The previous sibling's guide continues only if this sibling fits in the tree.
            for entry in &mut output[branch_start..] {
                entry.continuing_branches |= 1 << depth;
            }
            branch_start = output.len();
            let open = directory && self.expanded.contains(&path);
            output.push(Entry {
                path: path.clone(),
                depth,
                directory,
                continuing_branches: 0,
            });
            if open {
                let _ = self.walk(
                    &path,
                    depth + 1,
                    output,
                    follow_directory_links,
                    include_file,
                );
            }
        }
        Ok(())
    }

    /// Activate the selected entry; return a file to open or toggle a directory's expansion.
    pub fn activate(&mut self) -> Option<PathBuf> {
        let entry = self.entries.get(self.selected)?;
        if !entry.directory {
            return Some(entry.path.clone());
        }
        let path = entry.path.clone();
        if !self.expanded.remove(&path) {
            self.expanded.insert(path);
        }
        let _ = self.refresh();
        None
    }

    /// When `expand` is true, expand the selected directory. Otherwise fold the nearest branch.
    pub fn branch(&mut self, expand: bool) {
        let Some(entry) = self.entries.get(self.selected) else {
            return;
        };
        let path = entry.path.clone();
        if expand && entry.directory {
            self.expanded.insert(path);
        } else if !expand
            && !self.expanded.remove(&path)
            && let Some(parent) = path.parent()
            && let Some(index) = self.entries.iter().position(|e| e.path == parent)
        {
            self.selected = index;
            self.expanded.remove(parent);
        }
        let _ = self.refresh();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Guides continue across nested rows and stop at each group's last visible sibling.
    #[test]
    fn guide_continuations_follow_visible_siblings() -> Result<()> {
        let project = tempfile::tempdir()?;
        let root = project.path();
        for leaf in ["first/nested/leaf", "first/sibling", "last/nested/leaf"] {
            fs::create_dir_all(root.join(leaf))?;
        }
        let mut explorer = Explorer::new(root.to_path_buf());
        explorer.expanded.extend(
            ["first", "first/nested", "last", "last/nested"].map(|directory| root.join(directory)),
        );
        explorer.refresh()?;
        let rows = |tree: &Explorer| {
            tree.entries
                .iter()
                .map(|entry| (entry.depth, entry.continuing_branches))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            rows(&explorer),
            [
                (0, 0b01),
                (1, 0b11),
                (2, 0b11),
                (1, 0b01),
                (0, 0),
                (1, 0),
                (2, 0)
            ]
        );
        explorer.branch(false);
        assert_eq!(rows(&explorer), [(0, 0b01), (0, 0), (1, 0), (2, 0)]);
        Ok(())
    }
}
