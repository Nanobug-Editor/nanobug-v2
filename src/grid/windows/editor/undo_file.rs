//! Apply Ex undo archives to the displayed buffer without exposing mutable buffer storage.

use super::EditorWindow;
use std::path::Path;

impl EditorWindow {
    /// Write the undo history to `path` when `write` is true, otherwise load it for this buffer.
    /// `force` permits replacing a destination of a different format.
    pub(in crate::grid) fn ex_undo_file(
        &mut self,
        path: &Path,
        write: bool,
        force: bool,
    ) -> anyhow::Result<()> {
        let document = &mut self.buffers[self.active].vim.doc;
        if write {
            document.write_undo_file(path, force)
        } else {
            document.read_undo_file(path)
        }
    }
}
