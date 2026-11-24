//! Horizontal positions for workspace text, shared by drawing and input.
use gpui::{FontId, PlatformTextSystem};
use nanobug::document::display_width_with_tabstop;
use std::{borrow::Cow, cell::RefCell, collections::HashMap, iter::from_fn, sync::Arc};
use unicode_script::{Script, UnicodeScript};
use unicode_segmentation::UnicodeSegmentation;

/// Presentation of a measured label without changing its grid advance.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum LabelStyle {
    /// Ordinary grid text.
    Text,
    /// Ordinary grid text and shadow at half opacity.
    Dimmed,
    /// Underlined keyboard key name.
    Key,
    /// Text glyph from the symbol face.
    Symbol,
}

/// Fixed-point precision within a grid column, shared by painting and input.
pub const UNITS_PER_COLUMN: usize = 4096;

/// One grapheme's display cells and visual interval on its original line.
pub struct TextCell<'a> {
    /// UTF-8 start on the line.
    pub byte: usize,
    /// Original grapheme to shape.
    pub text: &'a str,
    /// Logical grid width, including tab stops.
    pub columns: usize,
    /// Start in spacing units from the beginning of the line.
    pub start: usize,
    /// Next boundary in spacing units from the beginning of the line.
    pub end: usize,
}

/// Measures Chinese glyph advances while keeping the base grid for code and tabs.
pub struct TextSpacing {
    /// Native text system that supplies the selected font's design advances.
    glyphs: Arc<dyn PlatformTextSystem>,
    /// The same bundled Chinese face used for drawing.
    font: FontId,
    /// Converts a font design unit to the editor's fixed-point position units.
    scale: f32,
    /// Per-character advances, including misses that should use the regular grid.
    advances: RefCell<HashMap<char, Option<usize>>>,
}

impl TextSpacing {
    /// Measure `font` through `glyphs` at `font_size` with a base column of `cell_width`.
    pub fn new(
        glyphs: Arc<dyn PlatformTextSystem>,
        font: FontId,
        font_size: f32,
        cell_width: f32,
    ) -> Self {
        let em = glyphs.font_metrics(font).units_per_em as f32;
        let scale = (font_size / em) * (UNITS_PER_COLUMN as f32 / cell_width);
        Self {
            glyphs,
            font,
            scale,
            advances: RefCell::default(),
        }
    }

    /// Identify the font and advance scale used to measure cached rows.
    pub(crate) fn metric_key(&self) -> (FontId, u32) {
        (self.font, self.scale.to_bits())
    }

    /// Return the actual Chinese advance of `ch`, when the bundled face contains the glyph.
    fn chinese_advance(&self, ch: char) -> Option<usize> {
        *self.advances.borrow_mut().entry(ch).or_insert_with(|| {
            let id = self
                .glyphs
                .glyph_for_char(self.font, ch)
                .filter(|id| id.0 != 0)?;
            let width = self.glyphs.advance(self.font, id).ok()?.width * self.scale;
            (width.is_finite() && width > 0.).then(|| (width.round() as usize).max(1))
        })
    }

    /// Project the graphemes of `line` into visual intervals without changing logical columns.
    pub fn cells<'a>(&'a self, line: &'a str) -> impl Iterator<Item = TextCell<'a>> {
        self.cells_from(line, 0, 0)
    }

