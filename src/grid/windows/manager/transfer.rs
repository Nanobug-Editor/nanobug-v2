//! Explorer paste transactions, with a private holding area for undone copies.
use crate::grid::windows::PathChange;
use anyhow::{Context, Result, bail, ensure};
use nanobug::i18n;
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs::{self, File, Metadata},
    io::{self, Read},
    path::{Path, PathBuf},
};
use tempfile::{Builder, TempDir};

/// Prefix reserved for the private staging and history directories of a transfer.
pub const STASH_PREFIX: &str = ".nanobug-paste-";
/// Total number of paste actions retained across undo and redo.
const HISTORY_LIMIT: usize = 10;

/// Whether the clipboard keeps the source in place after paste.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TransferKind {
    /// Make an independent copy.
    Copy,
    /// Move the source on the next successful paste.
    Cut,
}

/// The original, staged, and visible locations of a pasted item.
struct PasteItem {
    /// Selected source, used to invalidate related history after an edit.
    source: PathBuf,
    /// Location before the paste or after undo. Copies use a private staging path.
    before: PathBuf,
    /// Visible location after paste or redo.
    after: PathBuf,
    /// Content and timestamp digest taken before the item becomes visible.
    stamp: [u8; 32],
}

impl PasteItem {
    /// Source and destination when this item moves `forward`, or the reverse pair for undo.
    fn paths(&self, forward: bool) -> (&PathBuf, &PathBuf) {
        if forward {
            (&self.before, &self.after)
        } else {
            (&self.after, &self.before)
        }
    }
}

/// One complete paste, with its staging directory kept alive for undo and redo.
struct Paste {
    /// Items in this transaction, with descendants of selected directories already deduplicated.
    items: Vec<PasteItem>,
    /// Owns the private storage for a copy. Cut transactions need no staged data.
    stage: Option<TempDir>,
    /// Allow browser transfers to cross filesystem boundaries.
    cross_volume: bool,
}

