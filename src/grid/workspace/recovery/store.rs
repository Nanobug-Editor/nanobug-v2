//! Keep unpublished buffer text in atomically replaced local checkpoints.

use anyhow::{Context, Result, ensure};
use nanobug::document::RecoveryDraft;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
    time::SystemTime,
};

const FORMAT: &str = "nanobug-recovery-1";
const RUN_PREFIX: &str = "run-";
const STAGING_PREFIX: &str = "staged-";
const LEASE: &str = "owner.lock";

/// One shared buffer's text and the Session in which it was edited.
#[derive(Deserialize, Serialize)]
pub(super) struct Draft {
    pub(super) scope: PathBuf,
    pub(super) buffer: RecoveryDraft,
}

/// Checkpoints owned by a single editor instance in a shared local recovery directory.
pub(super) struct Store {
    root: PathBuf,
    run: PathBuf,
    lease: Option<File>,
}

/// A checkpoint that stays locked against other recovering editors until it is dropped.
pub(super) struct Candidate {
    pub(super) draft: Draft,
    path: PathBuf,
    abandoned_lease: Option<File>,
}

impl Candidate {
    /// Whether this checkpoint belongs to the current editor instance.
    pub(super) fn is_owned(&self) -> bool {
        self.abandoned_lease.is_none()
    }

    /// Remove the abandoned copy after the caller has preserved the recovered text.
    pub(super) fn remove(self) -> Result<()> {
        if let Some(lease) = self.abandoned_lease {
            remove_file(&self.path)?;
            if let Some(run) = self.path.parent() {
                clean_abandoned_run(run, lease);
            }
        }
        Ok(())
    }
}

impl Store {
    /// Reserve a private per-process run so an instance never overwrites another's drafts.
    pub(super) fn new(root: PathBuf) -> Result<Self> {
        fs::create_dir_all(&root)?;
        let root = root.canonicalize()?;
        let run = tempfile::Builder::new()
            .prefix(RUN_PREFIX)
            .tempdir_in(&root)?;
        let lease = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(run.path().join(LEASE))?;
        lease.lock()?;
        Ok(Self {
            root,
            run: run.keep(),
            lease: Some(lease),
        })
    }

    /// Return the current editor's stable checkpoint path for logical buffer `id`.
    pub(super) fn path(&self, id: u64) -> PathBuf {
        self.run.join(format!("draft-{id:016x}.json"))
    }

    /// Flush all text for `id`, then atomically replace its previous checkpoint.
    pub(super) fn write(&self, id: u64, draft: &Draft) -> Result<()> {
        ensure!(self.lease.is_some(), "Recovery store has been closed");
        let mut staged = tempfile::Builder::new()
            .prefix(STAGING_PREFIX)
            .tempfile_in(&self.run)?;
        {
            let mut writer = BufWriter::new(staged.as_file_mut());
            serde_json::to_writer(&mut writer, &(FORMAT, draft))?;
            writer.flush()?;
        }
        staged.as_file().sync_all()?;
        staged.persist(self.path(id))?;
        #[cfg(unix)]
        File::open(&self.run)?.sync_all()?;
        Ok(())
    }

    /// Remove a saved or discarded buffer's checkpoint from this editor's run.
    pub(super) fn remove(&self, id: u64) -> Result<()> {
        remove_file(&self.path(id))?;
        Ok(())
    }

