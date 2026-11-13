//! Durable, argument-free receipts for commands that must never be replayed.

use crate::plugins::package::{read_limited, validate_digest, write_json};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    time::SystemTime,
};

const RECENT_LIMIT: usize = 32;
const RECEIPT_LIMIT: u64 = 4096;

/// Durable process exit information. A pending record does not prove a side effect.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolOperationState {
    #[serde(rename = "result_pending")]
    Pending,
    Exited,
    NotStarted,
}

/// A reviewable native command, without command arguments, output or credentials.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolOperation {
    pub id: String,
    pub tool: String,
    pub state: ToolOperationState,
    pub code: Option<i32>,
}

#[derive(Deserialize, Serialize)]
struct Record {
    tool: String,
    state: ToolOperationState,
    #[serde(default)]
    code: Option<i32>,
}

/// A command may be launched only after its pending receipt has been published.
pub(super) struct Operation {
    id: String,
    path: PathBuf,
    tool: String,
}

impl Operation {
    pub(super) fn start(profile: &Path, namespace: &str, tool: &str) -> Result<Self> {
        let directory = directory(profile, namespace)?;
        fs::create_dir_all(&directory)?;
        let mut stage = tempfile::Builder::new()
            .prefix(".tool-")
            .tempfile_in(&directory)?;
        let id = stage
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix('.'))
            .context("Invalid tool operation identifier")?
            .to_owned();
        let path = directory.join(format!("{id}.json"));
        let tool = tool.to_owned();
        serde_json::to_writer(
            &mut stage,
            &Record {
                tool: tool.clone(),
                state: ToolOperationState::Pending,
                code: None,
            },
        )?;
        stage.as_file().sync_all()?;
        stage.persist_noclobber(&path)?;
        Ok(Self { id, path, tool })
    }

    pub(super) fn id(&self) -> &str {
        &self.id
    }

    pub(super) fn not_started(&self) -> Result<()> {
        self.update(ToolOperationState::NotStarted, None)
    }

    pub(super) fn exited(&self, code: Option<i32>) -> Result<()> {
        self.update(ToolOperationState::Exited, code)
    }

    fn update(&self, state: ToolOperationState, code: Option<i32>) -> Result<()> {
        write_json(
            &self.path,
            &Record {
                tool: self.tool.clone(),
                state,
                code,
            },
        )
    }
}

fn directory(profile: &Path, namespace: &str) -> Result<PathBuf> {
    validate_digest(namespace)?;
    Ok(profile.join("operations").join(namespace))
}

/// Read current receipts without launching any command or guessing external state.
pub(in crate::plugins) fn review(profile: &Path, namespace: &str) -> Result<Vec<ToolOperation>> {
    let entries = match fs::read_dir(directory(profile, namespace)?) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut recent = BTreeMap::new();
    for entry in entries {
        let entry = entry?;
        let filename = entry.file_name();
        let Some(id) = filename
            .to_str()
            .and_then(|name| name.strip_suffix(".json"))
            .filter(|id| {
                id.strip_prefix("tool-").is_some_and(|suffix| {
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
    Ok(recent
        .into_iter()
        .rev()
        .map(|((_, id), path)| {
            let record = read_limited(&path, RECEIPT_LIMIT)
                .and_then(|bytes| Ok(serde_json::from_slice::<Record>(&bytes)?))
                .unwrap_or(Record {
                    tool: "Unknown tool".into(),
                    state: ToolOperationState::Pending,
                    code: None,
                });
            ToolOperation {
                id,
                tool: record.tool,
                state: record.state,
                code: record.code,
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pending_receipt_survives_reopen_and_is_not_promoted_without_an_exit() -> Result<()> {
        let profile = tempfile::tempdir()?;
        let plugin = "1".repeat(64);
        let interrupted = Operation::start(profile.path(), &plugin, "interrupted")?;
        let interrupted_id = interrupted.id().to_owned();
        drop(interrupted);
        let finished = Operation::start(profile.path(), &plugin, "finished")?;
        finished.exited(Some(7))?;
        let records = review(profile.path(), &plugin)?;
        assert_eq!(records.len(), 2);
        assert!(records.iter().any(|record| {
            record.id == interrupted_id && record.state == ToolOperationState::Pending
        }));
        assert!(records.iter().any(|record| {
            record.id == finished.id()
                && record.state == ToolOperationState::Exited
                && record.code == Some(7)
        }));
        assert!(review(profile.path(), &"2".repeat(64))?.is_empty());
        assert_eq!(review(profile.path(), &plugin)?, records);
        Ok(())
    }
}
