//! OS-backed credentials with recoverable reference swaps. Secret bytes never enter profile files.

use super::{
    package::{hash_bytes, read_json, read_limited, write_bytes},
    query,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};
use zeroize::Zeroizing;

const SERVICE: &str = "Nanobug.Plugin.V1";
const INDEX_LIMIT: u64 = 512 * 1024;
/// Vault account prefix stored with a portable profile so its path can change.
const NAMESPACE_FILE: &str = "credential-namespace";
pub(super) const SECRET_LIMIT: usize = 2048;

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct Index {
    entries: BTreeMap<String, BTreeMap<String, String>>,
    #[serde(deserialize_with = "read_cleanup")]
    cleanup: BTreeMap<String, Option<String>>,
}

/// The new value is visible, but a failed storage flush left the commit uncertain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SaveOutcome {
    Saved,
    Uncertain,
}

impl SaveOutcome {
    pub(super) fn confirmed(self) -> Result<()> {
        ensure!(
            self == Self::Saved,
            "Credential save could not be confirmed. Both entries were kept. Retry Apply."
        );
        Ok(())
    }
}

/// Once detached, the SDK cannot read a credential even if vault cleanup needs a retry.
#[derive(Debug, Eq, PartialEq)]
pub(super) enum DeleteOutcome {
    Removed,
    CleanupPending(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Publication {
    Durable,
    Visible,
}

/// Earlier profiles stored cleanup references without their owning plugin.
fn read_cleanup<'de, D: serde::Deserializer<'de>>(
    input: D,
) -> Result<BTreeMap<String, Option<String>>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Cleanup {
        Owned(BTreeMap<String, Option<String>>),
        Legacy(BTreeSet<String>),
    }
    Ok(match Cleanup::deserialize(input)? {
        Cleanup::Owned(owned) => owned,
        Cleanup::Legacy(references) => references
            .into_iter()
            .map(|reference| (reference, None))
            .collect(),
    })
}

/// Local references identify system vault entries within the profile's credential namespace.
#[derive(Clone)]
pub(super) struct Credentials(PathBuf);

impl Credentials {
    pub(super) fn new(root: PathBuf) -> Self {
        Self(root)
    }

    /// Initialize `destination` with the vault namespace of its imported `source`, if any.
    pub(super) fn initialize_namespace(destination: &Path, source: Option<&Path>) -> Result<()> {
        let path = destination.join(NAMESPACE_FILE);
        if !path.try_exists()? {
            let namespace = if let Some(source) = source {
                Self::new(source.to_path_buf()).namespace()?
            } else {
                let marker = tempfile::NamedTempFile::new_in(destination)?;
                query::project_key(marker.path())
            };
            write_bytes(&path, namespace.as_bytes())?;
        }
        Ok(())
    }

    /// Read the portable vault namespace, or derive it from the path of an older profile.
    fn namespace(&self) -> Result<String> {
        match read_limited(&self.0.join(NAMESPACE_FILE), 64) {
            Ok(bytes) => {
                let namespace = String::from_utf8(bytes)?;
                ensure!(valid_reference(&namespace), "Invalid credential namespace");
                Ok(namespace)
            }
            Err(error)
                if error
                    .downcast_ref::<io::Error>()
                    .is_some_and(|error| error.kind() == io::ErrorKind::NotFound) =>
            {
                Ok(query::project_key(&query::resolve_prefix(&self.0)?))
            }
            Err(error) => Err(error),
        }
    }

    pub(super) fn names(&self) -> Result<BTreeSet<String>> {
        let index = self.index()?;
        Ok(index
            .entries
            .into_keys()
            .chain(index.cleanup.into_values().flatten())
            .collect())
    }

    pub(super) fn get(&self, owner: &str, key: &str) -> Result<Option<Zeroizing<String>>> {
        let _lock = self.lock()?;
        let index = self.index()?;
        let Some(reference) = index.entries.get(owner).and_then(|values| values.get(key)) else {
            return Ok(None);
        };
        self.entry(reference)?
            .get_password()
            .map(Zeroizing::new)
            .map(Some)
            .map_err(|error| storage_error(&error))
    }

    pub(super) fn set(&self, owner: &str, key: &str, secret: &str) -> Result<SaveOutcome> {
        self.replace(owner, key, secret, Self::save_secret, Self::write)
    }

    /// Stage a new vault entry, then publish its reference while the prior entry still exists.
    fn replace(
        &self,
        owner: &str,
        key: &str,
        secret: &str,
        save: impl FnOnce(&keyring::Entry, &str) -> Result<()>,
        publish: impl FnOnce(&Self, &Index) -> Result<Publication>,
    ) -> Result<SaveOutcome> {
        ensure!(
            !secret.is_empty() && secret.len() <= SECRET_LIMIT,
            "Credential is empty or too long"
        );
        let _lock = self.lock()?;
        let mut index = self.index()?;
        let marker = tempfile::Builder::new()
            .prefix("credential-")
            .tempfile_in(&self.0)?;
        let reference = hash_bytes(format!("{owner}:{key}:{}", marker.path().display()).as_bytes());
        ensure!(
            !index.cleanup.contains_key(&reference)
                && index
                    .entries
                    .values()
                    .all(|slots| !slots.values().any(|value| value == &reference)),
            "Could not allocate a new credential reference. Retry Apply."
        );
        let entry = self.entry(&reference)?;
        index.cleanup.insert(reference.clone(), Some(owner.into()));
        ensure!(
            self.write(&index)? == Publication::Durable,
            "Could not safely prepare the credential update"
        );
        save(&entry, secret)?;
        let staged = index.clone();
        if let Some(previous) = index
            .entries
            .entry(owner.to_owned())
            .or_default()
            .insert(key.to_owned(), reference.clone())
        {
            index.cleanup.insert(previous, Some(owner.into()));
        }
        index.cleanup.remove(&reference);
        // The old reference still works until the new one is published. Cleanup can be retried.
        match publish(self, &index)? {
            Publication::Durable => {}
            Publication::Visible => {
                // Keep both vault entries. Rollback itself may be visible without being durable.
                if matches!(self.write(&staged), Ok(Publication::Durable)) {
                    anyhow::bail!(
                        "Credential save could not be confirmed. The prior value was kept."
                    );
                }
                return Ok(SaveOutcome::Uncertain);
            }
        }
        self.finish_cleanup(&mut index, owner, Self::remove);
        let _ = self.write(&index);
        Ok(SaveOutcome::Saved)
    }

