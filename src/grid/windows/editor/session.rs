//! Window-owned buffer identity, native input state and view lifecycle.

use super::{Buffer, EditorWindow};
use crate::ui::ime::Composition;

impl EditorWindow {
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

    /// Select a file by its label, retaining the outgoing buffer's viewport.
    pub(in crate::grid) fn select_buffer(&mut self, id: u64, _now: f64) -> bool {
        let Some(index) = self.buffer_index(id).filter(|&index| index != self.active) else {
            return false;
        };
        self.cancel_pending();
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

    /// Cancel an incomplete command before another window receives input.
    pub(in crate::grid) fn cancel_pending(&mut self) {
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
            buffer.vim.message.clear();
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
