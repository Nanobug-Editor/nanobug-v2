//! Shared dialog bounds and the three-section plugin list layout.

use super::{ManagerView, ManagerWindow, Page, Prompt};
use crate::ui::components::grid_confirm::{ButtonAlign, ConfirmButton, GridConfirm};
use crate::ui::components::grid_dialog::GridDialog;
use crate::ui::components::grid_tab_bar::TitleTabs;
use crate::ui::components::{grid_key_hints::GridKeyHints, grid_text_field::FieldLayout};
use crate::{
    grid::render::{Frame, PromptRow, Rect, SEARCH_ICON},
    ui::{
        text::{TextSpacing, UNITS_PER_COLUMN},
        theme::{ACCENT, GOLD, MUTED, SELECTION, TEXT},
    },
};
use nanobug::i18n;
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

/// Labels appear in the same order as the numeric page shortcuts.
const LIST_TABS: [&str; 3] = [
    "plugin_manager.installed",
    "plugin_manager.marketplace",
    "plugin_manager.local_zip",
];
/// Maximum outer width shared by the plugin browser and its detail dialogs, in cells.
const MAX_WIDTH: usize = 112;
/// Maximum outer height shared by the plugin browser and its detail dialogs, in rows.
const MAX_HEIGHT: usize = 32;
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

impl ManagerWindow {
    /// Center every plugin dialog in the visible `cols` by `rows`, fitting its capped size.
    pub(in crate::grid) fn area(cols: usize, rows: usize) -> Rect {
        let horizontal_inset = if cols < 48 { 2 } else { 4 };
        let w = cols.saturating_sub(horizontal_inset).min(MAX_WIDTH);
        let reserved_rows = if rows >= 16 { 3 } else { 1 };
        let h = rows.saturating_sub(reserved_rows).min(MAX_HEIGHT);
        GridDialog::new(Rect {
            w: cols,
            h: rows,
            ..Rect::default()
        })
        .centered(w, h)
    }
}

impl ListLayout {
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

    /// Measure visible source titles and their adjacent click targets using `spacing`.
    pub(in crate::grid) fn tabs(
        &self,
        manager: &ManagerWindow,
        spacing: &TextSpacing,
    ) -> TitleTabs<3> {
        let local = self.list.border_title_columns(None);
        let compact = local.len() < 32;
        let columns = self.list.x + local.start..self.list.x + local.end;
        let labels = self.tab_labels(manager, spacing, columns.len(), compact);
        TitleTabs::new(columns, labels, compact, spacing)
    }

    /// Use short tab captions when the localized full set would not fit the title rule.
    fn tab_labels(
        &self,
        manager: &ManagerWindow,
        spacing: &TextSpacing,
        width: usize,
        compact: bool,
    ) -> [&'static str; 3] {
        let separator = if compact { 1 } else { 3 };
        let installed = i18n::text(LIST_TABS[0]);
        let visible = |labels: [&'static str; 3]| {
            std::array::from_fn(|index| {
                if manager.list_tab_open(Page::ALL[index]) {
                    labels[index]
                } else {
                    ""
                }
            })
        };
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
        .map(visible)
        .find(|labels| {
            let displayed = labels.iter().filter(|label| !label.is_empty());
            let gaps = displayed.clone().count().saturating_sub(1);
            displayed.map(|label| spacing.columns(label)).sum::<usize>() + gaps * separator <= width
        })
        .unwrap_or_else(|| visible(["[1]", "[2]", "[3]"]))
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

    /// Use the same clipped preedit and selection for the footer frame and its native caret.
    pub(in crate::grid) fn plugin_list_prompt(&self, columns: usize) -> FieldLayout<'_> {
        let input = self.manager.prompt.as_ref().expect("active list prompt");
        input
            .field
            .layout(&input.value, &self.composition, columns, self.spacing)
    }