    fn save_secret(entry: &keyring::Entry, secret: &str) -> Result<()> {
        entry
            .set_password(secret)
            .map_err(|error| storage_error(&error))
    }

    pub(super) fn delete(&self, owner: &str, key: Option<&str>) -> Result<DeleteOutcome> {
        self.delete_with(owner, key, Self::write, Self::remove)
    }

    fn delete_with(
        &self,
        owner: &str,
        key: Option<&str>,
        publish: impl FnOnce(&Self, &Index) -> Result<Publication>,
        remove: impl FnMut(&Self, &str) -> Result<()>,
    ) -> Result<DeleteOutcome> {
        let _lock = self.lock()?;
        let mut index = self.index()?;
        if let Some(keys) = index.entries.get_mut(owner) {
            keys.retain(|name, reference| {
                let keep = key.is_some_and(|key| key != name);
                if !keep {
                    index.cleanup.insert(reference.clone(), Some(owner.into()));
                }
                keep
            });
            if keys.is_empty() {
                index.entries.remove(owner);
            }
        }
        // Detach references durably before deleting their system entries.
        if publish(self, &index)? == Publication::Visible {
            return Ok(DeleteOutcome::CleanupPending(
                "Credential detached. Could not confirm the change on disk. Retry removal.".into(),
            ));
        }
        if let Some(error) = self.finish_cleanup(&mut index, owner, remove) {
            let _ = self.write(&index);
            return Ok(DeleteOutcome::CleanupPending(format!(
                "Credential detached. System cleanup is pending. {error}"
            )));
        }
        if !matches!(self.write(&index), Ok(Publication::Durable)) {
            return Ok(DeleteOutcome::CleanupPending(
                "Credential detached. Could not confirm cleanup on disk. Retry removal.".into(),
            ));
        }
        Ok(DeleteOutcome::Removed)
    }

    fn remove(&self, reference: &str) -> Result<()> {
        match self.entry(reference)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(storage_error(&error)),
        }
    }

    fn entry(&self, reference: &str) -> Result<keyring::Entry> {
        ensure!(valid_reference(reference), "Invalid credential reference");
        let profile = self.namespace()?;
        let user = format!("{profile}-{reference}");
        #[cfg(windows)]
        {
            // Keep the entry name compatible, but keep newly saved credentials on this device.
            if let Err(error) = keyring::Entry::store_status() {
                return Err(storage_error(error));
            }
            let modifiers = std::collections::HashMap::from([("persistence", "Local")]);
            keyring_core::Entry::new_with_modifiers(SERVICE, &user, &modifiers)
                .map(|inner| keyring::Entry { inner })
                .map_err(|error| storage_error(&error))
        }
        #[cfg(not(windows))]
        {
            if let Err(error) = keyring::Entry::store_status() {
                return Err(storage_error(error));
            }
            keyring::Entry::new(SERVICE, &user).map_err(|error| storage_error(&error))
        }
    }

    fn finish_cleanup(
        &self,
        index: &mut Index,
        owner: &str,
        mut remove: impl FnMut(&Self, &str) -> Result<()>,
    ) -> Option<anyhow::Error> {
        let mut failure = None;
        index.cleanup.retain(|reference, pending| {
            if pending.as_deref().is_some_and(|pending| pending != owner) {
                return true;
            }
            match remove(self, reference) {
                Ok(()) => false,
                Err(error) => {
                    if failure.is_none() {
                        failure = Some(error);
                    }
                    true
                }
            }
        });
        failure
    }

    fn index(&self) -> Result<Index> {
        let index: Index = read_json(&self.path(), INDEX_LIMIT)?.unwrap_or_default();
        let mut seen = BTreeSet::new();
        ensure!(
            index.entries.values().flat_map(|slots| slots.values()).all(
                |reference| valid_reference(reference)
                    && seen.insert(reference)
                    && !index.cleanup.contains_key(reference)
            ),
            "Invalid credential index"
        );
        ensure!(
            index
                .cleanup
                .keys()
                .all(|reference| valid_reference(reference)),
            "Invalid credential cleanup record"
        );
        Ok(index)
    }

    fn path(&self) -> PathBuf {
        self.0.join("credentials.json")
    }
    fn write(&self, index: &Index) -> Result<Publication> {
        let bytes = serde_json::to_vec_pretty(index)?;
        ensure!(
            bytes.len() as u64 <= INDEX_LIMIT,
            "Credential index exceeds its size budget"
        );
        let mut staged = tempfile::NamedTempFile::new_in(&self.0)
            .context("Could not stage credential references")?;
        staged
            .write_all(&bytes)
            .context("Could not write credential references")?;
        staged
            .as_file()
            .sync_all()
            .context("Could not sync credential references")?;
        let destination = self.path();
        replace_index(staged, &destination, &bytes)
            .context("Could not publish credential references")
    }

    fn lock(&self) -> Result<File> {
        #[cfg(unix)]
        if !self.0.exists() {
            let mut missing = Vec::new();
            let mut existing = self.0.as_path();
            while !existing.exists() {
                existing = existing
                    .parent()
                    .context("Invalid credential profile directory")?;
                missing.push(existing);
            }
            fs::create_dir_all(&self.0)?;
            for directory in missing.into_iter().rev() {
                File::open(directory)?.sync_all()?;
            }
        }
        fs::create_dir_all(&self.0)?;
        let lock = lock_file(&self.0.join("credentials.lock"))?;
        lock.lock()
            .context("Could not lock credential references")?;
        Ok(lock)
    }
}

#[cfg(unix)]
fn replace_index(
    staged: tempfile::NamedTempFile,
    path: &Path,
    _bytes: &[u8],
) -> io::Result<Publication> {
    staged.persist(path)?;
    let directory = path.parent().ok_or(io::ErrorKind::InvalidInput)?;
    if File::open(directory)
        .and_then(|file| file.sync_all())
        .is_err()
    {
        return Ok(Publication::Visible);
    }
    Ok(Publication::Durable)
}

