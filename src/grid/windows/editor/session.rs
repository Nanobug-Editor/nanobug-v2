//! Window-owned buffer identity, native input state and view lifecycle.

use super::{Buffer, EditorWindow};
use crate::{grid::windows::terminal::Terminal, ui::ime::Composition};

impl EditorWindow {
    /// Use the current layout's Tabpage script variables for this editor.
    pub(in crate::grid) fn set_script_tab_scope(&mut self, scope: nanobug::vim::ScriptTabScope) {
        self.register.set_script_tab_scope(scope);
    }
    /// Remove window-local navigation references to a wiped buffer.
    pub(in crate::grid) fn forget_buffer_positions(
        &mut self,
        number: u64,
        path: Option<&std::path::Path>,
    ) {
        let openings = std::iter::once(number)
            .chain(self.buffers.iter().filter_map(|buffer| {
                (buffer.vim.doc.buffer_number() == number).then_some(buffer.vim.doc.opening)
            }))
            .collect::<Vec<_>>();
        self.jump_history.forget_buffer(&openings, path);
        if path.is_some_and(|path| self.alternate_path.as_deref() == Some(path)) {
            self.alternate_path = None;
        }
    }

    /// Share workspace input definitions `mappings` between this pane's buffers.
    pub(in crate::grid) fn set_input_mappings(&mut self, mappings: nanobug::vim::InputMappings) {
        if self.register.has_input_mappings(&mappings) {
            return;
        }
        self.register.set_input_mappings(mappings);
        for buffer in &mut self.buffers {
            self.register.initialize_buffer_options(&mut buffer.vim);
        }
    }

    /// Read the local display options for this pane.
    pub(in crate::grid) fn window_options(&self) -> nanobug::vim::WindowOptions {
        self.vim().view_options(&self.register)
    }

    /// Copy current display options from the window that was split.
    pub(in crate::grid) fn inherit_window_options(&mut self, source: &Self) {
        let mut defaults = source.register.window_options();
        let mut options = source.window_options();
        if self.vim().doc.buffer_number() != source.vim().doc.buffer_number() {
            defaults.scrollbind = false;
            options.scrollbind = false;
        }
        self.register.set_window_options(defaults);
        self.buffers[self.active].vim.set_view_options(options);
    }

    /// Query the active terminal buffer, when the current editor displays one.
    pub(in crate::grid) fn terminal(&self) -> Option<&Terminal> {
        self.buffer().terminal()
    }

    /// Route an operation to the currently displayed terminal.
    pub(in crate::grid) fn terminal_mut(&mut self) -> Option<&mut Terminal> {
        self.buffers[self.active].terminal.as_mut()
    }

    /// Inspect all buffers owned by this window.
    pub(in crate::grid) fn buffers(&self) -> &[Buffer] {
        &self.buffers
    }

