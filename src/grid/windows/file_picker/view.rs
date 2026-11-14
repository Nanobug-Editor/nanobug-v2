//! Render the three browser columns, prompt, and selection in the shared character grid.

use super::{FilePicker, PickerEntry, Preview, PromptKind, delete_buttons, display_path, same_row};
use crate::ui::file_icons::EntryIcon;
use crate::{
    grid::render::{Frame, MessageLevel, Rect},
    ui::{
        text::{TextSpacing, UNITS_PER_COLUMN},
        theme::{ACCENT, CURRENT, GOLD, MUTED, SELECTION, TEXT},
    },
};
use nanobug::i18n;
use std::{borrow::Cow, ops::Range, path::Path};

/// Layout in window cells. Column rectangles are relative to `area`.
#[derive(Clone, Copy)]
pub(in crate::grid) struct PickerLayout {
    /// Outer border and all picker content.
    pub area: Rect,
    /// Parent directory rows.
    pub parent: Rect,
    /// Current directory rows.
    pub current: Rect,
    /// Preview rows.
    pub preview: Rect,
}

impl PickerLayout {
    /// Split `area` in a 2:3:4 ratio; `picker` reserves footer rows only for inline commands.
    pub fn new(area: Rect, picker: Option<&FilePicker>) -> Self {
        let usable = area.w.saturating_sub(4);
        let parent = usable * 2 / 9;
        let current = usable * 3 / 9;
        let first = 2;
        let input_rows = picker.map_or(0, |picker| {
            if picker.delete.is_some() {
                2
            } else {
                usize::from(
                    picker
                        .prompt
                        .as_ref()
                        .is_some_and(|prompt| !prompt.kind.is_floating())
                        || picker.goto,
                )
            }
        });
        let row = |x, w| Rect {
            x,
            y: first,
            w,
            h: area.h.saturating_sub(first + input_rows + 1),
        };
        Self {
            area,
            parent: row(1, parent),
            current: row(parent + 2, current),
            preview: row(parent + current + 3, usable - parent - current),
        }
    }

    /// Divide `area` into the selected file's parent list and its focused preview.
    pub fn for_file(area: Rect) -> Self {
        let usable = area.w.saturating_sub(3);
        let left = usable / 3;
        let h = area.h.saturating_sub(3);
        Self {
            area,
            parent: Rect {
                x: 1,
                y: 2,
                w: left,
                h,
            },
            current: Rect::default(),
            preview: Rect {
                x: left + 2,
                y: 2,
                w: usable - left,
                h,
            },
        }
    }

    /// Locate the focused preview pane, including its shared separator and heading.
    pub fn preview_editor_area(self) -> Rect {
        Rect {
            x: self.area.x + self.preview.x - 1,
            y: self.area.y + self.preview.y - 1,
            w: self.preview.w + 2,
            h: self.preview.h + 2,
        }
    }

    /// Text-only preview bounds, leaving five columns for line numbers.
    pub fn text_area(self) -> Rect {
        Rect {
            x: self.area.x + self.preview.x + 5,
            y: self.area.y + self.preview.y,
            w: self.preview.w.saturating_sub(5),
            h: self.preview.h,
        }
    }

    /// Lay out `message` above the preview with severity `level` and measured text `spacing`.
    pub fn message_frame(
        self,
        level: MessageLevel,
        message: &str,
        spacing: &TextSpacing,
    ) -> (Rect, Frame) {
        let width = spacing
            .columns(message)
            .max(level.heading_columns(spacing))
            .min(self.preview.w.saturating_sub(4))
            .min(54);
        let mut remaining = message;
        let mut lines = Vec::new();
        for _ in 0..2 {
            let end = spacing
                .fitting_cells(remaining, width * UNITS_PER_COLUMN)
                .last()
                .map_or(0, |cell| cell.byte + cell.text.len());
            if end == 0 || end == remaining.len() {
                break;
            }
            let end = if remaining[end..].starts_with(' ') {
                end
            } else {
                remaining[..end]
                    .rfind(' ')
                    .filter(|&split| split > 0)
                    .unwrap_or(end)
            };
            lines.push(Cow::Borrowed(&remaining[..end]));
            remaining = remaining[end..].trim_start();
        }
        lines.push(spacing.ellipsize(remaining, width));
        let w = width + 4;
        let area = Rect {
            x: self.area.x + self.area.w.saturating_sub(w + 1),
            y: self.area.y + 1,
            w,
            h: lines.len() + 2,
        };
        let (_, text) = level.colors();
        let mut frame = Frame::new(area.w, area.h);
        frame.message_border(level, spacing);
        for (row, line) in lines.into_iter().enumerate() {
            frame.label(row + 1, 2..area.w - 2, &line, text, spacing);
        }
        (area, frame)
    }

