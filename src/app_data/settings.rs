//! Read and atomically update the editor's portable settings file.

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

/// Apply `edit` to the latest settings at `path` under a lock and publish changes atomically.
fn update(path: &Path, edit: impl FnOnce(&mut DocumentMut) -> Result<bool>) -> Result<()> {
    let _lock = super::lock_data_file(path)?;
    let mut settings = read(path)?;
    if edit(&mut settings)? {
        let dir = path.parent().context("Settings file has no parent")?;
        let mut staged = tempfile::NamedTempFile::new_in(dir)?;
        staged.write_all(settings.to_string().as_bytes())?;
        staged.as_file().sync_all()?;
        staged
            .persist(path)
            .with_context(|| format!("Save {}", path.display()))?;
    }
    Ok(())
}
