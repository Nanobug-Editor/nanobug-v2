//! Project-local buffer lists referencing the editor's shared document pool.

use super::{Buffer, EditResult, EditorWindow};
use nanobug::{document::Document, vim::Mode};
use std::path::PathBuf;

/// One buffer's viewport on a particular page.
#[derive(Clone, Copy, Default)]
struct BufferView {
    /// Zero-based document line and display column, safe to resolve after text changes.
    caret: (usize, usize),
    /// First visible display row.
    top: usize,
    /// First visible grid column.
    left: usize,
}

impl BufferView {
    /// Capture the caret and scroll positions currently materialized in `buffer`.
    fn read(buffer: &Buffer) -> Self {
        let doc = &buffer.vim.doc;
        Self {
            caret: doc.location(doc.cursor),
            top: buffer.scroll.first_cell(),
            left: buffer.columns.first_cell(),
        }
    }

    /// Materialize the retained position in `buffer` at animation time `now`.
    fn install(self, buffer: &mut Buffer, now: f64) {
        let doc = &buffer.vim.doc;
        let offset = doc.offset(self.caret.0, self.caret.1);
        let row = self.caret.0.min(doc.starts.len() - 1);
        let offset = doc
            .collapsed_body_at(row)
            .map_or(offset, |hidden| doc.starts[hidden.start - 1]);
        buffer.vim.set_cursor(offset);
        buffer.scroll.snap(self.top, now);
        buffer.columns.snap(self.left, now);
    }
}

/// One page-local file label and its saved view.
struct Entry {
    /// Identity of the canonical buffer in the editor.
    id: u64,
    /// Last view left on this page.
    view: BufferView,
}

/// A page's ordered labels and selected buffer.
struct Page {
    /// The order shown in the page's bufferline.
    entries: Vec<Entry>,
    /// Identity of the page's selected buffer.
    active: u64,
}

impl Page {
    /// Start a page with a single buffer `id` displaying `view`.
    fn new(id: u64, view: BufferView) -> Self {
        Self {
            entries: vec![Entry { id, view }],
            active: id,
        }
    }

    /// Find the label position for buffer `id`, if this page displays it.
    fn index(&self, id: u64) -> Option<usize> {
        self.entries.iter().position(|entry| entry.id == id)
    }

    /// Remove buffer `id`, selecting its neighbor if it was active and the page still has labels.
    fn remove(&mut self, id: u64) {
        if let Some(index) = self.index(id) {
            self.entries.remove(index);
            if self.active == id
                && let Some(entry) = self.entries.get(index).or_else(|| self.entries.last())
            {
                self.active = entry.id;
            }
        }
    }
}

/// Ordered pages and their current and previous selections within one project editor.
pub(super) struct Tabpages {
    /// All pages in the order shown in the header.
    pages: Vec<Page>,
    /// Position of the page whose active view is live in the buffer pool.
    active: usize,
    /// Previous page, if it still exists.
    previous: Option<usize>,
}

impl Tabpages {
    /// Start with one page showing buffer `id`.
    pub(super) fn new(id: u64) -> Self {
        Self {
            pages: vec![Page::new(id, BufferView::default())],
            active: 0,
            previous: None,
        }
    }

    /// Read the selected page.
    fn current(&self) -> &Page {
        &self.pages[self.active]
    }

    /// Mutate the selected page's label order or saved views.
    fn current_mut(&mut self) -> &mut Page {
        &mut self.pages[self.active]
    }
}

impl EditorWindow {
    /// Enumerate the active page's labels in display order, borrowing their shared buffers.
    pub(in crate::grid) fn visible_buffers(&self) -> impl DoubleEndedIterator<Item = &Buffer> {
        self.tabpages.current().entries.iter().filter_map(|entry| {
            self.buffer_index(entry.id)
                .map(|index| &self.buffers[index])
        })
    }