    /// Center the search or filter box beneath the column headings.
    fn floating_prompt_area(self) -> Rect {
        let w = self.area.w.saturating_sub(4).min(64);
        let h = 3;
        Rect {
            x: self.area.x + (self.area.w - w) / 2,
            y: self.area.y + self.current.y.min(self.area.h.saturating_sub(h)),
            w,
            h,
        }
    }

    /// Relative row of the active prompt, jump hints or confirmation question.
    fn input_row(self) -> usize {
        self.current.y + self.current.h
    }

    /// Column rules in cells relative to the picker area.
    pub(in crate::grid) fn dividers(self) -> [Rect; 2] {
        let y = self.current.y - 1;
        [self.parent, self.current].map(|column| Rect {
            x: column.x + column.w,
            y,
            w: 1,
            h: self.input_row() - y,
        })
    }

    /// Relative row of the deletion confirmation buttons.
    fn delete_buttons_row(self) -> usize {
        self.area.h.saturating_sub(2)
    }

    /// Positions of the localized delete buttons and their actions.
    fn buttons(self, spacing: &TextSpacing) -> impl Iterator<Item = (bool, Range<usize>)> {
        let mut start = 2;
        delete_buttons().into_iter().map(move |(confirm, caption)| {
            let width: usize = caption
                .spans()
                .into_iter()
                .map(|(text, _)| spacing.columns(text))
                .sum();
            let slot = start..(start + width).min(self.area.w.saturating_sub(1));
            start = slot.end + 2;
            (confirm, slot)
        })
    }

    /// Resolve a click in area-relative `x`, `y` to the displayed confirmation button.
    pub fn delete_hit(self, x: usize, y: usize, spacing: &TextSpacing) -> Option<bool> {
        (y == self.delete_buttons_row()).then_some(())?;
        self.buttons(spacing)
            .find_map(|(confirm, slot)| slot.contains(&x).then_some(confirm))
    }
}

impl FilePicker {
    /// Draw the parent of `path` beside the preview, with optional loading or failure `status`.
    pub fn file_frame(
        &self,
        layout: PickerLayout,
        path: &Path,
        status: Option<&str>,
        active: bool,
        controls_hovered: bool,
        spacing: &TextSpacing,
    ) -> Frame {
        let PickerLayout {
            area,
            parent,
            preview,
            ..
        } = layout;
        let mut frame = picker_border(area, &display_path(path), active, controls_hovered, spacing);
        for (rect, key) in [
            (parent, "picker.parent"),
            (preview, "picker.readonly_preview"),
        ] {
            frame.label(
                1,
                rect.x + 1..rect.x + rect.w,
                i18n::text(key),
                MUTED,
                spacing,
            );
        }
        for row in 1..area.h.saturating_sub(1) {
            frame.text(parent.x + parent.w, row, "│", MUTED, 1);
        }
        if let Some(listing) = &self.listing {
            let selected = self.visible.get(self.selected).copied().unwrap_or(0);
            let first = selected.saturating_add(1).saturating_sub(parent.h);
            for (row, entry) in listing
                .entries
                .iter()
                .skip(first)
                .take(parent.h)
                .enumerate()
            {
                self.draw_row(
                    &mut frame,
                    entry,
                    parent,
                    row,
                    same_row(&entry.path, path),
                    false,
                    false,
                    spacing,
                );
            }
        }
        if let Some(status) = status {
            let slot = preview.x + 1..preview.x + preview.w;
            frame.label(
                preview.y,
                slot.clone(),
                &spacing.ellipsize(status, slot.len()),
                MUTED,
                spacing,
            );
        }
        frame
    }