    /// Measure `fragment` from its own left edge while retaining its original `byte_start` and tab `column`.
    pub(crate) fn cells_from<'a>(
        &'a self,
        fragment: &'a str,
        byte_start: usize,
        column: usize,
    ) -> impl Iterator<Item = TextCell<'a>> {
        self.buffer_cells(fragment, byte_start, column, 4)
    }

    /// Measure a `fragment` using the owning buffer's tab spacing.
    pub(crate) fn buffer_cells<'a>(
        &'a self,
        fragment: &'a str,
        byte_start: usize,
        mut column: usize,
        tabstop: usize,
    ) -> impl Iterator<Item = TextCell<'a>> + Clone {
        let mut graphemes = fragment.grapheme_indices(true);
        let mut start = 0;
        from_fn(move || {
            let (byte, text) = graphemes.next()?;
            let columns = display_width_with_tabstop(text, column, tabstop);
            let advance = chinese_base(text)
                .and_then(|ch| self.chinese_advance(ch))
                .unwrap_or(columns * UNITS_PER_COLUMN);
            let end = start + advance;
            let cell = TextCell {
                byte: byte_start + byte,
                text,
                columns,
                start,
                end,
            };
            column += columns;
            start = end;
            Some(cell)
        })
    }

    /// Return the leading graphemes of the first line in `text` that fit `width` spacing units.
    pub fn fitting_cells<'a>(
        &'a self,
        text: &'a str,
        width: usize,
    ) -> impl Iterator<Item = TextCell<'a>> {
        let line = text.split(['\r', '\n']).next().unwrap_or_default();
        self.cells(line).take_while(move |cell| cell.end <= width)
    }

    /// Fit the first line of `text` to `columns`, reserving one column for `…` only on overflow.
    pub fn ellipsize<'a>(&self, text: &'a str, columns: usize) -> Cow<'a, str> {
        let line = text.split(['\r', '\n']).next().unwrap_or_default();
        if columns == 0 {
            return Cow::Borrowed("");
        }
        let width = columns * UNITS_PER_COLUMN;
        let prefix_width = width - UNITS_PER_COLUMN;
        let mut prefix_end = 0;
        for cell in self.cells(line) {
            if cell.end > width {
                return Cow::Owned(format!("{}…", &line[..prefix_end]));
            }
            if cell.end <= prefix_width {
                prefix_end = cell.byte + cell.text.len();
            }
        }
        Cow::Borrowed(line)
    }

    /// Fit the end of the first line into `columns`, marking a hidden prefix with `…`.
    pub(crate) fn ellipsize_start<'a>(&self, text: &'a str, columns: usize) -> Cow<'a, str> {
        if columns == 0 {
            return Cow::Borrowed("");
        }
        let line = text.split(['\r', '\n']).next().unwrap_or_default();
        let end = self.cells(line).last().map_or(0, |cell| cell.end);
        let width = columns.saturating_mul(UNITS_PER_COLUMN);
        if end <= width {
            return Cow::Borrowed(line);
        }

        let first = end - (width - UNITS_PER_COLUMN);
        let mut label = String::from("…");
        for cell in self.cells(line).skip_while(|cell| cell.start < first) {
            if cell.text == "\t" {
                // Keep the tab's measured width when its original prefix is removed.
                label.extend(std::iter::repeat_n(' ', cell.columns));
            } else {
                label.push_str(cell.text);
            }
        }
        Cow::Owned(label)
    }

    /// Return the visual extent of `line` in whole grid columns for horizontal scrolling.
    pub fn columns(&self, line: &str) -> usize {
        self.cells(line)
            .last()
            .map_or(0, |cell| cell.end.div_ceil(UNITS_PER_COLUMN))
    }

    /// Return the UTF-8 boundary at `x` visual columns with the given `tabstop` width.
    pub fn byte_at(&self, line: &str, x: f64, tabstop: usize) -> usize {
        let units = x * UNITS_PER_COLUMN as f64;
        self.buffer_cells(line, 0, 0, tabstop)
            .find(|cell| units < cell.end as f64)
            .map_or(line.len(), |cell| cell.byte)
    }
}

/// Return the base character of `text` when it uses the bundled Chinese font.
pub fn chinese_base(text: &str) -> Option<char> {
    let ch = text.chars().next()?;
    (!ch.is_ascii()
        && (matches!(ch,
            '\u{3000}'..='\u{303f}'
            | '\u{fe10}'..='\u{fe1f}'
            | '\u{fe30}'..='\u{fe4f}'
            | '\u{ff01}'..='\u{ff60}'
            | '\u{ffe0}'..='\u{ffe6}'
        ) || ch.script() == Script::Han))
        .then_some(ch)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Measured Han and punctuation widths also apply next to Latin and before tabs.
    #[test]
    fn mixes_measured_chinese_with_grid_advances() {
        let spacing = TextSpacing::new(Arc::new(gpui::NoopTextSystem::new()), FontId(0), 16., 6.4);
        let line = "A中文，汉字\tB";
        let starts: Vec<_> = spacing.cells(line).map(|cell| cell.start).collect();
        assert_eq!(
            starts,
            [0, 2, 5, 8, 11, 14, 17, 19].map(|x| x * UNITS_PER_COLUMN / 2)
        );
        assert_eq!(spacing.columns(line), 11);
        for cell in spacing.cells(line) {
            let center = (cell.start + cell.end) as f64 / (2 * UNITS_PER_COLUMN) as f64;
            assert_eq!(spacing.byte_at(line, center, 4), cell.byte);
        }
        assert_eq!(spacing.byte_at(line, 11., 4), line.len());
    }

    /// Truncation reserves room for the ellipsis and keeps measured graphemes whole.
    #[test]
    fn ellipsis_fits_without_splitting_graphemes() {
        let spacing = TextSpacing::new(Arc::new(gpui::NoopTextSystem::new()), FontId(0), 16., 6.4);
        assert_eq!(spacing.ellipsize("a\u{301}文XYZ", 4), "a\u{301}文…");
        assert_eq!(spacing.ellipsize("a\u{301}文XYZ", 3), "a\u{301}…");
        assert_eq!(spacing.ellipsize("文档", 3), "文档");
        assert_eq!(spacing.ellipsize("文档", 2), "…");
        assert_eq!(spacing.ellipsize("readme.md", 0), "");
    }

    #[test]
    fn leading_ellipsis_keeps_the_measured_filename_suffix() {
        let spacing = TextSpacing::new(Arc::new(gpui::NoopTextSystem::new()), FontId(0), 16., 6.4);
        assert_eq!(spacing.ellipsize_start("src/main.rs", 11), "src/main.rs");
        assert_eq!(spacing.ellipsize_start("src/main.rs", 8), "…main.rs");
        assert_eq!(
            spacing.ellipsize_start("前缀/a\u{301}文件.rs", 8),
            "…a\u{301}文件.rs"
        );
        assert_eq!(spacing.ellipsize_start("a长文", 2), "…");
        assert_eq!(spacing.ellipsize_start("ab\tx", 4), "…  x");
        assert_eq!(spacing.ellipsize_start("file", 1), "…");
        assert_eq!(spacing.ellipsize_start("file", 0), "");
    }
}
