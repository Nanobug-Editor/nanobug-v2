//! Keep a settings session's exact override representation across overlapping TOML updates.

use super::{RestoreFields, SettingsSnapshot, edit_fields, publish, read};
use anyhow::{Context as _, Result, ensure};
use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};
use toml_edit::{Item, Value};

/// Opening values and this session's own successful writes, separate from other file edits.
pub struct OverrideFile {
    path: PathBuf,
    existed: bool,
    opening: SettingsSnapshot,
    applied: SettingsSnapshot,
}

impl OverrideFile {
    /// Capture existing comments, missing tables, and the absence of a file without creating it.
    pub fn load(path: &Path) -> Result<Self> {
        let (doc, existed) = match fs::read_to_string(path) {
            Ok(text) => (
                text.parse()
                    .with_context(|| format!("Parse {}", path.display()))?,
                true,
            ),
            Err(error) if error.kind() == ErrorKind::NotFound => {
                (toml_edit::DocumentMut::default(), false)
            }
            Err(error) => return Err(error.into()),
        };
        Ok(Self {
            path: path.to_owned(),
            existed,
            opening: SettingsSnapshot(doc.clone()),
            applied: SettingsSnapshot(doc),
        })
    }

    /// The raw opening override, independently of values inherited by the runtime.
    pub fn opening(&self, keys: &[&str]) -> Option<&Value> {
        self.opening.item(keys)?.as_value()
    }

    /// Return the original TOML for an owner that also retains uncommitted live state.
    pub fn opening_document(&self) -> String {
        self.opening.0.to_string()
    }

    /// Atomically patch only unchanged owner fields, validate the merged document, and restore
    /// any named original representations. An absent file is removed after its last edit is undone.
    pub fn write<T>(
        &mut self,
        fields: &[(&[&str], Option<Value>)],
        restore: &[&str],
        validate: impl FnOnce(&str) -> Result<T>,
    ) -> Result<T> {
        let _lock = crate::app_data::lock_data_file(&self.path)?;
        let mut current = SettingsSnapshot(read(&self.path)?);
        for (keys, _) in fields {
            ensure!(
                self.applied.item(keys).map(undecorated) == current.item(keys).map(undecorated),
                "{} changed since opening {}",
                keys.join("."),
                self.path.display()
            );
        }
        let baseline = Some(RestoreFields {
            snapshot: &self.opening,
            names: restore,
        });
        let changed = edit_fields(&mut current.0, fields, baseline)?;
        let contents = current.0.to_string();
        let result = validate(&contents)?;
        if changed {
            if !self.existed && contents.trim().is_empty() {
                fs::remove_file(&self.path)?;
            } else {
                publish(&self.path, &contents)?;
            }
        }
        // Remember exactly what was published, including comments added on unrelated rows.
        // Keep unedited fields' opening expectations so a future edit can detect a conflict.
        for (keys, _) in fields {
            let value = current.item(keys).and_then(Item::as_value).cloned();
            edit_fields(&mut self.applied.0, &[(*keys, value)], None)?;
        }
        Ok(result)
    }
}

/// Formatting is supplied by the latest file. A newly inserted TOML value acquires its
/// first leading space only when serialized, so formatting must not cause a write conflict.
fn undecorated(item: &Item) -> String {
    if let Some(mut value) = item.as_value().cloned() {
        value.decor_mut().clear();
        value.to_string()
    } else {
        item.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::bail;

    #[test]
    fn cancel_removes_new_override_and_its_empty_file() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("cursor-effects.toml");
        let mut doc = OverrideFile::load(&path)?;
        let keys = &["stages", "prepare", "duration_ms"][..];
        doc.write(&[(keys, Some(75.into()))], &[], |_| Ok(()))?;
        doc.write(&[(keys, None)], &["duration_ms"], |_| Ok(()))?;
        assert!(!path.exists());
        Ok(())
    }

    #[test]
    fn retries_preserve_comments_and_unrelated_writes_and_reject_conflicting_values() -> Result<()>
    {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("cursor-effects.toml");
        let initial = "# tuning\n[stages.prepare]\nduration_ms = 15 # milliseconds\n";
        fs::write(&path, initial)?;
        let mut doc = OverrideFile::load(&path)?;
        let keys = &["stages", "prepare", "duration_ms"][..];
        let proposed = &[(keys, Some(50.into()))];
        assert!(
            doc.write(proposed, &[], |_| -> Result<()> {
                bail!("validation rejected")
            })
            .is_err()
        );
        assert_eq!(fs::read_to_string(&path)?, initial);
        doc.write(proposed, &[], |_| Ok(()))?;
        let edited = fs::read_to_string(&path)? + "insert_ms = 8\n";
        fs::write(&path, &edited)?;
        doc.write(&[(keys, None)], &["duration_ms"], |_| Ok(()))?;
        assert_eq!(
            fs::read_to_string(&path)?,
            format!("{initial}insert_ms = 8\n")
        );
        fs::write(&path, edited.replace("50", "33"))?;
        assert!(doc.write(proposed, &[], |_| Ok(())).is_err());
        assert!(fs::read_to_string(&path)?.contains("33"));
        Ok(())
    }
}
