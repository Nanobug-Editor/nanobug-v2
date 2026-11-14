//! Geometry and rendering shared by the three top-level plugin pages.

use super::{ManagerView, Page, Prompt};
use crate::grid::windows::plugin_surface::field;
use crate::{
    grid::render::{
        Frame, LabelStyle, PromptRow, Rect, SEARCH_ICON, SHORTCUT_SEPARATOR, shortcut_hint,
    },
    ui::{
        text::{TextSpacing, UNITS_PER_COLUMN},
        theme::{ACCENT, GOLD, MUTED, SELECTION, TEXT},
    },
};
use nanobug::i18n;
use std::{borrow::Cow, ops::Range};
use unicode_segmentation::UnicodeSegmentation;

/// Labels appear in the same order as the numeric page shortcuts.
const LIST_TABS: [&str; 3] = [
    "plugin_manager.installed",
    "plugin_manager.marketplace",
    "plugin_manager.local_zip",
];
/// Normal list width at the default font size, in character cells.
const MAX_WIDTH: usize = 112;
/// Room for two description rows and the top and bottom edges of their outline.
const DESCRIPTION_HEIGHT: usize = 4;
/// List commands in display order, with the filter and exit keys first for narrow windows.
const SHORTCUTS: &[(&str, &str)] = &[
    ("plugin_manager.filter", "/"),
    ("picker.hint_back", "q/Esc"),
    ("picker.hint_move", "j/k"),
    ("picker.hint_open", "Enter"),
    ("plugin_manager.hint_tabs", "1/2/3"),
    ("picker.hint_help", "?"),
];

/// Insets and text positions relative to the plugin window's outer border.
pub(in crate::grid) struct ListLayout {
    /// Body with tabs in its top rule and a header for the package columns when space permits.
    pub(in crate::grid) list: Rect,
    /// List entries between the header and the bottom rule.
    pub(in crate::grid) entries: Rect,
    /// Selected entry's summary inside the dashed rule.
    pub(in crate::grid) description: Rect,
    /// Bottom row for shortcut hints or the active filter or path prompt.
    pub(in crate::grid) footer: Rect,
    /// Feedback row inside the list, or the header when the window is compact.
    feedback: Option<usize>,
    /// Name column after the command-mode caret.
    name: Range<usize>,
    /// Independently clipped version column.
    version: Range<usize>,
    /// Independently clipped status column.
    status: Range<usize>,
    /// Whether there is room for a header above the first selectable row.
    show_columns: bool,
}

impl ListLayout {
    /// Fit the window between workspace chrome in the available `cols` by `rows`.
    pub(in crate::grid) fn area(cols: usize, rows: usize) -> Rect {
        let horizontal_inset = if cols < 48 { 2 } else { 4 };
        let w = cols.saturating_sub(horizontal_inset).min(MAX_WIDTH);
        let reserved_rows = if rows >= 16 { 3 } else { 1 };
        let h = rows.saturating_sub(reserved_rows);
        Rect {
            x: (cols - w) / 2,
            y: 1,
            w,
            h,
        }
    }

    /// Partition the outer `area`, giving `has_feedback` a row inside the list when it fits.
    pub(in crate::grid) fn new(area: Rect, has_feedback: bool) -> Self {
        let section = |y, h| Rect {
            x: 2,
            y,
            w: area.w.saturating_sub(4),
            h,
        };
        let footer = section(area.h.saturating_sub(2), usize::from(area.h >= 3));
        let show_columns = area.h >= 7;
        let list_border_rows = 2 + usize::from(show_columns);
        // Keep space for a selectable row and at least one line of description when it fits.
        let height = footer.y.saturating_sub(list_border_rows + 2);
        let description_height = if height >= 3 {
            height.min(DESCRIPTION_HEIGHT)
        } else {
            0
        };
        let description = section(
            footer.y.saturating_sub(description_height),
            description_height,
        );
        let list = section(1, description.y.saturating_sub(1));
        let feedback_rows = usize::from(has_feedback && list.h > list_border_rows + 1);
        let entries = Rect {
            x: list.x + 1,
            y: list.y + list_border_rows - 1,
            w: list.w.saturating_sub(2),
            h: list.h.saturating_sub(list_border_rows + feedback_rows),
        };
        let left = entries.x + 1;
        let width = entries.w.saturating_sub(2);
        let right = left + width;
        let gap = 1 + usize::from(width >= 36);
        let status_start = right - (entries.w / 10).clamp(10, 14).min(width / 3);
        let version_start = status_start - (entries.w / 5).clamp(9, 21).min(width / 3);
        let feedback = if feedback_rows > 0 {
            Some(entries.y + entries.h)
        } else {
            (has_feedback && show_columns).then_some(list.y + 1)
        };
        Self {
            list,
            entries,
            description,
            footer,
            feedback,
            name: left..version_start.saturating_sub(gap).max(left),
            version: version_start..status_start.saturating_sub(gap).max(version_start),
            status: status_start..right,
            show_columns,
        }
    }

