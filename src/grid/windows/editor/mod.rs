//! Editing window state: retained buffers, modal input and transient native composition.

mod buffer;
mod commands;
mod header;
mod input;
mod pointer;
mod presentation;
mod session;
pub(in crate::grid) mod shortcuts;
mod tabpages;
mod transactions;
mod view;
mod viewport;
pub(in crate::grid) use transactions::{EditReceipt, TextChange, TextTarget};

pub(in crate::grid) use buffer::{Buffer, TEXT_OVERSCAN};
pub(in crate::grid) use header::HeaderTarget;
pub(in crate::grid) use input::EditResult;

use crate::ui::ime::Composition;
use gpui::{Pixels, Point};
use nanobug::{
    document::Document,
    vim::{Register, Vim},
};

/// The editor window owns shared document state and the editing views for each project page.
pub(in crate::grid) struct EditorWindow {
    /// Canonical buffers shared by all pages, including documents retained without a visible view.
    buffers: Vec<Buffer>,
    /// Buffer displayed in this window.
    active: usize,
    /// Page-local buffer order and views, with the current view materialized in the active buffer.
    tabpages: tabpages::Tabpages,
    /// Text registers and recorded macros shared across the window's buffers.
    register: Register,
    /// Native preedit text, committed to the document only once.
    composition: Composition,
    /// Selection anchor and original pointer position for drag-jitter rejection.
    dragging: Option<(usize, Point<Pixels>)>,
    /// Fractional horizontal wheel steps.
    horizontal_wheel: f32,
}

impl EditorWindow {
    /// Create a window around `doc`, initially showing one buffer in Normal mode.
    pub(in crate::grid) fn new(doc: Document) -> Self {
        Self::with_buffer(Buffer::new(doc))
    }

    /// Display `doc` with the normal editor's read-only Vim operations.
    pub(in crate::grid) fn read_only(doc: Document) -> Self {
        Self::with_buffer(Buffer::with_vim(Vim::read_only(doc)))
    }

    /// Display a retained buffer, including its edits, undo state and viewport.
    pub(in crate::grid) fn with_buffer(buffer: Buffer) -> Self {
        let tabpages = tabpages::Tabpages::new(buffer.id);
        Self {
            buffers: vec![buffer],
            active: 0,
            tabpages,
            register: Register::default(),
            composition: Composition::default(),
            dragging: None,
            horizontal_wheel: 0.,
        }
    }

    /// Return the modal model of the displayed buffer.
    pub(in crate::grid) fn vim(&self) -> &Vim {
        &self.buffers[self.active].vim
    }
}

#[cfg(test)]
mod tests;
