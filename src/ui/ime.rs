//! UTF-16 native composition over the active prompt's or editor's transient UTF-8 text.

use std::ops::Range;

/// Preedit text and selection, isolated from committed editor or filter content.
#[derive(Default)]
pub(crate) struct Composition {
    /// Provisional text being composed by the native input method.
    pub text: String,
    /// Selection within preedit in UTF-16 code units.
    pub selection: Range<usize>,
}

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

impl Composition {
    /// Whether there is no outstanding preedit text.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Discard preedit and its selection together.
    pub fn clear(&mut self) {
        self.text.clear();
        self.selection = 0..0;
    }

    /// End preedit and return its text for commitment by the owning surface.
    pub fn take(&mut self) -> String {
        self.selection = 0..0;
        std::mem::take(&mut self.text)
    }

    /// Read UTF-16 `range` from the transient composition and report the adjusted returned range.
    pub fn text_for_range(
        &self,
        range: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
    ) -> Option<String> {
        let range = utf8_range(&self.text, range);
        *adjusted = Some(
            self.text[..range.start].encode_utf16().count()
                ..self.text[..range.end].encode_utf16().count(),
        );
        Some(self.text[range].into())
    }

    /// Replace the optional UTF-16 `range` with preedit `text`, retaining native `selection`.
    pub fn mark(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selection: Option<Range<usize>>,
    ) {
        let range = range
            .map(|range| utf8_range(&self.text, range))
            .unwrap_or(0..self.text.len());
        self.text.replace_range(range, text);
        let end = self.text.encode_utf16().count();
        self.selection = selection.unwrap_or(end..end);
    }
}
