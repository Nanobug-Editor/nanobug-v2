//! Editing window state: retained buffers, modal input and transient native composition.

mod buffer;
mod commands;
mod input;
mod pointer;
pub(in crate::grid) mod shortcuts;
mod view;

pub(in crate::grid) use buffer::{Buffer, TEXT_OVERSCAN};
pub(in crate::grid) use input::EditResult;

use crate::ui::ime::Composition;
use gpui::{Pixels, Point};
use nanobug::{
    animation::scroll::GridScroll,
    document::Document,
    vim::{Register, Vim},
};

/// The editor window owns document state independently of workspace focus and layout.
pub(in crate::grid) struct EditorWindow {
    /// Open buffers retain unsaved text and their viewport positions.
    pub(in crate::grid) buffers: Vec<Buffer>,
    /// Buffer displayed in this window.
    pub(in crate::grid) active: usize,
    /// Unnamed yank/delete register shared across the window's buffers.
    pub(in crate::grid) register: Register,
    /// Native preedit text, committed to the document only once.
    pub(in crate::grid) composition: Composition,
    /// Selection anchor and original pointer position for drag-jitter rejection.
    pub(in crate::grid) dragging: Option<(usize, Point<Pixels>)>,
    /// Fractional horizontal wheel steps.
    pub(in crate::grid) horizontal_wheel: f32,
}

impl EditorWindow {
    /// Create a window around `doc`, initially showing one buffer in Normal mode.
    pub(in crate::grid) fn new(doc: Document) -> Self {
        Self {
            buffers: vec![Buffer {
                vim: Vim::new(doc),
                scroll: GridScroll::default(),
                columns: GridScroll::default(),
            }],
            active: 0,
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
