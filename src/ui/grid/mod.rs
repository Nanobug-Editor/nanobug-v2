//! Character frames and the retained panel renderer.
//! Frames contain derived display data; panels retain shaped rows until those rows change.

mod geometry;
mod panel;

pub(crate) use geometry::{CloseGlyph, Metrics, Rect};
pub(crate) use panel::{CursorCell, Panel};

use crate::{
    explorer::EntryIcon,
    ui::{
        text::{TextCell, TextSpacing, UNITS_PER_COLUMN},
        theme::{ACCENT, MUTED, TEXT},
    },
};
use gpui::{Bounds, Pixels, Point, point, px, size};
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Corner and horizontal rule cells between each side and its padded label.
const BORDER_LABEL_INSET: usize = 2;
/// The close glyph and its space on each side, in cells.
const CLOSE_LABEL_WIDTH: usize = 3;

/// Derived cell content; empty text marks the continuation of a wide grapheme.
#[derive(Clone, PartialEq, Eq)]
struct Cell {
    /// Grapheme text, with tabs expanded only for display.
    text: String,
    /// RGB foreground.
    fg: u32,
    /// Optional RGBA selection or current-line tint; zero means transparent.
    bg: u32,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            text: " ".into(),
            fg: TEXT,
            bg: 0,
        }
    }
}

/// One retained row, with explicit x positions only where its text diverges from the grid.
#[derive(Clone, PartialEq, Eq)]
struct FrameRow {
    /// Logical display cells, with empty text in the continuation of a wide grapheme.
    cells: Vec<Cell>,
    /// Cell boundaries in spacing units relative to the viewport, including the end of the text.
    positions: Vec<isize>,
}

impl FrameRow {
    /// Return the position of logical `column`, extrapolating regular-width blanks after the text.
    fn x(&self, column: usize) -> isize {
        match self.positions.get(column) {
            Some(&x) => x,
            None => {
                self.positions.last().copied().unwrap_or(0)
                    + ((column - self.positions.len().saturating_sub(1)) * UNITS_PER_COLUMN)
                        as isize
            }
        }
    }

    /// Resolve the start of a retained cell at `x` spacing units.
    fn column_at(&self, x: isize) -> Option<usize> {
        if self.positions.is_empty() {
            usize::try_from(x).ok().map(|x| x / UNITS_PER_COLUMN)
        } else {
            self.positions.binary_search(&x).ok()
        }
    }

    /// Place `cell` at the next logical `column`, relative to `left`, with foreground and tint.
    /// Return the first unused logical column.
    fn place(&mut self, column: usize, cell: TextCell<'_>, left: usize, fg: u32, bg: u32) -> usize {
        let x = cell.start as isize - left as isize;
        if self.positions.is_empty() {
            self.positions.push(x);
        }
        let advance = cell.end - cell.start;
        self.positions
            .extend((1..cell.columns).map(|col| x + (col * advance / cell.columns) as isize));
        self.positions.push(cell.end as isize - left as isize);

        let next = column + cell.columns;
        if next > self.cells.len() {
            self.cells.resize_with(next, Cell::default);
        }
        let cells = &mut self.cells[column..next];
        write_grapheme(cells, cell.text, fg);
        for cell in cells {
            cell.bg = bg;
        }
        next
    }

    /// Discard explicit positions when the row already follows the base grid exactly.
    fn omit_grid_positions(&mut self) {
        if self
            .positions
            .iter()
            .enumerate()
            .all(|(col, &x)| x == (col * UNITS_PER_COLUMN) as isize)
        {
            self.positions.clear();
        }
    }
}

/// Measured text anchored independently of the row's border and other fields.
#[derive(Clone, PartialEq, Eq)]
struct Label {
    /// Horizontal anchor in spacing units relative to the panel.
    x: usize,
    /// Graphemes and positions relative to the anchor.
    text: FrameRow,
}

impl Label {
    /// Retain the first line of `text` in `color`, fitted to `width` through `spacing`.
    fn new(text: &str, color: u32, width: usize, spacing: &TextSpacing) -> Option<Self> {
        let mut row = FrameRow {
            cells: Vec::new(),
            positions: Vec::new(),
        };
        let mut column = 0;
        for cell in spacing.fitting_cells(text, width) {
            column = row.place(column, cell, 0, color, 0);
        }
        row.omit_grid_positions();
        (column > 0).then_some(Self { x: 0, text: row })
    }

    /// Advance of the retained text in spacing units.
    fn width(&self) -> usize {
        self.text.x(self.text.cells.len()) as usize
    }
}

/// Store one `grapheme` in its `cells` with foreground `color`, expanding tabs to blanks.
fn write_grapheme(cells: &mut [Cell], grapheme: &str, color: u32) {
    let mut text = if grapheme == "\t" { " " } else { grapheme };
    for cell in cells {
        cell.text.clear();
        cell.text.push_str(text);
        cell.fg = color;
        text = if grapheme == "\t" { " " } else { "" };
    }
}

