//! Persist only changed input slots so simultaneous editor sessions keep each other's drafts.

use crate::plugins::package::{read_json, write_bytes};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};

const STORE_LIMIT: u64 = 16 * 1024 * 1024;

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct Stored {
    inputs: BTreeMap<String, BTreeMap<String, BTreeMap<String, String>>>,
}

#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
struct Slot {
    namespace: String,
    project: String,
    module: String,
}

impl Stored {
    fn value(&self, slot: &Slot) -> Option<&String> {
        self.inputs
            .get(&slot.namespace)?
            .get(&slot.project)?
            .get(&slot.module)
    }

    fn set(&mut self, slot: &Slot, value: String) -> Option<String> {
        self.inputs
            .entry(slot.namespace.clone())
            .or_default()
            .entry(slot.project.clone())
            .or_default()
            .insert(slot.module.clone(), value)
    }

    fn keep_project(&mut self, project: &str) {
        self.inputs.retain(|_, projects| {
            projects.retain(|key, _| key == project);
            !projects.is_empty()
        });
    }
}

/// Hold the current input and the last observed value of each changed slot until it is saved.
pub(super) struct Drafts {
    root: PathBuf,
    stored: Stored,
    pending: BTreeMap<Slot, Option<String>>,
}

impl Drafts {
    pub(super) fn load(root: PathBuf) -> (Self, Option<anyhow::Error>) {
        let saved = read_json::<Stored>(&root.join("drafts.json"), STORE_LIMIT);
        let (stored, error) = match saved {
            Ok(saved) => (saved.unwrap_or_default(), None),
            Err(error) => (Stored::default(), Some(error)),
        };
        (
            Self {
                root,
                stored,
                pending: BTreeMap::new(),
            },
            error,
        )
    }

    pub(super) fn get(&self, namespace: &str, project: &str, module: &str) -> Option<&String> {
        self.stored.inputs.get(namespace)?.get(project)?.get(module)
    }

    pub(super) fn record(&mut self, namespace: &str, project: String, module: &str, text: String) {
        let slot = Slot {
            namespace: namespace.into(),
            project,
            module: module.into(),
        };
        if self.stored.value(&slot) != Some(&text) {
            let prior = self.stored.set(&slot, text);
            self.pending.entry(slot).or_insert(prior);
        }
    }

    pub(super) fn pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Save unrelated changes even when a slot was modified by another editor.
    pub(super) fn flush(&mut self, project: Option<&str>) -> Result<()> {
        if !self
            .pending
            .keys()
            .any(|slot| project.is_none_or(|project| slot.project == project))
        {
            return Ok(());
        }
        let _lock = self.lock()?;
        let mut saved = self.read()?;
        let mut applied = Vec::new();
        let mut conflict = false;
        for (slot, prior) in self
            .pending
            .iter()
            .filter(|(slot, _)| project.is_none_or(|project| slot.project == project))
        {
            let current = self.stored.value(slot).expect("dirty draft exists");
            let disk = saved.value(slot);
            if disk == prior.as_ref() || disk == Some(current) {
                saved.set(slot, current.clone());
                applied.push(slot.clone());
            } else {
                conflict = true;
            }
        }
        if !applied.is_empty() {
            self.write(&saved)?;
            for slot in applied {
                self.pending.remove(&slot);
            }
            for slot in self.pending.keys() {
                saved.set(
                    slot,
                    self.stored.value(slot).expect("dirty draft exists").clone(),
                );
            }
            self.stored = saved;
        }
        ensure!(
            !conflict,
            "A draft changed in another editor. Export your input or discard the unsaved version."
        );
        Ok(())
    }

    pub(super) fn export(&self, destination: &Path, project: Option<&str>) -> Result<()> {
        let mut exported = self.stored.clone();
        if let Some(project) = project {
            exported.keep_project(project);
        }
        let mut file = tempfile::NamedTempFile::new_in(
            destination.parent().context("Invalid export destination")?,
        )?;
        serde_json::to_writer_pretty(file.as_file_mut(), &exported)?;
        file.as_file().sync_all()?;
        file.persist_noclobber(destination)?;
        Ok(())
    }

    pub(super) fn clean(&mut self, namespace: &str) -> Result<()> {
        let _lock = self.lock()?;
        let mut saved = self.read()?;
        if saved.inputs.remove(namespace).is_some() {
            self.write(&saved)?;
        }
        self.stored.inputs.remove(namespace);
        self.pending.retain(|slot, _| slot.namespace != namespace);
        Ok(())
    }

    pub(super) fn discard(&mut self, project: Option<&str>) {
        let mut saved = self.read().unwrap_or_default();
        if let Some(project) = project {
            saved.keep_project(project);
            self.stored.inputs.retain(|_, projects| {
                projects.remove(project);
                !projects.is_empty()
            });
            for (owner, projects) in saved.inputs {
                self.stored
                    .inputs
                    .entry(owner)
                    .or_default()
                    .extend(projects);
            }
            self.pending.retain(|slot, _| slot.project != project);
        } else {
            self.stored = saved;
            self.pending.clear();
        }
    }

    fn read(&self) -> Result<Stored> {
        Ok(read_json(&self.root.join("drafts.json"), STORE_LIMIT)
            .context("Existing draft file could not be read and was kept")?
            .unwrap_or_default())
    }

    fn write(&self, data: &Stored) -> Result<()> {
        let bytes = serde_json::to_vec(data)?;
        ensure!(
            bytes.len() as u64 <= STORE_LIMIT,
            "Saved plugin inputs exceed the profile budget"
        );
        write_bytes(&self.root.join("drafts.json"), &bytes)
    }

    fn lock(&self) -> Result<File> {
        fs::create_dir_all(&self.root)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.root.join("drafts.lock"))?;
        lock.lock()?;
        Ok(lock)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simultaneous_sessions_keep_unrelated_drafts_and_conflicting_input() -> Result<()> {
        let folder = tempfile::tempdir()?;
        let (mut left, _) = Drafts::load(folder.path().to_path_buf());
        let (mut right, _) = Drafts::load(folder.path().to_path_buf());
        left.record("fixture", "one".into(), "notes", "first".into());
        left.flush(None)?;
        right.record("fixture", "one".into(), "notes", "conflicting".into());
        right.record("fixture", "two".into(), "notes", "second".into());
        right.flush(Some("two"))?;
        assert!(right.flush(None).is_err());
        let (reopened, _) = Drafts::load(folder.path().to_path_buf());
        assert_eq!(
            reopened.get("fixture", "one", "notes").map(String::as_str),
            Some("first")
        );
        assert_eq!(
            reopened.get("fixture", "two", "notes").map(String::as_str),
            Some("second")
        );
        assert_eq!(
            right.get("fixture", "one", "notes").map(String::as_str),
            Some("conflicting")
        );
        let destination = folder.path().join("export.json");
        right.export(&destination, Some("one"))?;
        let exported = fs::read_to_string(destination)?;
        assert!(exported.contains("conflicting"));
        assert!(!exported.contains("second"));
        Ok(())
    }
}