impl Paste {
    /// Move the complete paste `forward` or back, rolling back any partial move.
    /// `verify` checks replayed data again after moving it to detect edits during replay.
    fn apply(&mut self, forward: bool, verify: bool) -> Result<Vec<PathChange>> {
        for item in &self.items {
            let (from, to) = item.paths(forward);
            let parent = to
                .parent()
                .context(i18n::text("manager.missing_destination"))?;
            ensure!(
                fs::canonicalize(from)? == *from && fs::canonicalize(parent)? == parent,
                i18n::format("manager.path_changed", &[("path", &to.display())])
            );
            require_vacant(to)?;
        }
        let mut moved = 0;
        let result = (|| -> Result<()> {
            for item in &self.items {
                let (from, to) = item.paths(forward);
                move_entry(from, to, self.cross_volume).with_context(|| {
                    i18n::format(
                        "manager.move_failed",
                        &[("source", &from.display()), ("destination", &to.display())],
                    )
                })?;
                moved += 1;
                if verify {
                    ensure!(
                        fingerprint(to, !self.cross_volume)? == item.stamp,
                        i18n::format(
                            "manager.changed_during_replay",
                            &[("path", &from.display())]
                        )
                    );
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            let mut rollback_errors = Vec::new();
            for item in self.items[..moved].iter().rev() {
                let (old, new) = item.paths(forward);
                if let Err(error) = move_entry(new, old, self.cross_volume) {
                    rollback_errors.push(format!(
                        "{} → {} ({error})",
                        new.display(),
                        old.display()
                    ));
                }
            }
            if !rollback_errors.is_empty() {
                // A staged item may now hold the only copy of data edited during the move.
                if let Some(stage) = &mut self.stage {
                    stage.disable_cleanup(true);
                }
                bail!(i18n::format(
                    "manager.rollback_failed",
                    &[
                        ("items", &rollback_errors.join(", ")),
                        ("error", &format_args!("{error:#}"))
                    ],
                ));
            }
            return Err(error);
        }

        Ok(self
            .items
            .iter()
            .map(|item| {
                let (from, to) = item.paths(forward);
                if self.stage.is_some() {
                    PathChange {
                        from: item.after.clone(),
                        to: None,
                    }
                } else {
                    PathChange {
                        from: from.clone(),
                        to: Some(to.clone()),
                    }
                }
            })
            .collect())
    }

    /// Whether this action involves the changed `path` or any ancestor that contains it.
    fn touches(&self, path: &Path) -> bool {
        self.items
            .iter()
            .any(|item| path.starts_with(&item.source) || path.starts_with(&item.after))
    }
}

/// In-process filesystem clipboard and the ten most recent paste actions.
#[derive(Default)]
pub struct FileTransfers {
    /// Kind and deduplicated absolute paths captured by the most recent copy or cut.
    clipboard: Option<(TransferKind, Vec<PathBuf>)>,
    /// Completed pastes in chronological order.
    undo: Vec<Paste>,
    /// Undone pastes, with the next redo at the end.
    redo: Vec<Paste>,
}

impl FileTransfers {
    /// Capture `marked` paths for `kind`, letting a selected parent stand for its descendants.
    pub fn capture(&mut self, marked: &HashSet<PathBuf>, kind: TransferKind) -> Result<usize> {
        ensure!(!marked.is_empty(), i18n::text("manager.mark_first"));
        let mut paths: Vec<_> = marked
            .iter()
            .filter(|path| {
                !path
                    .ancestors()
                    .skip(1)
                    .any(|parent| marked.contains(parent))
            })
            .cloned()
            .collect();
        paths.sort();
        let count = paths.len();
        self.clipboard = Some((kind, paths));
        Ok(count)
    }

    /// Paste into `directory` inside `root`, rejecting conflicts with `dirty` open buffers.
    /// Return the visible path changes after the complete transaction succeeds.
    pub fn paste(
        &mut self,
        root: &Path,
        directory: &Path,
        dirty: &[PathBuf],
    ) -> Result<Vec<PathChange>> {
        self.paste_into(Some(root), directory, dirty)
    }

    /// Paste into a canonical browser `directory`, guarding all `dirty` open paths.
    pub fn paste_browser(
        &mut self,
        directory: &Path,
        dirty: &[PathBuf],
    ) -> Result<Vec<PathChange>> {
        self.paste_into(None, directory, dirty)
    }

    /// Paths a pending cut would move. Copy leaves this list empty.
    pub fn cut_paths(&self) -> &[PathBuf] {
        match &self.clipboard {
            Some((TransferKind::Cut, paths)) => paths,
            _ => &[],
        }
    }

    /// Paste into `directory` with dirty-buffer checks and the optional project boundary `root`.
    fn paste_into(
        &mut self,
        root: Option<&Path>,
        directory: &Path,
        dirty: &[PathBuf],
    ) -> Result<Vec<PathChange>> {
        let (kind, sources) = self
            .clipboard
            .as_ref()
            .context(i18n::text("manager.copy_first"))?;
        ensure!(
            root.is_none_or(|root| directory.starts_with(root))
                && entry_metadata(directory)?.is_dir()
                && fs::canonicalize(directory)? == directory,
            i18n::text("manager.destination_outside_project")
        );
        let mut targets = HashSet::new();
        let mut pairs = Vec::new();
        for source in sources {
            entry_metadata(source)?;
            ensure!(
                source.parent().is_some()
                    && root.is_none_or(|root| source != root && source.starts_with(root))
                    && fs::canonicalize(source)? == *source,
                i18n::text("manager.source_outside_project")
            );
            ensure!(
                !directory.starts_with(source),
                i18n::text("manager.paste_into_self")
            );
            let name = source
                .file_name()
                .context(i18n::text("manager.missing_filename"))?;
            let after = directory.join(name);
            require_vacant(&after)?;
            ensure!(
                targets.insert(after.clone()),
                i18n::format(
                    "manager.duplicate_destination",
                    &[("path", &after.display())]
                )
            );
            ensure!(
                dirty
                    .iter()
                    .all(|p| !p.starts_with(source) && !p.starts_with(&after)),
                i18n::text("manager.unsaved_files")
            );
            pairs.push((source.clone(), after));
        }

        let stage = (*kind == TransferKind::Copy)
            .then(|| {
                Builder::new()
                    .prefix(STASH_PREFIX)
                    .tempdir_in(root.unwrap_or(directory))
            })
            .transpose()?;
        let cross_volume = root.is_none();
        let mut items = Vec::new();
        for (index, (source, after)) in pairs.into_iter().enumerate() {
            let before = if let Some(stage) = &stage {
                let path = stage.path().join(index.to_string());
                copy_entry(&source, &path).with_context(|| {
                    i18n::format("manager.copy_failed", &[("path", &source.display())])
                })?;
                path
            } else {
                source.clone()
            };
            items.push(PasteItem {
                stamp: fingerprint(&before, !cross_volume)?,
                source,
                before,
                after,
            });
        }
        let mut paste = Paste {
            items,
            stage,
            cross_volume,
        };
        let changes = paste.apply(true, false)?;
        if *kind == TransferKind::Cut {
            self.clipboard = None;
        }
        self.redo.clear();
        self.undo.push(paste);
        if self.undo.len() > HISTORY_LIMIT {
            self.undo.remove(0);
        }
        Ok(changes)
    }

    /// Replay the last paste backward or, when `redo` is set, forward. Protect `dirty` buffers.
    pub fn replay(&mut self, redo: bool, dirty: &[PathBuf]) -> Result<Vec<PathChange>> {
        let (from, to) = if redo {
            (&mut self.redo, &mut self.undo)
        } else {
            (&mut self.undo, &mut self.redo)
        };
        let paste = from.last_mut().context(i18n::text(if redo {
            "manager.nothing_to_redo"
        } else {
            "manager.nothing_to_undo"
        }))?;
        ensure!(
            dirty.iter().all(|path| !paste.touches(path)),
            i18n::text("manager.unsaved_files")
        );
        let intact = paste.items.iter().all(|item| {
            let path = if redo { &item.before } else { &item.after };
            fingerprint(path, !paste.cross_volume).is_ok_and(|stamp| stamp == item.stamp)
        });
        if !intact {
            if redo && let Some(stage) = &mut paste.stage {
                stage.disable_cleanup(true);
                let path = stage.path().to_path_buf();
                from.pop();
                bail!(i18n::format(
                    "manager.staged_items_changed",
                    &[("path", &path.display())]
                ));
            }
            from.pop();
            bail!(i18n::text("manager.pasted_items_changed"));
        }
        let changes = paste.apply(redo, true)?;
        to.push(from.pop().expect("applied paste"));
        Ok(changes)
    }

    /// Forget the actions that involve an edited `path`, even when the edit is still unsaved.
    pub fn invalidate(&mut self, path: &Path) {
        self.undo.retain(|paste| !paste.touches(path));
        self.redo.retain(|paste| !paste.touches(path));
    }
}

/// Read metadata for a regular entry at `path`, without traversing links or Windows reparse points.
fn entry_metadata(path: &Path) -> Result<Metadata> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| i18n::format("document.read_failed", &[("path", &path.display())]))?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        ensure!(
            metadata.file_attributes() & 0x400 == 0,
            i18n::format("manager.unsupported_link", &[("path", &path.display())])
        );
    }
    ensure!(
        metadata.is_file() || metadata.is_dir(),
        i18n::format("manager.unsupported_entry", &[("path", &path.display())])
    );
    Ok(metadata)
}

