//! Filesystem navigation, icon associations and reversible copy/cut/paste operations.
//! Workspace input routing lives in `app::workspace`; these modules own Explorer data.

mod icons;
mod transfer;
mod tree;

pub(crate) use icons::{EntryIcon, ExplorerIconCache};
pub(crate) use transfer::{FileTransfers, PathChange, TransferKind};
pub(crate) use tree::Explorer;
