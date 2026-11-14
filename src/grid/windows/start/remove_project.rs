//! Layout and input for removing a project from the recent list.

use super::projects::display_path;
use crate::{
    grid::{
        mnemonic::Mnemonic,
        render::{Frame, MessageLevel, Rect},
    },
    ui::{
        text::TextSpacing,
        theme::{MUTED, TEXT},
    },
};
use gpui::{KeyDownEvent, Modifiers};
use nanobug::i18n;
use std::{ops::Range, path::PathBuf};

/// Maximum dialog width in workspace columns.
const COLUMNS: usize = 54;
/// First detail row, one row below the title border to match the gap above the button border.
const DETAILS_ROW: usize = 1;
/// Blank columns between the two localized choices.
const BUTTON_GAP: usize = 8;

/// A recent project awaiting removal from the saved list.
pub(in crate::grid) struct RemoveProject {
    /// Exact history entry captured when the dialog opened.
    pub(in crate::grid) root: PathBuf,
}

/// Resolve the localized confirmation and cancellation captions in display order.
fn buttons() -> [(bool, Mnemonic); 2] {
    [
        (true, Mnemonic::parse(i18n::text("start.remove_yes"))),
        (false, Mnemonic::parse(i18n::text("start.remove_no"))),
    ]
}

/// Center the choices in `width` columns using their measured `spacing`.
fn button_slots(width: usize, spacing: &TextSpacing) -> [(bool, Mnemonic, Range<usize>); 2] {
    let buttons = buttons().map(|(confirm, label)| {
        let columns: usize = label
            .spans()
            .into_iter()
            .map(|(text, _)| spacing.columns(text))
            .sum();
        (confirm, label, columns)
    });
    let total = buttons
        .iter()
        .map(|&(_, _, columns)| columns)
        .sum::<usize>()
        + BUTTON_GAP;
    let mut start = width.saturating_sub(total) / 2;
    buttons.map(|(confirm, label, columns)| {
        let slot = start..start + columns;
        start = slot.end + BUTTON_GAP;
        (confirm, label, slot)
    })
}

impl RemoveProject {
    /// Center the dialog above the midpoint of the `cols` by `rows` grid, fitting its details.
    pub(in crate::grid) fn area(&self, cols: usize, rows: usize) -> Rect {
        let w = COLUMNS.min(cols.saturating_sub(4));
        let detail_rows = 1 + usize::from(self.root.file_name().is_some());
        let h = DETAILS_ROW + detail_rows + 1;
        Rect {
            x: (cols - w) / 2,
            y: (rows.saturating_sub(h) * 2 / 5).max(1),
            w,
            h,
        }
    }

    /// Draw the prompt and exact target in `area`, with captions measured by `spacing`.
    pub(in crate::grid) fn frame(&self, area: Rect, spacing: &TextSpacing) -> Frame {
        let mut frame = Frame::new(area.w, area.h);
        let level = MessageLevel::Warning;
        let (_, button_color) = level.colors();
        frame.message_border_with_title(level, Some(i18n::text("start.remove_project")), spacing);

        let path = display_path(&self.root);
        let name = self.root.file_name().map(|name| name.to_string_lossy());
        let slot = 3..area.w - 3;
        frame.label(
            DETAILS_ROW,
            slot.clone(),
            &spacing.ellipsize(name.as_deref().unwrap_or(&path), slot.len()),
            TEXT,
            spacing,
        );
        if name.is_some() {
            frame.label(
                DETAILS_ROW + 1,
                slot.clone(),
                &spacing.ellipsize(&path, slot.len()),
                MUTED,
                spacing,
            );
        }

        let row = area.h - 1;
        for (_, label, slot) in button_slots(area.w, spacing) {
            frame.outline_footer_gap(slot.start - 1..slot.end + 1);
            let mut start = slot.start;
            for (text, style) in label.spans() {
                start =
                    frame.styled_label(row, start..slot.end, text, button_color, spacing, style);
            }
        }
        frame
    }

    /// Resolve a fresh `event` to confirm or cancel, with Escape always offering cancellation.
    pub(in crate::grid) fn key_choice(event: &KeyDownEvent) -> Option<bool> {
        let key = &event.keystroke;
        let mods = key.modifiers;
        if event.is_held || mods.control || mods.alt || mods.platform || mods.function {
            return None;
        }
        if key.key == "escape" && mods == Modifiers::default() {
            return Some(false);
        }
        buttons()
            .into_iter()
            .find_map(|(confirm, label)| label.matches(&key.key).then_some(confirm))
    }

    /// Resolve cell `(x, y)` inside `area` against the drawn choices measured by `spacing`.
    pub(in crate::grid) fn click_choice(
        area: Rect,
        (x, y): (usize, usize),
        spacing: &TextSpacing,
    ) -> Option<bool> {
        if !area.contains(x, y) || y != area.y + area.h - 1 {
            return None;
        }
        button_slots(area.w, spacing)
            .into_iter()
            .find_map(|(confirm, _, slot)| slot.contains(&(x - area.x)).then_some(confirm))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::render::LabelStyle;
    use gpui::{FontId, Keystroke, NoopTextSystem};
    use nanobug::i18n::Locale;
    use std::sync::Arc;

    /// Press `binding` as a fresh key or repeat according to `is_held`, returning the dialog choice.
    fn press(binding: &'static str, is_held: bool) -> Option<bool> {
        RemoveProject::key_choice(&KeyDownEvent {
            keystroke: Keystroke::parse(binding).unwrap(),
            is_held,
            prefer_character_input: false,
        })
    }

    /// Fit one or two detail rows evenly between the title and button borders.
    #[test]
    fn dialog_height_fits_project_details() {
        let root = RemoveProject {
            root: PathBuf::from(std::path::MAIN_SEPARATOR_STR),
        };
        let project = RemoveProject {
            root: root.root.join("project"),
        };
        assert_eq!(root.area(90, 32).h, 3);
        assert_eq!(project.area(90, 32).h, 4);
    }

    /// Check localized labels, their underlined letters and equivalence between clicks and keys.
    #[test]
    fn captions_share_keyboard_and_mouse_choices() {
        let captions = match i18n::locale() {
            Locale::English => ["[Yes]", "[No]"],
            Locale::SimplifiedChinese => ["[是(Y)]", "[否(N)]"],
        };
        let spacing = TextSpacing::new(Arc::new(NoopTextSystem::new()), FontId(0), 16., 9.);
        let area = RemoveProject {
            root: PathBuf::from("team/project"),
        }
        .area(90, 32);
        let row = area.y + area.h - 1;
        for ((confirm, label, slot), (caption, key)) in button_slots(area.w, &spacing)
            .into_iter()
            .zip(captions.into_iter().zip(["y", "n"]))
        {
            assert_eq!(label.spans().map(|(text, _)| text).concat(), caption);
            let underlined: Vec<_> = label
                .spans()
                .into_iter()
                .filter_map(|(text, style)| (style == LabelStyle::Key).then_some(text))
                .collect();
            assert_eq!(underlined, [key.to_ascii_uppercase()]);
            assert_eq!(press(key, false), Some(confirm));
            assert_eq!(
                RemoveProject::click_choice(area, (area.x + slot.start, row), &spacing),
                Some(confirm)
            );
        }
        assert_eq!(press("shift-y", false), Some(true));
        assert_eq!(press("escape", false), Some(false));
        assert_eq!(press("y", true), None);
        assert_eq!(press("alt-y", false), None);
        assert_eq!(press("enter", false), None);
    }
}