    /// Measure the visible title in local description columns using grid `spacing`.
    pub(in crate::grid) fn description_title(&self, spacing: &TextSpacing) -> Range<usize> {
        let slot = self.description.border_title_columns(None);
        let width = spacing
            .fitting_cells(
                i18n::text("plugin_manager.description"),
                slot.len() * UNITS_PER_COLUMN,
            )
            .last()
            .map_or(0, |cell| cell.end.div_ceil(UNITS_PER_COLUMN));
        slot.start..slot.start + width
    }

    /// Lay out `kind`'s bottom input and actions using `spacing`, with room for the search icon.
    pub(in crate::grid) fn prompt(&self, kind: &Prompt, spacing: &TextSpacing) -> PromptRow {
        let icon_columns = 2 * usize::from(matches!(kind, Prompt::Search { .. }));
        PromptRow::new(
            self.footer.x + icon_columns,
            self.footer.x + self.footer.w,
            spacing,
        )
    }

    /// Return the visible tab slots, measured with the same `spacing` as their labels.
    pub(in crate::grid) fn tabs(&self, spacing: &TextSpacing) -> [Range<usize>; 3] {
        let local = self.list.border_title_columns(None);
        let columns = self.list.x + local.start..self.list.x + local.end;
        let mut left = columns.start;
        let separator = spacing.columns(self.tab_separator());
        self.tab_labels(spacing).map(|text| {
            let right = (left + spacing.columns(text)).min(columns.end);
            let slot = left..right;
            left = (right + separator).min(columns.end);
            slot
        })
    }

    /// Tighten the separators at high zoom so all three numeric shortcuts stay visible.
    fn tab_separator(&self) -> &'static str {
        if self.list.border_title_columns(None).len() < 32 {
            "|"
        } else {
            " | "
        }
    }

    /// Use short tab captions when the localized full set would not fit the title rule.
    fn tab_labels(&self, spacing: &TextSpacing) -> [&'static str; 3] {
        let width = self.list.border_title_columns(None).len();
        let separator = spacing.columns(self.tab_separator());
        let installed = i18n::text(LIST_TABS[0]);
        [
            LIST_TABS.map(i18n::text),
            [
                installed,
                i18n::text("plugin_manager.marketplace_short"),
                i18n::text("plugin_manager.local_zip_short"),
            ],
            [installed, "[2]", "[3]"],
        ]
        .into_iter()
        .find(|labels| {
            labels
                .iter()
                .map(|label| spacing.columns(label))
                .sum::<usize>()
                + 2 * separator
                <= width
        })
        .unwrap_or(["[1]", "[2]", "[3]"])
    }
}