    /// Show the committed query in `width` columns using the grid's text `spacing`.
    fn current_heading(&self, width: usize, spacing: &TextSpacing) -> Cow<'_, str> {
        if self.filter.is_empty() {
            return Cow::Borrowed(i18n::text("picker.current"));
        }
        let prefix = format!("[{}: ", i18n::text("picker.filter"));
        let query = spacing.ellipsize(
            &self.filter,
            width.saturating_sub(spacing.columns(&prefix) + 1),
        );
        Cow::Owned(format!("{prefix}{query}]"))
    }

    /// Draw picker chrome with shared `spacing`, focus `active` and visible `controls_hovered`.
    pub fn frame(
        &self,
        layout: PickerLayout,
        active: bool,
        controls_hovered: bool,
        spacing: &TextSpacing,
    ) -> Frame {
        let PickerLayout {
            area,
            parent,
            current,
            preview,
        } = layout;
        let title = self.listing.as_ref().map_or(Cow::Borrowed(""), |listing| {
            listing
                .directory
                .as_deref()
                .map_or(Cow::Borrowed(i18n::text("picker.drives")), display_path)
        });
        let mut frame = picker_border(area, &title, active, controls_hovered, spacing);
        let headings = current.y - 1;
        let current_label = self.current_heading(current.w.saturating_sub(1), spacing);
        for (rect, label) in [
            (parent, i18n::text("picker.parent")),
            (current, current_label.as_ref()),
        ] {
            frame.label(headings, rect.x + 1..rect.x + rect.w, label, MUTED, spacing);
        }
        let preview_label = i18n::text("picker.preview");
        let preview_label = if self.delete.is_none()
            && matches!(&self.preview, Preview::Text(text) if text.truncated)
        {
            Cow::Owned(format!(
                "{preview_label} {}",
                i18n::text("picker.truncated")
            ))
        } else {
            Cow::Borrowed(preview_label)
        };
        frame.label(
            headings,
            preview.x + 1..preview.x + preview.w,
            &preview_label,
            MUTED,
            spacing,
        );
        for divider in layout.dividers() {
            for row in divider.y..divider.y + divider.h {
                frame.text(divider.x, row, "│", MUTED, 1);
            }
        }
        if let Some(listing) = &self.listing {
            let parent_first = self.parent_first(layout);
            for (row, entry) in listing
                .parent
                .iter()
                .skip(parent_first)
                .take(parent.h)
                .enumerate()
            {
                let selected = self
                    .directory()
                    .is_some_and(|path| same_row(&entry.path, path));
                self.draw_row(
                    &mut frame, entry, parent, row, selected, false, false, spacing,
                );
            }
        }
        for (row, index) in (self.first..self.first + current.h).enumerate() {
            let Some(entry) = self.entry_at(index) else {
                break;
            };
            self.draw_row(
                &mut frame,
                entry,
                current,
                row,
                index == self.selected,
                active,
                self.search_rows.binary_search(&index).is_ok(),
                spacing,
            );
        }
        if self.visible.is_empty() && !self.busy {
            let key = if self
                .listing
                .as_ref()
                .is_some_and(|listing| listing.entries.is_empty())
            {
                "picker.empty"
            } else {
                "picker.no_matches"
            };
            frame.label(
                current.y,
                current.x + 1..current.x + current.w,
                i18n::text(key),
                MUTED,
                spacing,
            );
        }
        self.draw_preview(&mut frame, layout, spacing);
        self.draw_footer(&mut frame, layout, spacing);
        frame
    }

    /// First visible parent row, keeping the current directory in view.
    pub fn parent_first(&self, layout: PickerLayout) -> usize {
        self.listing
            .as_ref()
            .and_then(|listing| {
                let path = listing.directory.as_deref()?;
                listing
                    .parent
                    .iter()
                    .position(|entry| same_row(&entry.path, path))
            })
            .map_or(0, |row| {
                row.saturating_add(1).saturating_sub(layout.parent.h)
            })
    }

    /// Draw `entry` at offset `row` inside `rect` with the selected, active and matched states.
    #[allow(clippy::too_many_arguments)]
    fn draw_row(
        &self,
        frame: &mut Frame,
        entry: &PickerEntry,
        rect: Rect,
        row: usize,
        selected: bool,
        active: bool,
        matched: bool,
        spacing: &TextSpacing,
    ) {
        let y = rect.y + row;
        if selected {
            frame.tint(
                y,
                rect.x,
                rect.x + rect.w,
                if active { SELECTION } else { CURRENT },
            );
        }
        if self.marked.contains(&entry.path) {
            frame.text(rect.x + 1, y, "●", GOLD, 1);
        }
        frame.icon(
            rect.x + 2,
            y,
            EntryIcon::for_path(&entry.path, entry.directory, selected).image(),
        );
        let slot = rect.x + 4..rect.x + rect.w;
        let color = if matched {
            GOLD
        } else if selected && active {
            ACCENT
        } else {
            TEXT
        };
        frame.label(
            y,
            slot.clone(),
            &spacing.ellipsize(&entry.name(), slot.len()),
            color,
            spacing,
        );
    }

    /// Draw directory rows or the text gutter and preview status in the right column.
    fn draw_preview(&self, frame: &mut Frame, layout: PickerLayout, spacing: &TextSpacing) {
        let rect = layout.preview;
        if let Some(delete) = &self.delete {
            let slot = rect.x + 1..rect.x + rect.w;
            for (row, path) in delete
                .paths
                .iter()
                .skip(self.preview_first)
                .take(rect.h)
                .enumerate()
            {
                let name = path
                    .file_name()
                    .unwrap_or(path.as_os_str())
                    .to_string_lossy();
                let directory = path.parent().map_or(Cow::Borrowed(""), display_path);
                let caption = format!("{name} · {directory}");
                frame.label(
                    rect.y + row,
                    slot.clone(),
                    &spacing.ellipsize(&caption, slot.len()),
                    TEXT,
                    spacing,
                );
            }
            return;
        }
        let label = match &self.preview {
            Preview::Directory(entries) if !entries.is_empty() => {
                for (row, entry) in entries
                    .iter()
                    .skip(self.preview_first)
                    .take(rect.h)
                    .enumerate()
                {
                    self.draw_row(frame, entry, rect, row, false, false, false, spacing);
                }
                return;
            }
            Preview::Text(_) => {
                for row in 0..rect
                    .h
                    .min(self.preview.rows().saturating_sub(self.preview_first))
                {
                    frame.right_label(
                        rect.y + row,
                        rect.x + 4,
                        &(row + self.preview_first + 1).to_string(),
                        MUTED,
                        spacing,
                    );
                }
                return;
            }
            Preview::Directory(_) => i18n::text("picker.empty"),
            Preview::Empty => return,
            Preview::Loading => i18n::text("picker.loading"),
            Preview::Unavailable => i18n::text("picker.unavailable"),
            Preview::Failed(error) => error,
        };
        frame.label(rect.y, rect.x + 1..rect.x + rect.w, label, MUTED, spacing);
        if matches!(self.preview, Preview::Unavailable) && rect.h > 1 {
            let executable = cfg!(windows)
                && self.entry().is_some_and(|entry| {
                    entry
                        .path
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
                });
            let key = if executable {
                "picker.run_hint"
            } else {
                "picker.external_hint"
            };
            frame.label(
                rect.y + 1,
                rect.x + 1..rect.x + rect.w,
                i18n::text(key),
                MUTED,
                spacing,
            );
        }
    }

    /// Build the separate text frame for the right column using the current syntax spans.
    pub fn text_frame(&self, layout: PickerLayout, spacing: &TextSpacing) -> Option<Frame> {
        if self.delete.is_some() {
            return None;
        }
        let Preview::Text(text) = &self.preview else {
            return None;
        };
        let rect = layout.text_area();
        let mut frame = Frame::new(rect.w, rect.h);
        for row in 0..rect.h {
            let Some((offset, line)) = text.line(row + self.preview_first) else {
                break;
            };
            frame.viewport_line(row, spacing.cells(line), 0..rect.w, 0, |byte| {
                (text.color(offset + byte).unwrap_or(TEXT), 0, false)
            });
        }
        Some(frame)
    }

    /// Return the bounds in `layout` of an active search or filter prompt.
    pub fn floating_prompt_area(&self, layout: PickerLayout) -> Option<Rect> {
        self.prompt
            .as_ref()
            .filter(|prompt| prompt.kind.is_floating())
            .map(|_| layout.floating_prompt_area())
    }

    /// Draw the floating prompt and its title using `layout` and text `spacing`.
    pub fn floating_prompt_frame(
        &self,
        layout: PickerLayout,
        spacing: &TextSpacing,
    ) -> Option<Frame> {
        let area = self.floating_prompt_area(layout)?;
        let mut frame = Frame::new(area.w, area.h);
        frame.border(i18n::text(self.prompt_label()), true, false, spacing);
        self.draw_prompt(&mut frame, layout, spacing);
        Some(frame)
    }

    /// Draw inline input and confirmations into `frame` using `layout` and text `spacing`.
    fn draw_footer(&self, frame: &mut Frame, layout: PickerLayout, spacing: &TextSpacing) {
        let end = layout.area.w - 2;
        let input = layout.input_row();
        if let Some(delete) = &self.delete {
            let key = if delete.permanent {
                "picker.confirm_delete"
            } else {
                "picker.confirm_trash"
            };
            let target = delete
                .paths
                .first()
                .and_then(|path| path.file_name())
                .map_or(Cow::Borrowed(""), |name| name.to_string_lossy());
            let question = i18n::format(key, &[("count", &delete.paths.len()), ("path", &target)]);
            frame.label(
                input,
                2..end,
                &spacing.ellipsize(&question, end - 2),
                GOLD,
                spacing,
            );
            for ((_, slot), (_, caption)) in layout.buttons(spacing).zip(delete_buttons()) {
                let mut x = slot.start;
                for (text, style) in caption.spans() {
                    x = frame.styled_label(
                        layout.delete_buttons_row(),
                        x..slot.end,
                        text,
                        TEXT,
                        spacing,
                        style,
                    );
                }
            }
            return;
        }
        if self.prompt.is_some() {
            if self.floating_prompt_area(layout).is_none() {
                self.draw_prompt(frame, layout, spacing);
            }
        } else if self.goto {
            frame.label(
                input,
                2..end,
                i18n::text("picker.goto_hints"),
                MUTED,
                spacing,
            );
        }
    }

    /// Paint input into its owning `frame`, using `layout` and `spacing` for caret-aligned scrolling.
    fn draw_prompt(&self, frame: &mut Frame, layout: PickerLayout, spacing: &TextSpacing) {
        let Some((start, caret, first)) = self.prompt_view(layout, spacing) else {
            return;
        };
        let prompt = self.prompt.as_ref().expect("prompt view");
        let floating = self.floating_prompt_area(layout);
        let input = floating.map_or(layout.input_row(), |_| 1);
        let end = floating.unwrap_or(layout.area).w - 2;
        if floating.is_none() {
            frame.label(
                input,
                2..start,
                i18n::text(self.prompt_label()),
                ACCENT,
                spacing,
            );
        }
        let mut text = prompt.value[first..prompt.caret].to_owned();
        text.push_str(&self.composition.text);
        text.push_str(&prompt.value[prompt.caret..]);
        let (anchor, cursor) = if prompt.anchor < prompt.caret {
            (prompt.anchor, prompt.caret)
        } else {
            (prompt.caret, prompt.anchor)
        };
        if cursor > first && cursor != anchor {
            let x = start + spacing.columns(&prompt.value[first..anchor.max(first)]);
            frame.tint(
                input,
                x,
                (x + spacing.columns(&prompt.value[anchor.max(first)..cursor])).min(end),
                SELECTION,
            );
        }
        frame.label(input, start..end, &text, TEXT, spacing);
        debug_assert!(caret < end);
    }

    /// Catalog key for the label of the active prompt.
    fn prompt_label(&self) -> &'static str {
        match &self.prompt.as_ref().expect("active prompt").kind {
            PromptKind::Filter => "picker.filter",
            PromptKind::Search => "picker.search",
            PromptKind::Jump => "picker.jump",
            PromptKind::Create => "picker.create",
            PromptKind::Rename(_) => "picker.rename",
        }
    }

    /// Return panel-local text/caret columns and the first visible byte for `layout` and `spacing`.
    fn prompt_view(
        &self,
        layout: PickerLayout,
        spacing: &TextSpacing,
    ) -> Option<(usize, usize, usize)> {
        let prompt = self.prompt.as_ref()?;
        let floating = self.floating_prompt_area(layout);
        let area = floating.unwrap_or(layout.area);
        let start = if floating.is_some() {
            2
        } else {
            (spacing.columns(i18n::text(self.prompt_label())) + 3).min(area.w - 4)
        };
        let prefix = &prompt.value[..prompt.caret];
        let width = spacing.columns(prefix) * UNITS_PER_COLUMN;
        let available = (area.w - 3 - start) * UNITS_PER_COLUMN;
        let first = spacing
            .cells(prefix)
            .find(|cell| width.saturating_sub(cell.start) <= available)
            .map_or(prefix.len(), |cell| cell.byte);
        Some((start, start + spacing.columns(&prefix[first..]), first))
    }

    /// Return the window-relative caret and input/visibility flags for `layout` and text `spacing`.
    pub fn caret(
        &self,
        layout: PickerLayout,
        spacing: &TextSpacing,
    ) -> (f32, f32, f32, bool, bool) {
        let area = layout.area;
        if let Some((_, x, _)) = self.prompt_view(layout, spacing) {
            let floating = self.floating_prompt_area(layout);
            let input_area = floating.unwrap_or(area);
            return (
                (input_area.x + x) as f32,
                (input_area.y + floating.map_or(layout.input_row(), |_| 1)) as f32,
                1.,
                true,
                true,
            );
        }
        let row = self.selected.saturating_sub(self.first);
        (
            (area.x + layout.current.x) as f32,
            (area.y + layout.current.y + row) as f32,
            1.,
            false,
            self.entry().is_some() && row < layout.current.h && self.delete.is_none(),
        )
    }
}