/// One file-tree icon anchored to a grid cell.
#[derive(Clone, PartialEq, Eq)]
struct RowIcon {
    /// Logical column at the icon's left edge.
    column: usize,
    /// Logical row used to center the icon vertically.
    row: usize,
    /// Icon selected by the theme's association rules.
    icon: EntryIcon,
}

impl RowIcon {
    /// Return the 16 px icon slot within a panel at `origin` with the given cell `metrics`.
    fn bounds(&self, origin: Point<Pixels>, metrics: Metrics) -> Bounds<Pixels> {
        let side = px(metrics.font_size);
        let row_offset = (metrics.lh - metrics.font_size) * 0.5;
        Bounds::new(
            origin
                + point(
                    px(self.column as f32 * metrics.cw),
                    px(self.row as f32 * metrics.lh + row_offset),
                ),
            size(side, side),
        )
    }
}

/// One horizontal or vertical tree guide with half-cell coordinates.
#[derive(Clone, Copy, PartialEq, Eq)]
struct GuideSegment {
    /// Start of the line in half columns and half rows.
    from: (usize, usize),
    /// End of the line in the same coordinates, to the right or below `from`.
    to: (usize, usize),
}

impl GuideSegment {
    /// Turn this segment into a one-device-pixel stroke at the panel's `origin`.
    /// `metrics` supplies the cell dimensions and `scale` the device pixels per logical pixel.
    fn bounds(self, origin: Point<Pixels>, metrics: Metrics, scale: f32) -> Bounds<Pixels> {
        let snap = |(x, y): (usize, usize)| {
            let at = origin
                + point(
                    px(x as f32 * metrics.cw * 0.5),
                    px(y as f32 * metrics.lh * 0.5),
                );
            at.map(|v| px((f32::from(v) * scale).round() / scale))
        };
        let from = snap(self.from);
        let to = snap(self.to);
        let pixel = px(scale.recip());
        Bounds::new(
            from,
            size((to.x - from.x).max(pixel), (to.y - from.y).max(pixel)),
        )
    }
}

/// One panel's derived character cells, file-tree icons and guides.
#[derive(Clone, PartialEq, Eq)]
pub struct Frame {
    /// Visible rows and any viewport-relative positions for their cells.
    rows: Vec<FrameRow>,
    /// Independent measured labels on each row.
    labels: Vec<Vec<Label>>,
    /// Content index of the first visible row.
    first_row: usize,
    /// File-tree icons painted over the cells and retained separately from text shaping.
    icons: Vec<RowIcon>,
    /// Explorer guides painted over the row tints, before the icons.
    guides: Vec<GuideSegment>,
}

impl Frame {
    /// Allocate `width` by `height` blank transparent cells.
    pub fn new(width: usize, height: usize) -> Self {
        Self::viewport(width, height, 0)
    }

    /// Allocate `width` by `height` cells for the visible content starting at `first_row`.
    pub fn viewport(width: usize, height: usize, first_row: usize) -> Self {
        Self {
            rows: vec![
                FrameRow {
                    cells: vec![Cell::default(); width],
                    positions: Vec::new()
                };
                height
            ],
            labels: vec![Vec::new(); height],
            first_row,
            icons: Vec::new(),
            guides: Vec::new(),
        }
    }
    /// Write fixed-grid `text` at cell `(x, y)` in `fg`, clipped to `limit` cells.
    /// Return the first column after the written text.
    pub fn text(&mut self, x: usize, y: usize, text: &str, fg: u32, limit: usize) -> usize {
        let Some(row) = self.rows.get_mut(y).map(|row| &mut row.cells) else {
            return x;
        };
        let end = x.saturating_add(limit).min(row.len());
        let mut column = x;
        for ch in text.graphemes(true) {
            let width = if ch == "\t" {
                4 - (column - x) % 4
            } else {
                ch.width().max(1)
            };
            if column + width > end {
                break;
            }
            write_grapheme(&mut row[column..column + width], ch, fg);
            column += width;
        }
        column
    }

    /// Place `text` in `color` at the left of `slot` on row `y`, using measured `spacing`.
    /// Return the first grid column after the retained text.
    pub fn label(
        &mut self,
        y: usize,
        slot: Range<usize>,
        text: &str,
        color: u32,
        spacing: &TextSpacing,
    ) -> usize {
        let Some(row) = self.rows.get(y) else {
            return slot.start;
        };
        let width = slot.end.min(row.cells.len()).saturating_sub(slot.start) * UNITS_PER_COLUMN;
        let Some(mut label) = Label::new(text, color, width, spacing) else {
            return slot.start;
        };
        label.x = slot.start * UNITS_PER_COLUMN;
        let end = slot.start + label.width().div_ceil(UNITS_PER_COLUMN);
        self.labels[y].push(label);
        end
    }