/// Recursively copy a regular `source` to the private staging path `target`.
fn copy_entry(source: &Path, target: &Path) -> Result<()> {
    if entry_metadata(source)?.is_dir() {
        fs::create_dir(target)?;
        for child in fs::read_dir(source)? {
            let child = child?;
            copy_entry(&child.path(), &target.join(child.file_name()))?;
        }
    } else {
        fs::copy(source, target)?;
    }
    Ok(())
}

/// Digest contents and descendant names at `path`, optionally including file timestamps.
fn fingerprint(path: &Path, timestamps: bool) -> Result<[u8; 32]> {
    let mut digest = Sha256::new();
    hash_entry(path, &mut digest, &mut [0; 64 * 1024], timestamps)?;
    Ok(digest.finalize().into())
}

/// Add `path` and its children to `digest`, streaming through `buffer` and including requested `timestamps`.
fn hash_entry(path: &Path, digest: &mut Sha256, buffer: &mut [u8], timestamps: bool) -> Result<()> {
    let metadata = entry_metadata(path)?;
    if metadata.is_dir() {
        let mut children = fs::read_dir(path)?.collect::<io::Result<Vec<_>>>()?;
        children.sort_by_key(|entry| entry.file_name());
        digest.update(b"D");
        digest.update(children.len().to_le_bytes());
        for child in children {
            let name = child.file_name();
            let name = name.as_encoded_bytes();
            digest.update(name.len().to_le_bytes());
            digest.update(name);
            hash_entry(&child.path(), digest, buffer, timestamps)?;
        }
    } else {
        digest.update(b"F");
        digest.update(metadata.len().to_le_bytes());
        if timestamps {
            digest.update(format!("{:?}", metadata.modified()?));
        }
        let mut file = File::open(path)?;
        loop {
            let n = file.read(buffer)?;
            if n == 0 {
                break;
            }
            digest.update(&buffer[..n]);
        }
    }
    Ok(())
}

