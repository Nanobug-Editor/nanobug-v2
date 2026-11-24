//! Editing window state: retained buffers, modal input and transient native composition.

mod buffer;
mod command_window;
mod commands;
mod completion;
mod diagnostics;
mod directory;
mod ex_matches;
mod header;
mod hover;
mod input;
mod jumplist;
mod markdown;
mod pointer;
mod presentation;
mod project_session;
mod result_list;
mod session;
pub(in crate::grid) mod shortcuts;
mod signs;
mod transactions;
mod undo_file;
mod view;
mod viewport;
mod wrap;
pub(in crate::grid) use transactions::{EditReceipt, TextChange, TextTarget};

pub(in crate::grid) use buffer::{Buffer, TEXT_OVERSCAN};
pub(in crate::grid) use command_window::CommandWindow;
pub(in crate::grid) use completion::CompletionMenu;
pub(in crate::grid) use directory::DirectorySlot;
pub(in crate::grid) use header::HeaderTarget;
pub(in crate::grid) use hover::{DefinitionHoverContent, DefinitionHoverPopup};
pub(in crate::grid) use input::EditResult;
pub(in crate::grid) use markdown::{MarkdownCache, MarkdownPreview, MarkdownViewMode};
pub(in crate::grid) use project_session::EditorSnapshot;
pub(in crate::grid) use result_list::ResultList;
pub(in crate::grid) use view::EditorLayers;

use crate::ui::ime::Composition;
use gpui::{Pixels, Point};
use nanobug::{
    document::Document,
    vim::{JumpHistory, Register, Vim},
};

/// One editor pane owns its buffers, input state and viewports.
pub(in crate::grid) struct EditorWindow {
    /// Buffers shown in order in this window's file labels.
    buffers: Vec<Buffer>,
    /// Buffer displayed in this window.
    active: usize,
    /// Stable ID of the last buffer displayed before the current one.
    alternate: Option<u64>,
    /// Previous name retained by :file without adding a visible buffer label.
    alternate_path: Option<std::path::PathBuf>,
    /// Effective Ex directory override owned by this editing pane.
    directory: DirectorySlot,
    /// Text registers and recorded macros shared across the window's buffers.
    register: Register,
    /// Cursor positions visited in this pane, shared by its retained buffers.
    jump_history: JumpHistory,
    /// Whether the shared workspace diagnostic display is enabled for this pane.
    diagnostic_marks: bool,
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

    /// Display an Ex listing with the styles of its retained messages.
    pub(in crate::grid) fn message_output(output: nanobug::vim::ExMessageOutput) -> Self {
        Self::with_buffer(Buffer::message_output(output))
    }

    /// Display `doc` as a read-only Vim Help buffer in its own editor pane.
    pub(in crate::grid) fn help(doc: Document) -> Self {
        Self::with_buffer(Buffer::with_vim(Vim::help(doc)))
    }

    /// Display a retained buffer, including its edits, undo state and viewport.
    pub(in crate::grid) fn with_buffer(mut buffer: Buffer) -> Self {
        let jump_history = JumpHistory::default();
        buffer.vim.set_jump_history(jump_history.clone());
        Self {
            buffers: vec![buffer],
            active: 0,
            alternate: None,
            alternate_path: None,
            directory: DirectorySlot::default(),
            register: Register::default(),
            jump_history,
            diagnostic_marks: true,
            composition: Composition::default(),
            dragging: None,
            horizontal_wheel: 0.,
        }
    }

    /// Return the modal model of the displayed buffer.
    pub(in crate::grid) fn vim(&self) -> &Vim {
        &self.buffers[self.active].vim
    }

    /// Show or hide diagnostic decorations while keeping the provider data available to navigation.
    pub(in crate::grid) fn set_diagnostic_marks(&mut self, enabled: bool) -> bool {
        std::mem::replace(&mut self.diagnostic_marks, enabled) != enabled
    }

    /// Inspect this window's Ex directory override.
    pub(in crate::grid) fn directory(&self) -> &DirectorySlot {
        &self.directory
    }

    /// Update this window's Ex directory without moving the project or active buffer.
    pub(in crate::grid) fn directory_mut(&mut self) -> &mut DirectorySlot {
        &mut self.directory
    }

    /// Attach `buffer` to this pane's jump history and Session file marks before retention.
    fn push_buffer(&mut self, mut buffer: Buffer) {
        buffer.vim.set_jump_history(self.jump_history.clone());
        self.register.initialize_buffer_options(&mut buffer.vim);
        if buffer.supports_shared_view() {
            self.register.bind_file_marks(&buffer.vim.doc);
        }
        self.buffers.push(buffer);
    }
}

#[cfg(test)]
mod tests;
