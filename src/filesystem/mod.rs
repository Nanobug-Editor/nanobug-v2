//! Shared directory reads and reversible file transfers, independent of native windows.

mod listing;
mod transfer;

pub use listing::{DirectoryEntry, DirectoryOptions, read_directory};
use std::path::PathBuf;
pub use transfer::{FileTransfers, TransferKind, move_exclusive};

/// A visible filesystem change that open documents and listings must reconcile.
pub struct PathChange {
    /// Previous path, or a created path whose cached document is stale.
    pub from: PathBuf,
    /// Destination after a move; None closes stale documents at the previous path.
    pub to: Option<PathBuf>,
}