/// Move `source` into `target`, using staging only when a browser crosses volumes.
fn move_entry(source: &Path, target: &Path, cross_volume: bool) -> Result<()> {
    match move_exclusive(source, target) {
        Ok(()) => Ok(()),
        Err(error) if cross_volume && error.kind() == io::ErrorKind::CrossesDevices => {
            move_across_volumes(source, target)
        }
        Err(error) => Err(error.into()),
    }
}

/// Commit a cross-volume move from `source` to vacant `target`, restoring the source on failure.
fn move_across_volumes(source: &Path, target: &Path) -> Result<()> {
    let stash = |path: &Path| {
        Builder::new()
            .prefix(STASH_PREFIX)
            .tempdir_in(path.parent().unwrap_or(path))
    };
    let mut original = stash(source)?;
    let destination = stash(target)?;
    let staged_source = original.path().join("item");
    let staged_target = destination.path().join("item");
    move_exclusive(source, &staged_source)?;
    let result = copy_entry(&staged_source, &staged_target)
        .and_then(|()| move_exclusive(&staged_target, target).map_err(Into::into));
    if let Err(error) = result {
        if let Err(rollback) = move_exclusive(&staged_source, source) {
            original.disable_cleanup(true);
            bail!(i18n::format(
                "manager.rollback_failed",
                &[
                    (
                        "items",
                        &format_args!("{} ({rollback})", staged_source.display())
                    ),
                    ("error", &format_args!("{error:#}")),
                ],
            ));
        }
        return Err(error);
    }
    Ok(())
}

/// Refuse to overwrite `path`, including dangling symbolic links.
fn require_vacant(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => bail!(i18n::format(
            "manager.destination_exists",
            &[("path", &path.display())]
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error)
            .with_context(|| i18n::format("manager.inspect_failed", &[("path", &path.display())])),
    }
}

