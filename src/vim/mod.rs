//! Native modal editing. Documents own text; the host applies the actions returned by Vim.
//! Mode handling, motions, operators and commands share one per-buffer state.

mod commands;
mod input;
mod mode;
mod motions;
mod operators;
mod register;

pub use commands::Action;
pub use mode::Mode;
pub use register::Register;

use crate::document::Document;
use std::ops::Range;

/// One buffer's modal state and text; the shell owns the shared register and file tree.
pub struct Vim {
    /// Editable document.
    pub doc: Document,
    /// Current editing mode.
    pub mode: Mode,
    /// Current Ex command or literal search query, without its prefix.
    pub command: String,
    /// Last feedback shown in the status line.
    pub message: String,
    /// Visual selection anchor in UTF-8 bytes.
    pub anchor: usize,
    /// Pending motion/operator, such as `d` or `g`.
    pending: String,
    /// Count typed before the next operation.
    count: String,
    /// Count captured before a pending operator.
    operator_count: usize,
    /// Whether the active insert session has captured an undo checkpoint.
    insert_checkpointed: bool,
    /// Desired display column retained during vertical movement.
    desired_column: Option<usize>,
    /// Last accepted literal search pattern.
    last_search: String,
}

impl Vim {
    /// Wrap `doc` in normal mode at its existing cursor.
    pub fn new(doc: Document) -> Self {
        Self {
            doc,
            mode: Mode::Normal,
            command: String::new(),
            message: String::new(),
            anchor: 0,
            pending: String::new(),
            count: String::new(),
            operator_count: 1,
            insert_checkpointed: false,
            desired_column: None,
            last_search: String::new(),
        }
    }

    /// Return the inclusive visual selection as a half-open UTF-8 interval.
    pub fn selection(&self) -> Option<Range<usize>> {
        let a = self.anchor.min(self.doc.cursor);
        let b = self.anchor.max(self.doc.cursor);
        match self.mode {
            Mode::Visual => Some(a..self.doc.next(b)),
            Mode::VisualLine => {
                let first = self.doc.location(a).0;
                let last = self.doc.location(b).0;
                Some(
                    self.doc.starts[first]
                        ..self
                            .doc
                            .starts
                            .get(last + 1)
                            .copied()
                            .unwrap_or(self.doc.text.len()),
                )
            }
            _ => None,
        }
    }
}
