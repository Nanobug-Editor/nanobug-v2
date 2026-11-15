//! Buffer opening, saving and reconciliation after filesystem changes.

use super::{Buffer, EditorWindow};
use crate::grid::windows::PathChange;
use nanobug::document::Document;
use std::path::PathBuf;

impl EditorWindow {
    /// Format command or search input with its prompt, before any transient IME text.
    pub(in crate::grid) fn command_text(&self) -> String {
        let vim = self.vim();
        let (before, after) = vim.command.split_at(vim.command_cursor());
        format!("{}{before}{}{after}", vim.prompt(), self.composition.text)
    }

    /// Command-line text up to the logical caret, for the IME candidate anchor.
    pub(in crate::grid) fn command_caret_text(&self) -> String {
        let vim = self.vim();
        format!("{}{}", vim.prompt(), &vim.command[..vim.command_cursor()])
    }

    /// Open `path` at animation time `now`, reusing an existing buffer and retaining unsaved text.
    pub(in crate::grid) fn open(&mut self, path: PathBuf, now: f64) -> anyhow::Result<()> {
        let previous = self.buffer().id;
        let replace_empty = self.buffers.len() == 1
            && self.buffer().vim.doc.path.is_none()
            && self.buffer().vim.doc.text.is_empty()
            && !self.buffer().vim.doc.modified();
        let index = self.open_buffer(path)?;
        let id = self.buffers[index].id;
        self.select_buffer(id, now);
        if replace_empty && previous != id {
            self.remove_buffer_tab(previous, now);
        }
        self.composition.clear();
        Ok(())
    }

    /// Find or load a buffer for `path` in this window.
    fn open_buffer(&mut self, path: PathBuf) -> anyhow::Result<usize> {
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        if let Some(index) = self
            .buffers
            .iter()
            .position(|buffer| buffer.vim.doc.path.as_ref() == Some(&path))
        {
            return Ok(index);
        } else {
            let doc = Document::open(&path)?;
            self.buffers.push(Buffer::new(doc));
        }
        Ok(self.buffers.len() - 1)
    }

    /// Retain a prepared `doc` in this window without changing focus or doing disk I/O.
    pub(in crate::grid) fn retain_document(&mut self, doc: Document) -> usize {
        self.buffers.push(Buffer::new(doc));
        self.buffers.len() - 1
    }

    /// Move buffer `index` to another tab, retaining a valid editor in this tab.
    pub(in crate::grid) fn take_buffer(&mut self, index: usize) -> Buffer {
        let was_active = self.buffer().id;
        let mut buffer = self.buffers.remove(index);
        buffer.vim.cancel_pending();
        self.reconcile_buffers(was_active);
        buffer
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
        let id = buffer.id;
        *buffer = Buffer::new(buffer.vim.doc.saved_version());
        buffer.id = id;
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
        let was_active = self.buffer().id;
        self.buffers.retain_mut(|buffer| {
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
            false
        });
        self.reconcile_buffers(was_active);
        true
    }

    /// Remove a file label and its buffer after the caller resolves unsaved changes.
    pub(in crate::grid) fn remove_buffer_tab(&mut self, id: u64, _now: f64) -> bool {
        let Some(index) = self.buffer_index(id) else {
            return false;
        };
        self.take_buffer(index);
        true
    }

    /// Preserve the active buffer when possible, otherwise select its neighbor or create a blank.
    fn reconcile_buffers(&mut self, was_active: u64) {
        if self.buffers.is_empty() {
            self.buffers.push(Buffer::new(Document::new("")));
        }
        let active = self.buffer_index(was_active);
        self.active = active.unwrap_or_else(|| self.active.min(self.buffers.len() - 1));
        if active.is_none() {
            self.reset_buffer_input();
        }
    }
}
