//! Host-only receipts for file changes confirmed by the user. Review never replays them.

use super::{
    package::{read_json, validate_digest, write_bytes},
    project_path,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    time::SystemTime,
};

const RECORD_LIMIT: u64 = 512 * 1024;
const RECENT_LIMIT: usize = 32;
const STEP_LIMIT: usize = 32;
const REVIEW_BYTES: u64 = 8 * 1024 * 1024;

/// Only bytes and a digest are retained. File text is never written to the journal.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FileContents {
    bytes: u64,
    sha256: [u8; 32],
}

impl FileContents {
    /// Record the expected contents of a proposed new file.
    pub fn from_bytes(data: &[u8]) -> Self {
        Self {
            bytes: data.len() as u64,
            sha256: Sha256::digest(data).into(),
        }
    }
}

/// Exact contents and modification time of a regular file observed during preview.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileStamp {
    pub contents: FileContents,
    modified: Option<SystemTime>,
}

/// Hash a regular file off the editor thread and reject visible changes during the read.
pub fn stamp(path: &Path) -> Result<Option<FileStamp>> {
    stamp_with_limit(path, u64::MAX)
}

fn stamp_with_limit(path: &Path, limit: u64) -> Result<Option<FileStamp>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let bytes = metadata.len();
    let modified = metadata.modified().ok();
    ensure!(
        metadata.is_file() && bytes <= limit,
        "File cannot be verified as a regular file"
    );
    let mut file = File::open(path)?;
    let opened = file.metadata()?;
    ensure!(
        opened.is_file() && opened.len() == bytes && opened.modified().ok() == modified,
        "File changed while checking its contents"
    );
    let mut hash = Sha256::new();
    let read = io::copy(&mut (&mut file).take(bytes.saturating_add(1)), &mut hash)?;
    let after = fs::symlink_metadata(path)?;
    ensure!(
        read == bytes
            && after.is_file()
            && after.len() == bytes
            && after.modified().ok() == modified,
        "File changed while checking its contents"
    );
    Ok(Some(FileStamp {
        contents: FileContents {
            bytes,
            sha256: hash.finalize().into(),
        },
        modified,
    }))
}

/// An expected destination that can be checked without performing any file operation.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DestinationCheck {
    pub target: String,
    pub absent_source: Option<String>,
    pub contents: FileContents,
}

/// The display text and optional read-only check for a confirmed step.
#[derive(Debug, Deserialize, Serialize)]
pub struct FileOperationIntent {
    pub label: String,
    pub check: Option<DestinationCheck>,
}

/// Pending remains unknown even if later inspection finds a matching destination.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileStepState {
    NotRun,
    Pending,
    Done,
    Failed,
}

/// Review data supplied to the editor. No path from a receipt can be used for execution.
#[derive(Clone, Debug)]
pub struct FileOperationStep {
    pub label: String,
    pub state: FileStepState,
    pub destination_matches: bool,
}

/// Durable results of a single confirmation, in execution order.
#[derive(Clone, Debug)]
pub struct FileOperationReceipt {
    pub id: String,
    pub steps: Vec<FileOperationStep>,
}

#[derive(Deserialize, Serialize)]
struct Record {
    project: PathBuf,
    steps: Vec<StoredStep>,
}

#[derive(Deserialize, Serialize)]
struct StoredStep {
    intent: FileOperationIntent,
    state: FileStepState,
}

/// Own the receipt while executing one confirmed plan.
pub struct FileOperationJournal {
    path: PathBuf,
    record: Record,
}

impl FileOperationJournal {
    /// Publish the entire plan before taking its first action.
    pub fn start(
        profile: &Path,
        namespace: &str,
        project: &Path,
        intents: Vec<FileOperationIntent>,
    ) -> Result<Self> {
        ensure!(
            project.is_absolute() && (1..=STEP_LIMIT).contains(&intents.len()),
            "Invalid confirmed file plan"
        );
        let directory = directory(profile, namespace)?;
        fs::create_dir_all(&directory)?;
        let record = Record {
            project: project.to_path_buf(),
            steps: intents
                .into_iter()
                .map(|intent| StoredStep {
                    intent,
                    state: FileStepState::NotRun,
                })
                .collect(),
        };
        let mut stage = tempfile::Builder::new()
            .prefix(".files-")
            .tempfile_in(&directory)?;
        let id = stage
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix('.'))
            .context("Invalid file operation receipt name")?;
        let path = directory.join(format!("{id}.json"));
        stage.write_all(&serialize(&record)?)?;
        stage.as_file().sync_all()?;
        stage.persist_noclobber(&path)?;
        Ok(Self { path, record })
    }

    /// Persist the current step before the next one can begin.
    pub fn record(&mut self, index: usize, state: FileStepState) -> Result<()> {
        let prior = self
            .record
            .steps
            .get(index)
            .context("Unknown file operation step")?
            .state;
        ensure!(
            self.record.steps[..index]
                .iter()
                .all(|step| step.state == FileStepState::Done)
                && matches!(
                    (prior, state),
                    (FileStepState::NotRun, FileStepState::Pending)
                        | (
                            FileStepState::Pending,
                            FileStepState::Done | FileStepState::Failed
                        )
                ),
            "Invalid file operation result transition"
        );
        self.record.steps[index].state = state;
        if let Err(error) =
            serialize(&self.record).and_then(|bytes| write_bytes(&self.path, &bytes))
        {
            self.record.steps[index].state = prior;
            return Err(error);
        }
        Ok(())
    }
}

