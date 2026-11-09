//! Buffer opening, saving and reconciliation after filesystem changes.

use super::{Buffer, EditorWindow};
use crate::grid::windows::PathChange;
use nanobug::{
    animation::scroll::GridScroll,
    document::Document,
    vim::{Mode, Vim},
};
use std::path::PathBuf;

impl EditorWindow {
    /// Format command or search input with its prompt, before any transient IME text.
    pub(in crate::grid) fn command_text(&self) -> String {
        let vim = self.vim();
        let prefix = if vim.mode == Mode::Command { ':' } else { '/' };
        format!("{prefix}{}", vim.command)
    }

    /// Open `path` at animation time `now`, reusing an existing buffer and retaining unsaved text.
    pub(in crate::grid) fn open(&mut self, path: PathBuf, now: f64) -> anyhow::Result<()> {
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        if let Some(index) = self
            .buffers
            .iter()
            .position(|buffer| buffer.vim.doc.path.as_ref() == Some(&path))
        {
            self.active = index;
        } else {
            let doc = Document::open(&path)?;
            self.buffers.push(Buffer {
                vim: Vim::new(doc),
                scroll: GridScroll::default(),
                columns: GridScroll::default(),
            });
            self.active = self.buffers.len() - 1;
        }
        let buffer = &mut self.buffers[self.active];
        buffer.scroll.snap(buffer.scroll.first_cell(), now);
        buffer.columns.snap(buffer.columns.first_cell(), now);
        self.composition.clear();
        Ok(())
    }

    /// Save buffer `index` to `path` or its filename, returning the path whose transfer history is stale.
    pub(in crate::grid) fn save(
        &mut self,
        index: usize,
        path: Option<PathBuf>,
    ) -> anyhow::Result<Option<PathBuf>> {
        let doc = &mut self.buffers[index].vim.doc;
        let previous = doc.path.clone();
        let dirty = doc.modified();
        doc.save(path.as_deref())?;
        Ok(if dirty || doc.path != previous {
            doc.path.clone()
        } else {
            None
        })
    }

    /// Replace buffer `index` with its saved text and a fresh Vim session and viewport.
    pub(in crate::grid) fn discard(&mut self, index: usize) {
        let buffer = &mut self.buffers[index];
        buffer.vim = Vim::new(buffer.vim.doc.saved_version());
        buffer.scroll = GridScroll::default();
        buffer.columns = GridScroll::default();
    }

    /// Files with unsaved edits must not be overwritten by manager transfers.
    pub(in crate::grid) fn dirty_paths(&self) -> Vec<PathBuf> {
        self.buffers
            .iter()
            .filter(|buffer| buffer.vim.doc.modified())
            .filter_map(|buffer| buffer.vim.doc.path.clone())
            .collect()
    }

    /// Include unnamed modified buffers when guarding application exit.
    pub(in crate::grid) fn modified(&self) -> bool {
        self.buffers.iter().any(|buffer| buffer.vim.doc.modified())
    }

    /// Retarget buffers after moves in `changes` and drop stale views of replaced paths.
    /// Return whether changes were applied, requiring the workspace to rebase its cursor.
    pub(in crate::grid) fn apply_path_changes(&mut self, changes: &[PathChange]) -> bool {
        if changes.is_empty() {
            return false;
        }
        let mut previous_index = 0;
        let mut active = self.active;
        self.buffers.retain_mut(|buffer| {
            let index = previous_index;
            previous_index += 1;
            if let Some((change, relative)) = buffer.vim.doc.path.as_ref().and_then(|path| {
                changes.iter().find_map(|change| {
                    path.strip_prefix(&change.from)
                        .ok()
                        .map(|relative| (change, relative))
                })
            }) {
                if let Some(to) = &change.to {
                    let path = if relative.as_os_str().is_empty() {
                        to.clone()
                    } else {
                        to.join(relative)
                    };
                    buffer.vim.doc.path = Some(path);
                    return true;
                }
            } else if !buffer.vim.doc.path.as_ref().is_some_and(|path| {
                changes
                    .iter()
                    .any(|change| change.to.as_ref().is_some_and(|to| path.starts_with(to)))
            }) {
                return true;
            }
            active -= usize::from(index < self.active);
            false
        });
        if self.buffers.is_empty() {
            self.buffers.push(Buffer {
                vim: Vim::new(Document::new("")),
                scroll: GridScroll::default(),
                columns: GridScroll::default(),
            });
        }
        self.active = active.min(self.buffers.len() - 1);
        true
    }
}
