//! Host-local approval for exact file reads by activation rules.

use crate::plugins::safe_path::PinnedPath;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

const MAX_PATHS: usize = 4;
const MAX_PATH_BYTES: usize = 1024;

fn displayable(path: &str) -> bool {
    path.len() <= MAX_PATH_BYTES && !path.chars().any(|ch| {
        ch.is_control()
            || matches!(ch, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
    })
}

pub(super) fn validate(paths: &BTreeSet<String>, granted: bool) -> Result<()> {
    ensure!(
        granted != paths.is_empty(),
        "External rule files require the read_rule_paths permission and a rule_paths list"
    );
    ensure!(
        paths.len() <= MAX_PATHS,
        "Too many external rule files requested (maximum {MAX_PATHS})"
    );
    for path in paths {
        validate_path(path)?;
    }
    Ok(())
}

fn validate_path(path: &str) -> Result<()> {
    let native = Path::new(path);
    ensure!(
        displayable(path)
            && native.is_absolute()
            && native.file_name().is_some()
            && !native.components().any(|part| part == Component::ParentDir)
            && !path.contains(['*', '?']),
        "An external rule file must be an exact absolute path"
    );
    #[cfg(windows)]
    {
        use std::path::Prefix;
        ensure!(
            matches!(native.components().next(), Some(Component::Prefix(prefix))
            if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::UNC(..))),
            "Use an ordinary drive or UNC path for an external rule file"
        );
        ensure!(
            !path.get(2..).is_some_and(|tail| tail.contains(':')),
            "External rule paths cannot name an alternate data stream"
        );
        ensure!(
            native.components().all(|part| match part {
                Component::Normal(name) => windows_component(name.to_str().unwrap_or_default()),
                _ => true,
            }),
            "External rule paths must use ordinary Windows file names"
        );
    }
    Ok(())
}

/// Only names that have the same meaning in Win32 and verbatim paths are accepted.
#[cfg(windows)]
pub(crate) fn windows_component(name: &str) -> bool {
    if name.is_empty()
        || name.ends_with([' ', '.'])
        || name.contains([':', '<', '>', '|', '"', '*', '?'])
    {
        return false;
    }
    let device = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end()
        .to_ascii_uppercase();
    !matches!(
        device.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) && !device
        .strip_prefix("COM")
        .or_else(|| device.strip_prefix("LPT"))
        .is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ApprovedRulePaths(Arc<BTreeMap<String, ApprovedRuleFile>>);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ApprovedRuleFile {
    pub(crate) path: PathBuf,
    pub(crate) parent: DirectoryId,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct DirectoryId(u64, u64);

impl DirectoryId {
    /// Identify the opened directory object independently of its display path.
    pub(crate) fn from_file(file: &fs::File) -> std::io::Result<Self> {
        #[cfg(windows)]
        {
            use std::{mem::MaybeUninit, os::windows::io::AsRawHandle};
            use windows_sys::Win32::Storage::FileSystem::{
                BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
                GetFileInformationByHandle,
            };
            let mut info = MaybeUninit::uninit();
            // The owned file handle is live and the buffer has the expected Win32 structure size.
            if unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } == 0 {
                return Err(std::io::Error::last_os_error());
            }
            let info: BY_HANDLE_FILE_INFORMATION = unsafe { info.assume_init() };
            if info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0
                || info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
            {
                return Err(std::io::Error::from(std::io::ErrorKind::NotADirectory));
            }
            Ok(Self(
                u64::from(info.dwVolumeSerialNumber),
                (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
            ))
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let metadata = file.metadata()?;
            if !metadata.is_dir() {
                return Err(std::io::Error::from(std::io::ErrorKind::NotADirectory));
            }
            Ok(Self(metadata.dev(), metadata.ino()))
        }
        #[cfg(not(any(windows, unix)))]
        Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
    }
}

impl ApprovedRulePaths {
    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Pin only the exact paths that the caller has confirmed for this package.
    pub(super) fn approve(paths: &BTreeSet<String>) -> Result<Self> {
        let mut approved = BTreeMap::new();
        for path in paths {
            validate_path(path)?;
            let native: PathBuf = Path::new(path).components().collect();
            let target = PinnedPath::new(&native)
                .with_context(|| format!("Could not approve external rule file {path}"))?;
            let parent = DirectoryId::from_file(&target.parent()?)?;
            match target.metadata() {
                Ok(metadata) => ensure!(
                    metadata.is_file(),
                    "External rule scope must refer to a regular file"
                ),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("Could not approve external rule file {path}"));
                }
            }
            approved.insert(
                path.clone(),
                ApprovedRuleFile {
                    path: native,
                    parent,
                },
            );
        }
        Ok(Self(Arc::new(approved)))
    }

    /// Preserve the old directory identity for each path that did not receive a new review.
    pub(super) fn inherit(self, declared: &BTreeSet<String>, prior: Option<&Self>) -> Result<Self> {
        let mut approved = BTreeMap::new();
        for path in declared {
            let file = self
                .allows(path)
                .or_else(|| prior.and_then(|old| old.allows(path)))
                .context("Review the new external rule paths before installing")?;
            approved.insert(path.clone(), file.clone());
        }
        Ok(Self(Arc::new(approved)))
    }

    pub(super) fn validate_against(&self, declared: &BTreeSet<String>) -> Result<()> {
        ensure!(
            self.0.keys().eq(declared.iter())
                && self
                    .0
                    .values()
                    .all(|file| file.path.is_absolute() && file.path.file_name().is_some()),
            "Invalid installed external rule paths"
        );
        Ok(())
    }

    /// Identify new declared names without accessing any files during review.
    pub(crate) fn additional(
        declared: &BTreeSet<String>,
        prior: Option<&Self>,
    ) -> BTreeSet<String> {
        declared
            .iter()
            .filter(|path| prior.and_then(|old| old.allows(path)).is_none())
            .cloned()
            .collect()
    }

    pub(crate) fn allows(&self, requested: &str) -> Option<&ApprovedRuleFile> {
        self.0.get(requested)
    }
}
