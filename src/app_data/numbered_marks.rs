//! Portable history for Vim's numbered file marks.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    env, fs,
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
};

/// Maximum persisted positions, indexed from Vim mark 0 through 9.
const LIMIT: usize = 10;

/// A file location saved independently of an open document or pane.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Mark {
    /// File containing the last caret.
    pub path: PathBuf,
    /// Zero-based document row.
    pub row: usize,
    /// Zero-based display column.
    pub column: usize,
}

/// Locate the numbered history beside the running editor.
pub fn path() -> Result<PathBuf> {
    Ok(super::directory_for(&env::current_exe()?)?.join("vim-numbered-marks.json"))
}

/// Load the newest ten positions at `path`, or an empty history for a fresh installation.
pub fn load(path: &Path) -> Result<Vec<Mark>> {
    let mut marks: Vec<Mark> = match fs::read(path) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).with_context(|| format!("Parse {}", path.display()))?
        }
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| format!("Read {}", path.display())),
    };
    marks.truncate(LIMIT);
    Ok(marks)
}

/// Move `mark` to the head of the latest history and return the records published at `path`.
pub fn remember(path: &Path, mark: Mark) -> Result<Vec<Mark>> {
    let _lock = super::lock_data_file(path)?;
    let mut marks = load(path)?;
    marks.retain(|prior| prior != &mark);
    marks.insert(0, mark);
    marks.truncate(LIMIT);
    publish(path, &marks)?;
    Ok(marks)
}

/// Write live `marks`, merging older portable positions unless `force` replaces the file.
pub fn export(path: &Path, marks: &[Mark], force: bool) -> Result<Vec<Mark>> {
    let _lock = super::lock_data_file(path)?;
    let mut output = marks.to_vec();
    if !force {
        merge(&mut output, load(path)?);
    }
    output.truncate(LIMIT);
    publish(path, &output)?;
    Ok(output)
}

/// Append new caret positions to `marks` without displacing the existing mark numbers.
pub fn merge(marks: &mut Vec<Mark>, incoming: impl IntoIterator<Item = Mark>) {
    for mark in incoming {
        if marks.len() >= LIMIT {
            break;
        }
        if !mark.path.as_os_str().is_empty() && !marks.contains(&mark) {
            marks.push(mark);
        }
    }
}

/// Clear addressed `slots` while keeping the other numbered mark positions unchanged.
pub fn delete(path: &Path, slots: &[usize]) -> Result<Vec<Mark>> {
    let _lock = super::lock_data_file(path)?;
    let mut marks = load(path)?;
    clear_slots(&mut marks, slots);
    publish(path, &marks)?;
    Ok(marks)
}

/// Represent a cleared slot with an empty path so old numbered positions remain stable.
pub fn clear_slots(marks: &mut Vec<Mark>, slots: &[usize]) {
    for &slot in slots {
        if let Some(mark) = marks.get_mut(slot) {
            mark.path.clear();
            mark.row = 0;
            mark.column = 0;
        }
    }
    while marks
        .last()
        .is_some_and(|mark| mark.path.as_os_str().is_empty())
    {
        marks.pop();
    }
}

/// Publish the numbered history next to the currently running editor.
fn publish(path: &Path, marks: &[Mark]) -> Result<()> {
    let dir = path
        .parent()
        .context("Numbered marks have no parent directory")?;
    let mut staged = tempfile::NamedTempFile::new_in(dir)?;
    serde_json::to_writer(&mut staged, marks)?;
    staged.write_all(b"\n")?;
    staged.as_file().sync_all()?;
    staged
        .persist(path)
        .with_context(|| format!("Save {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A marker moved to the front keeps its exact caret and evicts no additional record.
    #[test]
    fn history_merges_records_and_rotates_ten_unique_positions() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("marks.json");
        assert!(load(&path)?.is_empty());
        let mut mark = Mark {
            path: dir.path().join("中文.txt"),
            row: 0,
            column: 4,
        };
        for row in 0..12 {
            mark.row = row;
            remember(&path, mark.clone())?;
        }
        assert_eq!(
            load(&path)?.iter().map(|mark| mark.row).collect::<Vec<_>>(),
            (2..12).rev().collect::<Vec<_>>()
        );
        mark.row = 7;
        let history = remember(&path, mark.clone())?;
        assert_eq!(history.len(), LIMIT);
        assert_eq!(history[0], mark);
        assert_eq!(history[1].row, 11);
        assert_eq!(load(&path)?, history);
        Ok(())
    }

    /// Remove a numbered mark without shifting any of its remaining neighbours.
    #[test]
    fn deletion_retains_numbered_slots_on_disk() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("marks.json");
        for row in 0..3 {
            remember(
                &path,
                Mark {
                    path: dir.path().join("remembered.txt"),
                    row,
                    column: row,
                },
            )?;
        }
        let marks = delete(&path, &[1])?;
        assert_eq!(marks[0].row, 2);
        assert!(marks[1].path.as_os_str().is_empty());
        assert_eq!(marks[2].row, 0);
        assert_eq!(load(&path)?, marks);
        Ok(())
    }
}
