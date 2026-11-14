//! Deterministic, shallow directory enumeration shared by trees and file pickers.

use super::transfer::STASH_PREFIX;
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Filesystem facts without tree expansion, selection or icon state.
pub struct DirectoryEntry {
    /// Visible path returned by the directory read.
    pub path: PathBuf,
    /// Whether the path is a directory under the requested link policy.
    pub directory: bool,
}

/// The tree and picker deliberately use different policies for directory links.
#[derive(Default)]
pub struct DirectoryOptions {
    /// Let the picker enter a link that points to a directory.
    pub follow_directory_links: bool,
    /// Hide the editor's private transfer history at the root of a listing.
    pub hide_transfer_staging: bool,
}

/// Read `directory`, applying `options` and `include_file`, then sort directories before files.
/// Return at most 10,000 entries; unreadable individual entries are skipped as in the tree.
pub fn read_directory(
    directory: &Path,
    options: DirectoryOptions,
    include_file: impl Fn(&Path) -> bool,
) -> anyhow::Result<Vec<DirectoryEntry>> {
    let mut children: Vec<_> = fs::read_dir(directory)?
        .filter_map(Result::ok)
        .filter(|entry| {
            !options.hide_transfer_staging
                || !entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(STASH_PREFIX)
        })
        .filter_map(|entry| {
            let kind = entry.file_type().ok()?;
            let path = entry.path();
            let directory = kind.is_dir()
                || options.follow_directory_links && kind.is_symlink() && path.is_dir();
            (directory || include_file(&path)).then_some(DirectoryEntry { path, directory })
        })
        .collect();
    children.sort_by_key(|entry| {
        (
            !entry.directory,
            entry
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase(),
        )
    });
    children.truncate(10_000);
    Ok(children)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trees and extension-filtered pickers agree on ordering and private staging visibility.
    #[test]
    fn filtered_listing_keeps_directories_and_hides_transfer_history() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        fs::create_dir(root.path().join("z-directory"))?;
        fs::create_dir(root.path().join(format!("{STASH_PREFIX}undo")))?;
        fs::write(root.path().join("B.rs"), "b")?;
        fs::write(root.path().join("a.rs"), "a")?;
        fs::write(root.path().join("skip.txt"), "x")?;
        let entries = read_directory(
            root.path(),
            DirectoryOptions {
                hide_transfer_staging: true,
                ..Default::default()
            },
            |path| path.extension().is_some_and(|extension| extension == "rs"),
        )?;
        let names: Vec<_> = entries
            .iter()
            .map(|entry| entry.path.file_name().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(names, ["z-directory", "a.rs", "B.rs"]);
        assert!(entries[0].directory);
        let visible = read_directory(root.path(), DirectoryOptions::default(), |_| true)?;
        assert_eq!(visible.len(), 5);
        Ok(())
    }
}
