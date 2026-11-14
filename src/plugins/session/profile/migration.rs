//! Import the previous per-user plugin state into a portable release.

use super::{check_legacy_mode, overlap};
use crate::{
    app_data::copy_missing_file,
    plugins::{
        credentials::Credentials,
        package::{InstallStore, read_json, write_bytes},
        query::resolve_prefix,
        session::CatalogEntry,
    },
};
use anyhow::{Context, Result, ensure};
use std::{
    ffi::OsStr,
    fs::{self, OpenOptions},
    io::ErrorKind,
    path::Path,
};

/// Marks the destination's first successful import, including fresh installations.
const IMPORTED: &str = "legacy-imported";

/// Persistent profile files, including the references used for OS-backed secrets.
const FILES: &[&str] = &[
    "drafts.json",
    "credentials.json",
    "execution.json",
    "retained.json",
    "developer.json",
];

/// Import `legacy` into the destination `store` before a session can modify it.
/// Leave the previous profile available to older builds and retry partial copies on restart.
pub(super) fn import(legacy: Option<&Path>, store: &InstallStore) -> Result<()> {
    let state = store.state_root();
    let completed = state.join(IMPORTED);
    if completed.try_exists()? {
        return Ok(());
    }
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(state.join("legacy-import.lock"))?;
    lock.lock()?;
    if completed.try_exists()? {
        return Ok(());
    }

    let legacy = legacy.map(resolve_prefix).transpose()?;
    let legacy = legacy.as_deref();
    if let Some(source) = legacy {
        ensure!(
            !overlap(source, state) && (source == store.root() || !overlap(source, store.root())),
            "Legacy plugin data overlaps the new profile"
        );
        check_legacy_mode(source)?;
        if source != store.root() {
            store.import_installations(&InstallStore::new(source.to_path_buf()))?;
        }
    }

    let namespace = match legacy {
        _ if state.join("credentials.json").try_exists()? => Some(state),
        Some(source) if source.join("credentials.json").try_exists()? => Some(source),
        _ => None,
    };
    // Publish the vault namespace before its references, without reading or copying secrets.
    Credentials::initialize_namespace(state, namespace)?;
    if let Some(source) = legacy {
        for name in FILES {
            copy_missing_file(&source.join(name), &state.join(name))?;
        }
        for directory in ["config", "operations"] {
            import_records(&source.join(directory), &state.join(directory))?;
        }
        import_catalog(source, state)?;
    }
    tempfile::NamedTempFile::new_in(state)?.persist_noclobber(completed)?;
    Ok(())
}

/// Preserve ZIP paths relative to `source` when moving its catalog to `state`.
fn import_catalog(source: &Path, state: &Path) -> Result<()> {
    let old = source.join("marketplace.json");
    let new = state.join("marketplace.json");
    if new.try_exists()? {
        return Ok(());
    }
    let Ok(Some(mut entries)) = read_json::<Vec<CatalogEntry>>(&old, 2 * 1024 * 1024) else {
        // Keep unreadable catalogs available for the normal Marketplace diagnostics.
        return copy_missing_file(&old, &new);
    };
    for entry in &mut entries {
        entry.zip = source.join(&entry.zip);
    }
    write_bytes(&new, &serde_json::to_vec(&entries)?)
}

/// Copy saved JSON records under `source` into the same layout at `destination`.
/// Configuration and receipt directories also contain transient lock or staging files.
fn import_records(source: &Path, destination: &Path) -> Result<()> {
    match fs::symlink_metadata(source) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => anyhow::bail!("{} is not a directory", source.display()),
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).with_context(|| format!("Read {}", source.display())),
    }
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let path = entry.path();
        let target = destination.join(entry.file_name());
        if kind.is_dir() {
            import_records(&path, &target)?;
        } else if kind.is_file() && path.extension() == Some(OsStr::new("json")) {
            copy_missing_file(&path, &target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Repair an interrupted copy and keep new edits and deletions on subsequent starts.
    #[test]
    fn resumes_a_partial_import_and_keeps_the_portable_state() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let legacy = temp.path().join("user/plugins-v1");
        let state = temp.path().join("release/data/plugins");
        let store =
            InstallStore::new(temp.path().join("release/plugins")).with_state_root(state.clone());
        fs::create_dir_all(legacy.join("config"))?;
        fs::create_dir_all(legacy.join("operations/plugin"))?;
        fs::create_dir_all(legacy.join("compiled"))?;
        fs::create_dir_all(&state)?;
        let draft = br#"{"inputs":{"owner":{"project":{"module":"saved draft"}}}}"#;
        let project_settings = br#"{"draft":{"plugins":[]}}"#;
        fs::write(legacy.join("drafts.json"), draft)?;
        fs::write(legacy.join("config/project-local.json"), project_settings)?;
        fs::write(legacy.join("config/project-local.lock"), b"")?;
        fs::write(legacy.join("operations/plugin/tool-result.json"), b"{}")?;
        fs::write(legacy.join("compiled/cache"), b"rebuildable")?;
        fs::write(
            legacy.join("marketplace.json"),
            br#"[{"identity":{"name":"Fixture","id":"fixture"},"version":"1.0.0","zip":"downloads/fixture.zip"}]"#,
        )?;
        fs::write(state.join("config"), b"inaccessible directory")?;

        assert!(import(Some(&legacy), &store).is_err());
        assert_eq!(fs::read(state.join("drafts.json"))?, draft);
        assert!(!state.join(IMPORTED).exists());
        fs::remove_file(state.join("config"))?;
        import(Some(&legacy), &store)?;
        assert_eq!(
            fs::read(state.join("config/project-local.json"))?,
            project_settings
        );
        assert!(state.join("operations/plugin/tool-result.json").is_file());
        assert!(!state.join("config/project-local.lock").exists());
        assert!(!state.join("compiled").exists());
        let catalog = read_json::<Vec<CatalogEntry>>(&state.join("marketplace.json"), 4096)?;
        assert_eq!(
            catalog.unwrap()[0].zip,
            legacy.canonicalize()?.join("downloads/fixture.zip")
        );

        fs::remove_file(state.join("drafts.json"))?;
        fs::write(state.join("config/project-local.json"), b"{}")?;
        import(Some(&legacy), &store)?;
        assert!(!state.join("drafts.json").exists());
        assert_eq!(fs::read(state.join("config/project-local.json"))?, b"{}");
        assert_eq!(fs::read(legacy.join("drafts.json"))?, draft);
        Ok(())
    }
}
