//! Derive manager frames and hit-test geometry from one immutable presentation.

use super::{
    Detail, ManagerWindow, Prompt, comparison,
    layout::*,
    rows::{self, Row, RowContext},
};
use crate::{
    grid::render::{Frame, LabelStyle, OutlineStyle, Rect},
    ui::{
        ime::Composition,
        text::{TextSpacing, UNITS_PER_COLUMN},
        theme::{ACCENT, GOLD, MUTED, TEXT},
    },
};
use nanobug::i18n;
use std::ops::{Deref, Range};
use unicode_segmentation::UnicodeSegmentation;

/// Editor-side operations supply only their visible content, without exposing task state.
pub(in crate::grid) struct Presentation {
    /// Optional title of a host operation that takes precedence over package browsing.
    pub title: Option<String>,
    /// Prepared operation rows, or ordinary package rows when absent.
    pub rows: Option<Vec<Row>>,
    /// Footer status for the document targeted by an operation.
    pub document_hint: Option<&'static str>,
    /// Feedback shown when a browser or operation has no visible rows.
    pub empty_label: &'static str,
    /// Resolved command bindings shown in the shortcuts page.
    pub bindings: Vec<String>,
}

/// One source for drawing, caret placement, scrolling and pointer hit testing.
pub(in crate::grid) struct ManagerView<'a> {
    /// Local navigation and input state used by all geometry calculations.
    pub manager: &'a ManagerWindow,
    /// Read-only package, project and document data.
    pub data: RowContext<'a>,
    /// Native preedit displayed without committing it to the prompt.
    pub composition: &'a Composition,
    /// Shared character measurement for rendering and hit testing.
    pub spacing: &'a TextSpacing,
    /// Host operation content prepared without exposing workspace state.
    pub presentation: Presentation,
}

/// Ordinary browser rows are derived lazily; operation rows are already prepared by the host.
pub(in crate::grid) enum VisibleRows<'a> {
    Browser(Vec<Row>),
    Operation(&'a [Row]),
}
impl Deref for VisibleRows<'_> {
    type Target = [Row];
    fn deref(&self) -> &Self::Target {
        match self {
            Self::Browser(rows) => rows,
            Self::Operation(rows) => rows,
        }
    }
}

