//! Editing window state: retained buffers, modal input and transient native composition.

mod buffer;
mod commands;
mod completion;
mod header;
mod hover;
mod input;
mod markdown;
mod pointer;
mod presentation;
mod project_session;
mod session;
pub(in crate::grid) mod shortcuts;
mod transactions;
mod view;
mod viewport;
pub(in crate::grid) use transactions::{EditReceipt, TextChange, TextTarget};

pub(in crate::grid) use buffer::{Buffer, TEXT_OVERSCAN};
pub(in crate::grid) use completion::CompletionMenu;
pub(in crate::grid) use header::HeaderTarget;
pub(in crate::grid) use hover::{DefinitionHoverContent, DefinitionHoverPopup};
pub(in crate::grid) use input::EditResult;
pub(in crate::grid) use markdown::{MarkdownCache, MarkdownPreview, MarkdownViewMode};
pub(in crate::grid) use project_session::EditorSnapshot;

use crate::ui::ime::Composition;
use gpui::{Pixels, Point};
use nanobug::{
    document::Document,
    vim::{Register, Vim},
};

/// One editor pane owns its buffers, input state and viewports.
pub(in crate::grid) struct EditorWindow {
    /// Buffers shown in order in this window's file labels.
    buffers: Vec<Buffer>,
    /// Buffer displayed in this window.
    active: usize,
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
        Self {
            buffers: vec![buffer],
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
