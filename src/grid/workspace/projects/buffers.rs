//! Shared buffers in a Session and the independent views that display them.

use super::{EditorWindow, PaneId, Project};
use crate::grid::windows::{editor::Buffer, terminal::Terminal};
use nanobug::document::Document;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
};

/// Buffer state retained by a Session without placing a label in a pane.
pub(super) struct DetachedBuffer {
    view: Buffer,
    loaded: bool,
    initial_line: Option<usize>,
}

impl Project {
    /// Remember a filename and assign its stable number without reading the file.
    pub(in crate::grid::workspace) fn list_unloaded_file(
        &mut self,
        path: PathBuf,
        line: Option<usize>,
    ) -> u64 {
        if let Some(number) = self.number_for_path(&path) {
            return number;
        }
        let mut doc = Document::new("");
        doc.set_ex_filename(Some(path));
        let number = doc.buffer_number();
        self.detached_buffers.insert(
            number,
            DetachedBuffer {
                view: Buffer::unloaded(doc),
                loaded: false,
                initial_line: line,
            },
        );
        number
    }

    /// Resolve an already listed filename, including unloaded entries.
    pub(in crate::grid::workspace) fn number_for_path(&self, path: &Path) -> Option<u64> {
        self.session_buffer_entries()
            .into_iter()
            .find_map(|(number, view)| {
                (view.vim.doc.path.as_deref() == Some(path)).then_some(number)
            })
    }

    /// Read an unloaded buffer on its first use, retaining its original number.
    pub(in crate::grid::workspace) fn load_detached_buffer(
        &mut self,
        number: u64,
    ) -> anyhow::Result<()> {
        if let Some(entry) = self
            .detached_buffers
            .get_mut(&number)
            .filter(|entry| !entry.loaded)
        {
            if entry.view.vim.doc.path.is_some() {
                entry.view.vim.doc.load_file_contents()?;
            }
            if let Some(line) = entry.initial_line.take() {
                let doc = &mut entry.view.vim.doc;
                doc.cursor = doc.starts[line.saturating_sub(1).min(doc.starts.len() - 1)];
            }
            entry.loaded = true;
        }
        Ok(())
    }

    /// Materialize a previously listed filename before reusing its shared document.
    pub(super) fn load_detached_path(&mut self, path: &Path) -> anyhow::Result<()> {
        let number = self.detached_buffers.iter().find_map(|(&number, entry)| {
            (entry.view.vim.doc.path.as_deref() == Some(path) && !entry.loaded).then_some(number)
        });
        if let Some(number) = number {
            self.load_detached_buffer(number)?;
        }
        Ok(())
    }

    /// Report whether a listed buffer has loaded data to display.
    pub(in crate::grid::workspace) fn session_buffer_loaded(&self, number: u64) -> bool {
        self.detached_buffers
            .get(&number)
            .is_none_or(|entry| entry.loaded)
    }

    /// Apply `:noswapfile` only to editable buffers first materialized after `before` was collected.
    pub(in crate::grid::workspace) fn disable_swap_for_new_buffers(
        &mut self,
        before: &HashSet<u64>,
    ) {
        for editor in std::iter::once(&mut self.editor).chain(self.inactive_editors.values_mut()) {
            for index in 0..editor.buffers().len() {
                let buffer = &editor.buffers()[index];
                if buffer.supports_shared_view()
                    && !before.contains(&buffer.vim.doc.buffer_number())
                {
                    editor.edit_buffer(index, |vim, _| {
                        vim.disable_swapfile();
                        nanobug::vim::Action::None
                    });
                }
            }
        }
        for (id, entry) in &mut self.detached_buffers {
            if entry.loaded && !before.contains(id) {
                entry.view.vim.disable_swapfile();
            }
        }
    }

    /// Visit retained text for close and write-all, including buffers without any label.
    pub(in crate::grid::workspace) fn detached_documents(&self) -> impl Iterator<Item = &Document> {
        self.detached_buffers
            .values()
            .filter(|entry| entry.loaded)
            .map(|entry| &entry.view.vim.doc)
    }

    /// Mutate detached text by its stable buffer number.
    pub(in crate::grid::workspace) fn detached_document_mut(
        &mut self,
        number: u64,
    ) -> Option<&mut Document> {
        Some(&mut self.detached_buffers.get_mut(&number)?.view.vim.doc)
    }

