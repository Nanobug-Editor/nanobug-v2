//! Resolve per-plugin project scan exclusions for native servers and document synchronization.

use super::{file_uri, uri_path};
use crate::plugins::package::{read_json, read_limited};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::BTreeSet,
    io,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

/// Source of editable exclusions. Absence can inherit the legacy Rust configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScanOverride {
    Missing(Vec<String>),
    Unset,
    Direct(Vec<String>),
}

impl ScanOverride {
    /// Editor-visible manual paths, excluding automatic plugin rules.
    pub fn directories(&self) -> &[String] {
        match self {
            Self::Missing(paths) | Self::Direct(paths) => paths,
            Self::Unset => &[],
        }
    }

    /// Read the native TOML override, preserving inheritance information for Cancel.
    pub fn read(root: &Path, plugin_id: &str) -> Result<Self> {
        let path = scan_path(root, plugin_id)?;
        match read_scan_document(&path)? {
            Some(doc) => override_in(&doc),
            None => Ok(Self::Missing(legacy_directories(root, plugin_id)?)),
        }
    }

    /// Patch just the manual list. Preserve unrelated plugin TOML and reject stale writes.
    pub(in crate::plugins) fn replace(
        &self,
        root: &Path,
        plugin_id: &str,
        next: &Self,
    ) -> Result<()> {
        for path in next.directories() {
            validate_directory(path, plugin_id)?;
        }
        let path = scan_path(root, plugin_id)?;
        let _lock = crate::app_data::lock_data_file(&path)?;
        let document = read_scan_document(&path)?;
        let current = match document.as_ref() {
            Some(doc) => override_in(doc)?,
            None => Self::Missing(legacy_directories(root, plugin_id)?),
        };
        ensure!(
            current == *self,
            "Scan exclusions changed on disk. Reopen settings."
        );
        let mut doc = document.unwrap_or_default();
        if let Self::Direct(items) = next {
            let array = items.iter().cloned().collect::<toml_edit::Array>();
            let mut replacement = toml_edit::Value::Array(array);
            if let Some(old) = doc.get("excludeDirs").and_then(toml_edit::Item::as_value) {
                *replacement.decor_mut() = old.decor().clone();
            }
            doc["excludeDirs"] = toml_edit::Item::Value(replacement);
        } else {
            doc.remove("excludeDirs");
        }
        let contents = doc.to_string();
        if let Self::Missing(opening) = next {
            ensure!(
                contents.trim().is_empty(),
                "The new plugin file gained other settings. Reopen settings to preserve those edits."
            );
            ensure!(
                &legacy_directories(root, plugin_id)? == opening,
                "Inherited exclusions changed on disk. Reopen settings."
            );
            if path.exists() {
                std::fs::remove_file(&path)?;
            }
        } else {
            ensure!(contents.len() <= 64 * 1024, "Scan settings exceed 64 KiB");
            crate::plugins::package::write_bytes(&path, contents.as_bytes())?;
        }
        Ok(())
    }
}

fn scan_path(root: &Path, plugin_id: &str) -> Result<PathBuf> {
    ensure!(
        crate::plugins::package::valid_id(plugin_id),
        "Invalid plugin ID"
    );
    crate::plugins::project_path(root, &format!(".nb/plugin-setting/{plugin_id}.toml"))
}

