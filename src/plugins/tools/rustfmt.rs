//! Prepare a rustfmt invocation for the current Rust buffer.

use crate::plugins::{package::read_limited, query::resolve_prefix};
use anyhow::{Context, Result, ensure};
use std::{
    env, io,
    path::{Path, PathBuf},
    process::Command,
};
use toml::Value;

/// Cap the data read for each manifest consulted during edition discovery.
const MANIFEST_LIMIT: u64 = 1024 * 1024;

/// Format `document_path` inside `project_root` via stdin and stdout.
/// The caller owns the process and supplies the buffer text.
pub(super) fn command(project_root: &Path, document_path: &Path) -> Result<Command> {
    let root = project_root.canonicalize()?;
    let document = resolve_prefix(&root.join(document_path))?;
    ensure!(
        document.starts_with(&root),
        "The Rust file is outside the project"
    );
    ensure!(
        document
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("rs")),
        "rustfmt requires a Rust file"
    );
    let directory = document
        .parent()
        .context("The Rust file has no parent directory")?
        .ancestors()
        .take_while(|parent| parent.starts_with(&root))
        .find(|parent| parent.is_dir())
        .context("The Rust file has no existing directory")?;

    // rustfmt uses the working directory to find its config when reading stdin.
    // Rustup also uses it to select the installed project toolchain.
    let mut command = super::command(&rustfmt_path()?, directory);
    command.args(["--emit", "stdout", "--color", "never"]);
    command.env("RUSTUP_AUTO_INSTALL", "0");
    if let Some(edition) = cargo_edition(&root, directory)? {
        command.arg(format!("--edition={edition}"));
    }
    Ok(command)
}

/// Resolve rustfmt from absolute entries in the editor's PATH.
fn rustfmt_path() -> Result<PathBuf> {
    let name = format!("rustfmt{}", env::consts::EXE_SUFFIX);
    env::var_os("PATH")
        .and_then(|path| {
            env::split_paths(&path)
                .filter(|entry| entry.is_absolute())
                .map(|entry| entry.join(&name))
                .find(|entry| {
                    let Ok(metadata) = entry.metadata() else {
                        return false;
                    };
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        if metadata.permissions().mode() & 0o111 == 0 {
                            return false;
                        }
                    }
                    metadata.is_file()
                })
        })
        // Keep the proxy's name when it is a symlink to rustup.
        .context("rustfmt is unavailable on PATH. Install it with `rustup component add rustfmt`.")
}

/// Find the edition of the nearest package between `directory` and `root`.
fn cargo_edition(root: &Path, directory: &Path) -> Result<Option<String>> {
    for directory in directory
        .ancestors()
        .take_while(|path| path.starts_with(root))
    {
        let Some(manifest) = read_manifest(root, &directory.join("Cargo.toml"))? else {
            continue;
        };
        let Some(package) = manifest.get("package") else {
            return Ok(None);
        };
        let Some(edition) = package.get("edition") else {
            // Cargo uses the 2015 edition for packages that omit this key.
            return Ok(Some("2015".into()));
        };
        if let Some(edition) = edition.as_str() {
            return Ok(Some(edition.to_owned()));
        }
        ensure!(
            edition.get("workspace").and_then(Value::as_bool) == Some(true),
            "Cargo package edition must be a string or inherited from the workspace"
        );
        return workspace_edition(root, directory, &manifest).map(Some);
    }
    Ok(None)
}

/// Resolve the inherited edition for `member` using its `manifest` and the open project `root`.
fn workspace_edition(root: &Path, member: &Path, manifest: &Value) -> Result<String> {
    let read_edition = |manifest: &Value| {
        manifest
            .get("workspace")?
            .get("package")?
            .get("edition")?
            .as_str()
            .map(str::to_owned)
    };
    let edition = if let Some(path) = manifest
        .get("package")
        .and_then(|package| package.get("workspace"))
        .and_then(Value::as_str)
    {
        read_manifest(root, &member.join(path).join("Cargo.toml"))?
            .as_ref()
            .and_then(read_edition)
    } else if manifest.get("workspace").is_some() {
        read_edition(manifest)
    } else {
        let mut edition = None;
        for directory in member
            .ancestors()
            .skip(1)
            .take_while(|path| path.starts_with(root))
        {
            if let Some(manifest) = read_manifest(root, &directory.join("Cargo.toml"))?
                && manifest.get("workspace").is_some()
            {
                edition = read_edition(&manifest);
                break;
            }
        }
        edition
    };
    edition.context("Cargo workspace does not specify an inherited edition within the open project")
}

/// Read a Cargo manifest at `path` when it exists within `root`.
fn read_manifest(root: &Path, path: &Path) -> Result<Option<Value>> {
    let resolved = match path.canonicalize() {
        Ok(resolved) => resolved,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("Cannot resolve {}", path.display()));
        }
    };
    ensure!(
        resolved.starts_with(root),
        "Cargo manifest is outside the open project"
    );
    ensure!(resolved.is_file(), "Cargo manifest is not a regular file");
    let bytes = read_limited(&resolved, MANIFEST_LIMIT)
        .with_context(|| format!("Cannot read {}", path.display()))?;
    let text =
        std::str::from_utf8(&bytes).with_context(|| format!("{} is not UTF-8", path.display()))?;
    let manifest =
        toml::from_str(text).with_context(|| format!("Cannot parse {}", path.display()))?;
    Ok(Some(manifest))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn cargo_edition_distinguishes_inheritance_from_the_legacy_default() -> Result<()> {
        let project = tempfile::tempdir()?;
        let root = project.path().canonicalize()?;
        let member = root.join("crates/member");
        let source = member.join("src");
        fs::create_dir_all(&source)?;
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = ['crates/member']\n[workspace.package]\nedition = '2024'\n",
        )?;
        let manifest = member.join("Cargo.toml");
        fs::write(
            &manifest,
            "[package]\nname = 'member'\nedition.workspace = true\n",
        )?;
        assert_eq!(cargo_edition(&root, &source)?.as_deref(), Some("2024"));
        fs::write(&manifest, "[package]\nname = 'member'\n")?;
        assert_eq!(cargo_edition(&root, &source)?.as_deref(), Some("2015"));
        assert_eq!(cargo_edition(&root, &root)?, None);
        Ok(())
    }
}
