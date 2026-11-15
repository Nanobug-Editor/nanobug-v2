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
        let manual = manual_scan_settings(root, &settings.plugin_id)?;
        let mut seen = BTreeSet::new();
        for directory in settings
            .automatic_exclusions
            .iter()
            .chain(manual.iter().flat_map(|settings| &settings.exclude_dirs))
        {
            let path = Path::new(directory);
            let mut components = path.components();
            ensure!(
                !directory.contains('\0')
                    && matches!(components.next(), Some(Component::Normal(_)))
                    && components.all(|part| matches!(part, Component::Normal(_))),
                "Scan exclusions for plugin {} must be relative to the workspace: {directory}",
                settings.plugin_id
            );
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

/// Read `plugin_id` settings under project `root`, with legacy fallback for Rust projects.
fn manual_scan_settings(root: &Path, plugin_id: &str) -> Result<Option<ScanSettings>> {
    const LIMIT: u64 = 64 * 1024;
    let path = root
        .join(".nb/plugin-setting")
        .join(format!("{plugin_id}.toml"));
    match read_limited(&path, LIMIT) {
        Ok(bytes) => {
            let settings = std::str::from_utf8(&bytes)
                .context("Settings must be UTF-8")
                .and_then(|text| Ok(toml::from_str(text)?))
                .with_context(|| format!("Read {}", path.display()))?;
            Ok(Some(settings))
        }
        Err(error)
            if error
                .downcast_ref::<io::Error>()
                .is_some_and(|e| e.kind() == io::ErrorKind::NotFound) =>
        {
            if plugin_id != "rust" {
                return Ok(None);
            }
            let legacy = root.join(".nb/rust-analyzer.json");
            read_json(&legacy, LIMIT).with_context(|| format!("Read {}", legacy.display()))
        }
        Err(error) => Err(error).with_context(|| format!("Read {}", path.display())),
    }
}
