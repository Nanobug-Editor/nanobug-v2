//! Grapheme-aware selection, composition and horizontal clipping for grid text fields.

use crate::ui::{
    ime::Composition,
    text::{TextSpacing, UNITS_PER_COLUMN},
};
use gpui::Keystroke;
use std::{borrow::Cow, cell::Cell, ops::Range};
use unicode_segmentation::{GraphemeCursor, UnicodeSegmentation};

const MAX_INPUT: usize = 64 * 1024;

/// Byte endpoints are on grapheme boundaries. The default caret is at the end.
#[derive(Default)]
pub(in crate::grid) struct Field {
    caret: Option<usize>,
    anchor: Option<usize>,
    first: Cell<usize>,
    pub(in crate::grid) dragging: bool,
}

/// The same retained substring determines painting, the caret, selection and pointer hit testing.
pub(in crate::grid) struct Layout<'a> {
    text: Cow<'a, str>,
    visible: Range<usize>,
    pub(in crate::grid) caret: usize,
    pub(in crate::grid) selection: Range<usize>,
}

impl Layout<'_> {
    pub(in crate::grid) fn text(&self) -> &str {
        &self.text[self.visible.clone()]
    }

    /// Resolve the nearest visible caret edge, including inside a wide grapheme.
    pub(in crate::grid) fn byte_at(&self, x: f32, spacing: &TextSpacing) -> usize {
        let units = x * UNITS_PER_COLUMN as f32;
        self.visible.start
            + spacing
                .cells(self.text())
                .find(|cell| units < (cell.start + cell.end) as f32 * 0.5)
                .map_or(self.visible.len(), |cell| cell.byte)
    }
}

impl Field {
    pub(in crate::grid) fn caret(&self, text: &str) -> usize {
        self.caret.unwrap_or(text.len())
    }

    pub(in crate::grid) fn selection(&self, text: &str) -> Range<usize> {
        let caret = self.caret(text);
        let anchor = self.anchor.unwrap_or(caret);
        anchor.min(caret)..anchor.max(caret)
    }

    pub(in crate::grid) fn place(&mut self, text: &str, byte: usize, extend: bool) {
        self.anchor = extend.then(|| self.anchor.unwrap_or(self.caret(text)));
        self.caret = Some(byte);
    }

    /// Substitute native preedit visually while keeping the confirmed draft untouched.
    pub(in crate::grid) fn displayed<'a>(
        &self,
        draft: &'a str,
        composition: &'a Composition,
    ) -> (Cow<'a, str>, usize) {
        if composition.is_empty() {
            return (Cow::Borrowed(draft), self.caret(draft));
        }
        let selection = self.selection(draft);
        let mut text = draft.to_owned();
        text.replace_range(selection.clone(), &composition.text);
        let prefix = composition
            .text_for_range(0..composition.selection.end, &mut None)
            .map_or(0, |text| text.len());
        let caret = grapheme_at_or_after(&text, selection.start + prefix);
        (Cow::Owned(text), caret)
    }

    /// Apply a confirmed insert atomically. The length limit accounts for replaced text.
    pub(in crate::grid) fn insert(&mut self, text: &mut String, insert: &str) -> bool {
        let selection = self.selection(text);
        if text.len() - selection.len() + insert.len() > MAX_INPUT {
            return false;
        }
        let point = selection.start + insert.len();
        if selection.start < self.first.get() {
            self.first.set(selection.start);
        }
        text.replace_range(selection, insert);
        self.place(text, grapheme_at_or_after(text, point), false);
        true
    }

    /// Return whether an editing key was handled and whether it changed committed text.
    pub(in crate::grid) fn edit(&mut self, text: &mut String, key: &Keystroke) -> Option<bool> {
        let modifiers = key.modifiers;
        if modifiers.alt || modifiers.function {
            return None;
        }
        if modifiers.control || modifiers.platform {
            return (key.key == "a" && !modifiers.shift).then(|| {
                self.anchor = Some(0);
                self.caret = Some(text.len());
                false
            });
        }
        let caret = self.caret(text);
        let selection = self.selection(text);
        let left = || {
            text[..caret]
                .grapheme_indices(true)
                .next_back()
                .map_or(0, |(index, _)| index)
        };
        let right = || caret + text[caret..].graphemes(true).next().map_or(0, str::len);
        let extend = modifiers.shift;
        let target = match key.key.as_str() {
            "left" if !extend && !selection.is_empty() => selection.start,
            "right" if !extend && !selection.is_empty() => selection.end,
            "left" => left(),
            "right" => right(),
            "home" => 0,
            "end" => text.len(),
            "backspace" | "delete" => {
                if selection.is_empty() {
                    self.anchor = Some(if key.key == "backspace" {
                        left()
                    } else {
                        right()
                    });
                }
                let changed = !self.selection(text).is_empty();
                self.insert(text, "");
                return Some(changed);
            }
            _ => return None,
        };
        self.place(text, target, extend);
        Some(false)
    }

    /// Lay out only the visible part of the line around the insertion endpoint.
    pub(in crate::grid) fn layout<'a>(
        &self,
        draft: &'a str,
        composition: &'a Composition,
        columns: usize,
        spacing: &TextSpacing,
    ) -> Layout<'a> {
        let selected = self.selection(draft);
        let (text, caret) = self.displayed(draft, composition);
        let (first, caret_units) = visible_left(
            &text,
            caret,
            self.first.get(),
            columns.saturating_sub(1) * UNITS_PER_COLUMN,
            spacing,
        );
        if composition.is_empty() {
            self.first.set(first);
        }
        let end = first
            + spacing
                .fitting_cells(&text[first..], columns * UNITS_PER_COLUMN)
                .last()
                .map_or(0, |cell| cell.byte + cell.text.len());
        let selection = if composition.is_empty() && !selected.is_empty() {
            let position = |byte: usize| line_units(&text[first..byte.clamp(first, end)], spacing);
            position(selected.start)..position(selected.end)
        } else {
            0..0
        };
        Layout {
            text,
            visible: first..end,
            caret: caret_units,
            selection,
        }
    }
}