impl ManagerView<'_> {
    /// Lay out the plugin list within `area` with room for any visible feedback.
    pub(in crate::grid) fn plugin_list_layout(&self, area: Rect) -> ListLayout {
        ListLayout::new(area, !self.plugin_list_feedback().is_empty())
    }

    /// Return the current manager message, with local input feedback taking priority.
    fn plugin_list_feedback(&self) -> &str {
        self.manager
            .feedback
            .as_deref()
            .unwrap_or(&self.data.view.message)
    }

    /// Use the three-section layout for the plugin browser's top-level pages.
    pub(in crate::grid) fn uses_plugin_list_layout(&self) -> bool {
        let manager = &self.manager;
        manager.detail.is_none()
            && !manager.help
            && manager.confirm_remove.is_none()
            && self.data.view.review.is_none()
            && self.presentation.rows.is_none()
            && !self.data.view.waiting_for_operations
            && manager.prompt.as_ref().is_none_or(|input| {
                matches!(input.kind, Prompt::Search { .. } | Prompt::Development)
            })
    }

    /// Fit the committed prompt and IME preedit together. The returned column is in spacing units.
    pub(in crate::grid) fn plugin_list_prompt(&self, columns: usize) -> (Cow<'_, str>, usize) {
        let input = self.manager.prompt.as_ref().expect("active list prompt");
        let composition = &self.composition;
        let (text, caret) = if composition.is_empty() {
            (Cow::Borrowed(input.value.as_str()), input.value.len())
        } else {
            let marked_prefix = composition
                .text_for_range(0..composition.selection.end, &mut None)
                .map_or(0, |prefix| prefix.len());
            let text = format!("{}{}", input.value, composition.text);
            let caret = field::grapheme_at_or_after(&text, input.value.len() + marked_prefix);
            (Cow::Owned(text), caret)
        };
        let available = columns.saturating_sub(1) * UNITS_PER_COLUMN;
        let (start, units) = field::visible_left(&text, caret, 0, available, self.spacing);
        let suffix = match text {
            Cow::Borrowed(text) => Cow::Borrowed(&text[start..]),
            Cow::Owned(mut text) => {
                text.replace_range(..start, "");
                Cow::Owned(text)
            }
        };
        (suffix, units)
    }

    /// Draw the package list, selected description and footer on `frame` using `layout`.
    pub(in crate::grid) fn draw_plugin_list(&self, frame: &mut Frame, layout: &ListLayout) {
        let manager = &self.manager;
        let spacing = self.spacing;
        let tabs = layout.tabs(spacing);
        let labels = layout.tab_labels(spacing);
        frame.section_border(layout.list, MUTED, |frame, _| {
            for (index, (label, slot)) in labels.into_iter().zip(tabs.iter()).enumerate() {
                if index > 0 {
                    frame.label(
                        layout.list.y,
                        tabs[index - 1].end..slot.start,
                        layout.tab_separator(),
                        MUTED,
                        spacing,
                    );
                }
                frame.label(
                    layout.list.y,
                    slot.clone(),
                    label,
                    if index == manager.page as usize {
                        ACCENT
                    } else {
                        MUTED
                    },
                    spacing,
                );
            }
            tabs[2].end
        });

        let header = layout.list.y + 1;
        let left = layout.name.start;
        let right = layout.status.end;
        if let Some(y) = layout.feedback {
            let slot = if y == header && manager.page != Page::Zip {
                left + spacing.columns(i18n::text("plugin_manager.column_name")) + 2
                    ..layout.name.end
            } else {
                left..right
            };
            let message = spacing.ellipsize(self.plugin_list_feedback(), slot.len());
            frame.label(y, slot, &message, GOLD, spacing);
        }

        if manager.page != Page::Zip && layout.show_columns {
            for (columns, key) in [
                (layout.name.clone(), "plugin_manager.column_name"),
                (layout.version.clone(), "plugin_manager.column_version"),
                (layout.status.clone(), "plugin_manager.column_status"),
            ] {
                frame.label(header, columns, i18n::text(key), MUTED, spacing);
            }
        }

        let rows = self.rows();
        let selected = manager.selection.min(rows.len().saturating_sub(1));
        let first = self.plugin_visible_rows(layout.entries.h, rows.len()).start;
        for (offset, row) in rows.iter().skip(first).take(layout.entries.h).enumerate() {
            let y = layout.entries.y + offset;
            let color = if first + offset == selected {
                frame.tint(
                    y,
                    layout.entries.x,
                    layout.entries.x + layout.entries.w,
                    SELECTION,
                );
                ACCENT
            } else {
                TEXT
            };
            let name_end = if let Some(package) = &row.package {
                let version = spacing.ellipsize(&package.version, layout.version.len());
                frame.label(y, layout.version.clone(), &version, color, spacing);
                let status = format!(
                    "{}{}",
                    package.status,
                    if package.update_available { " ↑" } else { "" }
                );
                frame.label(y, layout.status.clone(), &status, color, spacing);
                layout.name.end
            } else {
                right
            };
            let name = spacing.ellipsize(&row.label, name_end.saturating_sub(left));
            frame.label(y, left..name_end, &name, color, spacing);
        }
        if rows.is_empty() && layout.entries.h > 0 {
            frame.label(
                layout.entries.y,
                left..right,
                i18n::text("plugin_manager.empty"),
                MUTED,
                spacing,
            );
        }

        let description = rows
            .get(selected)
            .map_or("", |row| row.description.as_str());
        let top = layout.description.y + 1;
        let lines = layout.description.h.saturating_sub(2);
        if lines > 0 {
            let title = layout.description_title(spacing);
            frame.label(
                layout.description.y,
                layout.description.x + title.start..layout.description.x + title.end,
                i18n::text("plugin_manager.description"),
                MUTED,
                spacing,
            );
        }
        if lines == 1 {
            let text = spacing.ellipsize(description, right - left);
            frame.label(top, left..right, &text, TEXT, spacing);
        } else if lines >= 2 {
            for (index, line) in wrap_description(description, right - left, spacing)
                .iter()
                .enumerate()
            {
                frame.label(top + index, left..right, line, TEXT, spacing);
            }
        }

        self.draw_plugin_footer(frame, layout);
    }

    /// Draw the active input or complete shortcut hints in `layout`'s footer on `frame`.
    fn draw_plugin_footer(&self, frame: &mut Frame, layout: &ListLayout) {
        if layout.footer.h == 0 {
            return;
        }
        let footer = layout.footer;
        let spacing = self.spacing;
        if let Some(input) = &self.manager.prompt {
            let slots = layout.prompt(&input.kind, spacing);
            let (text, _) = self.plugin_list_prompt(slots.input.len());
            let placeholder = match input.kind {
                Prompt::Development => "plugin_manager.build_output_path",
                _ => {
                    frame.icon(footer.x, footer.y, SEARCH_ICON);
                    "plugin_manager.filter"
                }
            };
            let (text, color) = if input.value.is_empty() && self.composition.is_empty() {
                (i18n::text(placeholder), MUTED)
            } else {
                (text.as_ref(), TEXT)
            };
            frame.label(footer.y, slots.input.clone(), text, color, spacing);
            slots.draw_buttons(frame, footer.y, spacing);
            return;
        }

        let mut text = String::new();
        let mut remaining = footer.w;
        for &(action, key) in SHORTCUTS {
            let hint = shortcut_hint(i18n::text(action), key);
            let separator = if text.is_empty() {
                ""
            } else {
                SHORTCUT_SEPARATOR
            };
            let columns = spacing.columns(separator) + spacing.columns(&hint);
            if columns > remaining {
                break;
            }
            text.push_str(separator);
            text.push_str(&hint);
            remaining -= columns;
        }
        frame.styled_label(
            footer.y,
            footer.x..footer.x + footer.w,
            &text,
            TEXT,
            spacing,
            LabelStyle::Dimmed,
        );
    }
}

