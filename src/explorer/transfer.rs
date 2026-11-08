//! Explorer paste transactions, with a private holding area for undone copies.
use anyhow::{Context, Result, bail, ensure};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs::{self, File, Metadata},
    io::{self, Read},
    path::{Path, PathBuf},
};
use tempfile::{Builder, TempDir};

/// Prefix reserved for paste history directories under a project root.
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

/// Change to a visible path that open buffers must follow.
pub struct PathChange {
    /// Previous visible path, or a newly created path whose old cached buffer is stale.
    pub from: PathBuf,
    /// New path, or None when the old buffer should be closed.
    pub to: Option<PathBuf>,
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
}

impl Paste {
    /// Move the complete paste `forward` or back, rolling back any partial move.
    /// `verify` checks replayed data again after moving it to detect edits during replay.
    fn apply(&mut self, forward: bool, verify: bool) -> Result<Vec<PathChange>> {
        for item in &self.items {
            let (from, to) = item.paths(forward);
            let parent = to.parent().context("缺少目标目录")?;
            ensure!(
                fs::canonicalize(from)? == *from && fs::canonicalize(parent)? == parent,
                "源路径或目标目录已变更：{}",
                to.display()
            );
            require_vacant(to)?;
        }
        let mut moved = 0;
        let result = (|| -> Result<()> {
            for item in &self.items {
                let (from, to) = item.paths(forward);
                move_exclusive(from, to)
                    .with_context(|| format!("无法移动 {} → {}", from.display(), to.display()))?;
                moved += 1;
                if verify {
                    ensure!(
                        fingerprint(to)? == item.stamp,
                        "重放时文件已被编辑：{}",
                        from.display()
                    );
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            let mut rollback_errors = Vec::new();
            for item in self.items[..moved].iter().rev() {
                let (old, new) = item.paths(forward);
                if let Err(error) = move_exclusive(new, old) {
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
                bail!(
                    "移动失败，以下项目需手动恢复：{}。原错误：{error:#}",
                    rollback_errors.join("，")
                );
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
        ensure!(!marked.is_empty(), "请先用 s 标记文件或目录");
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
        let (kind, sources) = self
            .clipboard
            .as_ref()
            .context("请先复制或剪切文件或目录")?;
        ensure!(
            directory.starts_with(root)
                && entry_metadata(directory)?.is_dir()
                && fs::canonicalize(directory)? == directory,
            "粘贴目标必须是项目内的目录"
        );
        let mut targets = HashSet::new();
        let mut pairs = Vec::new();
        for source in sources {
            entry_metadata(source)?;
            ensure!(
                source != root && source.starts_with(root) && fs::canonicalize(source)? == *source,
                "源项目必须位于项目目录内"
            );
            ensure!(
                !directory.starts_with(source),
                "不能将目录粘贴到自身或子目录"
            );
            let name = source.file_name().context("缺少文件名")?;
            let after = directory.join(name);
            require_vacant(&after)?;
            ensure!(
                targets.insert(after.clone()),
                "多项粘贴包含同名文件：{}",
                after.display()
            );
            ensure!(
                dirty
                    .iter()
                    .all(|p| !p.starts_with(source) && !p.starts_with(&after)),
                "请先保存相关文件的修改"
            );
            pairs.push((source.clone(), after));
        }

        let stage = (*kind == TransferKind::Copy)
            .then(|| Builder::new().prefix(STASH_PREFIX).tempdir_in(root))
            .transpose()?;
        let mut items = Vec::new();
        for (index, (source, after)) in pairs.into_iter().enumerate() {
            let before = if let Some(stage) = &stage {
                let path = stage.path().join(index.to_string());
                copy_entry(&source, &path)
                    .with_context(|| format!("无法复制 {}", source.display()))?;
                path
            } else {
                source.clone()
            };
            items.push(PasteItem {
                stamp: fingerprint(&before)?,
                source,
                before,
                after,
            });
        }
        let mut paste = Paste { items, stage };
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
        let paste = from.last_mut().context(if redo {
            "没有可重做的粘贴"
        } else {
            "没有可撤销的粘贴"
        })?;
        ensure!(
            dirty.iter().all(|path| !paste.touches(path)),
            "请先保存相关文件的修改"
        );
        let intact = paste.items.iter().all(|item| {
            let path = if redo { &item.before } else { &item.after };
            fingerprint(path).is_ok_and(|stamp| stamp == item.stamp)
        });
        if !intact {
            if redo && let Some(stage) = &mut paste.stage {
                stage.disable_cleanup(true);
                let path = stage.path().to_path_buf();
                from.pop();
                bail!(
                    "暂存项已改变，重做历史已失效。文件保留在 {}",
                    path.display()
                );
            }
            from.pop();
            bail!("粘贴项已改变或无法读取，该条历史已失效");
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
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("无法读取 {}", path.display()))?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        ensure!(
            metadata.file_attributes() & 0x400 == 0,
            "暂不支持链接或重解析点：{}",
            path.display()
        );
    }
    ensure!(
        metadata.is_file() || metadata.is_dir(),
        "暂不支持此类文件：{}",
        path.display()
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

/// Digest contents, descendant names and file timestamps of the item at `path`.
fn fingerprint(path: &Path) -> Result<[u8; 32]> {
    let mut digest = Sha256::new();
    hash_entry(path, &mut digest, &mut [0; 64 * 1024])?;
    Ok(digest.finalize().into())
}

/// Add `path` and its children to `digest`, reusing `buffer` to stream file contents.
fn hash_entry(path: &Path, digest: &mut Sha256, buffer: &mut [u8]) -> Result<()> {
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
            hash_entry(&child.path(), digest, buffer)?;
        }
    } else {
        digest.update(b"F");
        digest.update(metadata.len().to_le_bytes());
        digest.update(format!("{:?}", metadata.modified()?));
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

/// Refuse to overwrite `path`, including dangling symbolic links.
fn require_vacant(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => bail!("目标已存在：{}", path.display()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("无法检查 {}", path.display())),
    }
}

/// Rename `source` to `target` atomically with overwrite disabled.
#[cfg(windows)]
fn move_exclusive(source: &Path, target: &Path) -> io::Result<()> {
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
fn move_exclusive(source: &Path, target: &Path) -> io::Result<()> {
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
}
