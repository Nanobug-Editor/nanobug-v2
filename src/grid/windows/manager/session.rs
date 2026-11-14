//! Keep tree selection, transfer history and pending input under the manager's ownership.

use super::{Explorer, ManagerWindow};
use gpui::Task;
use nanobug::animation::scroll::{GridScroll, ScrollInput};
use std::path::{Path, PathBuf};

impl ManagerWindow {
    /// Inspect the visible tree without changing its selection or expansion state.
    pub(in crate::grid) fn tree(&self) -> &Explorer {
        &self.tree
    }

    /// Inspect the animated viewport for native placement and cursor effects.
    pub(in crate::grid) fn scroll(&self) -> &GridScroll {
        &self.scroll
    }

    /// Refresh directory contents while retaining path-based selection and marks.
    pub(in crate::grid) fn refresh(&mut self) -> anyhow::Result<()> {
        self.tree.refresh()
    }

    /// Select a visible `path`; leave the selection unchanged when it is absent.
    #[cfg(test)]
    pub(in crate::grid) fn select_path(&mut self, path: &Path) -> bool {
        if let Some(index) = self
            .tree
            .entries
            .iter()
            .position(|entry| entry.path == path)
        {
            self.tree.selected = index;
            true
        } else {
            false
        }
    }

    /// Reveal a fixture's path through the same expansion and refresh used by navigation.
    #[cfg(test)]
    pub(in crate::grid) fn reveal_path(&mut self, path: &Path) -> anyhow::Result<bool> {
        self.tree
            .expanded
            .extend(path.ancestors().skip(1).map(PathBuf::from));
        self.refresh()?;
        Ok(self.select_path(path))
    }

    /// Invalidate transfers touching `path` after an editor or another window changes it.
    pub(in crate::grid) fn invalidate_history(&mut self, path: &Path) {
        self.transfers.invalidate(path);
    }

    /// Stop waiting for a standalone undo when another input or focus change supersedes it.
    pub(in crate::grid) fn cancel_pending_undo(&mut self) {
        self.paste_timer = None;
    }

    /// Retain a delayed undo callback owned by the workspace's native event loop.
    pub(in crate::grid) fn schedule_undo(&mut self, task: Task<()>) {
        self.paste_timer = Some(task);
    }

    /// Cancel a partial repeat count without modifying selection.
    pub(in crate::grid) fn clear_count(&mut self) {
        self.count = 0;
    }

    /// Transfer the current nonzero repeat count to a workspace window command.
    pub(in crate::grid) fn take_window_count(&mut self) -> Option<usize> {
        let count = std::mem::take(&mut self.count);
        (count != 0).then_some(count)
    }

    /// Forget the first click when another window takes pointer input.
    pub(in crate::grid) fn clear_click(&mut self) {
        self.click = None;
    }

    /// Consume the prior click when deciding whether a pointer activation is a double click.
    pub(in crate::grid) fn take_click(&mut self) -> Option<PathBuf> {
        self.click.take()
    }

    /// Apply `input` at animation time `now` within `rows`, keeping selection and scroll together.
    pub(in crate::grid) fn track_view(
        &mut self,
        input: ScrollInput,
        rows: usize,
        now: f64,
    ) -> (bool, f32) {
        let (selected, delta) = self.scroll.track_rows(
            input,
            Some(self.tree.selected),
            rows,
            self.tree.entries.len(),
            now,
        );
        let selected = selected.unwrap_or(0);
        let changed = self.tree.selected != selected;
        self.tree.selected = selected;
        (changed, delta)
    }
}
