//! Native text and IME bridge. Preedit stays local; only committed text changes the document.
use super::{Editor, layout::Focus};
use gpui::{
    Bounds, ClipboardItem, Context, EntityInputHandler, Pixels, Point, UTF16Selection, Window, px,
    size,
};
use nanobug::editor::Mode;
use std::ops::Range;

/// Convert UTF-16 `range` in `text` to UTF-8, rounding split surrogate offsets down safely.
fn utf8_range(text: &str, range: Range<usize>) -> Range<usize> {
    let boundary = |target: usize| {
        let mut units = 0;
        for (byte, ch) in text.char_indices() {
            if units + ch.len_utf16() > target {
                return byte;
            }
            units += ch.len_utf16();
        }
        text.len()
    };
    boundary(range.start)..boundary(range.end.max(range.start))
}

impl EntityInputHandler for Editor {
    /// Read UTF-16 `range` from the transient composition and report the adjusted returned range.
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = utf8_range(&self.composition, range);
        *adjusted = Some(
            self.composition[..range.start].encode_utf16().count()
                ..self.composition[..range.end].encode_utf16().count(),
        );
        Some(self.composition[range].into())
    }

    /// Expose the native composition selection, or an empty local text-input interval.
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.composition_selection.clone(),
            reversed: false,
        })
    }

    /// Report the current native preedit interval in UTF-16 units.
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        (!self.composition.is_empty()).then(|| 0..self.composition.encode_utf16().count())
    }

    /// Finalize any outstanding native composition when the OS unmarks it.
    fn unmark_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = std::mem::take(&mut self.composition);
        self.composition_selection = 0..0;
        if !text.is_empty() {
            self.commit_text(&text, window, cx);
        } else {
            self.sync(window, cx);
        }
    }

    /// Commit native `text` once; preedit is discarded before handing text to the modal editor.
    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.composition.clear();
        self.composition_selection = 0..0;
        self.commit_text(text, window, cx);
    }

    /// Update transient `text` and its UTF-16 `selection`; no document/undo mutation occurs yet.
    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selection: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.focus != Focus::Editor
            || !matches!(self.vim().mode, Mode::Insert | Mode::Command | Mode::Search)
        {
            return;
        }
        let range = range
            .map(|range| utf8_range(&self.composition, range))
            .unwrap_or(0..self.composition.len());
        self.composition.replace_range(range, text);
        let end = self.composition.encode_utf16().count();
        self.composition_selection = selection.unwrap_or(end..end);
        self.sync(window, cx);
    }

    /// Anchor OS candidate UI to the logical input point, independently of visual interpolation.
    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let (x, y, width, _, _) = self.caret();
        Some(Bounds::new(
            self.metrics.at(x, y),
            size(px(self.metrics.cw * width), px(self.metrics.lh)),
        ))
    }

    /// The local composition surface exposes only its current insertion endpoint for native hit tests.
    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.composition_selection.end)
    }

    /// Size of the local native composition surface, measured in UTF-16 units.
    fn text_length_utf16(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        Some(self.composition.encode_utf16().count())
    }

    /// IME is enabled only for text-entry modes; normal/visual Vim keys bypass composition.
    fn accepts_text_input(&self, _: &mut Window, _: &mut Context<Self>) -> bool {
        self.focus == Focus::Editor
            && matches!(self.vim().mode, Mode::Insert | Mode::Command | Mode::Search)
    }

    /// Paste literal clipboard text into insert, search or command input; normal mode ignores paste.
    fn paste(&mut self, item: ClipboardItem, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.vim().mode, Mode::Insert | Mode::Command | Mode::Search)
            && let Some(text) = item.text()
        {
            self.commit_text(&text, window, cx);
        }
    }
}
