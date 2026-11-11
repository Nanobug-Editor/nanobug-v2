//! File-manager window state, filesystem navigation, icon associations and reversible transfers.

mod commands;
mod icons;
mod input;
mod pointer;
pub(in crate::grid) mod shortcuts;
mod transfer;
mod tree;
mod view;

pub(in crate::grid) use icons::EntryIcon;
pub(in crate::grid) use input::ManagerCommand;
pub(in crate::grid) use transfer::{FileTransfers, TransferKind, move_exclusive};

use super::PathChange;

use gpui::Task;
use nanobug::animation::scroll::GridScroll;
use std::path::PathBuf;
pub(in crate::grid) use tree::{Entry, Explorer};

/// The manager window owns filesystem selection, transfer history and scrolling.
pub(in crate::grid) struct ManagerWindow {
    /// Lazily expanded tree rooted at the current project.
    pub(in crate::grid) tree: Explorer,
    /// Clipboard and undo history for filesystem pastes.
    pub(in crate::grid) transfers: FileTransfers,
    /// Continuous vertical viewport position.
    pub(in crate::grid) scroll: GridScroll,
    /// Repeat count accumulated before a tree motion.
    pub(in crate::grid) count: usize,
    /// Delayed standalone undo, canceled when a second prefix key arrives.
    pub(in crate::grid) paste_timer: Option<Task<()>>,
    /// First pressed path used to recognize double-click activation.
    pub(in crate::grid) click: Option<PathBuf>,
}

impl ManagerWindow {
    /// Read the initial tree for `project`, with an empty clipboard and transfer history.
    pub(in crate::grid) fn new(project: PathBuf) -> Self {
        Self {
            tree: Explorer::new(project),
            transfers: FileTransfers::default(),
            scroll: GridScroll::default(),
            count: 0,
            paste_timer: None,
            click: None,
        }
    }
}