    /// Find canonical storage for buffer `id`, independent of the selected page.
    pub(in crate::grid) fn buffer_index(&self, id: u64) -> Option<usize> {
        self.buffers.iter().position(|buffer| buffer.id == id)
    }

    /// Count the pages in this project editor.
    pub(in crate::grid) fn tabpage_count(&self) -> usize {
        self.tabpages.pages.len()
    }

    /// Zero-based position of the selected page.
    pub(in crate::grid) fn active_tabpage(&self) -> usize {
        self.tabpages.active
    }

    /// Return the previously selected page, provided it still exists.
    pub(in crate::grid) fn previous_tabpage(&self) -> Option<usize> {
        self.tabpages.previous
    }

    /// Select page `index` at animation time `now`, returning whether it changed.
    pub(in crate::grid) fn select_tabpage(&mut self, index: usize, now: f64) -> bool {
        let previous = self.tabpages.active;
        if index == previous || index >= self.tabpages.pages.len() {
            return false;
        }
        self.retain_current_view();
        self.tabpages.previous = Some(previous);
        self.tabpages.active = index;
        self.show_current_view(now);
        true
    }

    /// Insert and select a page showing a fresh view of the current buffer at animation time `now`.
    pub(in crate::grid) fn add_tabpage(&mut self, now: f64) {
        self.insert_tabpage(self.active, now);
    }

    /// Insert a page showing `path` at animation time `now` once opening the file succeeds.
    pub(in crate::grid) fn open_tabpage(&mut self, path: PathBuf, now: f64) -> anyhow::Result<()> {
        let index = self.open_buffer(path)?;
        self.insert_tabpage(index, now);
        Ok(())
    }

    /// Insert and activate the page displaying canonical buffer `index` at animation time `now`.
    fn insert_tabpage(&mut self, index: usize, now: f64) {
        self.retain_current_view();
        let previous = self.tabpages.active;
        let buffer = &self.buffers[index];
        self.tabpages.active += 1;
        self.tabpages.previous = Some(previous);
        self.tabpages.pages.insert(
            self.tabpages.active,
            Page::new(buffer.id, BufferView::read(buffer)),
        );
        self.show_current_view(now);
    }

    /// Close the active page at animation time `now`, keeping at least one page.
    /// Move unsaved views whose final label is gone to the newly selected page.
    pub(in crate::grid) fn remove_tabpage(&mut self, now: f64) -> bool {
        if self.tabpages.pages.len() == 1 {
            return false;
        }
        self.retain_current_view();
        let closed = self.tabpages.active;
        let page = self.tabpages.pages.remove(closed);
        self.tabpages.active = closed.min(self.tabpages.pages.len() - 1);
        self.tabpages.previous = self
            .tabpages
            .previous
            .filter(|&index| index != closed)
            .map(|index| index - usize::from(index > closed))
            .filter(|&index| index != self.tabpages.active);
        for entry in page.entries {
            if self.buffer_index(entry.id).is_some_and(|index| {
                self.buffers[index].vim.doc.modified()
                    && self
                        .tabpages
                        .pages
                        .iter()
                        .all(|page| page.index(entry.id).is_none())
            }) {
                self.tabpages.current_mut().entries.push(entry);
            }
        }
        self.show_current_view(now);
        true
    }

    /// Select buffer `id` on the current page at animation time `now`, if the label exists.
    pub(in crate::grid) fn select_buffer(&mut self, id: u64, now: f64) -> bool {
        let page = self.tabpages.current();
        if id == page.active || page.index(id).is_none() {
            return false;
        }
        self.retain_current_view();
        self.tabpages.current_mut().active = id;
        self.show_current_view(now);
        true
    }

    /// Check whether another page would retain buffer `id` after the active page removes its label.
    pub(in crate::grid) fn buffer_in_other_tabpage(&self, id: u64) -> bool {
        self.tabpages
            .pages
            .iter()
            .enumerate()
            .any(|(index, page)| index != self.tabpages.active && page.index(id).is_some())
    }