    /// Place `text` in `color` on row `y`, ending at `right` using measured `spacing`.
    /// Return the grid column containing the left edge, for reserving space beside it.
    pub fn right_label(
        &mut self,
        y: usize,
        right: usize,
        text: &str,
        color: u32,
        spacing: &TextSpacing,
    ) -> usize {
        let Some(row) = self.rows.get(y) else {
            return right;
        };
        let right = right.min(row.cells.len());
        let end = right * UNITS_PER_COLUMN;
        let Some(mut label) = Label::new(text, color, end, spacing) else {
            return right;
        };
        label.x = end - label.width();
        let left = label.x / UNITS_PER_COLUMN;
        self.labels[y].push(label);
        left
    }

    /// Retain the measured `cells` intersecting `viewport` grid columns in row `y`.
    /// `tint` fills the trailing blanks, and `style` maps line byte offsets to foreground and tint.
    pub fn viewport_line<'a>(
        &mut self,
        y: usize,
        cells: impl Iterator<Item = TextCell<'a>>,
        viewport: Range<usize>,
        tint: u32,
        mut style: impl FnMut(usize) -> (u32, u32),
    ) {
        let row = &mut self.rows[y];
        let left = viewport.start * UNITS_PER_COLUMN;
        let right = viewport.end * UNITS_PER_COLUMN;
        let mut column = 0;
        for cell in cells
            .skip_while(|cell| cell.end <= left)
            .take_while(|cell| cell.start < right)
        {
            let (fg, bg) = style(cell.byte);
            column = row.place(column, cell, left, fg, bg);
        }
        let remaining = ((right - left) as isize - row.x(column)).max(0) as usize;
        let needed = column + remaining.div_ceil(UNITS_PER_COLUMN);
        if needed > row.cells.len() {
            row.cells.resize_with(needed, Cell::default);
        }
        for blank in &mut row.cells[column..] {
            blank.bg = tint;
        }
        row.omit_grid_positions();
    }
    /// Apply RGBA `color` to columns `start..end` on `y`.
    pub fn tint(&mut self, y: usize, start: usize, end: usize, color: u32) {
        if let Some(row) = self.rows.get_mut(y).map(|row| &mut row.cells) {
            let end = end.min(row.len());
            for cell in &mut row[start.min(end)..end] {
                cell.bg = color;
            }
        }
    }

    /// Place `icon` at `column`, vertically centered within `row`.
    pub fn icon(&mut self, column: usize, row: usize, icon: EntryIcon) {
        self.icons.push(RowIcon { column, row, icon });
    }

    /// Add a trunk at `column` and `row`. `continues` makes it reach the next row.
    /// `branch_before` adds a horizontal arm that stops before the given icon column.
    pub fn tree_guide(
        &mut self,
        column: usize,
        row: usize,
        continues: bool,
        branch_before: Option<usize>,
    ) {
        let x = column * 2 + 1;
        let y = row * 2;
        let middle = y + 1;
        self.guides.push(GuideSegment {
            from: (x, y),
            to: (x, middle + usize::from(continues)),
        });
        if let Some(icon_column) = branch_before {
            self.guides.push(GuideSegment {
                from: (x, middle),
                to: ((x + 2).min(icon_column * 2), middle),
            });
        }
    }

    /// Draw a character border with measured `title` using `spacing` and the focus accent when `active`.
    /// Leave a padded slot for the overlaid glyph when `show_close` is true.
    pub fn border(&mut self, title: &str, active: bool, show_close: bool, spacing: &TextSpacing) {
        let height = self.rows.len();
        let width = self.rows.first().map_or(0, |row| row.cells.len());
        if width < 2 || height < 2 {
            return;
        }
        let color = if active { ACCENT } else { MUTED };
        self.text(0, 0, &format!("╭{}╮", "─".repeat(width - 2)), color, width);
        self.text(
            0,
            height - 1,
            &format!("╰{}╯", "─".repeat(width - 2)),
            color,
            width,
        );
        for row in 1..height - 1 {
            self.text(0, row, "│", color, 1);
            self.text(width - 1, row, "│", color, 1);
        }
        let close = Rect {
            w: width,
            h: height,
            ..Rect::default()
        }
        .close_button();
        let title_start = BORDER_LABEL_INSET + 1;
        // Leave room for the title's trailing space and one horizontal rule before the button.
        let title_end = close.map_or(width.saturating_sub(3), |button| button.x - 2);
        if title_end > title_start {
            let end = self.label(0, title_start..title_end, title, color, spacing);
            let gap = end + 1 - BORDER_LABEL_INSET;
            self.text(BORDER_LABEL_INSET, 0, &" ".repeat(gap), color, gap);
        }
        if show_close && let Some(button) = close {
            self.text(button.x, 0, "   ", color, button.w);
        }
    }
}