    /// Remove a buffer together with its file marks and window navigation references.
    pub(in crate::grid::workspace) fn wipe_session_buffer(&mut self, number: u64, now: f64) {
        let Some(buffer) = self.session_buffer(number) else {
            return;
        };
        let path = buffer.vim.doc.path.clone();
        self.file_marks.forget_buffer(number, path.as_deref());
        for editor in std::iter::once(&mut self.editor).chain(self.inactive_editors.values_mut()) {
            editor.forget_buffer_positions(number, path.as_deref());
        }
        self.remove_session_buffer(number, now);
    }

    /// Keep the text of a closing editor's buffers available even without pane labels.
    pub(in crate::grid::workspace) fn retain_hidden_editor(&mut self, pane: PaneId) {
        let views = self
            .editor_by_id(pane)
            .into_iter()
            .flat_map(EditorWindow::buffers)
            .filter(|buffer| buffer.supports_shared_view())
            .map(|buffer| (buffer.vim.doc.buffer_number(), buffer.shared_view()))
            .collect::<Vec<_>>();
        for (number, view) in views {
            self.detached_buffers.insert(
                number,
                DetachedBuffer {
                    view,
                    loaded: true,
                    initial_line: None,
                },
            );
        }
    }

    /// Retain and remove the outgoing label after a successful `:hide {cmd}` file change.
    pub(in crate::grid::workspace) fn detach_active_label(&mut self, id: u64, now: f64) {
        let Some(index) = self.editor.buffer_index(id) else {
            return;
        };
        let buffer = &self.editor.buffers()[index];
        if buffer.supports_shared_view() {
            let number = buffer.vim.doc.buffer_number();
            let view = self.editor.take_buffer(index);
            self.detached_buffers.insert(
                number,
                DetachedBuffer {
                    view,
                    loaded: true,
                    initial_line: None,
                },
            );
        } else if buffer.terminal().is_some() {
            self.editor.remove_buffer_tab(id, now);
        }
    }

    /// Drop file contents and labels while leaving the number available for reopening.
    pub(in crate::grid::workspace) fn unload_session_buffer(
        &mut self,
        number: u64,
        now: f64,
    ) -> bool {
        let Some(mut view) = self
            .session_buffer(number)
            .filter(|view| view.supports_shared_view())
            .map(Buffer::shared_view)
        else {
            return self.remove_session_buffer(number, now);
        };
        self.remove_session_buffer(number, now);
        view.vim.doc.unload_file_contents();
        self.detached_buffers.insert(
            number,
            DetachedBuffer {
                view,
                loaded: false,
                initial_line: None,
            },
        );
        true
    }

    /// List one pane and index per buffer that has a label, in stable Ex number order.
    pub(in crate::grid::workspace) fn session_buffers(&self) -> Vec<(PaneId, usize)> {
        let mut buffers = BTreeMap::new();
        for (pane, editor) in self.editors() {
            for (index, buffer) in editor.buffers().iter().enumerate() {
                let number = buffer.vim.doc.buffer_number();
                if pane == self.active_editor_id() {
                    buffers.insert(number, (pane, index));
                } else {
                    buffers.entry(number).or_insert((pane, index));
                }
            }
        }
        buffers.into_values().collect()
    }

    /// Find the buffer by its stable number, including terminals with no remaining labels.
    pub(in crate::grid::workspace) fn session_buffer(&self, number: u64) -> Option<&Buffer> {
        self.editor
            .buffers()
            .iter()
            .find(|buffer| buffer.vim.doc.buffer_number() == number)
            .or_else(|| {
                self.editors()
                    .filter(|(pane, _)| *pane != self.active_editor_id())
                    .flat_map(|(_, editor)| editor.buffers())
                    .find(|buffer| buffer.vim.doc.buffer_number() == number)
            })
            .or_else(|| self.terminal_buffers.get(&number))
            .or_else(|| self.detached_buffers.get(&number).map(|entry| &entry.view))
    }

    /// Enumerate one representative per Ex buffer, preferring the focused pane's view.
    pub(in crate::grid::workspace) fn session_buffer_entries(&self) -> BTreeMap<u64, &Buffer> {
        let active = self.active_editor_id();
        let mut buffers = BTreeMap::new();
        for (pane, editor) in self.editors() {
            for buffer in editor.buffers() {
                let number = buffer.vim.doc.buffer_number();
                if pane == active {
                    buffers.insert(number, buffer);
                } else {
                    buffers.entry(number).or_insert(buffer);
                }
            }
        }
        for (&number, buffer) in &self.terminal_buffers {
            buffers.entry(number).or_insert(buffer);
        }
        for (&number, entry) in &self.detached_buffers {
            buffers.entry(number).or_insert(&entry.view);
        }
        buffers
    }