    /// Remove buffer `id` from this page at animation time `now`.
    /// The caller resolves unsaved changes before removing the last reference.
    pub(in crate::grid) fn remove_buffer_tab(&mut self, id: u64, now: f64) -> bool {
        if self.tabpages.current().index(id).is_none() {
            return false;
        }
        let was_active = self.buffer().id;
        self.tabpages.current_mut().remove(id);
        if !self.buffer_in_other_tabpage(id)
            && let Some(index) = self.buffer_index(id)
        {
            self.buffers.remove(index);
        }
        self.reconcile_tabpage_buffers(was_active, now);
        true
    }

    /// Settle the current input sequence before switching or checking for unsaved changes.
    pub(in crate::grid) fn finish_buffer_input(&mut self) -> EditResult {
        self.composition.clear();
        self.dragging = None;
        self.reset_wheel();
        self.edit(|vim, register| {
            if vim.mode == Mode::Normal {
                vim.cancel_pending();
                nanobug::vim::Action::None
            } else {
                vim.key("Escape", register)
            }
        })
    }

    /// Add canonical buffer `index` to this page, preserving an existing page-local view.
    pub(super) fn attach_buffer(&mut self, index: usize) {
        let buffer = &self.buffers[index];
        let page = self.tabpages.current_mut();
        if page.index(buffer.id).is_none() {
            page.entries.push(Entry {
                id: buffer.id,
                view: BufferView::read(buffer),
            });
        }
    }

    /// Reset every retained view after buffer `id` is reset to its saved text.
    pub(super) fn clear_buffer_views(&mut self, id: u64) {
        for page in &mut self.tabpages.pages {
            if let Some(index) = page.index(id) {
                page.entries[index].view = BufferView::default();
            }
        }
    }

    /// Find the selected page's active label's position within the ordered labels.
    pub(super) fn visible_buffer_index(&self) -> usize {
        let page = self.tabpages.current();
        page.index(page.active)
            .expect("selected buffer is on its page")
    }

    /// Prune stale labels after modifying the canonical buffer pool.
    /// Preserve the view if buffer `was_active` survives, or install its successor at time `now`.
    pub(super) fn reconcile_tabpage_buffers(&mut self, was_active: u64, now: f64) {
        let buffers = &mut self.buffers;
        for page in &mut self.tabpages.pages {
            let active_index = page.index(page.active).unwrap_or_default();
            page.entries
                .retain(|entry| buffers.iter().any(|buffer| buffer.id == entry.id));
            if page.entries.is_empty() {
                let blank = Buffer::new(Document::new(""));
                page.entries.push(Entry {
                    id: blank.id,
                    view: BufferView::default(),
                });
                buffers.push(blank);
            }
            if page.index(page.active).is_none() {
                page.active = page.entries[active_index.min(page.entries.len() - 1)].id;
            }
        }
        let id = self.tabpages.current().active;
        if was_active == id {
            self.active = self.buffer_index(id).expect("selected buffer is retained");
        } else {
            self.show_current_view(now);
        }
    }

    /// Save the outgoing buffer's page-local view before switching.
    fn retain_current_view(&mut self) {
        let buffer = &mut self.buffers[self.active];
        buffer.vim.cancel_pending();
        let page = self.tabpages.current_mut();
        let index = page
            .index(buffer.id)
            .expect("selected buffer is on its page");
        page.entries[index].view = BufferView::read(buffer);
    }

    /// Install the selected page's active view at animation time `now`.
    fn show_current_view(&mut self, now: f64) {
        let page = self.tabpages.current();
        let id = page.active;
        let entry = &page.entries[page.index(id).expect("selected buffer is on its page")];
        self.active = self.buffer_index(id).expect("selected buffer is retained");
        entry.view.install(&mut self.buffers[self.active], now);
        self.composition.clear();
        self.dragging = None;
        self.reset_wheel();
    }
}
