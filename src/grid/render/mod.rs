//! Character frames and the retained panel renderer.
//! Frames contain derived display data; panels retain shaped rows until those rows change.

mod cursor;
mod geometry;
mod layer;
mod panel;

pub(in crate::grid) use cursor::Cursor;
pub(crate) use geometry::{Metrics, Rect};
pub(in crate::grid) use layer::GridLayer;
pub(crate) use panel::{CursorCell, Panel};

use crate::ui::{
    icons::{Icon, KeyIcon},
    text::{TextCell, TextSpacing, UNITS_PER_COLUMN},
    theme::{ACCENT, GOLD, MUTED, TEXT},
};
use gpui::{Bounds, Pixels, Point, point, px, size};
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Corner and horizontal rule cells between each side and its padded label.
const BORDER_LABEL_INSET: usize = 2;
/// Logical pixels between a key's visible path and its footer caption.
const KEY_HINT_GAP: f32 = 4.;

/// Center `hints` within `columns` and return their hit ranges using `cell_width` and `spacing`.
pub(in crate::grid) fn key_hint_slots(
    columns: usize,
    cell_width: f32,
    spacing: &TextSpacing,
    hints: [(KeyIcon, &str); 2],
) -> [Range<usize>; 2] {
    let [left, right] = hints
        .map(|(key, caption)| key.columns(cell_width, KEY_HINT_GAP) + spacing.columns(caption));
    let start = columns.saturating_sub(left + 2 + right) / 2;
    let end = start + left + 2;
    [start..start + left, end..end + right]
}

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

/// Presentation of a measured label without changing its grid advance.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::grid) enum LabelStyle {
    /// Ordinary grid text.
    Text,
    /// Underlined keyboard key name.
    Key,
    /// Text glyph from the symbol face.
    Symbol,
}

/// Measured text anchored independently of the row's border and other fields.
#[derive(Clone, PartialEq, Eq)]
struct Label {
    /// Horizontal anchor in spacing units relative to the panel.
    x: usize,
    /// Graphemes and positions relative to the anchor.
    text: FrameRow,
    /// Font or decoration for the label's text.
    style: LabelStyle,
}

