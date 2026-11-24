//! Resolve Rust tools for desktop processes with an older Windows environment.

use anyhow::{Context, Result};
use std::{
    env,
    ffi::{OsStr, OsString, c_void},
    os::windows::{
        ffi::OsStringExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::Path,
    process::Command,
    ptr,
};
use windows_sys::Win32::{
    Security::{TOKEN_DUPLICATE, TOKEN_QUERY},
    System::{
        Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock},
        Threading::{GetCurrentProcess, OpenProcessToken},
    },
};

/// Launch rust-analyzer in `root`, preserving process overrides and finding newly saved tools.
pub(super) fn rust_analyzer_command(root: &Path) -> Result<Command> {
    let saved = SavedRustEnvironment::read().unwrap_or_default();
    let inherited = env::var_os("PATH").unwrap_or_default();
    let inherited_cargo_home = env::var_os("CARGO_HOME").filter(|value| !value.is_empty());
    let cargo_bin = inherited_cargo_home
        .as_ref()
        .or(saved.cargo_home.as_ref())
        .map(|home| Path::new(home).join("bin"))
        .or_else(|| dirs::home_dir().map(|home| home.join(".cargo").join("bin")));
    let mut directories = Vec::new();
    // Rustup installs its proxies in Cargo's bin directory even when it is absent from PATH.
    for directory in env::split_paths(&inherited)
        .chain(env::split_paths(&saved.path))
        .chain(cargo_bin)
    {
        if directory.is_absolute() && !directories.contains(&directory) {
            directories.push(directory);
        }
    }
    let path = directories
        .iter()
        .map(|dir| dir.join("rust-analyzer.exe"))
        .find(|candidate| candidate.is_file())
        .context("rust-analyzer is unavailable on PATH and in Cargo's bin directory. Install it with rustup component add rust-analyzer.")?;
    let mut command = super::super::super::command(&path, root);
    // Cargo and rustc also need the discovered directories when rust-analyzer loads the workspace.
    command.env("PATH", env::join_paths(&directories)?);
    for (name, saved) in [
        ("CARGO_HOME", saved.cargo_home),
        ("RUSTUP_HOME", saved.rustup_home),
    ] {
        if env::var_os(name).is_none_or(|value| value.is_empty())
            && let Some(value) = saved
        {
            command.env(name, value);
        }
    }
    command.env("RUSTUP_AUTO_INSTALL", "0");
    Ok(command)
}

/// The Rust-related settings Windows would give a new process for the current user.
#[derive(Default)]
struct SavedRustEnvironment {
    path: OsString,
    cargo_home: Option<OsString>,
    rustup_home: Option<OsString>,
}

impl SavedRustEnvironment {
    /// Read the merged user and machine settings without modifying the editor environment.
    fn read() -> Option<Self> {
        let mut token = ptr::null_mut();
        // SAFETY: The current process pseudo-handle is valid and token is an output pointer.
        if unsafe {
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_QUERY | TOKEN_DUPLICATE,
                &mut token,
            )
        } == 0
        {
            return None;
        }
        // SAFETY: OpenProcessToken succeeded and transferred ownership of the handle.
        let token = unsafe { OwnedHandle::from_raw_handle(token) };
        let mut block = ptr::null_mut();
        // SAFETY: The token stays open, and block is an output pointer.
        if unsafe { CreateEnvironmentBlock(&mut block, token.as_raw_handle(), 0) } == 0 {
            return None;
        }
        let block = EnvironmentBlock(block);
        let mut entry = block.0.cast::<u16>();
        let mut saved = Self::default();
        // SAFETY: Windows provides a UTF-16 sequence of NUL-terminated strings, followed by an
        // extra NUL. The owning block stays alive for the entire scan.
        unsafe {
            while *entry != 0 {
                let mut len = 0;
                while *entry.add(len) != 0 {
                    len += 1;
                }
                let units = std::slice::from_raw_parts(entry, len);
                if let Some(separator) = units.iter().position(|unit| *unit == u16::from(b'=')) {
                    let name = OsString::from_wide(&units[..separator]);
                    let value = &units[separator + 1..];
                    if name.eq_ignore_ascii_case(OsStr::new("PATH")) {
                        saved.path = OsString::from_wide(value);
                    } else if !value.is_empty()
                        && name.eq_ignore_ascii_case(OsStr::new("CARGO_HOME"))
                    {
                        saved.cargo_home = Some(OsString::from_wide(value));
                    } else if !value.is_empty()
                        && name.eq_ignore_ascii_case(OsStr::new("RUSTUP_HOME"))
                    {
                        saved.rustup_home = Some(OsString::from_wide(value));
                    }
                }
                entry = entry.add(len + 1);
            }
        }
        Some(saved)
    }
}

/// Own the allocation returned by CreateEnvironmentBlock.
struct EnvironmentBlock(*mut c_void);

impl Drop for EnvironmentBlock {
    fn drop(&mut self) {
        // SAFETY: The pointer came from CreateEnvironmentBlock and is freed exactly once.
        unsafe { DestroyEnvironmentBlock(self.0) };
    }
}
