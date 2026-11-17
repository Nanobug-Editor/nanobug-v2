//! Retained current and previous directories for independently scoped Ex paths.

use std::path::{Path, PathBuf};

/// Working directory and last effective location for a single scope.
#[derive(Default)]
pub(in crate::grid) struct DirectorySlot {
    path: Option<PathBuf>,
    previous: Option<PathBuf>,
}

impl DirectorySlot {
    /// Access the directory set for this scope, if any.
    pub(in crate::grid) fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Access the directory replaced by the last change to this scope.
    pub(in crate::grid) fn previous(&self) -> Option<&Path> {
        self.previous.as_deref()
    }

    /// Install a validated local directory, retaining the old effective directory.
    pub(in crate::grid) fn replace(&mut self, path: PathBuf, previous: PathBuf) {
        self.path = Some(path);
        self.previous = Some(previous);
    }

    /// Unset the directory without altering the history used by :cd -.
    pub(in crate::grid) fn clear(&mut self) {
        self.path = None;
    }

    /// Give a new window or Tabpage the local directory of its originating view.
    pub(in crate::grid) fn inherited(&self) -> Self {
        Self {
            path: self.path.clone(),
            previous: None,
        }
    }
}
