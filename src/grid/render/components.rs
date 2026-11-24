//! Adapt the shared UI components to character-cell frames.

use super::{Frame, LabelStyle};
use crate::ui::components::grid_key_hints::GridKeyHints;
use crate::ui::{
    components::grid_text_field::FieldLayout,
    text::UNITS_PER_COLUMN,
    theme::{SELECTION, TEXT},
};
use crate::ui::{
    components::{
        grid_button_row::ButtonSlot,
        grid_file_row::{FileRowLabel, GridFileRow},
        grid_toast::GridToast,
        grid_tree::TreeRow,
    },
    grid_geometry::Rect,
    text::TextSpacing,
    theme::GOLD,
};
use std::{borrow::Cow, ops::Range};

impl Frame {
    /// Draw complete hints with the footer's label and separator inks at half opacity.
    pub(in crate::grid) fn key_hints(
        &mut self,
        y: usize,
        left: usize,
        hints: &GridKeyHints,
        inks: (u32, u32),
        spacing: &TextSpacing,
    ) {
        let end = left + hints.width;
        let mut x = left;
        for (i, label) in hints.labels.iter().enumerate() {
            if i > 0 {
                x = self.styled_label(
                    y,
                    x..end,
                    GridKeyHints::SEPARATOR,
                    inks.1,
                    spacing,
                    LabelStyle::Dimmed,
                );
            }
            x = self.styled_label(y, x..end, label, inks.0, spacing, LabelStyle::Dimmed);
        }
    }

    /// Paint the visible draft and selection using the layout shared with the native caret.
    pub(in crate::grid) fn text_field(
        &mut self,
        y: usize,
        slot: Range<usize>,
        field: &FieldLayout<'_>,
        spacing: &TextSpacing,
    ) {
        if !field.selection.is_empty() {
            self.tint(
                y,
                slot.start + field.selection.start / UNITS_PER_COLUMN,
                (slot.start + field.selection.end.div_ceil(UNITS_PER_COLUMN)).min(slot.end),
                SELECTION,
            );
        }
        self.label(y, slot, field.text(), TEXT, spacing);
    }

    /// Draw a file row. When its name fits better in an allowed overflow layer, return that
    /// name to the owner and leave the label slot clear for the overlay.
    pub(in crate::grid) fn file_row<'a>(
        &mut self,
        y: usize,
        row: &'a GridFileRow<'_>,
        spacing: &TextSpacing,
        overflow_width: Option<usize>,
    ) -> Option<Cow<'a, str>> {
        if let Some(tint) = row.state.tint() {
            self.tint(y, row.columns.start, row.columns.end, tint);
        }
        if let Some(column) = row.mark.filter(|col| row.columns.contains(col)) {
            self.text(column, y, "●", GOLD, 1);
        }
        let icon = row.icon_column();
        if row.columns.contains(&icon) {
            self.icon(icon, y, row.icon);
        }
        match row.label(spacing, overflow_width) {
            FileRowLabel::Inline(name) => {
                self.label(y, row.name_columns.clone(), &name, row.ink(), spacing);
                None
            }
            FileRowLabel::Overflow(name) => Some(name),
        }
    }

    /// Draw tree connectors up to the actual icon or disclosure column of the row.
    pub(in crate::grid) fn tree_row_guides(&mut self, y: usize, row: &TreeRow<'_>, control: usize) {
        row.guides(control, |column, continues, arm| {
            self.tree_guide(column, y, continues, arm)
        });
    }

    /// Paint `toast` using the measured `spacing`, returning its placed frame within `viewport`.
    /// `available` limits the width of the message column.
    pub(in crate::grid) fn toast(
        toast: &GridToast,
        viewport: Rect,
        available: usize,
        spacing: &TextSpacing,
    ) -> (Rect, Self) {
        let (area, lines) = toast.layout(viewport, available, spacing);
        let mut frame = Self::new(area.w, area.h);
        frame.message_border(toast.level, spacing);
        let ink = toast.level.colors().1;
        for (row, line) in lines.iter().take(area.h.saturating_sub(2)).enumerate() {
            frame.label(row + 1, 2..area.w.saturating_sub(2), line, ink, spacing);
        }
        (area, frame)
    }

    /// Draw the visible part of a measured `button` at `row` using `ink` and `spacing`.
    pub(in crate::grid) fn button<A>(
        &mut self,
        row: usize,
        button: &ButtonSlot<A>,
        ink: u32,
        spacing: &TextSpacing,
    ) {
        let mut start = button.columns.start;
        for (text, style) in button.caption.spans() {
            start = self.styled_label(row, start..button.columns.end, text, ink, spacing, style);
        }
    }

    /// Allocate a fixed frame at `area`, measuring its plain `title` with `spacing`.
    pub(in crate::grid) fn titled(area: Rect, title: &str, spacing: &TextSpacing) -> Self {
        Self::with_title(area, true, false, |frame, slot, ink| {
            frame.label(0, slot, title, ink, spacing)
        })
    }

    /// Create a frame at `area` with `active` focus ink and an optional `close_control`.
    /// `title` paints available title columns and returns the occupied right edge.
    pub(in crate::grid) fn with_title(
        area: Rect,
        active: bool,
        close_control: bool,
        title: impl FnOnce(&mut Self, Range<usize>, u32) -> usize,
    ) -> Self {
        let mut frame = Self::new(area.w, area.h);
        frame.border_with_title(active, close_control, false, title);
        frame
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::{components::grid_file_row::FileRowState, theme::CURRENT};
    use gpui::{FontId, NoopTextSystem};
    use std::{path::Path, sync::Arc};

    /// Marks, tints and elided names stay within one picker column; an overflow paints the name only once.
    #[test]
    fn file_row_preserves_matched_names_gutters_and_overflow() {
        let spacing = TextSpacing::new(Arc::new(NoopTextSystem::new()), FontId(0), 16., 6.4);
        let mut row = GridFileRow::new(Path::new("a\u{301}文XYZ"), false, false, 5..13);
        row.state = FileRowState::InactiveSelection;
        row.matched = true;
        row.mark = Some(6);
        let mut frame = Frame::new(16, 3);
        assert!(frame.file_row(0, &row, &spacing, None).is_none());
        assert_eq!(frame.icons[0].column, 7);
        assert_eq!(frame.rows[0].cells[6].text, "●");
        assert_eq!(frame.cell_paint_at(0, 5).background, CURRENT);
        assert_eq!(frame.cell_paint_at(0, 13).background, 0);
        let cells = &frame.labels[0][0].text.cells;
        assert_eq!(
            cells
                .iter()
                .map(|cell| cell.text.as_str())
                .collect::<String>(),
            "a\u{301}文…"
        );
        assert_eq!(cells[0].paint.foreground, GOLD);
        assert_eq!(
            frame.file_row(1, &row, &spacing, Some(10)).as_deref(),
            Some("a\u{301}文XYZ")
        );
        assert!(frame.labels[1].is_empty());

        let mut narrow = GridFileRow::new(Path::new("src"), true, true, 10..11);
        narrow.mark = Some(11);
        frame.file_row(2, &narrow, &spacing, None);
        assert_eq!(frame.icons.len(), 2);
        assert!(frame.labels[2].is_empty());
        assert_eq!(frame.rows[2].cells[11].text, " ");
    }
}