    /// Enumerate stable Ex numbers once each, including hidden terminals.
    pub(in crate::grid::workspace) fn session_buffer_numbers(&self) -> Vec<u64> {
        self.session_buffer_entries().into_keys().collect()
    }

    /// Register a terminal with the Session and place its first view in the active pane.
    pub(in crate::grid::workspace) fn retain_terminal(
        &mut self,
        terminal: Terminal,
        now: f64,
    ) -> u64 {
        let buffer = Buffer::terminal_view(terminal);
        let number = buffer.vim.doc.buffer_number();
        let view = buffer.preview_view();
        let id = view.id;
        self.terminal_buffers.insert(number, buffer);
        self.editor.retain_buffer_view(view);
        self.editor.select_buffer(id, now);
        number
    }

    /// Find the Session-owned PTY independently of pane focus or retained labels.
    pub(in crate::grid::workspace) fn terminal(&self, number: u64) -> Option<&Terminal> {
        self.terminal_buffers.get(&number)?.terminal()
    }

    /// Activate buffer `number` in the current pane, adding a view when needed.
    pub(in crate::grid::workspace) fn select_session_buffer(
        &mut self,
        number: u64,
        now: f64,
    ) -> bool {
        if let Err(error) = self.load_detached_buffer(number) {
            self.status = format!("{error:#}");
            return false;
        }
        let local = self
            .editor
            .buffers()
            .iter()
            .find(|buffer| buffer.vim.doc.buffer_number() == number);
        let id = if let Some(buffer) = local {
            buffer.id
        } else {
            let Some(view) = self.session_buffer(number).and_then(Buffer::split_view) else {
                return false;
            };
            let id = view.id;
            self.editor.retain_buffer_view(view);
            id
        };
        self.editor.select_buffer(id, now);
        true
    }

    /// Forget buffer `number` and all its labels while retaining the panes themselves.
    pub(in crate::grid::workspace) fn remove_session_buffer(
        &mut self,
        number: u64,
        _now: f64,
    ) -> bool {
        let mut removed = self.terminal_buffers.remove(&number).is_some();
        removed |= self.detached_buffers.remove(&number).is_some();
        removed |= self.editor.remove_session_buffer(number);
        for editor in self.inactive_editors.values_mut() {
            removed |= editor.remove_session_buffer(number);
        }
        removed
    }

    /// Open `path` in the current pane and share contents with any existing Session view.
    pub(in crate::grid::workspace) fn open_editor_file(
        &mut self,
        path: PathBuf,
        now: f64,
    ) -> anyhow::Result<()> {
        self.editor.refresh_shared_documents();
        let path = path.canonicalize().unwrap_or(path);
        self.load_detached_path(&path)?;
        let view = shared_view(self.inactive_editors.values(), None, Some(&path))
            .or_else(|| detached_view(self.detached_buffers.values(), None, Some(&path)));
        self.editor.open_shared(path, view, now)
    }

    /// Create a view for an already open file at canonical `path`.
    pub(super) fn shared_file_view(&self, path: &Path) -> Option<Buffer> {
        shared_view(self.editors().map(|(_, editor)| editor), None, Some(path))
            .or_else(|| detached_view(self.detached_buffers.values(), None, Some(path)))
    }