#[cfg(windows)]
fn replace_index(
    staged: tempfile::NamedTempFile,
    path: &Path,
    bytes: &[u8],
) -> io::Result<Publication> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_NORMAL, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
        SetFileAttributesW,
    };

    let source = staged.path().canonicalize()?;
    let parent = source.parent().ok_or(io::ErrorKind::InvalidInput)?;
    let destination = parent.join(path.file_name().ok_or(io::ErrorKind::InvalidInput)?);
    let wide = |path: &Path| {
        path.as_os_str()
            .encode_wide()
            .chain([0])
            .collect::<Vec<_>>()
    };
    let from = wide(&source);
    let to = wide(&destination);
    // Both strings are terminated paths inside the same profile directory.
    let moved = unsafe {
        if SetFileAttributesW(from.as_ptr(), FILE_ATTRIBUTE_NORMAL) == 0 {
            return Err(io::Error::last_os_error());
        }
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved != 0 {
        return Ok(Publication::Durable);
    }
    let error = io::Error::last_os_error();
    if !source.exists()
        && super::package::read_limited(&destination, INDEX_LIMIT)
            .is_ok_and(|current| current == bytes)
    {
        return Ok(Publication::Visible);
    }
    Err(error)
}

#[cfg(not(any(unix, windows)))]
fn replace_index(
    _staged: tempfile::NamedTempFile,
    _path: &Path,
    _bytes: &[u8],
) -> io::Result<Publication> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

fn lock_file(path: &Path) -> Result<File> {
    Ok(OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?)
}

fn valid_reference(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn storage_error(error: &keyring::Error) -> anyhow::Error {
    const UNAVAILABLE: &str = "The system credential store is unavailable";
    const LOCKED: &str = "The system credential store is locked";
    const DENIED: &str = "The system credential store denied access";
    const CANCELLED: &str = "The system credential request was cancelled";
    const FAILED: &str = "The system credential store could not complete the request";

    // Keep native metadata out of both the message and the error chain.
    let message = match error {
        keyring::Error::NoEntry => "Credential is missing",
        keyring::Error::NoDefaultStore => UNAVAILABLE,
        keyring::Error::NoStorageAccess(cause) | keyring::Error::PlatformFailure(cause) => {
            let source = cause.as_ref();
            let native = native_storage_error(source);
            if let Some(outcome) = native {
                match outcome {
                    VaultOutcome::Unavailable => UNAVAILABLE,
                    VaultOutcome::Locked => LOCKED,
                    VaultOutcome::Denied => DENIED,
                    VaultOutcome::Cancelled => CANCELLED,
                }
            } else if source
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::PermissionDenied)
            {
                DENIED
            } else if matches!(error, keyring::Error::NoStorageAccess(_)) {
                "The system credential store is locked or access was denied"
            } else {
                FAILED
            }
        }
        _ => FAILED,
    };
    anyhow::anyhow!(message)
}

#[allow(dead_code)]
enum VaultOutcome {
    Unavailable,
    Locked,
    Denied,
    Cancelled,
}

#[cfg(test)]
mod outcomes {
    use super::*;

