//! Filesystem commands run by the picker on a background worker.

use crate::grid::windows::{
    PathChange,
    file_picker::Delete,
    manager::{FileTransfers, move_exclusive},
};
use anyhow::{Context, Result, ensure};
use nanobug::i18n;
use std::{
    fs::{self, OpenOptions},
    io,
    path::{Component, Path, PathBuf},
};

/// Filesystem action captured before it leaves the UI thread.
pub(super) enum Operation {
    /// Create a file, or a directory when the name ends in a slash.
    Create(String),
    /// Change the basename of this path.
    Rename(PathBuf, String),
    /// Delete the confirmed selection.
    Delete(Delete),
    /// Paste the captured paths into the current directory.
    Paste,
    /// Open the selected path using the system's file association.
    External(PathBuf),
}

/// Effects to apply after an operation, including paths changed before any batch failure.
pub(super) struct Outcome {
    /// Paths for workspace buffer and tree reconciliation.
    pub changes: Vec<PathChange>,
    /// New selection, if the action created or renamed a visible row.
    pub select: Option<PathBuf>,
    /// Localized operation feedback.
    pub status: String,
}

impl Outcome {
    /// Build feedback with no filesystem changes.
    fn message(status: String) -> Self {
        Self {
            changes: Vec::new(),
            select: None,
            status,
        }
    }
}

impl Operation {
    /// Execute this action in `directory` using `transfers`, protecting `dirty` buffers and project `roots`.
    pub(super) fn run(
        self,
        directory: Option<&Path>,
        transfers: &mut FileTransfers,
        dirty: &[PathBuf],
        roots: &[PathBuf],
    ) -> Outcome {
        self.apply(directory, transfers, dirty, roots)
            .unwrap_or_else(|error| Outcome::message(format!("{error:#}")))
    }

    /// Apply the operation and collect all visible changes for the workspace.
    fn apply(
        self,
        directory: Option<&Path>,
        transfers: &mut FileTransfers,
        dirty: &[PathBuf],
        roots: &[PathBuf],
    ) -> Result<Outcome> {
        if let Self::External(path) = self {
            open::that_detached(&path).with_context(|| path.display().to_string())?;
            return Ok(Outcome::message(
                i18n::text("picker.external_opened").into(),
            ));
        }
        let directory = directory.context(i18n::text("picker.no_directory"))?;
        ensure!(
            fs::canonicalize(directory)? == directory,
            i18n::text("picker.path_changed")
        );
        match self {
            Self::Create(value) => {
                let folder = value.ends_with('/') || cfg!(windows) && value.ends_with('\\');
                let name = if folder {
                    &value[..value.len() - 1]
                } else {
                    &value
                };
                let path = child_path(directory, name)?;
                guard_dirty(&path, dirty)?;
                if folder {
                    fs::create_dir(&path)?;
                } else {
                    OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&path)?;
                }
                Ok(Outcome {
                    changes: vec![PathChange {
                        from: path.clone(),
                        to: None,
                    }],
                    select: Some(path),
                    status: i18n::text("picker.created").into(),
                })
            }
            Self::Rename(path, name) => {
                validate_parent(&path)?;
                let target = child_path(path.parent().expect("validated parent"), &name)?;
                if path == target {
                    return Ok(Outcome::message(String::new()));
                }
                guard_move(&path, roots)?;
                guard_dirty(&path, dirty)?;
                guard_dirty(&target, dirty)?;
                move_exclusive(&path, &target).with_context(|| {
                    i18n::format(
                        "manager.move_failed",
                        &[
                            ("source", &path.display()),
                            ("destination", &target.display()),
                        ],
                    )
                })?;
                Ok(Outcome {
                    changes: vec![PathChange {
                        from: path,
                        to: Some(target.clone()),
                    }],
                    select: Some(target),
                    status: i18n::text("picker.renamed").into(),
                })
            }
            Self::Delete(delete) => delete_paths(delete, dirty, roots),
            Self::Paste => {
                for path in transfers.cut_paths() {
                    guard_move(path, roots)?;
                }
                let changes = transfers.paste_browser(directory, dirty)?;
                let select = changes
                    .first()
                    .map(|change| change.to.as_ref().unwrap_or(&change.from).clone());
                Ok(Outcome {
                    changes,
                    select,
                    status: i18n::text("manager.pasted").into(),
                })
            }
            Self::External(_) => {
                unreachable!("external open was handled before directory validation")
            }
        }
    }
}