    /// Navigate in the active pane using the same Session's open file contents.
    pub(in crate::grid::workspace) fn jump_file(
        &mut self,
        path: &Path,
        require_file: bool,
        now: f64,
        resolve: impl FnOnce(&Document) -> Option<usize>,
    ) -> anyhow::Result<bool> {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_owned());
        self.load_detached_path(&canonical)?;
        let peers = &self.inactive_editors;
        let detached = &self.detached_buffers;
        self.editor
            .jump_file(path, require_file, now, resolve, |opening, path| {
                shared_view(peers.values(), opening, path)
                    .or_else(|| detached_view(detached.values(), opening, path))
            })
    }

    /// Follow a file mark at `location` without leaving its originating editing pane.
    pub(in crate::grid::workspace) fn jump_file_mark(
        &mut self,
        name: char,
        file: (u64, Option<&Path>),
        location: (usize, usize),
        linewise: bool,
        now: f64,
    ) -> anyhow::Result<()> {
        let peers = &self.inactive_editors;
        let detached = &self.detached_buffers;
        self.editor
            .jump_file_mark(name, file, location, linewise, now, |opening, path| {
                shared_view(peers.values(), opening, path)
                    .or_else(|| detached_view(detached.values(), opening, path))
            })
    }

    /// Walk the active pane's history, reusing an open buffer even if its local label was closed.
    pub(in crate::grid::workspace) fn visit_editor_jump(
        &mut self,
        forward: bool,
        count: usize,
        now: f64,
    ) -> bool {
        let peers = &self.inactive_editors;
        let detached = &self.detached_buffers;
        self.editor
            .visit_jump(forward, count, now, |opening, path| {
                shared_view(peers.values(), opening, path)
                    .or_else(|| detached_view(detached.values(), opening, path))
            })
    }

    /// Materialize the latest shared edits in the panes before layout, rendering and close checks.
    pub(in crate::grid::workspace) fn refresh_shared_documents(&mut self) {
        self.editor.refresh_shared_documents();
        for entry in self
            .detached_buffers
            .values_mut()
            .filter(|entry| entry.loaded)
        {
            entry.view.vim.refresh_shared_document();
        }
        for editor in self.inactive_editors.values_mut() {
            editor.refresh_shared_documents();
        }
    }

    /// Share restored contents by canonical path while retaining each pane's saved position.
    pub(super) fn join_restored_buffers(&mut self) {
        let mut owners = HashMap::new();
        self.editor.join_restored_buffers(&mut owners);
        for editor in self.inactive_editors.values_mut() {
            editor.join_restored_buffers(&mut owners);
        }
    }
}

/// Resolve a detached loaded view without creating a label for it.
fn detached_view<'a>(
    entries: impl Iterator<Item = &'a DetachedBuffer>,
    opening: Option<u64>,
    path: Option<&Path>,
) -> Option<Buffer> {
    entries
        .filter(|entry| entry.loaded)
        .find(|entry| {
            let doc = &entry.view.vim.doc;
            opening == Some(doc.buffer_number())
                || path.is_some_and(|path| Some(path) == doc.path.as_deref())
        })
        .map(|entry| entry.view.shared_view())
}

/// Resolve a retained view by `opening` or canonical `path` without changing window focus.
fn shared_view<'a>(
    editors: impl Iterator<Item = &'a EditorWindow>,
    opening: Option<u64>,
    path: Option<&Path>,
) -> Option<Buffer> {
    editors
        .flat_map(EditorWindow::buffers)
        .find(|buffer| {
            let doc = &buffer.vim.doc;
            buffer.supports_shared_view()
                && (opening == Some(doc.opening)
                    || opening == Some(doc.buffer_number())
                    || path.is_some_and(|path| doc.path.as_deref() == Some(path)))
        })
        .map(Buffer::shared_view)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nanobug::vim::Action;

    /// Keep a listed file lazy, retain its hidden draft, and reopen it from disk after unloading.
    #[test]
    fn detached_buffers_preserve_numbers_and_keep_hidden_drafts() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("listed.txt");
        let mut project = Project::new(directory.path().to_owned(), None);
        let initial = project.editor.buffer().id;
        let number = project.list_unloaded_file(path.clone(), Some(2));
        assert!(!project.session_buffer_loaded(number));
        assert_eq!(project.editor.buffer().id, initial);
        assert_eq!(project.number_for_path(&path), Some(number));
        std::fs::write(&path, "first\nsecond\n")?;
        assert!(project.select_session_buffer(number, 0.));
        assert_eq!(project.editor.vim().doc.cursor, "first\n".len());
        let active = project.editor.buffer().id;
        project.editor.edit(|vim, _| {
            vim.doc.replace(0..5, "draft", true);
            Action::None
        });
        assert!(project.editor.select_buffer(initial, 0.));
        project.detach_active_label(active, 0.);
        assert!(project.detached_documents().any(|doc| doc.modified()));
        assert!(project.select_session_buffer(number, 0.));
        assert!(project.editor.vim().doc.text.starts_with("draft"));
        assert!(project.unload_session_buffer(number, 0.));
        assert!(!project.session_buffer_loaded(number));
        assert_eq!(project.editor.buffer().id, initial);
        std::fs::write(&path, "updated on disk\n")?;
        assert!(project.select_session_buffer(number, 0.));
        assert_eq!(project.editor.vim().doc.text, "updated on disk\n");
        assert_eq!(project.editor.vim().doc.buffer_number(), number);
        project.wipe_session_buffer(number, 0.);
        assert!(project.session_buffer(number).is_none());
        assert_eq!(std::fs::read_to_string(path)?, "updated on disk\n");
        Ok(())
    }
}