    #[test]
    fn native_failures_publish_fixed_messages_without_raw_diagnostics() {
        let denied = keyring::Error::NoStorageAccess(Box::new(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "temporary secret in a native diagnostic",
        )));
        let failed = keyring::Error::PlatformFailure(Box::new(std::io::Error::other(
            "temporary secret in a native diagnostic",
        )));
        for (error, expected) in [
            (denied, "The system credential store denied access"),
            (
                failed,
                "The system credential store could not complete the request",
            ),
            (keyring::Error::NoEntry, "Credential is missing"),
            (
                keyring::Error::NoDefaultStore,
                "The system credential store is unavailable",
            ),
        ] {
            let reported = storage_error(&error);
            assert_eq!(reported.to_string(), expected);
            assert!(!format!("{reported:#?}").contains("temporary secret"));
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_distinguishes_a_cancelled_request_and_absent_logon_session() {
        use windows_sys::Win32::Foundation::{ERROR_CANCELLED, ERROR_NO_SUCH_LOGON_SESSION};

        for (code, expected) in [
            (
                ERROR_CANCELLED,
                "The system credential request was cancelled",
            ),
            (
                ERROR_NO_SUCH_LOGON_SESSION,
                "The system credential store is unavailable",
            ),
        ] {
            let error = keyring::Error::PlatformFailure(Box::new(
                std::io::Error::from_raw_os_error(code as i32),
            ));
            assert_eq!(storage_error(&error).to_string(), expected);
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn apple_distinguishes_locked_cancelled_and_denied() {
        for (code, expected) in [
            (-128, "The system credential request was cancelled"),
            (-25308, "The system credential store is locked"),
            (-25293, "The system credential store denied access"),
        ] {
            let error = keyring::Error::PlatformFailure(Box::new(
                security_framework::base::Error::from_code(code),
            ));
            assert_eq!(storage_error(&error).to_string(), expected);
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_distinguishes_locked_cancelled_and_denied() {
        use secret_service::Error;
        for (cause, expected) in [
            (Error::Prompt, "The system credential request was cancelled"),
            (Error::Locked, "The system credential store is locked"),
            (
                Error::ZbusFdo(zbus::fdo::Error::AccessDenied("temporary metadata".into())),
                "The system credential store denied access",
            ),
        ] {
            let error = keyring::Error::PlatformFailure(Box::new(cause));
            assert_eq!(storage_error(&error).to_string(), expected);
        }
    }
}

#[cfg(windows)]
fn native_storage_error(error: &(dyn std::error::Error + 'static)) -> Option<VaultOutcome> {
    use windows_sys::Win32::Foundation::{
        ERROR_ACCESS_DENIED, ERROR_CANCELLED, ERROR_NO_SUCH_LOGON_SESSION, ERROR_PRIVILEGE_NOT_HELD,
    };

    // The pinned Windows backend keeps its numeric carrier private and formats only the code.
    let code = if let Some(io) = error.downcast_ref::<std::io::Error>() {
        u32::try_from(io.raw_os_error()?).ok()?
    } else {
        let message = error.to_string();
        if message == "Windows ERROR_NO_SUCH_LOGON_SESSION" {
            return Some(VaultOutcome::Unavailable);
        }
        message.strip_prefix("Windows error code ")?.parse().ok()?
    };
    match code {
        ERROR_NO_SUCH_LOGON_SESSION => Some(VaultOutcome::Unavailable),
        ERROR_ACCESS_DENIED | ERROR_PRIVILEGE_NOT_HELD => Some(VaultOutcome::Denied),
        ERROR_CANCELLED => Some(VaultOutcome::Cancelled),
        _ => None,
    }
}

#[cfg(target_os = "macos")]
fn native_storage_error(error: &(dyn std::error::Error + 'static)) -> Option<VaultOutcome> {
    match error
        .downcast_ref::<security_framework::base::Error>()?
        .code()
    {
        -128 => Some(VaultOutcome::Cancelled), // errSecUserCanceled
        -25308 | -25315 => Some(VaultOutcome::Locked), // Interaction requires an unlock
        -61 | -25244 | -25292 | -25293 | -34018 => Some(VaultOutcome::Denied),
        -25291 | -25294 | -25295 => Some(VaultOutcome::Unavailable),
        _ => None,
    }
}

#[cfg(target_os = "linux")]
fn native_storage_error(error: &(dyn std::error::Error + 'static)) -> Option<VaultOutcome> {
    use secret_service::Error;

    match error.downcast_ref::<Error>()? {
        Error::Unavailable => Some(VaultOutcome::Unavailable),
        Error::Locked => Some(VaultOutcome::Locked),
        Error::Prompt => Some(VaultOutcome::Cancelled),
        Error::ZbusFdo(zbus::fdo::Error::AccessDenied(_) | zbus::fdo::Error::AuthFailed(_)) => {
            Some(VaultOutcome::Denied)
        }
        Error::Zbus(zbus::Error::MethodError(name, _, _)) => match name.as_str() {
            "org.freedesktop.Secret.Error.IsLocked" => Some(VaultOutcome::Locked),
            "org.freedesktop.DBus.Error.AccessDenied" | "org.freedesktop.DBus.Error.AuthFailed" => {
                Some(VaultOutcome::Denied)
            }
            "org.freedesktop.DBus.Error.ServiceUnknown"
            | "org.freedesktop.DBus.Error.NameHasNoOwner" => Some(VaultOutcome::Unavailable),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn native_storage_error(_: &(dyn std::error::Error + 'static)) -> Option<VaultOutcome> {
    None
}

#[cfg(all(test, any(windows, target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;
    use crate::plugins::package::Identity;
    use std::sync::Mutex;

    static VAULT_TEST: Mutex<()> = Mutex::new(());

    /// A test failure still removes the test identities' entries from the system store.
    struct Cleanup<'a>(&'a Credentials, Vec<String>);

    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            for owner in &self.1 {
                let _ = self.0.delete(owner, None);
            }
        }
    }

    /// Import `source`'s references and verify that renaming the portable folder preserves vault access.
    #[test]
    #[ignore = "requires the operating system credential store and uses temporary entries"]
    fn portable_profile_keeps_the_existing_system_credential() -> Result<()> {
        let _vault = VAULT_TEST.lock().unwrap_or_else(|lock| lock.into_inner());
        let fixture = tempfile::tempdir()?;
        let source = fixture.path().join("legacy");
        let destination = fixture.path().join("portable");
        fs::create_dir(&destination)?;
        let original = Credentials::new(source.clone());
        let owner = Identity::new("Migration Fixture".into(), "fixture".into())?.namespace();
        let _cleanup = Cleanup(&original, vec![owner.clone()]);
        let value = "fixture-only-vault-value";
        original.set(&owner, "login", value)?.confirmed()?;
        Credentials::initialize_namespace(&destination, Some(&source))?;
        crate::app_data::copy_missing_file(
            &original.path(),
            &destination.join("credentials.json"),
        )?;

        let moved = fixture.path().join("moved");
        fs::rename(&destination, &moved)?;
        let imported = Credentials::new(moved);
        assert!(
            imported
                .get(&owner, "login")?
                .is_some_and(|secret| *secret == value)
        );
        assert!(!fs::read_to_string(imported.path())?.contains(value));
        Ok(())
    }

    #[test]
    fn pending_cleanup_keeps_the_plugin_visible_and_reads_old_lists() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let credentials = Credentials::new(directory.path().to_path_buf());
        let reference = "a".repeat(64);
        fs::write(
            credentials.path(),
            serde_json::to_vec(&serde_json::json!({"cleanup":[reference.clone()]}))?,
        )?;
        assert_eq!(credentials.index()?.cleanup[&reference], None);
        let mut index = credentials.index()?;
        index
            .cleanup
            .insert(reference, Some("retained-plugin".into()));
        credentials.write(&index)?;
        assert!(credentials.names()?.contains("retained-plugin"));
        assert!(credentials.get("retained-plugin", "login")?.is_none());
        Ok(())
    }

    #[test]
    #[ignore = "requires the operating system credential store and uses temporary entries"]
    fn native_vault_is_profile_scoped_and_keeps_only_references_on_disk() -> Result<()> {
        let _vault = VAULT_TEST.lock().unwrap_or_else(|lock| lock.into_inner());
        let first = tempfile::tempdir()?;
        let second = tempfile::tempdir()?;
        let first = Credentials::new(first.path().to_path_buf());
        let second = Credentials::new(second.path().to_path_buf());
        let owner = Identity::new("Fixture One".into(), "fixture".into())?.namespace();
        let other = Identity::new("Fixture Two".into(), "fixture".into())?.namespace();
        let _cleanup = Cleanup(&first, vec![owner.clone(), other.clone()]);
        let _cleanup_second = Cleanup(&second, vec![owner.clone()]);
        let fake_secret = "fixture-private-test-value";
        first.set(&owner, "login", fake_secret)?;
        first.set(&other, "login", "another-private-test-value")?;
        second.set(&owner, "login", "dev-private-test-value")?;
        assert!(
            first
                .get(&owner, "login")?
                .is_some_and(|secret| *secret == fake_secret)
        );
        assert!(
            first
                .get(&other, "login")?
                .is_some_and(|secret| *secret == "another-private-test-value")
        );
        assert!(
            second
                .get(&owner, "login")?
                .is_some_and(|secret| *secret == "dev-private-test-value")
        );
        assert!(second.get(&other, "login")?.is_none());
        for path in [first.path(), second.path()] {
            assert!(!fs::read_to_string(path)?.contains("private-test-value"));
        }
        let reference = first.index()?.entries[&owner]["login"].clone();
        #[cfg(windows)]
        {
            let attributes = first
                .entry(&reference)?
                .inner
                .get_attributes()
                .map_err(|error| storage_error(&error))?;
            assert!(attributes["persistence"].eq_ignore_ascii_case("local"));
        }
        first.remove(&reference)?;
        assert_eq!(
            first.get(&owner, "login").unwrap_err().to_string(),
            "Credential is missing"
        );
        assert!(first.get(&other, "login")?.is_some());
        Ok(())
    }

    /// Reopen test credentials across independent Secret Service login sessions.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "run each phase with a private, password-protected GNOME Keyring session"]
    fn linux_vault_persists_across_login_sessions() -> Result<()> {
        let directory = std::env::var_os("NANOBUG_TEST_VAULT_DIRECTORY")
            .map(PathBuf::from)
            .context("set NANOBUG_TEST_VAULT_DIRECTORY to a disposable fixture directory")?;
        let directory = directory.canonicalize()?;
        let phase = std::env::var("NANOBUG_TEST_VAULT_PHASE")?;
        let marker = directory.join("nanobug-vault-fixture");
        let regular = Credentials::new(directory.join("regular"));
        let development = Credentials::new(directory.join("development"));
        let first = Identity::new("Vault fixture A".into(), "vault-fixture".into())?.namespace();
        let second = Identity::new("Vault fixture B".into(), "vault-fixture".into())?.namespace();
        let value = "nanobug-vault-round-trip-only";

        if phase == "write" {
            ensure!(
                fs::read_dir(&directory)?.next().is_none(),
                "fixture directory must be empty"
            );
            fs::write(marker, b"Nanobug credential test only\n")?;
            regular.set(&first, "login", value)?.confirmed()?;
            regular
                .set(&second, "login", "different fixture identity")?
                .confirmed()?;
            development
                .set(&first, "login", "different fixture profile")?
                .confirmed()?;
        } else if phase == "read" {
            ensure!(
                fs::read(&marker)? == b"Nanobug credential test only\n",
                "unexpected fixture directory"
            );
            let _cleanup_regular = Cleanup(&regular, vec![first.clone(), second.clone()]);
            let _cleanup_development = Cleanup(&development, vec![first.clone()]);
            assert!(
                regular
                    .get(&first, "login")?
                    .is_some_and(|saved| *saved == value)
            );
            assert!(
                regular
                    .get(&second, "login")?
                    .is_some_and(|saved| *saved == "different fixture identity")
            );
            assert!(
                development
                    .get(&first, "login")?
                    .is_some_and(|saved| *saved == "different fixture profile")
            );
            assert!(development.get(&second, "login")?.is_none());
            for (credentials, owner) in [
                (&regular, &first),
                (&regular, &second),
                (&development, &first),
            ] {
                assert_eq!(credentials.delete(owner, None)?, DeleteOutcome::Removed);
                assert!(credentials.get(owner, "login")?.is_none());
            }
        } else {
            anyhow::bail!("vault phase must be write or read");
        }
        for credentials in [&regular, &development] {
            let file = fs::read_to_string(credentials.path())?;
            for secret in [
                value,
                "different fixture identity",
                "different fixture profile",
            ] {
                assert!(!file.contains(secret));
            }
        }
        Ok(())
    }

    /// Exercise actual Keychain statuses against a disposable file, including an optional UI cancel.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires macOS Keychain Services and creates only a temporary keychain"]
    fn macos_temporary_keychain_reports_native_failures() -> Result<()> {
        use security_framework::os::macos::{
            keychain::{CreateOptions, SecKeychain},
            passwords::find_generic_password,
        };

        let _vault = VAULT_TEST.lock().unwrap_or_else(|lock| lock.into_inner());
        let folder = tempfile::tempdir()?;
        let path = folder.path().join("Nanobug-acceptance.keychain");
        let password = "nanobug-disposable-keychain";
        let mut keychain = CreateOptions::new().password(password).create(&path)?;

        // Delete the test keychain even when a native assertion fails, including its search entry.
        struct CleanupKeychain(PathBuf);
        impl Drop for CleanupKeychain {
            fn drop(&mut self) {
                let _ = std::process::Command::new("/usr/bin/security")
                    .arg("delete-keychain")
                    .arg(&self.0)
                    .output();
            }
        }
        let cleanup = CleanupKeychain(path.clone());
        let read = |keychain: &SecKeychain| {
            find_generic_password(
                Some(std::slice::from_ref(keychain)),
                SERVICE,
                "test-account",
            )
        };
        let message = |error: security_framework::base::Error| {
            if error.code() == -25300 {
                storage_error(&keyring::Error::NoEntry).to_string()
            } else {
                storage_error(&keyring::Error::PlatformFailure(Box::new(error))).to_string()
            }
        };
        assert_eq!(
            message(read(&keychain).unwrap_err()),
            "Credential is missing"
        );
        keychain.set_generic_password(SERVICE, "test-account", b"temporary fixture value")?;
        let lock = std::process::Command::new("/usr/bin/security")
            .arg("lock-keychain")
            .arg(&path)
            .output()?;
        ensure!(
            lock.status.success(),
            "Could not lock the temporary keychain"
        );
        {
            let _quiet = SecKeychain::disable_user_interaction()?;
            assert_eq!(
                message(read(&keychain).unwrap_err()),
                "The system credential store is locked"
            );
            assert_eq!(
                message(
                    keychain
                        .unlock(Some("incorrect fixture password"))
                        .unwrap_err()
                ),
                "The system credential store denied access"
            );
        }
        if std::env::var_os("NANOBUG_MAC_KEYCHAIN_CANCEL").is_some() {
            eprintln!("Click Cancel in the prompt for Nanobug-acceptance.keychain.");
            assert_eq!(
                message(read(&keychain).unwrap_err()),
                "The system credential request was cancelled"
            );
        }
        keychain.unlock(Some(password))?;
        assert!(read(&keychain)?.0.as_ref() == b"temporary fixture value");
        drop(cleanup);
        Ok(())
    }

    #[test]
    #[ignore = "requires the operating system credential store and uses temporary entries"]
    fn failed_reference_publication_keeps_the_prior_credential() -> Result<()> {
        let _vault = VAULT_TEST.lock().unwrap_or_else(|lock| lock.into_inner());
        let directory = tempfile::tempdir()?;
        let credentials = Credentials::new(directory.path().to_path_buf());
        let _cleanup = Cleanup(&credentials, vec!["fixture".into()]);
        // A reference created by older versions uses the same Windows credential name.
        let legacy = "a".repeat(64);
        let profile = query::project_key(&query::resolve_prefix(directory.path())?);
        let mut index = Index::default();
        index.entries.insert(
            "fixture".into(),
            BTreeMap::from([("login".into(), legacy.clone())]),
        );
        credentials.write(&index)?;
        keyring::Entry::new(SERVICE, &format!("{profile}-{legacy}"))
            .and_then(|entry| entry.set_password("old-test-credential"))
            .map_err(|error| storage_error(&error))?;
        let result = (|| -> Result<()> {
            let failure = credentials.replace(
                "fixture",
                "login",
                "new-test-credential",
                Credentials::save_secret,
                |_, _| anyhow::bail!("simulated reference publication failure"),
            );
            assert!(failure.is_err());
            assert_eq!(
                credentials
                    .get("fixture", "login")?
                    .as_deref()
                    .map(String::as_str),
                Some("old-test-credential")
            );
            let index = credentials.index()?;
            assert_eq!(index.cleanup.len(), 1);
            let contents = fs::read_to_string(credentials.path())?;
            assert!(!contents.contains("test-credential"));
            let failure_after_rename = credentials.replace(
                "fixture",
                "login",
                "second-new-test-credential",
                Credentials::save_secret,
                |store, index| {
                    store.write(index)?;
                    // Exercise the outcome after a visible replacement whose durability is unknown.
                    Ok(Publication::Visible)
                },
            );
            assert!(
                failure_after_rename
                    .unwrap_err()
                    .to_string()
                    .contains("prior value was kept")
            );
            let index = credentials.index()?;
            assert_eq!(index.entries["fixture"]["login"], legacy);
            assert_eq!(index.cleanup.len(), 2);
            assert!(
                credentials
                    .get("fixture", "login")?
                    .is_some_and(|secret| *secret == "old-test-credential")
            );
            Ok(())
        })();
        credentials.delete("fixture", None)?;
        assert!(credentials.index()?.cleanup.is_empty());
        result
    }

    #[test]
    #[ignore = "requires the operating system credential store and uses temporary entries"]
    fn detached_credential_cannot_be_read_when_vault_cleanup_is_pending() -> Result<()> {
        let _vault = VAULT_TEST.lock().unwrap_or_else(|lock| lock.into_inner());
        let directory = tempfile::tempdir()?;
        let credentials = Credentials::new(directory.path().to_path_buf());
        let _cleanup = Cleanup(&credentials, vec!["fixture".into()]);
        credentials
            .set("fixture", "login", "temporary-delete-fixture")?
            .confirmed()?;
        let reference = credentials.index()?.entries["fixture"]["login"].clone();
        let pending = credentials.delete_with(
            "fixture",
            Some("login"),
            |store, index| {
                store.write(index)?;
                Ok(Publication::Visible)
            },
            |_, _| panic!("an unconfirmed reference update must not retire the vault entry"),
        )?;
        assert!(matches!(pending, DeleteOutcome::CleanupPending(_)));
        let reopened = Credentials::new(directory.path().to_path_buf());
        assert!(reopened.get("fixture", "login")?.is_none());
        assert!(reopened.names()?.contains("fixture"));
        let rejected =
            reopened.delete_with("fixture", Some("login"), Credentials::write, |_, _| {
                Err(storage_error(&keyring::Error::NoStorageAccess(Box::new(
                    std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "temporary-delete-fixture",
                    ),
                ))))
            })?;
        let DeleteOutcome::CleanupPending(message) = rejected else {
            panic!("cleanup failure must keep its retry outcome");
        };
        assert!(message.contains("denied access"));
        assert!(!message.contains("temporary-delete-fixture"));
        assert!(reopened.get("fixture", "login")?.is_none());
        assert!(reopened.entry(&reference)?.get_password().is_ok());
        assert_eq!(
            reopened.delete("fixture", Some("login"))?,
            DeleteOutcome::Removed
        );
        assert!(reopened.names()?.is_empty());
        assert!(matches!(
            reopened.entry(&reference)?.get_password(),
            Err(keyring::Error::NoEntry)
        ));
        Ok(())
    }

    #[test]
    #[cfg(windows)]
    #[ignore = "uses a temporary Windows vault entry and injected native write failure"]
    fn denied_vault_write_keeps_the_prior_credential_and_redacts_the_diagnostic() -> Result<()> {
        use std::os::windows::fs::OpenOptionsExt;

        let _vault = VAULT_TEST.lock().unwrap_or_else(|lock| lock.into_inner());
        let directory = tempfile::tempdir()?;
        let credentials = Credentials::new(directory.path().to_path_buf());
        let _cleanup = Cleanup(&credentials, vec!["fixture".into()]);
        credentials.set("fixture", "login", "old-test-credential")?;
        let old_reference = credentials.index()?.entries["fixture"]["login"].clone();
        let locked_index = OpenOptions::new()
            .read(true)
            .share_mode(0x01 | 0x02)
            .open(credentials.path())?;
        let prepare_failed = credentials.replace(
            "fixture",
            "login",
            "new-test-credential",
            |_, _| panic!("failed reference staging must not write to the vault"),
            |_, _| panic!("failed reference staging must not publish"),
        );
        assert!(prepare_failed.is_err());
        drop(locked_index);
        assert!(credentials.index()?.cleanup.is_empty());
        let failure = credentials.replace(
            "fixture",
            "login",
            "new-test-credential",
            |_, secret| {
                Err(storage_error(&keyring::Error::NoStorageAccess(Box::new(
                    std::io::Error::new(std::io::ErrorKind::PermissionDenied, secret.to_owned()),
                ))))
            },
            |_, _| panic!("failed native write must not publish"),
        );
        assert_eq!(
            failure.unwrap_err().to_string(),
            "The system credential store denied access"
        );
        let failed = credentials.index()?;
        assert_eq!(failed.entries["fixture"]["login"], old_reference);
        assert_eq!(failed.cleanup.len(), 1);
        assert!(!fs::read_to_string(credentials.path())?.contains("test-credential"));
        let reopened = Credentials::new(directory.path().to_path_buf());
        assert!(
            reopened
                .get("fixture", "login")?
                .is_some_and(|secret| *secret == "old-test-credential")
        );
        reopened.set("fixture", "login", "new-test-credential")?;
        assert!(
            reopened
                .get("fixture", "login")?
                .is_some_and(|secret| *secret == "new-test-credential")
        );
        let published = reopened.index()?;
        assert!(published.cleanup.is_empty());
        assert_ne!(published.entries["fixture"]["login"], old_reference);
        let old_entry = reopened.entry(&old_reference)?;
        let old_missing = matches!(old_entry.get_password(), Err(keyring::Error::NoEntry));
        // Clear the temporary reference even if the backend fails to retire it.
        reopened.remove(&old_reference)?;
        assert!(old_missing);
        Ok(())
    }

    #[test]
    #[ignore = "requires the system vault and uses a temporary plugin/profile"]
    fn uninstall_keeps_credentials_until_explicit_cleanup() -> Result<()> {
        use crate::plugins::session::{Action, Config, ConfigScope, Options, Session, View};
        use std::{
            io::Write,
            sync::Arc,
            time::{Duration, Instant},
        };

        let _vault = VAULT_TEST.lock().unwrap_or_else(|lock| lock.into_inner());
        let fixture = tempfile::tempdir()?;
        let profile = fixture.path().join("profile");
        let archive = fixture.path().join("fixture.zip");
        let mut zip = zip::ZipWriter::new(File::create(&archive)?);
        zip.start_file(
            "nanobug-plugin.toml",
            zip::write::SimpleFileOptions::default(),
        )?;
        zip.write_all(
            br#"
name = 'Credential Fixture'
id = 'credential-fixture'
version = '1.0.0'
permissions = ['credentials']
[engines]
nanobug = '^0.1.0'
[settings.login]
title = 'Login'
secret = true
[settings.label]
title = 'Label'
default = 'original'
"#,
        )?;
        zip.finish()?;
        let options = Options::take(
            &mut vec!["--plugin-profile".into(), profile.as_os_str().into()],
            fixture.path(),
        )?;
        let host = Session::start(options).0;
        let wait = |predicate: &dyn Fn(&View) -> bool| -> Result<Arc<View>> {
            let deadline = Instant::now() + Duration::from_secs(8);
            loop {
                let view = host.view();
                if predicate(&view) {
                    return Ok(view);
                }
                ensure!(Instant::now() < deadline, "Credential lifecycle timed out");
                std::thread::sleep(Duration::from_millis(15));
            }
        };
        let install = || -> Result<Identity> {
            host.send(Action::InspectZip(archive.clone()))?;
            let review = wait(&|view| view.review.is_some())?;
            let review = review.review.as_ref().context("missing review")?;
            let identity = review.manifest.identity();
            host.send(Action::Approve(review.token))?;
            wait(&|view| view.installed.len() == 1)?;
            Ok(identity)
        };
        let identity = install()?;
        let owner = identity.namespace();
        let credentials = Credentials::new(profile);
        let _cleanup = Cleanup(&credentials, vec![owner.clone()]);
        let (reply, answer) = async_channel::bounded(1);
        let secret = "temporary-credential-retention-fixture";
        host.send(Action::SetCredential {
            namespace: owner.clone(),
            slot: "login".into(),
            value: Zeroizing::new(secret.into()),
            reply,
        })?;
        wait(&|view| view.credential_owners.contains(&owner))?;
        answer.try_recv()?.map_err(anyhow::Error::msg)?;
        let check_saved = || -> Result<()> {
            assert!(
                credentials
                    .get(&owner, "login")?
                    .is_some_and(|value| *value == secret)
            );
            assert!(!fs::read_to_string(credentials.path())?.contains(secret));
            Ok(())
        };
        let mut config = Config::default();
        config
            .plugin_mut(identity.clone())
            .settings
            .insert("label".into(), "changed".into());
        host.send(Action::StageConfig(
            ConfigScope::AllProjects,
            config.clone(),
        ))?;
        host.send(Action::ApplyConfig(ConfigScope::AllProjects))?;
        wait(&|view| view.configurations[&ConfigScope::AllProjects].applied == config)?;
        let uninstall = || -> Result<()> {
            host.send(Action::Uninstall(owner.clone()))?;
            wait(&|view| view.installed.is_empty() && view.retained.contains_key(&owner))?;
            Ok(())
        };
        uninstall()?;
        check_saved()?;
        host.send(Action::CleanLocalData {
            namespace: owner.clone(),
            local: true,
            credentials: false,
        })?;
        wait(&|view| {
            view.configurations[&ConfigScope::AllProjects]
                .applied
                .plugin(&owner)
                .is_none_or(|plugin| plugin.settings.is_empty())
        })?;
        check_saved()?;
        assert_eq!(install()?, identity);
        check_saved()?;
        uninstall()?;
        host.send(Action::CleanLocalData {
            namespace: owner.clone(),
            local: false,
            credentials: true,
        })?;
        wait(&|view| !view.credential_owners.contains(&owner))?;
        assert!(credentials.get(&owner, "login")?.is_none());
        assert_eq!(install()?, identity);
        assert!(credentials.get(&owner, "login")?.is_none());
        Ok(())
    }

    #[test]
    #[ignore = "requires the system vault and the bundled plugin runtime beside the test binary"]
    fn rejected_new_authentication_keeps_the_new_credential_and_never_logs_it() -> Result<()> {
        use crate::plugins::session::{Action, Key, Options, Project, Session, State, View};
        use nanobug_plugin_sdk::Instance;
        use std::{
            io::Write,
            sync::Arc,
            time::{Duration, Instant},
        };

        let _vault = VAULT_TEST.lock().unwrap_or_else(|lock| lock.into_inner());
        let fixture = tempfile::tempdir()?;
        let profile = fixture.path().join("profile");
        let project = fixture.path().join("project");
        fs::create_dir(&project)?;
        let archive = fixture.path().join("fixture.zip");
        let marker = "reject-test-only";
        let needle = i64::from_le_bytes(*b"reject-t");
        let read =
            r#"{"services":[{"token":7,"operation":{"service":"read_credential","key":"login"}}]}"#;
        let reject = format!(r#"{{"credential_rejected":true,"message":"{marker}"}}"#);
        let encode = |value: &str| {
            value
                .bytes()
                .map(|byte| format!("\\{byte:02x}"))
                .collect::<String>()
        };
        let change = i64::from_le_bytes(*b"credenti");
        let service = i64::from_le_bytes(*b"service_");
        let event = r#"{"event":{"event":""#.len();
        let wasm = wat::parse_str(format!(
            r#"(module
                (memory (export "memory") 20)
                (data (i32.const 16) "{}")
                (data (i32.const 512) "{}")
                (data (i32.const 1024) "{{}}")
                (func (export "nb_alloc") (param i32) (result i32) i32.const 4096)
                (func (export "nb_run") (param $input i32) (param $length i32) (result i64)
                    (local $offset i32)
                    (local $event i64)
                    (local.set $event (i64.load offset={event} (local.get $input)))
                    (if (i64.eq (local.get $event) (i64.const {change}))
                        (then (return (i64.const {}))))
                    (if (i64.eq (local.get $event) (i64.const {service}))
                        (then
                            (block $done (loop $scan
                                (br_if $done (i32.gt_u (i32.add (local.get $offset) (i32.const 8)) (local.get $length)))
                                (if (i64.eq (i64.load (i32.add (local.get $input) (local.get $offset))) (i64.const {needle}))
                                    (then (return (i64.const {}))))
                                (local.set $offset (i32.add (local.get $offset) (i32.const 1)))
                                (br $scan)))))
                    (i64.const {}))
                (func (export "nb_free") (param i32 i32)))"#,
            encode(read),
            encode(&reject),
            (16_u64 << 32) | read.len() as u64,
            (512_u64 << 32) | reject.len() as u64,
            (1024_u64 << 32) | 2
        ))?;
        let manifest = br#"
name = 'Authentication Fixture'
id = 'authentication-fixture'
version = '1.0.0'
permissions = ['credentials']
[engines]
nanobug = '^0.1.0'
[[modules]]
id = 'main'
wasm = 'main.wasm'
credential_events = true
[settings.login]
title = 'Login'
secret = true
"#;
        let mut zip = zip::ZipWriter::new(File::create(&archive)?);
        for (path, data) in [
            ("nanobug-plugin.toml", manifest.as_slice()),
            ("main.wasm", &wasm),
        ] {
            zip.start_file(path, zip::write::SimpleFileOptions::default())?;
            zip.write_all(data)?;
        }
        zip.finish()?;
        let options = Options::take(
            &mut vec!["--plugin-profile".into(), profile.as_os_str().into()],
            fixture.path(),
        )?;
        let (host, _) = Session::start(options);
        let wait = |predicate: &dyn Fn(&View) -> bool| -> Result<Arc<View>> {
            let deadline = Instant::now() + Duration::from_secs(8);
            loop {
                let view = host.view();
                if predicate(&view) {
                    return Ok(view);
                }
                ensure!(
                    Instant::now() < deadline,
                    "Credential authentication test timed out"
                );
                std::thread::sleep(Duration::from_millis(15));
            }
        };
        host.send(Action::InspectZip(archive))?;
        let review = wait(&|view| view.review.is_some())?;
        let review = review.review.as_ref().context("missing review")?;
        let owner = review.manifest.identity().namespace();
        let credentials = Credentials::new(profile.clone());
        let _cleanup = Cleanup(&credentials, vec![owner.clone()]);
        host.send(Action::Approve(review.token))?;
        host.projects(
            Some(1),
            vec![Project {
                id: 1,
                root: project.clone(),
                document: None,
                open_documents: Vec::new(),
            }],
        );
        let key = Key {
            namespace: owner.clone(),
            instance: Instance {
                project: 1,
                module: "main".into(),
            },
        };
        wait(&|view| view.modules.get(&key) == Some(&State::Ready))?;
        host.send(Action::SampleRuntime(owner.clone(), true))?;
        let save = |secret: &str| -> Result<()> {
            let (reply, answer) = async_channel::bounded(1);
            host.send(Action::SetCredential {
                namespace: owner.clone(),
                slot: "login".into(),
                value: Zeroizing::new(secret.into()),
                reply,
            })?;
            let deadline = Instant::now() + Duration::from_secs(8);
            loop {
                match answer.try_recv() {
                    Ok(result) => return result.map_err(anyhow::Error::msg),
                    Err(async_channel::TryRecvError::Empty) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => return Err(error.into()),
                }
            }
        };
        let old = "old-test-only";
        save(old)?;
        let done = wait(&|view| view.runtime_status[&owner].completed >= 3)?;
        assert!(done.credential_errors.is_empty());
        let prior = credentials.index()?.entries[&owner]["login"].clone();
        save(marker)?;
        let failed = wait(&|view| {
            view.credential_errors
                .get(&key)
                .is_some_and(|slots| slots.contains("login"))
        })?;
        assert_eq!(failed.modules.get(&key), Some(&State::Ready));
        assert!(
            credentials
                .get(&owner, "login")?
                .is_some_and(|secret| *secret == marker)
        );
        assert!(matches!(
            credentials.entry(&prior)?.get_password(),
            Err(keyring::Error::NoEntry)
        ));
        for private in [old, marker] {
            assert!(!failed.message.contains(private));
            assert!(
                failed.logs[&owner]
                    .iter()
                    .all(|line| !line.contains(private))
            );
            assert!(!fs::read_to_string(credentials.path())?.contains(private));
        }
        assert!(!project.join(".nb").exists());
        let exported = fixture.path().join("export.json");
        let (reply, answer) = async_channel::bounded(1);
        host.send(Action::ExportDrafts {
            path: exported.clone(),
            project: None,
            reply,
        })?;
        wait(&|_| !answer.is_empty() || answer.is_closed())?;
        answer.try_recv()?.map_err(anyhow::Error::msg)?;
        let exported = fs::read_to_string(exported)?;
        assert!(!exported.contains(old) && !exported.contains(marker));
        Ok(())
    }
}