/// Draw the picker boundary and fitted `title` for either column layout.
fn picker_border(
    area: Rect,
    title: &str,
    active: bool,
    controls: bool,
    spacing: &TextSpacing,
) -> Frame {
    let mut frame = Frame::new(area.w, area.h);
    frame.border_with_title(active, controls, true, |frame, slot, color| {
        let title = spacing.ellipsize(title, slot.len());
        frame.label(0, slot, &title, color, spacing)
    });
    frame
}

#[cfg(test)]
mod tests {
    use super::super::{Delete, Prompt};
    use super::*;
    use gpui::{FontId, NoopTextSystem};
    use std::sync::Arc;

    /// A focused file fills the former current and preview columns with one body.
    #[test]
    fn focused_file_has_one_separator_and_an_editor_sized_body() {
        let area = Rect {
            x: 0,
            y: 1,
            w: 90,
            h: 30,
        };
        let layout = PickerLayout::for_file(area);
        let body = crate::grid::windows::editor::EditorWindow::body(layout.preview_editor_area());
        assert_eq!(layout.current.w, 0);
        assert_eq!(layout.parent.x + layout.parent.w + 1, layout.preview.x);
        assert_eq!(body.x + body.w, area.x + area.w - 1);
        assert_eq!(body.y + body.h, area.y + area.h - 1);
    }