/// Rename `source` to `target` atomically with overwrite disabled.
#[cfg(windows)]
pub(in crate::grid) fn move_exclusive(source: &Path, target: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    let source: Vec<_> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let target: Vec<_> = target.as_os_str().encode_wide().chain(Some(0)).collect();
    // Both buffers are NUL-terminated native paths and remain alive for the call.
    if unsafe {
        windows_sys::Win32::Storage::FileSystem::MoveFileExW(source.as_ptr(), target.as_ptr(), 0)
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Rename `source` to `target` atomically with overwrite disabled.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(in crate::grid) fn move_exclusive(source: &Path, target: &Path) -> io::Result<()> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};
    Ok(renameat_with(
        CWD,
        source,
        CWD,
        target,
        RenameFlags::NOREPLACE,
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a disposable canonical project and a directory to paste into.
    fn project() -> (TempDir, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let target = root.join("target");
        fs::create_dir(&target).unwrap();
        (temp, root, target)
    }

    /// Undo and redo keep the copied data independent of the original file.
    #[test]
    fn copy_undo_redo_handles_a_marked_directory_and_child_once() {
        let (_temp, root, target) = project();
        let source = root.join("folder");
        fs::create_dir(&source).unwrap();
        let child = source.join("中文.txt");
        fs::write(&child, "saved content").unwrap();
        let mut transfers = FileTransfers::default();
        let marks = HashSet::from([source, child.clone()]);
        assert_eq!(transfers.capture(&marks, TransferKind::Copy).unwrap(), 1);
        let pasted = target.join("folder");
        let created = transfers.paste(&root, &target, &[]).unwrap();
        assert_eq!(created[0].from, pasted);
        assert!(created[0].to.is_none());
        let removed = transfers.replay(false, &[]).unwrap();
        assert_eq!(removed[0].from, pasted);
        assert!(removed[0].to.is_none());
        assert!(!pasted.exists());
        assert!(child.exists());
        transfers.replay(true, &[]).unwrap();
        assert_eq!(
            fs::read_to_string(pasted.join("中文.txt")).unwrap(),
            "saved content"
        );
    }

    /// Reject the complete cut on any conflict, then move and replay both paths together.
    #[test]
    fn cut_replays_atomically_and_keeps_existing_files() {
        let (_temp, root, target) = project();
        let a = root.join("a.txt");
        let b = root.join("b.txt");
        let collision = target.join("b.txt");
        fs::write(&a, "a").unwrap();
        fs::write(&b, "b").unwrap();
        fs::write(&collision, "existing").unwrap();
        let mut transfers = FileTransfers::default();
        transfers
            .capture(&HashSet::from([a.clone(), b.clone()]), TransferKind::Cut)
            .unwrap();
        assert!(transfers.paste(&root, &target, &[]).is_err());
        assert!(a.exists() && b.exists());
        assert_eq!(fs::read_to_string(&collision).unwrap(), "existing");
        fs::remove_file(&collision).unwrap();
        let changes = transfers.paste(&root, &target, &[]).unwrap();
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].from, a);
        assert_eq!(changes[0].to, Some(target.join("a.txt")));
        assert!(!a.exists() && !b.exists());
        transfers.replay(false, &[]).unwrap();
        assert!(a.exists() && b.exists());
        transfers.replay(true, &[]).unwrap();
        assert_eq!(fs::read_to_string(&collision).unwrap(), "b");
        assert!(!a.exists() && !b.exists());
    }

    /// An external or in-memory edit invalidates its paste without discarding unrelated actions.
    #[test]
    fn changes_invalidate_only_related_pastes() {
        let (_temp, root, target) = project();
        let mut transfers = FileTransfers::default();
        for name in ["untouched.txt", "changed.txt", "unsaved.txt"] {
            let source = root.join(name);
            fs::write(&source, "original").unwrap();
            transfers
                .capture(&HashSet::from([source]), TransferKind::Copy)
                .unwrap();
            transfers.paste(&root, &target, &[]).unwrap();
        }
        let changed = target.join("changed.txt");
        let unsaved = target.join("unsaved.txt");
        fs::write(&changed, "changed!").unwrap();
        transfers.invalidate(&unsaved);
        assert!(transfers.replay(false, &[]).is_err());
        assert_eq!(fs::read_to_string(&changed).unwrap(), "changed!");
        transfers.replay(false, &[]).unwrap();
        assert!(!target.join("untouched.txt").exists());
        assert!(unsaved.exists());
        transfers.invalidate(&root.join("untouched.txt"));
        assert!(transfers.replay(true, &[]).is_err());
    }

    /// The eleventh paste ages out the first action and preserves its visible output.
    #[test]
    fn history_keeps_ten_whole_pastes() {
        let (_temp, root, target) = project();
        let mut transfers = FileTransfers::default();
        for i in 0..11 {
            let source = root.join(format!("{i}.txt"));
            fs::write(&source, "data").unwrap();
            transfers
                .capture(&HashSet::from([source]), TransferKind::Copy)
                .unwrap();
            transfers.paste(&root, &target, &[]).unwrap();
        }
        for _ in 0..10 {
            transfers.replay(false, &[]).unwrap();
        }
        assert!(transfers.replay(false, &[]).is_err());
        assert_eq!(fs::read_dir(&target).unwrap().count(), 1);
        assert!(target.join("0.txt").exists());
        for _ in 0..10 {
            transfers.replay(true, &[]).unwrap();
        }
        assert_eq!(fs::read_dir(&target).unwrap().count(), 11);
    }

    /// A paste into its own subtree or with pending document edits has no effect on disk.
    #[test]
    fn paste_rejects_self_insertion_and_unsaved_source() {
        let (_temp, root, target) = project();
        let source = root.join("source");
        let child = source.join("child");
        fs::create_dir_all(&child).unwrap();
        let file = source.join("data.txt");
        fs::write(&file, "disk").unwrap();
        let mut transfers = FileTransfers::default();
        transfers
            .capture(&HashSet::from([source.clone()]), TransferKind::Cut)
            .unwrap();
        assert!(transfers.paste(&root, &child, &[]).is_err());
        assert!(
            transfers
                .paste(&root, &target, std::slice::from_ref(&file))
                .is_err()
        );
        assert!(file.exists());
        assert!(!target.join("source").exists());
    }

    /// A locked second item rolls back the first and leaves the cut available to retry.
    #[cfg(windows)]
    #[test]
    fn partial_cut_failure_rolls_back_the_first_move() {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};

        let (_temp, root, target) = project();
        let a = root.join("a.txt");
        let b = root.join("b.txt");
        fs::write(&a, "first").unwrap();
        fs::write(&b, "second").unwrap();
        let locked = fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(&b)
            .unwrap();
        let mut transfers = FileTransfers::default();
        transfers
            .capture(&HashSet::from([a.clone(), b.clone()]), TransferKind::Cut)
            .unwrap();
        assert!(transfers.paste(&root, &target, &[]).is_err());
        assert!(a.exists() && b.exists());
        assert_eq!(fs::read_dir(&target).unwrap().count(), 0);
        drop(locked);
        assert_eq!(transfers.paste(&root, &target, &[]).unwrap().len(), 2);
    }
    /// Picker paste accepts a sibling directory while the project-bound entry point refuses it.
    #[test]
    fn browser_paste_can_cross_project_roots() {
        let (_temp, root, destination) = project();
        let source_dir = root.join("source");
        fs::create_dir(&source_dir).unwrap();
        let source = source_dir.join("notes.txt");
        fs::write(&source, "content").unwrap();
        let mut transfers = FileTransfers::default();
        transfers
            .capture(&[source.clone()].into_iter().collect(), TransferKind::Copy)
            .unwrap();
        assert!(transfers.paste(&source_dir, &destination, &[]).is_err());
        transfers.paste_browser(&destination, &[]).unwrap();
        assert_eq!(
            fs::read_to_string(destination.join("notes.txt")).unwrap(),
            "content"
        );
        assert!(source.exists());
    }

    /// Exercise cross-volume staging even on machines with one mounted volume.
    #[test]
    fn cross_volume_fallback_restores_the_source_on_collision() {
        let (_temp, root, destination) = project();
        let source = root.join("source.txt");
        let target = destination.join("target.txt");
        fs::write(&source, "source").unwrap();
        fs::write(&target, "target").unwrap();
        assert!(move_across_volumes(&source, &target).is_err());
        assert_eq!(fs::read_to_string(&source).unwrap(), "source");
        assert_eq!(fs::read_to_string(&target).unwrap(), "target");
        let vacant = destination.join("vacant.txt");
        move_across_volumes(&source, &vacant).unwrap();
        assert!(!source.exists());
        assert_eq!(fs::read_to_string(vacant).unwrap(), "source");
    }
}
