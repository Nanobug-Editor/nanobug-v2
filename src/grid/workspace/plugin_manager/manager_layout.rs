//! Geometry and rendering shared by the three top-level plugin pages.

use super::{Page, Prompt, Workspace, field, wrap_description};
use crate::{
    grid::render::{Frame, Rect},
    ui::{
        text::{TextSpacing, UNITS_PER_COLUMN},
        theme::{ACCENT, GOLD, MUTED, SELECTION, TEXT},
    },
};
use nanobug::i18n;
use std::{borrow::Cow, ops::Range};

/// Space reserved to the right of an active prompt for the Enter and Esc buttons.
pub(super) const SEARCH_BUTTONS: usize = 23;
/// Labels appear in the same order as the numeric page shortcuts.
const LIST_TABS: [&str; 3] = [
    "plugin_manager.installed",
    "plugin_manager.marketplace",
    "plugin_manager.local_zip",
];
/// Normal list width at the default font size, in character cells.
const MAX_WIDTH: usize = 112;
/// The description has a top rule, two text rows and a bottom rule.
const DESCRIPTION_HEIGHT: usize = 4;

/// Insets and text positions relative to the plugin window's outer border.
pub(super) struct ListLayout {
    /// Titled filter or ZIP path entry at the top of the window.
    pub(super) filter: Rect,
    /// Body with tabs in its top rule and a header for the package columns when space permits.
    pub(super) list: Rect,
    /// List entries between the header and the bottom rule.
    pub(super) entries: Rect,
    /// Selected entry's summary inside the dashed rule.
    pub(super) description: Rect,
    /// Feedback row above the description, or the header when the window is compact.
    feedback: Option<usize>,
    /// Left edge of text in the Name column, after the command-mode caret.
    name: usize,
    /// Independently clipped version column.
    version: Range<usize>,
    /// Independently clipped status column.
    status: Range<usize>,
    /// Whether there is room for a header above the first selectable row.
    show_columns: bool,
}