    /// Release the run after the normal close flow has dealt with all modified text.
    pub(super) fn finish(&mut self) -> Result<()> {
        if self.lease.is_none() {
            return Ok(());
        }
        for file in fs::read_dir(&self.run)? {
            let file = file?;
            let name = file.file_name();
            if file.file_type()?.is_file()
                && (draft_filename(&file.path())
                    || name
                        .to_str()
                        .is_some_and(|name| name.starts_with(STAGING_PREFIX)))
            {
                remove_file(&file.path())?;
            }
        }
        drop(self.lease.take());
        remove_file(&self.run.join(LEASE))?;
        match fs::remove_dir(&self.run) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::DirectoryNotEmpty => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    /// Find the most recent abandoned checkpoint for `path`, preferring `scope`.
    /// Untitled drafts belong exclusively to their own scope.
    pub(super) fn find(&self, scope: &Path, path: Option<&Path>) -> Result<Option<Candidate>> {
        let mut best = None;
        let mut priority = None;
        let canonical = path.and_then(|path| path.canonicalize().ok());
        for run in fs::read_dir(&self.root)? {
            let run = run?;
            if !run.file_type()?.is_dir() || !is_run(&run.path()) || run.path() == self.run {
                continue;
            }
            let Some(lease) = lock_abandoned(&run.path()) else {
                continue;
            };
            let Ok(files) = fs::read_dir(run.path()) else {
                continue;
            };
            let mut selected = None;
            for file in files.flatten() {
                let file_path = file.path();
                if !draft_filename(&file_path) || !file.file_type().is_ok_and(|kind| kind.is_file())
                {
                    continue;
                }
                let Ok(draft) = read_draft(&file_path) else {
                    continue;
                };
                let same_scope = draft.scope == scope;
                let draft_path = draft.buffer.path.as_deref();
                let same_path = draft_path == path
                    || draft_path
                        .zip(canonical.as_ref())
                        .is_some_and(|(draft, expected)| {
                            draft.canonicalize().is_ok_and(|path| path == *expected)
                        });
                if !same_path || (path.is_none() && !same_scope) {
                    continue;
                }
                let updated = file
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                let score = (same_scope, updated, file_path.clone());
                if priority.as_ref().is_none_or(|previous| score > *previous) {
                    priority = Some(score);
                    selected = Some((draft, file_path));
                }
            }
            if let Some((draft, path)) = selected {
                best = Some(Candidate {
                    draft,
                    path,
                    abandoned_lease: Some(lease),
                });
            }
        }
        Ok(best)
    }

    /// Open a checkpoint by its exact path within this installation's recovery directory.
    pub(super) fn open(&self, path: &Path) -> Result<Option<Candidate>> {
        if !draft_filename(path) {
            return Ok(None);
        }
        let path = match path.canonicalize() {
            Ok(path) => path,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let Some(run) = path.parent() else {
            return Ok(None);
        };
        if run.parent() != Some(&self.root) || !is_run(run) || !draft_filename(&path) {
            return Ok(None);
        }
        let abandoned_lease = if run == self.run {
            None
        } else {
            let Some(lease) = lock_abandoned(run) else {
                return Ok(None);
            };
            Some(lease)
        };
        Ok(Some(Candidate {
            draft: read_draft(&path)?,
            path,
            abandoned_lease,
        }))
    }
}

/// Recognize a direct child directory created by this recovery store.
fn is_run(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(RUN_PREFIX))
}

/// Recognize the fixed local checkpoint filename used for one logical buffer.
fn draft_filename(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix("draft-")?.strip_suffix(".json"))
        .is_some_and(|id| id.len() == 16 && id.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

/// Acquire a formerly running instance's lease without interrupting a live editor.
fn lock_abandoned(run: &Path) -> Option<File> {
    let path = run.join(LEASE);
    if !path.symlink_metadata().ok()?.is_file() {
        return None;
    }
    let file = OpenOptions::new().read(true).write(true).open(path).ok()?;
    file.try_lock().ok()?;
    Some(file)
}

/// Remove interrupted writes and retire an abandoned run once it has no useful files.
fn clean_abandoned_run(run: &Path, lease: File) {
    let Ok(files) = fs::read_dir(run) else {
        return;
    };
    let mut remains = false;
    for file in files {
        let Ok(file) = file else {
            remains = true;
            continue;
        };
        let name = file.file_name();
        if name == LEASE {
            continue;
        }
        if name
            .to_str()
            .is_some_and(|name| name.starts_with(STAGING_PREFIX))
            && file.file_type().is_ok_and(|kind| kind.is_file())
        {
            remains |= remove_file(&file.path()).is_err();
        } else {
            remains = true;
        }
    }
    if !remains {
        drop(lease);
        let _ = remove_file(&run.join(LEASE));
        let _ = fs::remove_dir(run);
    }
}

/// Refuse malformed or unrelated JSON files in explicit recovery requests.
fn read_draft(path: &Path) -> Result<Draft> {
    ensure!(
        path.symlink_metadata()?.is_file(),
        "Recovery path is not a file"
    );
    let file = File::open(path)?;
    let (format, draft): (String, Draft) = serde_json::from_reader(BufReader::new(file))
        .with_context(|| format!("Read recovery file {}", path.display()))?;
    ensure!(format == FORMAT, "Unknown recovery file format");
    Ok(draft)
}

/// Remove a checkpoint idempotently, including when a buffer never acquired one.
fn remove_file(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nanobug::document::Document;

    /// Make a checkpoint using the same document snapshot as the workspace.
    fn draft(scope: &Path, path: &Path, text: &str) -> Result<Draft> {
        let mut buffer = Document::open(path)?;
        buffer.replace(0..buffer.text.len(), text, true);
        Ok(Draft {
            scope: scope.into(),
            buffer: buffer.recovery_draft(),
        })
    }

    #[test]
    fn replaces_a_shared_buffer_checkpoint_and_recovers_after_its_owner_exits() -> Result<()> {
        let fixture = tempfile::tempdir()?;
        let root = fixture.path().join("recovery");
        let scope = fixture.path();
        let path = scope.join("file.txt");
        let first = Store::new(root.clone())?;
        first.write(2, &draft(scope, &path, "first\n")?)?;
        first.write(2, &draft(scope, &path, "second\n")?)?;
        fs::write(first.run.join("staged-interrupted"), "incomplete")?;
        let saved = first.path(2);
        let mut second = Store::new(root)?;
        assert!(second.find(scope, Some(&path))?.is_none());
        assert!(second.open(&saved)?.is_none());
        let own = first.open(&saved)?.expect("owned draft");
        assert!(own.is_owned());
        assert_eq!(own.draft.buffer.text, "second\n");
        drop(first);

        let recovered = second.find(scope, Some(&path))?.expect("abandoned draft");
        assert_eq!(recovered.draft.buffer.text, "second\n");
        assert!(!recovered.is_owned());
        assert!(second.open(&saved)?.is_none());
        recovered.remove()?;
        assert!(!saved.exists());
        assert!(!saved.parent().unwrap().exists());
        second.finish()?;
        assert!(!second.run.exists());
        Ok(())
    }

    #[test]
    fn chooses_current_scope_and_cleans_only_the_current_run() -> Result<()> {
        let fixture = tempfile::tempdir()?;
        let local = fixture.path().join("local");
        let other = fixture.path().join("other");
        let path = fixture.path().join("file.txt");
        let local_run = Store::new(fixture.path().into())?;
        local_run.write(1, &draft(&local, &path, "local")?)?;
        let local_record = local_run.path(1);
        drop(local_run);
        let other_run = Store::new(fixture.path().into())?;
        other_run.write(1, &draft(&other, &path, "other")?)?;
        let other_record = other_run.path(1);
        drop(other_run);

        let mut active = Store::new(fixture.path().into())?;
        active.write(2, &draft(&other, &path, "active")?)?;
        let candidate = active.find(&local, Some(&path))?.expect("local draft");
        assert_eq!(candidate.draft.buffer.text, "local");
        candidate.remove()?;
        let candidate = active
            .find(&local, Some(&path))?
            .expect("draft in other scope");
        assert_eq!(candidate.draft.buffer.text, "other");
        drop(candidate);
        active.finish()?;
        assert!(!local_record.exists());
        assert!(other_record.exists());
        assert!(!active.path(2).exists());
        Ok(())
    }

    /// A file created after the draft was saved may have a new canonical spelling.
    #[test]
    fn finds_a_draft_when_its_previously_missing_file_now_exists() -> Result<()> {
        let fixture = tempfile::tempdir()?;
        let path = fixture.path().join("file.txt");
        let writer = Store::new(fixture.path().into())?;
        writer.write(1, &draft(fixture.path(), &path, "pending draft")?)?;
        drop(writer);
        fs::write(&path, "new disk contents")?;
        let normalized = path.canonicalize()?;
        let reader = Store::new(fixture.path().into())?;
        let candidate = reader.find(fixture.path(), Some(&normalized))?.unwrap();
        assert_eq!(candidate.draft.buffer.text, "pending draft");
        assert!(!candidate.draft.buffer.matches_disk()?);
        Ok(())
    }
}