/// Join a single native basename `name` to canonical `directory`.
fn child_path(directory: &Path, name: &str) -> Result<PathBuf> {
    let mut components = Path::new(name).components();
    ensure!(
        matches!(components.next(), Some(Component::Normal(value)) if value == name)
            && components.next().is_none()
            && !name.starts_with(".nanobug-paste-")
            && (!cfg!(windows)
                || !name.contains(['<', '>', ':', '"', '/', '\\', '|', '?', '*'])
                    && !name.ends_with([' ', '.'])),
        i18n::text("picker.invalid_name")
    );
    Ok(directory.join(name))
}

/// Verify that the parent of `path` is still the canonical directory originally listed.
fn validate_parent(path: &Path) -> Result<()> {
    let parent = path.parent().context(i18n::text("picker.no_directory"))?;
    ensure!(
        fs::canonicalize(parent)? == parent,
        i18n::text("picker.path_changed")
    );
    Ok(())
}

/// Protect any dirty buffer whose file would be affected by changing `path`.
fn guard_dirty(path: &Path, dirty: &[PathBuf]) -> Result<()> {
    ensure!(
        !dirty.iter().any(|file| file.starts_with(path)),
        i18n::text("manager.unsaved_files")
    );
    Ok(())
}

/// Protect `roots` of open projects from a move or deletion of `path`.
fn guard_move(path: &Path, roots: &[PathBuf]) -> Result<()> {
    ensure!(
        !roots.iter().any(|root| root.starts_with(path)),
        i18n::text("picker.open_project_protected")
    );
    Ok(())
}

/// Delete the confirmed selection after checking all `dirty` paths and project `roots`.
fn delete_paths(delete: Delete, dirty: &[PathBuf], roots: &[PathBuf]) -> Result<Outcome> {
    for path in &delete.paths {
        validate_parent(path)?;
        guard_dirty(path, dirty)?;
        guard_move(path, roots)?;
    }
    let mut outcome = Outcome::message(i18n::text("picker.deleted").into());
    for path in delete.paths {
        let result = if delete.permanent {
            permanently_delete(&path).map_err(anyhow::Error::from)
        } else {
            trash::delete(&path).map_err(anyhow::Error::from)
        };
        match result {
            Ok(()) => outcome.changes.push(PathChange {
                from: path,
                to: None,
            }),
            Err(error) => {
                outcome.status = i18n::format(
                    "picker.delete_failed",
                    &[("path", &path.display()), ("error", &error)],
                );
                break;
            }
        }
    }
    Ok(outcome)
}

/// Remove `path` without following symbolic links.
fn permanently_delete(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileTypeExt;
        if metadata.file_type().is_symlink_dir() {
            return fs::remove_dir(path);
        }
    }
    if metadata.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A marked batch is validated before a delete can remove even its first item.
    #[test]
    fn delete_guards_dirty_buffers_and_reconciles_removed_paths() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let first = root.join("a.txt");
        let second = root.join("b.txt");
        fs::write(&first, "first").unwrap();
        fs::write(&second, "second").unwrap();
        let paths = vec![first.clone(), second.clone()];
        let mut transfers = FileTransfers::default();
        let deleted = Operation::Delete(Delete {
            paths: paths.clone(),
            permanent: true,
        })
        .run(
            Some(&root),
            &mut transfers,
            std::slice::from_ref(&second),
            &[],
        );
        assert!(deleted.changes.is_empty());
        assert!(first.is_file() && second.is_file());
        let deleted = Operation::Delete(Delete {
            paths,
            permanent: true,
        })
        .run(Some(&root), &mut transfers, &[], &[]);
        assert_eq!(deleted.changes.len(), 2);
        assert!(!first.exists() && !second.exists());
    }

    /// Creating and renaming use one basename and never replace a conflicting file.
    #[test]
    fn create_and_rename_reject_traversal_and_collisions() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let first = root.join("a.txt");
        let second = root.join("b.txt");
        fs::write(&first, "first").unwrap();
        fs::write(&second, "second").unwrap();
        let mut transfers = FileTransfers::default();
        let conflict = Operation::Rename(first.clone(), "b.txt".into()).run(
            Some(&root),
            &mut transfers,
            &[],
            &[],
        );
        assert!(conflict.changes.is_empty());
        assert_eq!(fs::read_to_string(&second).unwrap(), "second");
        assert!(child_path(&root, "../escape").is_err());
        let renamed =
            Operation::Rename(first, "中文.txt".into()).run(Some(&root), &mut transfers, &[], &[]);
        assert_eq!(renamed.select, Some(root.join("中文.txt")));
        assert_eq!(fs::read_to_string(root.join("中文.txt")).unwrap(), "first");
    }
}