impl Label {
    /// Retain the first line of `text` in `color` and `style`, fitted to `width` through `spacing`.
    fn new(
        text: &str,
        color: u32,
        width: usize,
        spacing: &TextSpacing,
        style: LabelStyle,
    ) -> Option<Self> {
        let mut row = FrameRow {
            cells: Vec::new(),
            positions: Vec::new(),
        };
        let mut column = 0;
        for cell in spacing.fitting_cells(text, width) {
            column = row.place(column, cell, 0, color, 0);
        }
        row.omit_grid_positions();
        (column > 0).then_some(Self {
            x: 0,
            text: row,
            style,
        })
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

/// One SVG icon anchored to a grid cell.
#[derive(Clone, PartialEq, Eq)]
struct RowIcon {
    /// Logical column from which the SVG is positioned.
    column: usize,
    /// Logical row used to center the icon vertically.
    row: usize,
    /// SVG drawn at the cell anchor.
    icon: Icon,
    /// Custom canvas relative to the row anchor. Other icons use a font-sized square.
    canvas: Option<Bounds<Pixels>>,
}

impl RowIcon {
    /// Return the icon's slot at panel `origin`, centered in its row using `metrics`.
    fn bounds(&self, origin: Point<Pixels>, metrics: Metrics) -> Bounds<Pixels> {
        let canvas = self.canvas.unwrap_or_else(|| {
            let side = px(metrics.font_size);
            Bounds::new(point(px(0.), -side / 2.), size(side, side))
        });
        let anchor = origin
            + point(
                px(self.column as f32 * metrics.cw),
                px((self.row as f32 + 0.5) * metrics.lh),
            );
        Bounds::new(anchor + canvas.origin, canvas.size)
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

/// One column of tapered background joining a status badge to its neighbor.
#[derive(Clone, Copy, PartialEq, Eq)]
struct RowChevron {
    /// Grid position of the column occupied by the tip.
    column: usize,
    /// Grid row containing the badge.
    row: usize,
    /// Whether the tip faces left and joins the tint on its right.
    points_left: bool,
    /// RGBA fill shared with the adjoining badge.
    color: u32,
}

/// One panel's derived character cells and decorations.
#[derive(Clone, PartialEq, Eq)]
pub struct Frame {
    /// Visible rows and any viewport-relative positions for their cells.
    rows: Vec<FrameRow>,
    /// Independent measured labels on each row.
    labels: Vec<Vec<Label>>,
    /// Content index of the first visible row.
    first_row: usize,
    /// Icons painted over the cells and retained separately from text shaping.
    icons: Vec<RowIcon>,
    /// Explorer guides painted over the row tints, before the icons.
    guides: Vec<GuideSegment>,
    /// Tapered badge edges painted over row tints.
    chevrons: Vec<RowChevron>,
    /// Vertical inset of row tint fills, in logical pixels.
    tint_inset: Pixels,
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
            chevrons: Vec::new(),
            tint_inset: px(0.),
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
        self.styled_label(y, slot, text, color, spacing, LabelStyle::Text)
    }

    /// Retain `text` with `color` and `style` inside `slot` on row `y`.
    /// Fit with `spacing` and return the next grid column.
    pub(in crate::grid) fn styled_label(
        &mut self,
        y: usize,
        slot: Range<usize>,
        text: &str,
        color: u32,
        spacing: &TextSpacing,
        style: LabelStyle,
    ) -> usize {
        let Some(row) = self.rows.get(y) else {
            return slot.start;
        };
        let width = slot.end.min(row.cells.len()).saturating_sub(slot.start) * UNITS_PER_COLUMN;
        let Some(mut label) = Label::new(text, color, width, spacing, style) else {
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
        let Some(mut label) = Label::new(text, color, end, spacing, LabelStyle::Text) else {
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

    /// Inset the frame's row tints by `padding` on both vertical edges without moving its text.
    pub(in crate::grid) fn inset_row_tints(&mut self, padding: Pixels) {
        self.tint_inset = padding;
    }

    /// Add an RGBA `color` tip at `column` on `row`, extending left when `points_left` is set.
    pub(in crate::grid) fn chevron(
        &mut self,
        row: usize,
        column: usize,
        points_left: bool,
        color: u32,
    ) {
        self.chevrons.push(RowChevron {
            column,
            row,
            points_left,
            color,
        });
    }

    /// Position a tint of `width` within the row at `origin`, using its logical `height`.
    fn tint_bounds(&self, origin: Point<Pixels>, width: Pixels, height: f32) -> Bounds<Pixels> {
        let inset = self.tint_inset;
        Bounds::new(
            origin + point(px(0.), inset),
            size(width, (px(height) - inset * 2.).max(px(0.))),
        )
    }

    /// Place a `chevron` in the frame at `origin`, sharing the tints' vertical inset and `metrics`.
    fn chevron_bounds(
        &self,
        chevron: RowChevron,
        origin: Point<Pixels>,
        metrics: Metrics,
    ) -> Bounds<Pixels> {
        self.tint_bounds(
            origin
                + point(
                    px(chevron.column as f32 * metrics.cw),
                    px(chevron.row as f32 * metrics.lh),
                ),
            px(metrics.cw),
            metrics.lh,
        )
    }

    /// Place `icon` at `column`, vertically centered within `row`.
    pub fn icon(&mut self, column: usize, row: usize, icon: Icon) {
        self.icons.push(RowIcon {
            column,
            row,
            icon,
            canvas: None,
        });
    }

    /// Center `key` on `row`, with its visible left edge `offset` pixels from `column`.
    pub(in crate::grid) fn key_icon(
        &mut self,
        column: usize,
        row: usize,
        key: KeyIcon,
        offset: f32,
    ) {
        self.image(column, row, key.image, key.canvas(offset));
    }

    /// Paint `icon` in a custom `canvas` relative to the left edge of `column` and midpoint of `row`.
    pub(in crate::grid) fn image(
        &mut self,
        column: usize,
        row: usize,
        icon: Icon,
        canvas: Bounds<Pixels>,
    ) {
        self.icons.push(RowIcon {
            column,
            row,
            icon,
            canvas: Some(canvas),
        });
    }

    /// Paint two `hints` on `row`, using `cell_width` and `spacing` to align their captions.
    pub(in crate::grid) fn key_hints(
        &mut self,
        row: usize,
        cell_width: f32,
        spacing: &TextSpacing,
        hints: [(KeyIcon, &str); 2],
    ) {
        let slots = key_hint_slots(self.rows[row].cells.len(), cell_width, spacing, hints);
        for ((key, caption), slot) in hints.into_iter().zip(slots) {
            let start = slot.start + key.columns(cell_width, KEY_HINT_GAP);
            self.key_icon(start, row, key, -key.visible_width - KEY_HINT_GAP);
            self.label(row, start..slot.end, caption, MUTED, spacing);
        }
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
    /// Add the pane's corner controls when `show_controls` is true.
    pub fn border(
        &mut self,
        title: &str,
        active: bool,
        show_controls: bool,
        spacing: &TextSpacing,
    ) {
        self.border_with_title(active, show_controls, true, |frame, slot, color| {
            frame.label(0, slot, title, color, spacing)
        });
    }

    /// Draw the border with focus color from `active` and render corner controls when `show_controls`.
    /// `include_help` enables the pane's question mark beside the close icon's reserved gap.
    /// `draw_title` receives the available columns and ink color, and returns the end of its title.
    pub(in crate::grid) fn border_with_title(
        &mut self,
        active: bool,
        show_controls: bool,
        include_help: bool,
        draw_title: impl FnOnce(&mut Self, Range<usize>, u32) -> usize,
    ) {
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
        let controls = if show_controls {
            Rect {
                w: width,
                h: height,
                ..Rect::default()
            }
            .corner_controls(include_help)
        } else {
            None
        };
        let title_start = BORDER_LABEL_INSET + 1;
        // Leave room for the title's trailing space and a rule before the controls.
        let title_end = controls
            .as_ref()
            .map_or(width.saturating_sub(3), |controls| controls.area().x - 2);
        if title_end > title_start {
            let end = draw_title(self, title_start..title_end, color);
            let gap = end + 1 - BORDER_LABEL_INSET;
            self.text(BORDER_LABEL_INSET, 0, &" ".repeat(gap), color, gap);
        }
        if let Some(controls) = controls {
            let area = controls.area();
            self.text(area.x, 0, &" ".repeat(area.w), color, area.w);
            if let Some(help) = controls.help {
                self.text(help.x + help.w / 2, 0, "?", GOLD, 1);
            }
        }
    }
}
