//! Display names shared by file-row painting and picker matching.

use std::{borrow::Cow, path::Path};

/// Use the basename of an entry, retaining the display path for a volume root.
pub(crate) fn file_name(path: &Path) -> Cow<'_, str> {
    path.file_name()
        .map_or_else(|| display_path(path), |name| name.to_string_lossy())
}

/// Display Windows drive and UNC roots in their conventional spelling.
pub(crate) fn display_path(path: &Path) -> Cow<'_, str> {
    let text = path.to_string_lossy();
    if cfg!(windows) {
        if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
            return Cow::Owned(format!(r"\\{unc}"));
        }
        if let Some(drive) = text.strip_prefix(r"\\?\") {
            return Cow::Owned(drive.to_owned());
        }
    }
    text
}