    /// The prompt sits below the headings and only a committed query replaces Current.
    #[test]
    fn filter_uses_top_center_and_shows_the_committed_query() {
        let area = Rect {
            x: 1,
            y: 2,
            w: 90,
            h: 36,
        };
        let spacing = TextSpacing::new(Arc::new(NoopTextSystem::new()), FontId(0), 16., 9.);
        let mut picker = FilePicker {
            prompt: Some(Prompt::new(PromptKind::Filter, "notes".into(), 5)),
            ..FilePicker::default()
        };
        let layout = PickerLayout::new(area, Some(&picker));
        let filter = picker.floating_prompt_area(layout).unwrap();
        assert_eq!(filter.x, area.x + (area.w - filter.w) / 2);
        assert_eq!(filter.y, area.y + 2);
        let width = layout.current.w - 1;
        assert_eq!(
            picker.current_heading(width, &spacing),
            i18n::text("picker.current")
        );

        picker.filter = picker.prompt.take().unwrap().value;
        let label = i18n::text("picker.filter");
        assert!(picker.floating_prompt_area(layout).is_none());
        assert_eq!(
            picker.current_heading(width, &spacing),
            format!("[{label}: notes]")
        );
        let narrow = spacing.columns(&format!("[{label}: 123]"));
        assert_eq!(
            picker.current_heading(narrow, &spacing),
            format!("[{label}: no…]")
        );

        picker.prompt = Some(Prompt::new(PromptKind::Search, String::new(), 0));
        let search_layout = PickerLayout::new(area, Some(&picker));
        assert!(picker.floating_prompt_area(search_layout) == Some(filter));
        assert_eq!(search_layout.current.h, layout.current.h);
        assert_eq!(picker.prompt_label(), "picker.search");
    }

    /// Columns fill the lower grid, with space reserved for active commands.
    #[test]
    fn columns_fill_the_bottom_until_a_command_needs_rows() {
        let area = Rect {
            w: 90,
            h: 24,
            ..Rect::default()
        };
        let mut picker = FilePicker::default();
        let idle = PickerLayout::new(area, Some(&picker));
        assert_eq!(idle.current.y, 2);
        assert_eq!(idle.input_row(), area.h - 1);

        picker.goto = true;
        let jump = PickerLayout::new(area, Some(&picker));
        assert_eq!(jump.input_row(), area.h - 2);

        picker.goto = false;
        picker.delete = Some(Delete {
            paths: Vec::new(),
            permanent: false,
        });
        let confirmation = PickerLayout::new(area, Some(&picker));
        assert_eq!(confirmation.input_row(), area.h - 3);
        assert_eq!(confirmation.delete_buttons_row(), area.h - 2);
    }
}