/// First item row in a secondary dialog, below its scope, tabs, and feedback.
pub(in crate::grid) const LIST_START: usize = 4;
/// Minimum grid height for the secondary dialogs' content and footer.
pub(in crate::grid) const MIN_HEIGHT: usize = 13;
/// Columns reserved for Enter and Esc in secondary dialogs.
pub(in crate::grid) const SEARCH_BUTTONS: usize = 23;

pub(in crate::grid) fn list_offset(offset: usize, selected: usize, capacity: usize) -> usize {
    offset
        .min(selected)
        .max((selected + 1).saturating_sub(capacity))
}

/// Place secondary dialog entries below the tabs and above the two-line description.
pub(in crate::grid) fn secondary_list_entries(area: Rect) -> Rect {
    Rect {
        x: 2,
        y: LIST_START,
        w: area.w.saturating_sub(4),
        h: area.h.saturating_sub(LIST_START + 5),
    }
}

/// Position the two description rows inside the lower part of a secondary dialog's `area`.
pub(in crate::grid) fn secondary_description(area: Rect) -> Rect {
    Rect {
        x: 1,
        y: area.h.saturating_sub(5),
        w: area.w.saturating_sub(2),
        h: 4,
    }
}

/// Locate the Enter and Esc buttons from the prompt's reserved left edge.
pub(in crate::grid) fn prompt_button_columns(left: usize) -> [Range<usize>; 2] {
    [left..left + 9, left + 11..left + 18]
}

/// Resolve a click at `column` to its visible button. `true` means cancel.
pub(in crate::grid) fn prompt_button_at(left: usize, column: usize) -> Option<bool> {
    prompt_button_columns(left)
        .iter()
        .position(|slot| slot.contains(&column))
        .map(|button| button == 1)
}

pub(in crate::grid) fn tail(text: &str, columns: usize) -> String {
    let mut width = 0;
    let mut start = text.len();
    for (index, grapheme) in text.grapheme_indices(true).rev() {
        width += unicode_width::UnicodeWidthStr::width(grapheme).max(1);
        if width > columns {
            break;
        }
        start = index;
    }
    text[start..].to_owned()
}

/// Wrap the `text` to two rows using native glyph advances. Truncate the second with an ellipsis.
pub(in crate::grid) fn wrap_description(
    text: &str,
    columns: usize,
    spacing: &TextSpacing,
) -> Vec<String> {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let end = spacing
        .fitting_cells(&text, columns * UNITS_PER_COLUMN)
        .last()
        .map_or(0, |cell| cell.byte + cell.text.len());
    if end == 0 || end == text.len() {
        return vec![spacing.ellipsize(&text, columns).into_owned()];
    }
    let split = if text[end..].starts_with(' ') {
        end
    } else {
        text[..end]
            .rfind(' ')
            .filter(|index| *index > end / 2)
            .unwrap_or(end)
    };
    vec![
        text[..split].to_owned(),
        spacing
            .ellipsize(text[split..].trim_start(), columns)
            .into_owned(),
    ]
}
