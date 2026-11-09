//! The visual context supplied to a concrete grid window for one frame.

use crate::{
    grid::render::{Frame, Rect},
    ui::{
        text::TextSpacing,
        theme::{CURRENT, SELECTION},
    },
};

/// Shared placement and selection state; editable data stays with the concrete window.
#[derive(Clone, Copy)]
pub(in crate::grid) struct WindowFrame<'a> {
    /// Window rectangle in grid cells, including its border.
    pub area: Rect,
    /// Whether this window owns the current selection.
    pub selected: bool,
    /// Whether a modal popup currently owns input.
    pub modal_open: bool,
    /// Whether the close button must be reserved in the border.
    pub close_hovered: bool,
    /// Shared font measurements used for labels and document cells.
    pub spacing: &'a TextSpacing,
}

impl WindowFrame<'_> {
    /// Draw `title` around `frame`, retaining the shared border and close-button rules.
    pub fn border(self, frame: &mut Frame, title: &str) {
        frame.border(title, self.selected, self.close_hovered, self.spacing);
    }

    /// Dim retained pane selections while a modal popup owns input.
    pub fn selection_tint(self) -> u32 {
        if self.modal_open { CURRENT } else { SELECTION }
    }
}