fn serialize(record: &Record) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec(record)?;
    ensure!(
        bytes.len() as u64 <= RECORD_LIMIT,
        "File operation receipt exceeds its size limit"
    );
    Ok(bytes)
}

fn directory(profile: &Path, namespace: &str) -> Result<PathBuf> {
    validate_digest(namespace)?;
    Ok(profile.join("operations").join(namespace))
}

/// Review only the latest receipts and budget the amount of disk data checked for unknown steps.
pub fn review(profile: &Path, namespace: &str) -> Result<Vec<FileOperationReceipt>> {
    let entries = match fs::read_dir(directory(profile, namespace)?) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut recent = BTreeMap::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(id) = name
            .to_str()
            .and_then(|name| name.strip_suffix(".json"))
            .filter(|id| {
                id.strip_prefix("files-").is_some_and(|suffix| {
                    !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
                })
            })
        else {
            continue;
        };
        if !entry.file_type()?.is_file() {
            continue;
        }
        let modified = entry
            .metadata()?
            .modified()
            .unwrap_or(SystemTime::UNIX_EPOCH);
        recent.insert((modified, id.to_owned()), entry.path());
        if recent.len() > RECENT_LIMIT {
            recent.pop_first();
        }
    }
    let mut available = REVIEW_BYTES;
    Ok(recent
        .into_iter()
        .rev()
        .map(|((_, id), path)| {
            let steps = read_json::<Record>(&path, RECORD_LIMIT)
                .ok()
                .flatten()
                .filter(|record| (1..=STEP_LIMIT).contains(&record.steps.len()))
                .map_or_else(
                    || {
                        vec![FileOperationStep {
                            label: "Unreadable file operation record".into(),
                            state: FileStepState::Pending,
                            destination_matches: false,
                        }]
                    },
                    |record| {
                        record
                            .steps
                            .into_iter()
                            .map(|step| {
                                let destination_matches = step.state == FileStepState::Pending
                                    && step.intent.check.as_ref().is_some_and(|check| {
                                        check_destination(&record.project, check, &mut available)
                                            .unwrap_or(false)
                                    });
                                FileOperationStep {
                                    label: step.intent.label,
                                    state: step.state,
                                    destination_matches,
                                }
                            })
                            .collect()
                    },
                );
            FileOperationReceipt { id, steps }
        })
        .collect())
}

fn check_destination(root: &Path, check: &DestinationCheck, available: &mut u64) -> Result<bool> {
    let relative = |name: &str| -> Result<PathBuf> {
        ensure!(
            !name.is_empty()
                && !name.contains(':')
                && Path::new(name)
                    .components()
                    .all(|item| matches!(item, Component::Normal(_))),
            "Invalid relative file operation path"
        );
        project_path(root, name)
    };
    if root.canonicalize()? != root || check.contents.bytes > *available {
        return Ok(false);
    }
    if let Some(source) = &check.absent_source {
        match fs::symlink_metadata(relative(source)?) {
            Ok(_) => return Ok(false),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    *available -= check.contents.bytes;
    Ok(
        stamp_with_limit(&relative(&check.target)?, check.contents.bytes)?
            .is_some_and(|stamp| stamp.contents == check.contents),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupted_steps_are_reviewable_without_replaying_or_guessing_results() -> Result<()> {
        let profile = tempfile::tempdir()?;
        let project = tempfile::tempdir()?;
        let root = project.path().canonicalize()?;
        let namespace = "1".repeat(64);
        let expected = "planned text";
        let mut journal = FileOperationJournal::start(
            profile.path(),
            &namespace,
            &root,
            vec![
                FileOperationIntent {
                    label: "Create first.txt".into(),
                    check: Some(DestinationCheck {
                        target: "first.txt".into(),
                        absent_source: None,
                        contents: FileContents::from_bytes(expected.as_bytes()),
                    }),
                },
                FileOperationIntent {
                    label: "Remove second.txt".into(),
                    check: None,
                },
            ],
        )?;
        assert!(journal.record(1, FileStepState::Pending).is_err());
        journal.record(0, FileStepState::Pending)?;
        fs::write(root.join("first.txt"), expected)?;
        // The editor stops after the file changed and before recording a known result.
        drop(journal);

        let reviewed = review(profile.path(), &namespace)?;
        assert_eq!(reviewed.len(), 1);
        assert_eq!(reviewed[0].steps[0].state, FileStepState::Pending);
        assert!(reviewed[0].steps[0].destination_matches);
        assert_eq!(reviewed[0].steps[1].state, FileStepState::NotRun);
        fs::write(root.join("first.txt"), "edited later")?;
        let reread = review(profile.path(), &namespace)?;
        assert_eq!(reread[0].steps[0].state, FileStepState::Pending);
        assert!(!reread[0].steps[0].destination_matches);
        assert_eq!(fs::read_to_string(root.join("first.txt"))?, "edited later");
        assert!(review(profile.path(), &"2".repeat(64))?.is_empty());
        Ok(())
    }
}
