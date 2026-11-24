//! File-row placement, appearance and width-aware filename measurement.

use super::file_name;
use crate::ui::{
    file_icons::EntryIcon,
    icons::Icon,
    text::TextSpacing,
    theme::{ACCENT, ACTIVE_FILE, CURRENT, GOLD, SELECTION, TEXT},
};
use std::{borrow::Cow, ops::Range, path::Path};

/// Selection retained under a modal or in a secondary picker column uses a subdued tint.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileRowState {
    Normal,
    Selected,
    InactiveSelection,
    ActiveFile,
}

impl FileRowState {
    /// Choose the foreground and background for an active or retained selection.
    pub(crate) fn selection(active: bool) -> Self {
        if active {
            Self::Selected
        } else {
            Self::InactiveSelection
        }
    }

    /// Background role for the full row, including its gutters.
    pub(crate) fn tint(self) -> Option<u32> {
        match self {
            Self::Normal => None,
            Self::Selected => Some(SELECTION),
            Self::InactiveSelection => Some(CURRENT),
            Self::ActiveFile => Some(ACTIVE_FILE),
        }
    }
}

/// Destination of the measured name. The grid paints an inline name once, or hands it to the owner.
pub(crate) enum FileRowLabel<'a> {
    Inline(Cow<'a, str>),
    Overflow(Cow<'a, str>),
}

/// A filesystem entry placed in frame-relative columns. The caller owns selection and actions.
pub(crate) struct GridFileRow<'a> {
    pub(crate) columns: Range<usize>,
    /// The name slot also anchors the icon two columns to its left.
    pub(crate) name_columns: Range<usize>,
    pub(crate) icon: Icon,
    name: Cow<'a, str>,
    pub(crate) state: FileRowState,
    /// Optional mark column in the same frame as the name slot.
    pub(crate) mark: Option<usize>,
    pub(crate) matched: bool,
}

impl<'a> GridFileRow<'a> {
    /// Place a flat entry with two gutter cells followed by its icon and name.
    pub(crate) fn new(
        path: &'a Path,
        directory: bool,
        expanded: bool,
        columns: Range<usize>,
    ) -> Self {
        Self {
            name_columns: columns.start + 4..columns.end,
            columns,
            icon: EntryIcon::for_path(path, directory, expanded).image(),
            name: file_name(path),
            state: FileRowState::Normal,
            mark: None,
            matched: false,
        }
    }

    /// Keep the tree arm and the icon aligned when a deep row is squeezed into a narrow pane.
    pub(crate) fn icon_column(&self) -> usize {
        self.name_columns.start.saturating_sub(2)
    }

    /// A search match takes precedence over the accent on the selected name.
    pub(crate) fn ink(&self) -> u32 {
        if self.matched {
            GOLD
        } else if self.state == FileRowState::Selected {
            ACCENT
        } else {
            TEXT
        }
    }

    /// Fit the name to its slot. A clipped name can use a wider, caller-owned overlay.
    pub(crate) fn label(
        &self,
        spacing: &TextSpacing,
        overflow_width: Option<usize>,
    ) -> FileRowLabel<'_> {
        let fitted = spacing.ellipsize(&self.name, self.name_columns.len());
        if let Some(width) = overflow_width
            && width > self.name_columns.len()
            && fitted.as_ref() != self.name.as_ref()
        {
            return FileRowLabel::Overflow(spacing.ellipsize(&self.name, width));
        }
        FileRowLabel::Inline(fitted)
    }
}
