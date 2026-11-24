//! File-manager navigation and interaction around shared filesystem transfers.

mod commands;
mod input;
mod pointer;
mod session;
pub(in crate::grid) mod shortcuts;
mod tree;
mod view;

pub(in crate::grid) use input::ManagerCommand;
use nanobug::filesystem::{FileTransfers, TransferKind};
pub(in crate::grid) use session::ExplorerSnapshot;

use super::PathChange;

use gpui::Task;
use nanobug::animation::scroll::GridScroll;
use std::path::PathBuf;
pub(in crate::grid) use tree::{Entry, Explorer};

/// The manager window owns filesystem selection, transfer history and scrolling.
pub(in crate::grid) struct ManagerWindow {
    /// Lazily expanded tree rooted at the current project.
    tree: Explorer,
    /// Clipboard and undo history for filesystem pastes.
    transfers: FileTransfers,
    /// Continuous vertical viewport position.
    scroll: GridScroll,
    /// Repeat count accumulated before a tree motion.
    count: usize,
    /// Delayed standalone undo, canceled when a second prefix key arrives.
    paste_timer: Option<Task<()>>,
    /// First pressed path used to recognize double-click activation.
    click: Option<PathBuf>,
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
