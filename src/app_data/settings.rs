//! Read and atomically update the editor's portable settings file.

mod overrides;
pub use overrides::OverrideFile;

use anyhow::{Context, Result};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    env, fs,
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
};
use toml_edit::{DocumentMut, Item, TableLike, Value};

/// Locate `settings.toml` in the running executable's data directory.
pub fn path() -> Result<PathBuf> {
    Ok(super::directory_for(&env::current_exe()?)?.join("settings.toml"))
}

/// Read the document at `path`, treating a missing file as empty settings.
pub(crate) fn read(path: &Path) -> Result<DocumentMut> {
    match fs::read_to_string(path) {
        Ok(contents) => contents
            .parse()
            .with_context(|| format!("Parse {}", path.display())),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(DocumentMut::new()),
        Err(error) => Err(error).with_context(|| format!("Read {}", path.display())),
    }
}

/// Read the `[ui]` table at `path` and pass it to `read_ui` when present.
pub fn read_ui<T>(path: &Path, read_ui: impl FnOnce(&dyn TableLike) -> T) -> Result<Option<T>> {
    let settings = read(path)?;
    Ok(settings
        .get("ui")
        .and_then(Item::as_table_like)
        .map(read_ui))
}

/// Read serialized Vim settings from the `[vim]` table at `path`.
pub(crate) fn read_vim<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    let mut settings = read(path)?;
    let Some(item) = settings.remove("vim") else {
        return Ok(None);
    };
    let table = item
        .into_table()
        .ok()
        .with_context(|| format!("The vim setting in {} must be a table", path.display()))?;
    toml_edit::de::from_document(DocumentMut::from(table))
        .map(Some)
        .with_context(|| format!("Read Vim settings in {}", path.display()))
}

/// Replace the `[vim]` table at `path` with `value`, retaining adjacent settings.
pub(crate) fn save_vim<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let next = toml_edit::ser::to_document(value)?.into_table();
    update(path, |settings| {
        settings.insert("vim", Item::Table(next));
        Ok(true)
    })
}

/// Set one `[ui]` `key` at `path` to `value`, preserving adjacent settings and comments.
pub fn save_ui_value(path: &Path, key: &str, mut value: Value) -> Result<()> {
    update(path, |settings| {
        let ui = settings
            .entry("ui")
            .or_insert(toml_edit::table())
            .as_table_like_mut()
            .context("The ui setting must be a table")?;
        if let Some(previous) = ui.get_mut(key) {
            if let Some(old_value) = previous.as_value() {
                if old_value
                    .as_str()
                    .is_some_and(|old| Some(old) == value.as_str())
                {
                    return Ok(false);
                }
                *value.decor_mut() = old_value.decor().clone();
                if old_value.to_string() == value.to_string() {
                    return Ok(false);
                }
            }
            *previous = Item::Value(value);
        } else {
            ui.insert(key, Item::Value(value));
        }
        Ok(true)
    })
}

/// The original portable settings document used to recover the disk state at dialog open.
#[derive(Default)]
pub struct SettingsSnapshot(DocumentMut);

impl SettingsSnapshot {
    /// Read the on-disk baseline, including absent fields and unsaved-in-memory differences.
    pub fn load(path: Option<&Path>) -> Result<Self> {
        path.map(read)
            .transpose()
            .map(|doc| Self(doc.unwrap_or_default()))
    }

    /// Read the opening spelling of a string preference when restoring a settings page.
    pub(crate) fn string(&self, keys: &[&str]) -> Option<&str> {
        self.item(keys)?.as_str()
    }

    fn item(&self, keys: &[&str]) -> Option<&Item> {
        let mut item = self.0.as_item();
        for &key in keys {
            item = item.get(key)?;
        }
        Some(item)
    }
}

/// Settings that have been returned to their opening values should also regain their
/// original representation on disk (including absence or an independently saved value).
#[derive(Clone, Copy)]
pub struct RestoreFields<'a> {
    pub snapshot: &'a SettingsSnapshot,
    pub names: &'a [&'a str],
}

/// Atomically change only the specified leaves, preserving unrelated concurrent writes.
pub fn save_fields(
    path: &Path,
    fields: &[(&[&str], Option<Value>)],
    restore: Option<RestoreFields<'_>>,
) -> Result<()> {
    update(path, |settings| edit_fields(settings, fields, restore))
}

/// Apply the leaf patch in memory, retaining neighboring entries and original decorations.
fn edit_fields(
    settings: &mut DocumentMut,
    fields: &[(&[&str], Option<Value>)],
    restore: Option<RestoreFields<'_>>,
) -> Result<bool> {
    let mut changed = false;
    for (keys, value) in fields {
        let (key, parents) = keys.split_last().context("Missing settings field")?;
        let opening = restore.filter(|restore| restore.names.contains(key));
        let value = if let Some(opening) = opening {
            opening.snapshot.item(keys).and_then(Item::as_value)
        } else {
            value.as_ref()
        };
        let mut table: &mut dyn TableLike = settings.as_table_mut();
        let mut absent = false;
        for &parent in parents {
            if value.is_none() && !table.contains_key(parent) {
                absent = true;
                break;
            }
            table = table
                .entry(parent)
                .or_insert(toml_edit::table())
                .as_table_like_mut()
                .with_context(|| format!("Expected a settings table at {parent}"))?;
        }
        if absent {
            continue;
        }
        if let Some(mut next) = value.cloned() {
            if let Some(old) = table.get(key).and_then(Item::as_value) {
                *next.decor_mut() = old.decor().clone();
                if next.to_string() == old.to_string() {
                    continue;
                }
            }
            table.insert(key, Item::Value(next));
            changed = true;
        } else {
            changed |= table.remove(key).is_some();
            if let Some(opening) = opening {
                prune_new_tables(settings.as_table_mut(), parents, opening.snapshot, &[]);
            }
        }
    }
    Ok(changed)
}

/// Remove empty parent tables introduced by this dialog while retaining original tables
/// and any entries another settings writer added after the dialog opened.
fn prune_new_tables(
    table: &mut dyn TableLike,
    path: &[&str],
    opening: &SettingsSnapshot,
    parents: &[&str],
) {
    let Some((&key, tail)) = path.split_first() else {
        return;
    };
    let Some(child) = table.get_mut(key).and_then(Item::as_table_like_mut) else {
        return;
    };
    let mut full_path = parents.to_vec();
    full_path.push(key);
    prune_new_tables(child, tail, opening, &full_path);
    if child.is_empty() && opening.item(&full_path).is_none() {
        table.remove(key);
    }
}

/// Apply `edit` to the latest settings at `path` under a lock and publish changes atomically.
fn update(path: &Path, edit: impl FnOnce(&mut DocumentMut) -> Result<bool>) -> Result<()> {
    let _lock = super::lock_data_file(path)?;
    let mut settings = read(path)?;
    if edit(&mut settings)? {
        publish(path, &settings.to_string())?;
    }
    Ok(())
}

fn publish(path: &Path, contents: &str) -> Result<()> {
    let dir = path.parent().context("Settings file has no parent")?;
    let mut staged = tempfile::NamedTempFile::new_in(dir)?;
    staged.write_all(contents.as_bytes())?;
    staged.as_file().sync_all()?;
    staged
        .persist(path)
        .with_context(|| format!("Save {}", path.display()))?;
    Ok(())
}
