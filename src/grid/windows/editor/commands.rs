//! Buffer opening, saving and reconciliation after filesystem changes.

use super::{Buffer, EditorWindow};
use crate::grid::windows::PathChange;
use nanobug::document::Document;
use std::path::PathBuf;

impl EditorWindow {
    /// Format command or search input with preedit and any completion output after the prompt.
    pub(in crate::grid) fn command_text(&self) -> String {
        let vim = self.vim();
        let (before, after) = vim.command.split_at(vim.command_cursor());
        let mut text = format!("{}{before}{}{after}", vim.prompt(), self.composition.text);
        if !vim.message.is_empty() {
            text.push_str("  ");
            text.push_str(&vim.message);
        }
        text
    }

    /// Command-line text up to the logical caret, for the IME candidate anchor.
    pub(in crate::grid) fn command_caret_text(&self) -> String {
        let vim = self.vim();
        format!("{}{}", vim.prompt(), &vim.command[..vim.command_cursor()])
    }

    /// Open `path` at animation time `now`, reusing an existing buffer and retaining unsaved text.
    pub(in crate::grid) fn open(&mut self, path: PathBuf, now: f64) -> anyhow::Result<()> {
        self.open_shared(path, None, now)
    }

    /// Open a matching `view` from this Session, or load `path` on a fresh buffer.
    pub(in crate::grid) fn open_shared(
        &mut self,
        path: PathBuf,
        view: Option<Buffer>,
        now: f64,
    ) -> anyhow::Result<()> {
        let previous = self.buffer().id;
        let replace_empty = self.buffers.len() == 1
            && self.terminal().is_none()
            && self.buffer().vim.doc.path.is_none()
            && self.buffer().vim.doc.text.is_empty()
            && !self.buffer().vim.doc.modified();
        let index = self.open_buffer(path, view)?;
        let id = self.buffers[index].id;
        self.select_buffer(id, now);
        if replace_empty && previous != id {
            self.remove_buffer_tab(previous, now);
        }
        self.composition.clear();
        Ok(())
    }

    /// Find or load a buffer for `path` in this window.
    fn open_buffer(&mut self, path: PathBuf, view: Option<Buffer>) -> anyhow::Result<usize> {
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        if let Some(index) = self
            .buffers
            .iter()
            .position(|buffer| buffer.vim.doc.path.as_ref() == Some(&path))
        {
            return Ok(index);
        } else {
            let buffer = match view {
                Some(view) => view,
                None => Buffer::new(Document::open(&path)?),
            };
            self.push_buffer(buffer);
        }
        Ok(self.buffers.len() - 1)
    }

    /// Retain a prepared `doc` in this window without changing focus or doing disk I/O.
    pub(in crate::grid) fn retain_document(&mut self, doc: Document) -> usize {
        self.retain_buffer_view(Buffer::new(doc))
    }

    /// Retain a new `view` of a Session buffer without changing the active label.
    pub(in crate::grid) fn retain_buffer_view(&mut self, view: Buffer) -> usize {
        self.push_buffer(view);
        self.buffers.len() - 1
    }

    /// Synchronize shared text, file names, and dirty status before consuming this pane's state.
    pub(in crate::grid) fn refresh_shared_documents(&mut self) {
        for buffer in &mut self.buffers {
            let previous = buffer.vim.doc.path.clone();
            buffer.refresh_shared_document();
            if buffer.vim.doc.path != previous {
                let doc = &buffer.vim.doc;
                self.jump_history
                    .update_path(doc.opening, doc.path.as_deref());
            }
        }
    }

    /// Reuse a restored buffer for each canonical filename while keeping window view state.
    pub(in crate::grid) fn join_restored_buffers(
        &mut self,
        owners: &mut std::collections::HashMap<PathBuf, Buffer>,
    ) {
        use std::collections::hash_map::Entry;
        for buffer in &mut self.buffers {
            let Some(path) = buffer
                .vim
                .doc
                .path
                .clone()
                .filter(|_| buffer.supports_shared_view())
            else {
                continue;
            };
            match owners.entry(path) {
                Entry::Occupied(owner) => {
                    let source = &owner.get().vim;
                    buffer.vim.doc.join_buffer(&source.doc);
                    buffer.vim.share_buffer_options(source);
                }
                Entry::Vacant(entry) => {
                    entry.insert(buffer.shared_view());
                }
            }
        }
    }

    /// Remove all local labels for the shared buffer `number`.
    pub(in crate::grid) fn remove_session_buffer(&mut self, number: u64) -> bool {
        let before = self.buffers.len();
        let was_active = self.buffer().id;
        self.buffers
            .retain(|buffer| buffer.vim.doc.buffer_number() != number);
        let changed = self.buffers.len() != before;
        if changed {
            self.reconcile_buffers(was_active);
        }
        changed
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
    /// `exclusive` requires an explicit, vacant destination.
    pub(in crate::grid) fn save(
        &mut self,
        index: usize,
        path: Option<PathBuf>,
        exclusive: bool,
    ) -> anyhow::Result<Option<PathBuf>> {
        let doc = &mut self.buffers[index].vim.doc;
        doc.refresh_shared();
        let previous = doc.path.clone();
        let dirty = doc.modified();
        if exclusive {
            doc.save_new(path.as_deref().expect("explicit save target"))?;
        } else {
            doc.save(path.as_deref())?;
        }
        if doc.path != previous {
            self.jump_history
                .update_path(doc.opening, doc.path.as_deref());
        }
        Ok(if dirty || doc.path != previous {
            doc.path.clone()
        } else {
            None
        })
    }

    /// Restore the saved state of buffer `index` in every Session view.
    pub(in crate::grid) fn discard(&mut self, index: usize) {
        let buffer = &mut self.buffers[index];
        buffer.vim.discard_changes();
        buffer.set_presentation(None);
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
                    buffer.vim.doc.relocate(path);
                    let doc = &buffer.vim.doc;
                    self.jump_history
                        .update_path(doc.opening, doc.path.as_deref());
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
            self.push_buffer(Buffer::new(Document::new("")));
        }
        let active = self.buffer_index(was_active);
        self.active = active.unwrap_or_else(|| self.active.min(self.buffers.len() - 1));
        self.alternate = self.alternate.filter(|&id| {
            self.buffer_index(id)
                .is_some_and(|index| index != self.active)
        });
        if active.is_none() {
            self.reset_buffer_input();
        }
    }
}
