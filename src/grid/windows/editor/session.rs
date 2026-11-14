//! Window-owned buffer identity, native input state and view lifecycle.

use super::{Buffer, EditorWindow};
use crate::ui::ime::Composition;

impl EditorWindow {
    /// Inspect all canonical buffers, including those retained by other pages.
    pub(in crate::grid) fn buffers(&self) -> &[Buffer] {
        &self.buffers
    }

    /// Index of the buffer currently presented by this window.
    pub(in crate::grid) fn active_index(&self) -> usize {
        self.active
    }

    /// Inspect the displayed buffer and its retained viewport.
    pub(in crate::grid) fn buffer(&self) -> &Buffer {
        &self.buffers[self.active]
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

    /// Navigate this page's buffers in `forward` order or select the `edge`.
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
            (self.visible_buffer_index() + if forward { distance } else { len - distance }) % len
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