impl ManagerView<'_> {
    pub(in crate::grid) fn rows(&self) -> VisibleRows<'_> {
        match &self.presentation.rows {
            Some(rows) => VisibleRows::Operation(rows),
            None => VisibleRows::Browser(rows::build(&self.data)),
        }
    }
    pub(in crate::grid) fn plugin_prompt_text(&self, columns: usize) -> (String, String) {
        let Some(input) = &self.manager.prompt else {
            return (String::new(), String::new());
        };
        let label = match &input.kind {
            Prompt::Search { .. } => "/ ",
            Prompt::Development => "Build output path  ",
            Prompt::DevSource { .. } => "Cargo project path  ",
            Prompt::Rule { .. } => "Rule  ",
            Prompt::Setting { .. } => "Value  ",
            Prompt::Credential { .. } => "New credential  ",
            Prompt::ExportDraft => "Export path  ",
            Prompt::Shortcut { .. } => "Shortcut  ",
        };
        if let Prompt::Credential { value, .. } = &input.kind {
            let count = value
                .graphemes(true)
                .count()
                .min(columns.saturating_sub(label.len() + 1));
            return (label.into(), "•".repeat(count));
        }
        let content = input.value.rsplit('\n').next().unwrap_or_default();
        (
            label.into(),
            tail(content, columns.saturating_sub(label.len() + 1)),
        )
    }

    /// Compose the manager's content and frame decorations within `area`.
    pub(in crate::grid) fn manager_frame(&self, area: Rect) -> Frame {
        let manager = &self.manager;
        let mut frame = Frame::new(area.w, area.h);
        let manifest = rows::manifest(&self.data);
        let title = if let Some(title) = &self.presentation.title {
            title.clone()
        } else if manager.help {
            i18n::text("plugin_manager.shortcuts").into()
        } else if self.data.view.review.is_some() {
            i18n::text("plugin_manager.install").into()
        } else if manager.confirm_remove.is_some() {
            i18n::text("plugin_manager.uninstall").into()
        } else if let Some(preview) = self.comparison_preview() {
            i18n::text(preview.source.title()).into()
        } else {
            manifest.map_or_else(
                || i18n::text("plugin_manager.title").into(),
                |manifest| i18n::format("plugin_manager.details", &[("name", &manifest.name)]),
            )
        };
        frame.border(&title, true, false, self.spacing);
        if self.uses_plugin_list_layout() {
            self.draw_plugin_list(&mut frame, &self.plugin_list_layout(area));
            return frame;
        }
        let right = area.w.saturating_sub(2);
        let document_list = self.presentation.document_hint.is_some();
        if manager.help {
            for (index, line) in rows::HELP.iter().enumerate() {
                frame.label(index + 1, 2..right, line, TEXT, self.spacing);
            }
            for (index, binding) in self
                .presentation
                .bindings
                .iter()
                .enumerate()
                .take(area.h.saturating_sub(rows::HELP.len() + 2))
            {
                frame.label(
                    rows::HELP.len() + index + 1,
                    2..right,
                    binding,
                    TEXT,
                    self.spacing,
                );
            }
            return frame;
        }
        if let Some(review) = &self.data.view.review {
            frame.label(
                1,
                2..right,
                &format!(
                    "{}  {}  ({})",
                    review.manifest.name, review.manifest.version, review.manifest.id
                ),
                TEXT,
                self.spacing,
            );
            frame.label(
                3,
                2..right,
                if review.additional.is_empty() && review.additional_rule_paths.is_empty() {
                    i18n::text("plugin_manager.no_new_permissions")
                } else {
                    i18n::text("plugin_manager.allow_permissions")
                },
                GOLD,
                self.spacing,
            );
            let (pages, page) = self.install_review_page(review, area);
            for (row, line) in pages[page].iter().enumerate() {
                frame.label(row + 4, 2..right, line, TEXT, self.spacing);
            }
            if pages.len() > 1 {
                frame.label(
                    area.h - 3,
                    2..right,
                    &format!("{} / {}   ← Previous   → Next", page + 1, pages.len()),
                    MUTED,
                    self.spacing,
                );
            }
            self.prompt_buttons(&mut frame, area.h - 2, right.saturating_sub(SEARCH_BUTTONS));
            if page + 1 < pages.len() {
                frame.label(
                    area.h - 2,
                    2..right.saturating_sub(SEARCH_BUTTONS),
                    "Enter continues review",
                    MUTED,
                    self.spacing,
                );
            }
            return frame;
        }
        if manager.confirm_remove.is_some() {
            frame.label(
                2,
                2..right,
                "Uninstall this plugin? Local settings and drafts will be kept.",
                TEXT,
                self.spacing,
            );
            self.prompt_buttons(&mut frame, area.h - 2, right.saturating_sub(SEARCH_BUTTONS));
            return frame;
        }
        if manager.prompt.is_some() {
            let (label, text) = self.plugin_prompt_text(area.w.saturating_sub(SEARCH_BUTTONS + 5));
            let preedit = if manager
                .prompt
                .as_ref()
                .is_some_and(|input| matches!(input.kind, Prompt::Credential { .. }))
            {
                "•".repeat(self.composition.text.graphemes(true).count())
            } else {
                self.composition.text.clone()
            };
            let displayed = label + &text + &preedit;
            frame.label(
                1,
                2..right.saturating_sub(SEARCH_BUTTONS),
                &displayed,
                TEXT,
                self.spacing,
            );
            self.prompt_buttons(&mut frame, 1, right.saturating_sub(SEARCH_BUTTONS));
        } else if !document_list
            && matches!(
                manager.detail_page,
                Detail::Rules | Detail::Settings | Detail::Compare
            )
        {
            frame.label(
                1,
                2..right,
                self.manager.scope_label(self.data.active),
                MUTED,
                self.spacing,
            );
        }
        let (labels, selected): (&[_], usize) = if document_list {
            (&[], 0)
        } else if manager.detail.is_some() {
            (
                &[
                    "plugin_manager.overview",
                    "plugin_manager.modules",
                    "plugin_manager.rules",
                    "plugin_manager.logs",
                ],
                match manager.detail_page {
                    Detail::Overview | Detail::Settings => 0,
                    Detail::Modules => 1,
                    Detail::Rules | Detail::Compare => 2,
                    Detail::Logs | Detail::Operations => 3,
                    Detail::Cleanup => 0,
                },
            )
        } else {
            (&[], 0)
        };
        let mut x = 2;
        for (index, label) in labels.iter().enumerate() {
            if index != 0 {
                x = frame.label(2, x..right, " | ", MUTED, self.spacing);
            }
            x = frame.label(
                2,
                x..right,
                i18n::text(label),
                if index == selected { ACCENT } else { MUTED },
                self.spacing,
            );
        }
        let entries = secondary_list_entries(area);
        let preview = self.comparison_preview();
        let lines = preview.map(|preview| preview.lines(right.saturating_sub(2), self.spacing));
        let rows = if preview.is_some() {
            VisibleRows::Operation(&[])
        } else {
            self.rows()
        };
        let count = lines.as_ref().map_or(rows.len(), |lines| lines.len());
        let selected = manager.selection.min(count.saturating_sub(1));
        let visible = self.plugin_visible_rows(entries.h, count);
        let offset = visible.start;
        for index in visible {
            let text = match (preview, &lines) {
                (Some(preview), Some(lines)) => preview.line(lines[index].clone()),
                _ => &rows[index].label,
            };
            frame.label(
                entries.y + index - offset,
                2..right,
                text,
                if index == selected { ACCENT } else { TEXT },
                self.spacing,
            );
        }
        if count == 0 {
            frame.label(
                entries.y,
                2..right,
                self.presentation.empty_label,
                MUTED,
                self.spacing,
            );
        }
        let status = manager
            .feedback
            .as_deref()
            .unwrap_or(&self.data.view.message);
        if !document_list && !status.is_empty() {
            frame.label(
                3,
                2..right,
                status.lines().next().unwrap_or(""),
                GOLD,
                self.spacing,
            );
        }
        let position = preview.map(|_| {
            i18n::format(
                "plugin_manager.compare_position",
                &[("current", &(selected + 1)), ("total", &count)],
            )
        });
        let description = position
            .as_deref()
            .or_else(|| rows.get(selected).map(|row| row.description.as_str()))
            .unwrap_or_default();
        let description_area = secondary_description(area);
        if document_list {
            frame.outline(description_area, MUTED, OutlineStyle::DashedSquare, []);
        }
        for (index, line) in wrap_description(description, right.saturating_sub(3), self.spacing)
            .into_iter()
            .enumerate()
        {
            frame.label(
                description_area.y + 1 + index,
                3..right,
                &line,
                if document_list { MUTED } else { TEXT },
                self.spacing,
            );
        }
        if document_list {
            let hint = self.presentation.document_hint.expect("document results");
            frame.styled_label(
                area.h - 2,
                2..right,
                i18n::text(hint),
                TEXT,
                self.spacing,
                LabelStyle::Dimmed,
            );
        }
        frame
    }

    /// Only show the preview while the comparison owns the manager's current focus.
    pub(in crate::grid) fn comparison_preview(&self) -> Option<&comparison::Preview> {
        if self.manager.detail_page != Detail::Compare
            || self.presentation.rows.is_some()
            || self.data.view.waiting_for_operations
        {
            return None;
        }
        self.manager.comparison.as_ref()?.preview.as_ref()
    }

    /// Count exactly the visual rows used by rendering, the cursor and pointer hit testing.
    pub(in crate::grid) fn plugin_row_count(&self, area: Rect) -> usize {
        self.comparison_preview().map_or_else(
            || self.rows().len(),
            |preview| preview.lines(area.w.saturating_sub(4), self.spacing).len(),
        )
    }

    /// Resolve visible row indices for drawing, caret placement and hit testing after list changes.
    pub(in crate::grid) fn plugin_visible_rows(
        &self,
        capacity: usize,
        count: usize,
    ) -> Range<usize> {
        let manager = &self.manager;
        let selected = manager.selection.min(count.saturating_sub(1));
        let offset =
            list_offset(manager.offset, selected, capacity).min(count.saturating_sub(capacity));
        offset..(offset + capacity).min(count)
    }

    /// Draw the prompt actions on `row`, starting at the same column used for mouse input.
    pub(in crate::grid) fn prompt_buttons(&self, frame: &mut Frame, row: usize, start: usize) {
        for (slot, (prefix, key)) in prompt_button_columns(start)
            .into_iter()
            .zip([("[✓ ", "Enter"), ("[✕ ", "Esc")])
        {
            let start = frame.label(row, slot.clone(), prefix, TEXT, self.spacing);
            let end = frame.styled_label(
                row,
                start..slot.end,
                key,
                TEXT,
                self.spacing,
                LabelStyle::Key,
            );
            frame.label(row, end..slot.end, "]", TEXT, self.spacing);
        }
    }

    /// Keep every displayed permission and external path visible before granting access.
    pub(in crate::grid) fn install_review_page(
        &self,
        review: &nanobug::plugins::session::InstallReview,
        area: Rect,
    ) -> (Vec<Vec<String>>, usize) {
        let columns = area.w.saturating_sub(4).max(1) * UNITS_PER_COLUMN;
        let capacity = area.h.saturating_sub(7).max(1);
        let mut lines = Vec::new();
        for text in review
            .additional
            .iter()
            .map(|permission| permission.description().to_owned())
            .chain(
                review
                    .additional_rule_paths
                    .iter()
                    .map(|path| path.display().to_string()),
            )
        {
            let mut remaining = text.as_str();
            while !remaining.is_empty() {
                let end = self
                    .spacing
                    .fitting_cells(remaining, columns)
                    .last()
                    .map_or_else(
                        || {
                            remaining
                                .graphemes(true)
                                .next()
                                .expect("nonempty review line")
                                .len()
                        },
                        |cell| cell.byte + cell.text.len(),
                    );
                lines.push(remaining[..end].to_owned());
                remaining = &remaining[end..];
            }
        }
        let mut pages = lines
            .chunks(capacity)
            .map(<[String]>::to_vec)
            .collect::<Vec<_>>();
        if pages.is_empty() {
            pages.push(Vec::new());
        }
        let current = if self.manager.review_token == Some(review.token) {
            self.manager.review_page.min(pages.len() - 1)
        } else {
            0
        };
        (pages, current)
    }

    pub(in crate::grid) fn caret(&self, area: Rect) -> Option<(f32, f32, f32, bool, bool)> {
        let manager = &self.manager;
        if manager.help || manager.confirm_remove.is_some() || self.data.view.review.is_some() {
            return Some((0., 0., 1., false, false));
        }
        let layout = self
            .uses_plugin_list_layout()
            .then(|| self.plugin_list_layout(area));
        if let Some(input) = &manager.prompt {
            let (column, row, units, visible) = if let Some(layout) = &layout {
                let slot = layout.prompt(&input.kind, self.spacing).input;
                let (_, units) = self.plugin_list_prompt(slot.len());
                (
                    slot.start,
                    layout.footer.y,
                    units,
                    layout.footer.h > 0 && !slot.is_empty(),
                )
            } else {
                let (label, text) =
                    self.plugin_prompt_text(area.w.saturating_sub(SEARCH_BUTTONS + 5));
                let units = self
                    .spacing
                    .cells(&(label + &text))
                    .last()
                    .map_or(0, |cell| cell.end);
                (2, 1, units, true)
            };
            return Some((
                (area.x + column) as f32 + units as f32 / UNITS_PER_COLUMN as f32,
                (area.y + row) as f32,
                1.,
                true,
                visible,
            ));
        }
        let entries = layout.map_or_else(|| secondary_list_entries(area), |layout| layout.entries);
        let len = self.plugin_row_count(area);
        let selected = manager.selection.min(len.saturating_sub(1));
        let visible = self.plugin_visible_rows(entries.h, len);
        Some((
            (area.x + entries.x) as f32,
            (area.y + entries.y + selected.saturating_sub(visible.start)) as f32,
            1.,
            false,
            visible.contains(&selected),
        ))
    }
}
