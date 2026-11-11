//! Render the three browser columns, prompt, and selection in the shared character grid.

use super::{FilePicker, PickerEntry, Preview, PromptKind, delete_buttons, display_path, same_row};
use crate::{
    grid::{
        render::{Frame, Rect},
        windows::manager::EntryIcon,
    },
    ui::{
        text::{TextSpacing, UNITS_PER_COLUMN},
        theme::{ACCENT, CURRENT, GOLD, MUTED, SELECTION, TEXT},
    },
};
use nanobug::i18n;
use std::{borrow::Cow, ops::Range};

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
                        .is_some_and(|prompt| !matches!(prompt.kind, PromptKind::Filter))
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

    /// Text-only preview bounds, leaving five columns for line numbers.
    pub fn text_area(self) -> Rect {
        Rect {
            x: self.area.x + self.preview.x + 5,
            y: self.area.y + self.preview.y,
            w: self.preview.w.saturating_sub(5),
            h: self.preview.h,
        }
    }

    /// Lay out a message above the preview without taking rows from the three columns.
    pub fn message_frame(self, message: &str, spacing: &TextSpacing) -> (Rect, Frame) {
        const BACKGROUND: u32 = 0x882d3aff;
        const FOREGROUND: u32 = 0xffe8eb;
        let width = spacing
            .columns(message)
            .min(self.preview.w.saturating_sub(2))
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
        let area = Rect {
            x: self.area.x + self.area.w.saturating_sub(width + 3),
            y: self.area.y + 1,
            w: width + 2,
            h: lines.len() + 2,
        };
        let mut frame = Frame::new(area.w, area.h);
        for row in 0..area.h {
            frame.tint(row, 0, area.w, BACKGROUND);
        }
        for (row, line) in lines.into_iter().enumerate() {
            frame.label(row + 1, 1..area.w - 1, &line, FOREGROUND, spacing);
        }
        (area, frame)
    }

    /// Center the filter box horizontally in the upper third of the window work area.
    fn filter_area(self) -> Rect {
        let w = self.area.w.saturating_sub(4).min(64);
        let h = 3;
        Rect {
            x: self.area.x + (self.area.w - w) / 2,
            y: self.area.y + self.area.h.saturating_sub(h) / 3,
            w,
            h,
        }
    }

    /// Relative row of the active prompt, jump hints or confirmation question.
    fn input_row(self) -> usize {
        self.current.y + self.current.h
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
        let mut frame = Frame::new(area.w, area.h);
        let title = self.listing.as_ref().map_or(Cow::Borrowed(""), |listing| {
            listing
                .directory
                .as_deref()
                .map_or(Cow::Borrowed(i18n::text("picker.drives")), display_path)
        });
        frame.border_with_title(active, controls_hovered, true, |frame, slot, color| {
            let title = spacing.ellipsize(&title, slot.len());
            frame.label(0, slot, &title, color, spacing)
        });
        let headings = current.y - 1;
        for (rect, key) in [(parent, "picker.parent"), (current, "picker.current")] {
            frame.label(
                headings,
                rect.x + 1..rect.x + rect.w,
                i18n::text(key),
                MUTED,
                spacing,
            );
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
        for x in [parent.x + parent.w, current.x + current.w] {
            for row in headings..layout.input_row() {
                frame.text(x, row, "│", MUTED, 1);
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
                self.draw_row(&mut frame, entry, parent, row, selected, false, spacing);
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

    /// Draw one `entry` at row offset `row` inside `rect` using the selection and focus flags.
    #[allow(clippy::too_many_arguments)]
    fn draw_row(
        &self,
        frame: &mut Frame,
        entry: &PickerEntry,
        rect: Rect,
        row: usize,
        selected: bool,
        active: bool,
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
        frame.label(
            y,
            slot.clone(),
            &spacing.ellipsize(&entry.name(), slot.len()),
            if selected && active { ACCENT } else { TEXT },
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
                    self.draw_row(frame, entry, rect, row, false, false, spacing);
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
                (text.color(offset + byte).unwrap_or(TEXT), 0)
            });
        }
        Some(frame)
    }

    /// Return the floating filter bounds in `layout` while that prompt owns text input.
    pub fn filter_area(&self, layout: PickerLayout) -> Option<Rect> {
        self.prompt
            .as_ref()
            .filter(|prompt| matches!(prompt.kind, PromptKind::Filter))
            .map(|_| layout.filter_area())
    }

    /// Draw the floating filter title and input using `layout` and shared text `spacing`.
    pub fn filter_frame(&self, layout: PickerLayout, spacing: &TextSpacing) -> Option<Frame> {
        let area = self.filter_area(layout)?;
        let mut frame = Frame::new(area.w, area.h);
        frame.border(i18n::text("picker.filter"), true, false, spacing);
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
            if self.filter_area(layout).is_none() {
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
        let floating = self.filter_area(layout);
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
        let floating = self.filter_area(layout);
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
            let floating = self.filter_area(layout);
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

#[cfg(test)]
mod tests {
    use super::super::Delete;
    use super::*;

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