fn read_scan_document(path: &Path) -> Result<Option<toml_edit::DocumentMut>> {
    match read_limited(path, 64 * 1024) {
        Ok(bytes) => Ok(Some(std::str::from_utf8(&bytes)?.parse()?)),
        Err(error)
            if error
                .downcast_ref::<io::Error>()
                .is_some_and(|e| e.kind() == io::ErrorKind::NotFound) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn override_in(doc: &toml_edit::DocumentMut) -> Result<ScanOverride> {
    let Some(item) = doc.get("excludeDirs") else {
        return Ok(ScanOverride::Unset);
    };
    let paths = item
        .as_array()
        .context("excludeDirs must be an array of paths")?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .context("Expected directory name")
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(ScanOverride::Direct(paths))
}

fn validate_directory(directory: &str, plugin_id: &str) -> Result<()> {
    let mut components = Path::new(directory).components();
    ensure!(
        !directory.contains('\0')
            && matches!(components.next(), Some(Component::Normal(_)))
            && components.all(|part| matches!(part, Component::Normal(_))),
        "Scan exclusions for plugin {plugin_id} must be relative to the workspace: {directory}"
    );
    Ok(())
}

/// Identify the owning plugin and the scan exclusions from its current JS match.
pub(in crate::plugins) struct ProjectSettings {
    /// Validated package manifest ID used to name the project's TOML file.
    pub(in crate::plugins) plugin_id: String,
    /// Project-relative directories proposed by the plugin rule.
    pub(in crate::plugins) automatic_exclusions: Arc<[String]>,
}

/// Additional exclusions read from `.nb/plugin-setting/<plugin-id>.toml`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ScanSettings {
    /// Concrete paths relative to the project root.
    #[serde(default)]
    exclude_dirs: Vec<String>,
}

/// The merged plugin and project directories for one language-server session.
#[derive(Default)]
pub(super) struct ScanExclusions {
    /// Relative paths with separators normalized for native server settings.
    directories: Vec<String>,
    /// Absolute prefixes for editor paths and their corresponding file URIs.
    prefixes: Vec<PathBuf>,
}

impl ScanExclusions {
    /// Load the owning plugin's project `settings` under `root` and retain each directory once.
    pub(super) fn load(root: &Path, settings: Option<&ProjectSettings>) -> Result<Self> {
        let mut exclusions = Self::default();
        let Some(settings) = settings else {
            return Ok(exclusions);
        };
        let manual = ScanOverride::read(root, &settings.plugin_id)?;
        let mut seen = BTreeSet::new();
        for directory in settings
            .automatic_exclusions
            .iter()
            .chain(manual.directories())
        {
            let path = Path::new(directory);
            validate_directory(directory, &settings.plugin_id)?;
            let relative = path.components().collect::<PathBuf>();
            if seen.insert(relative.clone()) {
                let prefix = root.join(relative);
                if let Some(uri_prefix) = file_uri(&prefix).as_deref().and_then(uri_path)
                    && uri_prefix != prefix
                {
                    exclusions.prefixes.push(uri_prefix);
                }
                exclusions.prefixes.push(prefix);
                exclusions.directories.push(
                    path.iter()
                        .map(|part| part.to_str().expect("UTF-8 exclusion path"))
                        .collect::<Vec<_>>()
                        .join("/"),
                );
            }
        }
        Ok(exclusions)
    }

    /// Return project-relative paths for the native server's own scanning configuration.
    pub(super) fn directories(&self) -> &[String] {
        &self.directories
    }

    /// Match an absolute editor path `path` by whole path components.
    pub(super) fn contains(&self, path: &Path) -> bool {
        self.prefixes.iter().any(|prefix| path.starts_with(prefix))
            || !self.prefixes.is_empty()
                && file_uri(path)
                    .as_deref()
                    .and_then(uri_path)
                    .is_some_and(|path| self.prefixes.iter().any(|prefix| path.starts_with(prefix)))
    }

    /// Match a language-server file `uri` after decoding it to a native path.
    pub(super) fn contains_uri(&self, uri: &str) -> bool {
        !self.prefixes.is_empty() && uri_path(uri).is_some_and(|path| self.contains(&path))
    }

    /// Remove excluded local destinations from a definition `result` (Location or LocationLink).
    pub(super) fn filter_definitions(&self, result: &mut Value) {
        let keep = |item: &Value| {
            item.get("targetUri")
                .or_else(|| item.get("uri"))
                .and_then(Value::as_str)
                .is_none_or(|uri| !self.contains_uri(uri))
        };
        if let Some(items) = result.as_array_mut() {
            items.retain(keep);
        } else if !keep(result) {
            *result = Value::Null;
        }
    }
}

/// Only Rust inherits an earlier JSON list when the per-plugin TOML file is missing.
fn legacy_directories(root: &Path, plugin_id: &str) -> Result<Vec<String>> {
    if plugin_id != "rust" {
        return Ok(vec![]);
    }
    let legacy = crate::plugins::project_path(root, ".nb/rust-analyzer.json")?;
    let settings: Option<ScanSettings> =
        read_json(&legacy, 64 * 1024).with_context(|| format!("Read {}", legacy.display()))?;
    Ok(settings
        .map(|settings| settings.exclude_dirs)
        .unwrap_or_default())
}

#[cfg(test)]
mod dialog_tests {
    use super::*;
    use std::fs;

    #[test]
    fn settings_dialog_scan_list_round_trips_inheritance_and_preserves_unrelated_fields()
    -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        fs::create_dir_all(root.join(".nb"))?;
        fs::write(
            root.join(".nb/rust-analyzer.json"),
            r#"{"excludeDirs":["legacy"]}"#,
        )?;
        let opening = ScanOverride::read(root, "rust")?;
        assert_eq!(opening, ScanOverride::Missing(vec!["legacy".into()]));
        let next = ScanOverride::Direct(vec!["build/缓存".into()]);
        opening.replace(root, "rust", &next)?;
        assert_eq!(ScanOverride::read(root, "rust")?, next);
        assert!(opening.replace(root, "rust", &next).is_err());
        next.replace(root, "rust", &opening)?;
        assert_eq!(ScanOverride::read(root, "rust")?, opening);
        let path = scan_path(root, "rust")?;
        fs::write(&path, "# Preserve these fields\nworkers = 3\n")?;
        ScanOverride::Unset.replace(root, "rust", &next)?;
        let bad = ScanOverride::Direct(vec!["../elsewhere".into()]);
        assert!(next.replace(root, "rust", &bad).is_err());
        next.replace(root, "rust", &ScanOverride::Unset)?;
        assert_eq!(
            fs::read_to_string(path)?,
            "# Preserve these fields\nworkers = 3\n"
        );
        Ok(())
    }
}
