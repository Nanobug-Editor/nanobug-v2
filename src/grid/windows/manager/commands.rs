//! Filesystem actions and feedback owned by the file-manager window.

use super::{ManagerWindow, PathChange};
use nanobug::i18n;
use std::path::{Path, PathBuf};

impl ManagerWindow {
    /// Apply paste/undo/redo from `key` within `project`, protecting unsaved `dirty` paths.
    /// Return path changes for editor reconciliation and feedback including any refresh error.
    pub(in crate::grid) fn transfer(
        &mut self,
        key: &str,
        project: &Path,
        dirty: &[PathBuf],
    ) -> (Vec<PathChange>, String) {
        let directory = self.tree.directory().to_path_buf();
        let result = if key == "v" {
            self.transfers.paste(project, &directory, dirty)
        } else {
            self.transfers.replay(key == "Z", dirty)
        };
        let mut changes_applied = Vec::new();
        let mut message: String = match result {
            Ok(changes) => {
                changes_applied = changes;
                if key == "v" && directory != project {
                    self.tree.expanded.insert(directory);
                }
                i18n::text(match key {
                    "v" => "manager.pasted",
                    "z" => "manager.paste_undone",
                    _ => "manager.paste_redone",
                })
                .into()
            }
            Err(error) => format!("{error:#}"),
        };
        if let Err(error) = self.tree.refresh() {
            message.push_str(" · ");
            message.push_str(&i18n::format(
                "manager.refresh_failed",
                &[("error", &format_args!("{error:#}"))],
            ));
        }
        (changes_applied, message)
    }
}