    /// Enumerate this window's file labels in display order.
    pub(in crate::grid) fn visible_buffers(&self) -> std::slice::Iter<'_, Buffer> {
        self.buffers.iter()
    }

    /// Resolve a file label's stable buffer identity to this window's buffer index.
    pub(in crate::grid) fn buffer_index(&self, id: u64) -> Option<usize> {
        self.buffers.iter().position(|buffer| buffer.id == id)
    }

    /// Return the previous buffer in this editor, when it can still be selected.
    pub(in crate::grid) fn alternate_buffer_id(&self) -> Option<u64> {
        self.alternate
    }

    /// Return the alternate filename from a rename or the retained alternate label.
    pub(in crate::grid) fn alternate_path(&self) -> Option<&std::path::Path> {
        self.alternate_path.as_deref().or_else(|| {
            let id = self.alternate_buffer_id()?;
            self.buffers[self.buffer_index(id)?].vim.doc.path.as_deref()
        })
    }

    /// Remember a prior name without adding a visible buffer label.
    pub(in crate::grid) fn remember_alternate_path(&mut self, path: std::path::PathBuf) {
        let doc = &self.buffers[self.active].vim.doc;
        self.jump_history
            .update_path(doc.opening, doc.path.as_deref());
        if !nanobug::vim::ex_keeps_alternate() {
            self.alternate = None;
            self.alternate_path = Some(path);
        }
    }

    /// Select a file by its label, retaining the outgoing buffer's viewport.
    pub(in crate::grid) fn select_buffer(&mut self, id: u64, _now: f64) -> bool {
        let Some(index) = self.buffer_index(id).filter(|&index| index != self.active) else {
            return false;
        };
        self.cancel_pending();
        if !nanobug::vim::ex_keeps_alternate() {
            self.alternate = Some(self.buffer().id);
            self.alternate_path = None;
        }
        self.active = index;
        self.reset_buffer_input();
        true
    }

    /// Cancel pointer and native composition state when changing buffers or panes.
    pub(super) fn reset_buffer_input(&mut self) {
        self.composition.clear();
        self.dragging = None;
        self.reset_wheel();
    }

    /// Settle the current input sequence before leaving the editor or checking for unsaved changes.
    pub(in crate::grid) fn finish_buffer_input(&mut self) -> super::EditResult {
        self.reset_buffer_input();
        if let Some(terminal) = self.terminal_mut() {
            terminal.clear_prefix();
            terminal.enter_normal();
        }
        self.edit(|vim, register| {
            if vim.mode == nanobug::vim::Mode::Normal {
                vim.cancel_pending();
                nanobug::vim::Action::None
            } else {
                vim.key("Escape", register)
            }
        })
    }

    /// Index of the buffer currently presented by this window.
    pub(in crate::grid) fn active_index(&self) -> usize {
        self.active
    }

    /// Inspect the displayed buffer and its retained viewport.
    pub(in crate::grid) fn buffer(&self) -> &Buffer {
        &self.buffers[self.active]
    }

    /// Override the SQL dialect for the selected buffer without touching its text.
    pub(in crate::grid) fn set_sql_dialect(&mut self, dialect: Option<String>) {
        self.buffers[self.active].set_sql_dialect(dialect);
    }

    /// Native input routes use this preedit without changing committed document text.
    pub(in crate::grid) fn composition(&self) -> &Composition {
        &self.composition
    }

    /// Native input routes replace or cancel the current preedit through this composition.
    pub(in crate::grid) fn composition_mut(&mut self) -> &mut Composition {
        &mut self.composition
    }

    /// Inspect the shared register to publish a completed yank to the native clipboard.
    pub(in crate::grid) fn register(&self) -> &nanobug::vim::Register {
        &self.register
    }

    /// Attach `marks` from this Session to each named buffer already retained in the pane.
    pub(in crate::grid) fn set_file_marks(&mut self, marks: nanobug::vim::FileMarks) {
        self.register.set_file_marks(marks);
        for buffer in self
            .buffers
            .iter()
            .filter(|buffer| buffer.supports_shared_view())
        {
            self.register.bind_file_marks(&buffer.vim.doc);
        }
    }

    /// Update the pane's numbered file marks from the workspace-wide history.
    pub(in crate::grid) fn set_numbered_marks(
        &mut self,
        marks: std::sync::Arc<[nanobug::app_data::numbered_marks::Mark]>,
    ) {
        self.register.set_numbered_marks(marks);
    }

    /// Cancel an incomplete command before another window receives input.
    pub(in crate::grid) fn cancel_pending(&mut self) {
        if let Some(terminal) = self.terminal_mut() {
            terminal.clear_prefix();
            terminal.enter_normal();
        }
        self.buffers[self.active].vim.cancel_pending();
    }

    /// Hand a standalone normal-mode count to the workspace window-command parser.
    pub(in crate::grid) fn take_window_count(&mut self) -> Option<Option<usize>> {
        self.buffers[self.active].vim.take_window_count()
    }

    /// End a Visual selection when a window command changes its owner.
    pub(in crate::grid) fn end_window_selection(&mut self) {
        let vim = &mut self.buffers[self.active].vim;
        if vim.mode.is_visual() {
            vim.key("Escape", &mut self.register);
        }
    }

    /// Stop pointer selection and report whether this window owned a drag.
    pub(in crate::grid) fn stop_drag(&mut self) -> bool {
        self.dragging.take().is_some()
    }

    /// Whether a pointer selection started in this editor is still active.
    pub(in crate::grid) fn is_dragging(&self) -> bool {
        self.dragging.is_some()
    }

    /// Discard fractional horizontal wheel movement when the window loses its input session.
    pub(in crate::grid) fn reset_wheel(&mut self) {
        self.horizontal_wheel = 0.;
    }

    /// Accumulate wheel movement measured in columns and return the completed steps.
    pub(in crate::grid) fn wheel_columns(&mut self, delta: f32) -> isize {
        self.horizontal_wheel += delta;
        let columns = self.horizontal_wheel.trunc() as isize;
        self.horizontal_wheel -= columns as f32;
        columns
    }

    /// Clear formatted feedback after the application changes locale.
    pub(in crate::grid) fn clear_messages(&mut self) {
        for buffer in &mut self.buffers {
            buffer.vim.clear_ex_message();
        }
    }

    /// Move to `offset` after a validated definition or diagnostic jump.
    pub(in crate::grid) fn jump_to(&mut self, offset: usize) {
        let buffer = &mut self.buffers[self.active];
        buffer.vim.jump_to(offset);
    }

    /// Navigate this window's buffers in `forward` order or select the `edge`.
    /// An explicit `count` on an edge command selects a one-based buffer index.
    pub(in crate::grid) fn navigate_buffers(
        &mut self,
        forward: bool,
        edge: bool,
        count: Option<usize>,
    ) -> bool {
        let len = self.visible_buffers().count();
        let index = if edge {
            let default = if forward { len } else { 1 };
            let index = count.unwrap_or(default).saturating_sub(1);
            if index >= len {
                return false;
            }
            index
        } else {
            let distance = count.unwrap_or(1) % len;
            (self.active + if forward { distance } else { len - distance }) % len
        };
        let id = self.visible_buffers().nth(index).unwrap().id;
        self.select_buffer(id, 0.);
        true
    }

    /// Toggle the nearest fold of the displayed buffer.
    pub(in crate::grid) fn toggle_fold(&mut self) -> bool {
        self.buffers[self.active].toggle_fold()
    }
}

#[cfg(test)]
impl EditorWindow {
    /// Arrange the active native-renderer fixture without exposing mutable buffers in the app.
    pub(in crate::grid) fn buffer_mut(&mut self) -> &mut Buffer {
        &mut self.buffers[self.active]
    }
    /// Arrange retained-buffer fixtures without permitting changes to collection length.
    pub(in crate::grid) fn test_buffers_mut(&mut self) -> &mut [Buffer] {
        &mut self.buffers
    }
}