/// Move a UTF-8 position to the next grapheme boundary when it falls inside a character cluster.
pub(in crate::grid) fn grapheme_at_or_after(text: &str, byte: usize) -> usize {
    let mut cursor = GraphemeCursor::new(byte, text.len(), true);
    if cursor.is_boundary(text, 0).unwrap_or(true) {
        byte
    } else {
        cursor
            .next_boundary(text, 0)
            .ok()
            .flatten()
            .unwrap_or(text.len())
    }
}

fn line_units(text: &str, spacing: &TextSpacing) -> usize {
    spacing.cells(text).last().map_or(0, |cell| cell.end)
}

/// Return the visible start byte and caret advance within `available` spacing units.
/// Retain `first` from the previous draw when it still shows `caret` in `text`.
pub(in crate::grid) fn visible_left(
    text: &str,
    caret: usize,
    first: usize,
    available: usize,
    spacing: &TextSpacing,
) -> (usize, usize) {
    if first > caret {
        return (caret, 0);
    }
    let mut first = first;
    while !text.is_char_boundary(first) {
        first -= 1;
    }
    let first = grapheme_at_or_after(text, first);
    let last = spacing.fitting_cells(&text[first..caret], available).last();
    if last.as_ref().map_or(0, |cell| cell.byte + cell.text.len()) == caret - first {
        return (first, last.map_or(0, |cell| cell.end));
    }
    let mut first = caret;
    let mut width = 0;
    for (byte, grapheme) in text[..caret].grapheme_indices(true).rev() {
        let advance = line_units(grapheme, spacing);
        if width + advance > available {
            break;
        }
        first = byte;
        width += advance;
    }
    (first, width)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::Modifiers;

    fn key(key: &str, shift: bool) -> Keystroke {
        Keystroke {
            key: key.into(),
            key_char: None,
            modifiers: Modifiers {
                shift,
                ..Modifiers::default()
            },
        }
    }

    #[test]
    fn plugin_field_replaces_a_selection_without_splitting_graphemes() {
        let mut text = String::from("A中文👩‍💻B");
        let mut field = Field::default();
        assert_eq!(field.edit(&mut text, &key("left", false)), Some(false));
        assert_eq!(field.edit(&mut text, &key("left", true)), Some(false));
        assert_eq!(&text[field.selection(&text)], "👩‍💻");
        assert!(field.insert(&mut text, "📝"));
        assert_eq!(text, "A中文📝B");
        assert_eq!(field.edit(&mut text, &key("backspace", false)), Some(true));
        assert_eq!(text, "A中文B");
        assert_eq!(field.caret(&text), "A中文".len());
    }

    #[test]
    fn plugin_field_keeps_the_caret_after_a_joined_grapheme() {
        let mut text = String::from("👩💻");
        let mut field = Field::default();
        field.edit(&mut text, &key("left", false));
        assert!(field.insert(&mut text, "\u{200d}"));
        assert_eq!(text, "👩‍💻");
        assert_eq!(field.caret(&text), text.len());
        assert_eq!(field.edit(&mut text, &key("backspace", false)), Some(true));
        assert!(text.is_empty());
    }

    #[test]
    fn plugin_field_uses_the_same_boundaries_for_hit_caret_and_preedit() {
        let spacing = TextSpacing::new(
            std::sync::Arc::new(gpui::NoopTextSystem::new()),
            gpui::FontId(0),
            13.,
            8.,
        );
        let text = "A中B";
        let mut field = Field::default();
        let mut composition = Composition::default();
        let hit_x = line_units("A中", &spacing) as f32 / UNITS_PER_COLUMN as f32;
        let hit = field
            .layout(text, &composition, 8, &spacing)
            .byte_at(hit_x, &spacing);
        assert_eq!(hit, "A中".len());
        field.place(text, hit, true);
        let selected = field.layout(text, &composition, 8, &spacing);
        assert_eq!(selected.caret, line_units("A中", &spacing));
        assert_eq!(
            selected.selection,
            selected.caret..line_units(text, &spacing)
        );

        composition.mark(None, "汉字", Some(1..1));
        let composed = field.layout(text, &composition, 8, &spacing);
        assert_eq!(composed.text(), "A中汉字");
        assert_eq!(composed.caret, line_units("A中汉", &spacing));
        assert!(composed.selection.is_empty());
        assert_eq!(&text[field.selection(text)], "B");
    }
}