impl ListLayout {
    /// Fit the window between workspace chrome in the available `cols` by `rows`.
    pub(super) fn area(cols: usize, rows: usize) -> Rect {
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

    /// Partition the outer `area`. Prefer blank separators when the window has room.
    pub(super) fn new(area: Rect) -> Self {
        let show_columns = area.h >= 12;
        let section = |y, h| Rect {
            x: 2,
            y,
            w: area.w.saturating_sub(4),
            h,
        };
        let narrow = area.w < 32;
        let filter = if narrow {
            Rect {
                x: 1,
                y: 1,
                w: area.w.saturating_sub(2),
                h: 3,
            }
        } else {
            section(1, 3)
        };
        // Keep at least one entry, compacting the description when the client is very short.
        let description_height = if show_columns {
            DESCRIPTION_HEIGHT
        } else {
            area.h.saturating_sub(7).min(DESCRIPTION_HEIGHT)
        };
        let feedback_rows = usize::from(area.h >= 14);
        let description = section(
            area.h.saturating_sub(description_height + 1),
            description_height,
        );
        let list_top = filter.y + filter.h - usize::from(area.h < 14);
        let list = section(
            list_top,
            description.y.saturating_sub(list_top + feedback_rows),
        );
        let reserved = 2 + usize::from(show_columns);
        let entries = Rect {
            x: list.x + 1,
            y: list.y + reserved - 1,
            w: list.w.saturating_sub(2),
            h: list.h.saturating_sub(reserved),
        };
        let left = entries.x + 1;
        let right = left + entries.w.saturating_sub(2);
        let status_start = right.saturating_sub((entries.w / 10).clamp(10, 14));
        let version_start = status_start.saturating_sub((entries.w / 5).clamp(9, 21));
        Self {
            filter,
            list,
            entries,
            description,
            feedback: if feedback_rows > 0 {
                Some(description.y - 1)
            } else {
                show_columns.then_some(entries.y - 1)
            },
            name: left,
            version: version_start..status_start.saturating_sub(2),
            status: status_start..right,
            show_columns,
        }
    }

    /// Return the filter's editable row, excluding the top and bottom rules.
    pub(super) fn filter_row(&self) -> usize {
        self.filter.y + 1
    }

    /// Return the right boundary shared by the filter text and prompt buttons.
    pub(super) fn prompt_right(&self) -> usize {
        (self.filter.x + self.filter.w).saturating_sub(2)
    }

    /// Reserve the button slots only while the filter or path prompt accepts input.
    pub(super) fn filter_text(&self, active: bool) -> Range<usize> {
        let left = self.filter.x + 2;
        let end = self.prompt_right();
        let reserved = if end.saturating_sub(left) <= SEARCH_BUTTONS {
            19
        } else {
            SEARCH_BUTTONS
        };
        left..end.saturating_sub(if active { reserved } else { 0 })
    }

    /// Return the visible tab slots, measured with the same `spacing` as their labels.
    pub(super) fn tabs(&self, spacing: &TextSpacing) -> [Range<usize>; 3] {
        let columns = self.list.border_title_columns();
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
        if self.list.border_title_columns().len() < 32 {
            "|"
        } else {
            " | "
        }
    }

    /// Use short tab captions when the localized full set would not fit the title rule.
    fn tab_labels(&self, spacing: &TextSpacing) -> [&'static str; 3] {
        let width = self.list.border_title_columns().len();
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

impl Workspace {
    /// Use the three-section layout for the plugin browser's top-level pages.
    pub(super) fn uses_plugin_list_layout(&self) -> bool {
        let ui = &self.plugins;
        let manager = &ui.manager;
        manager.detail.is_none()
            && !manager.help
            && manager.confirm_remove.is_none()
            && ui.view.review.is_none()
            && !ui.credential_waiting
            && ui.credential_draft.is_none()
            && ui.formatting.choices.is_none()
            && !ui.file_busy
            && ui.file_plan.is_none()
            && ui.file_report.is_empty()
            && ui.language.is_none()
            && ui.outline.is_none()
            && ui.edit_plan.is_none()
            && ui.edit_check.is_none()
            && ui.close_draft.is_none()
            && !ui.view.waiting_for_operations
            && manager.prompt.as_ref().is_none_or(|input| {
                matches!(
                    input.kind,
                    Prompt::Search { .. } | Prompt::Zip | Prompt::Development
                )
            })
    }

    /// Fit the committed prompt and IME preedit together. The returned column is in spacing units.
    pub(super) fn plugin_list_prompt(&self, columns: usize) -> (Cow<'_, str>, usize) {
        let input = self
            .plugins
            .manager
            .prompt
            .as_ref()
            .expect("active list prompt");
        let composition = &self.plugins.composition;
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
        let (start, units) = field::visible_left(&text, caret, 0, available, &self.spacing);
        let suffix = match text {
            Cow::Borrowed(text) => Cow::Borrowed(&text[start..]),
            Cow::Owned(mut text) => {
                text.replace_range(..start, "");
                Cow::Owned(text)
            }
        };
        (suffix, units)
    }

    /// Draw the filter, tabbed package list and selected description in `layout`.
    pub(super) fn draw_plugin_list(&self, frame: &mut Frame, layout: &ListLayout) {
        let manager = &self.plugins.manager;
        let spacing = &self.spacing;
        let filter_title = match manager.prompt.as_ref().map(|input| &input.kind) {
            Some(Prompt::Zip) => "plugin_manager.zip_path",
            Some(Prompt::Development) => "plugin_manager.build_output_path",
            _ => "plugin_manager.filter",
        };
        let editing = manager.prompt.is_some();
        let filter_color = if editing { ACCENT } else { MUTED };
        frame.section_border(layout.filter, filter_color, false, |frame, slot| {
            frame.label(
                layout.filter.y,
                slot,
                i18n::text(filter_title),
                filter_color,
                spacing,
            )
        });
        let slot = layout.filter_text(editing);
        let buttons_start = slot.end;
        let filter_text = if editing {
            self.plugin_list_prompt(slot.len()).0
        } else {
            spacing.ellipsize(&manager.search, slot.len())
        };
        frame.label(layout.filter_row(), slot, &filter_text, TEXT, spacing);
        if editing {
            self.prompt_buttons(frame, layout.filter_row(), buttons_start);
        }

        let tabs = layout.tabs(spacing);
        let labels = layout.tab_labels(spacing);
        frame.section_border(layout.list, MUTED, false, |frame, _| {
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

        let feedback = manager
            .feedback
            .as_deref()
            .unwrap_or(&self.plugins.view.message);
        let header = layout.list.y + 1;
        if let Some(y) = layout.feedback.filter(|_| !feedback.is_empty()) {
            let slot = if y == header && manager.page != Page::Zip {
                layout.name + spacing.columns(i18n::text("plugin_manager.column_name")) + 2
                    ..layout.version.start.saturating_sub(2)
            } else {
                layout.name..layout.status.end
            };
            let message = spacing.ellipsize(feedback, slot.len());
            frame.label(y, slot, &message, GOLD, spacing);
        }

        let left = layout.name;
        let right = layout.status.end;
        if manager.page != Page::Zip && layout.show_columns {
            for (columns, key) in [
                (
                    left..layout.version.start.saturating_sub(2),
                    "plugin_manager.column_name",
                ),
                (layout.version.clone(), "plugin_manager.column_version"),
                (layout.status.clone(), "plugin_manager.column_status"),
            ] {
                frame.label(header, columns, i18n::text(key), MUTED, spacing);
            }
        }

        let rows = self.plugins.rows();
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
                layout.version.start.saturating_sub(2)
            } else {
                right
            };
            let name = spacing.ellipsize(&row.label, name_end.saturating_sub(left));
            frame.label(y, left..name_end, &name, color, spacing);
        }
        if rows.is_empty() {
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
        frame.section_border(layout.description, MUTED, true, |frame, slot| {
            frame.label(
                layout.description.y,
                slot,
                i18n::text("plugin_manager.description"),
                MUTED,
                spacing,
            )
        });
        let top = layout.description.y + 1;
        let lines = layout.description.h.saturating_sub(2);
        if lines == 1 {
            let text = spacing.ellipsize(description, right - left);
            frame.label(top, left..right, &text, MUTED, spacing);
        } else if lines >= 2 {
            for (index, line) in wrap_description(description, right - left, spacing)
                .iter()
                .enumerate()
            {
                frame.label(top + index, left..right, line, MUTED, spacing);
            }
        }
    }
}