    /// Draw the package list, selected description and footer on `frame` using `layout`.
    pub(in crate::grid) fn draw_plugin_list(
        &self,
        frame: &mut Frame,
        layout: &ListLayout,
    ) -> Frame {
        let manager = &self.manager;
        let spacing = self.spacing;
        let tabs = layout.tabs(manager, spacing);
        frame.section_border(layout.list, ACCENT, |frame, columns| {
            self.draw_tabs(frame, layout.list.y, tabs, manager.page as usize)
                .unwrap_or(columns.start)
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
        let visible = self.plugin_list(layout.entries.h, rows.len()).range();
        let first = visible.start;
        let mut body = Frame::viewport(layout.entries.w, layout.entries.h + 1, first);
        body.hide_theme_background();
        let local = |columns: &Range<usize>| {
            columns.start.saturating_sub(layout.entries.x)
                ..columns.end.saturating_sub(layout.entries.x)
        };
        for (offset, row) in rows[visible].iter().enumerate() {
            let y = offset;
            let color = if first + offset == selected {
                body.tint(y, 0, layout.entries.w, SELECTION);
                ACCENT
            } else {
                TEXT
            };
            let name_end = if let Some(package) = &row.package {
                let version = spacing.ellipsize(&package.version, layout.version.len());
                body.label(y, local(&layout.version), &version, color, spacing);
                let status = format!(
                    "{}{}",
                    package.status,
                    if package.update_available { " ↑" } else { "" }
                );
                body.label(y, local(&layout.status), &status, color, spacing);
                layout.name.end
            } else {
                right
            };
            let name = spacing.ellipsize(&row.label, name_end.saturating_sub(left));
            body.label(y, local(&(left..name_end)), &name, color, spacing);
        }
        if rows.is_empty() && layout.entries.h > 0 {
            body.label(
                0,
                local(&(left..right)),
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
        body
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
            let text = self.plugin_list_prompt(slots.input.len());
            let placeholder = match input.kind {
                Prompt::Development => "plugin_manager.build_output_path",
                _ => {
                    frame.icon(footer.x, footer.y, SEARCH_ICON);
                    "plugin_manager.filter"
                }
            };
            if input.value.is_empty() && self.composition.is_empty() {
                frame.label(
                    footer.y,
                    slots.input.clone(),
                    i18n::text(placeholder),
                    MUTED,
                    spacing,
                );
            } else {
                frame.text_field(footer.y, slots.input.clone(), &text, spacing);
            }
            slots.draw_buttons(frame, footer.y, spacing);
            return;
        }

        let visible_bindings = Page::ALL
            .into_iter()
            .zip(["1", "2", "3"])
            .filter_map(|(page, key)| self.manager.list_tab_open(page).then_some(key))
            .collect::<Vec<_>>()
            .join("/");
        let hints = GridKeyHints::fit(
            footer.w,
            SHORTCUTS.iter().map(|&(action, key)| {
                let key = if action == "plugin_manager.hint_tabs" {
                    visible_bindings.as_str()
                } else {
                    key
                };
                (i18n::text(action), key)
            }),
            spacing,
        );
        frame.key_hints(footer.y, footer.x, &hints, (TEXT, TEXT), spacing);
    }
}

/// First item row in a secondary dialog, below its scope, tabs, and feedback.
pub(in crate::grid) const LIST_START: usize = 4;
/// Columns reserved for Enter and Esc in secondary dialogs.
pub(in crate::grid) const SEARCH_BUTTONS: usize = 23;

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
pub(in crate::grid) fn prompt_buttons(
    left: usize,
    spacing: &TextSpacing,
) -> Vec<ConfirmButton<bool>> {
    GridConfirm::enter_escape().slots(left..left + 18, ButtonAlign::Start, 2, spacing)
}

/// Resolve local `column` in the row measured from `left` with `spacing`. True cancels.
pub(in crate::grid) fn prompt_button_at(
    left: usize,
    column: usize,
    spacing: &TextSpacing,
) -> Option<bool> {
    GridConfirm::hit(&prompt_buttons(left, spacing), column).map(|accept| !accept)
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
