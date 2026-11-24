//! Character frames and the retained panel renderer.
//! Frames contain derived display data; panels retain shaped rows until those rows change.

mod components;
mod cursor;
mod geometry;
mod layer;
mod outline;
mod panel;
mod popup;
mod prompt;

use crate::ui::components::grid_toast::MESSAGE_ICON_COLUMNS;
pub(crate) use crate::ui::grid_geometry::Rect;
pub(in crate::grid) use crate::ui::{components::grid_toast::MessageLevel, text::LabelStyle};
pub(in crate::grid) use cursor::Cursor;
pub(crate) use geometry::Metrics;
pub(in crate::grid) use geometry::exposed_regions;
pub(in crate::grid) use layer::GridLayer;
pub(in crate::grid) use outline::{OutlineStyle, outline_path, solid_stroke_width};
pub(in crate::grid) use panel::sample_waves;
pub(crate) use panel::{CursorCell, Panel};
pub(in crate::grid) use popup::{dashed_outline_element, dialog_backdrop, paint_dashed_outline};
pub(in crate::grid) use prompt::{PromptRow, SEARCH_ICON};

use crate::ui::{
    components::grid_window::{GridWindow, title_hit_columns},
    icons::{Icon, KeyIcon},
    text::{TextCell, TextSpacing, UNITS_PER_COLUMN},
    theme::{ACCENT, GOLD, MUTED, Style, TEXT, TREE_GUIDE, ThemeComponent},
};
use gpui::{Bounds, Pixels, Point, point, px, size};
use nanobug::vim::{HighlightAttributes, HighlightStyle};
use outline::{EdgeGaps, Outline};
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Separator between complete action and key captions in a grid footer.
pub(in crate::grid) const SHORTCUT_SEPARATOR: &str = " | ";

/// Corner and horizontal rule cells between each side and its padded label.
const BORDER_LABEL_INSET: usize = 2;
/// Logical pixels between a key's visible path and its footer caption.
const KEY_HINT_GAP: f32 = 4.;
/// Center `hints` within `columns` and return their hit ranges using `cell_width` and `spacing`.
pub(in crate::grid) fn key_hint_slots<const N: usize>(
    columns: usize,
    cell_width: f32,
    spacing: &TextSpacing,
    hints: [(KeyIcon, &str); N],
) -> [Range<usize>; N] {
    let widths = hints
        .map(|(key, caption)| key.columns(cell_width, KEY_HINT_GAP) + spacing.columns(caption));
    let total = widths.iter().sum::<usize>() + 2 * N.saturating_sub(1);
    let mut start = columns.saturating_sub(total) / 2;
    widths.map(|width| {
        let slot = start..start + width;
        start = slot.end + 2;
        slot
    })
}

/// Derived cell content; empty text marks the continuation of a wide grapheme.
#[derive(Clone, PartialEq, Eq)]
struct Cell {
    /// Grapheme text, with tabs expanded only for display.
    text: String,
    paint: CellPaint,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            text: " ".into(),
            paint: CellPaint::new(TEXT, 0, false),
        }
    }
}

/// Text, background and decorations assigned to one measured grapheme.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::grid) struct CellPaint {
    /// RGB foreground, which may be a retained palette role.
    pub foreground: u32,
    /// RGBA background or retained palette role. Zero leaves the underlying theme visible.
    pub background: u32,
    /// Draw the grapheme at half opacity when presenting folded text.
    dimmed: bool,
    /// Optional explicit ink for decorations.
    special: Option<u32>,
    /// Preserve grid advances while applying text effects at draw time.
    attributes: HighlightAttributes,
}

impl CellPaint {
    /// Construct an ordinary grapheme using an existing palette foreground and tint.
    pub fn new(foreground: u32, background: u32, dimmed: bool) -> Self {
        Self {
            foreground,
            background,
            dimmed,
            special: None,
            attributes: HighlightAttributes::default(),
        }
    }

    /// Override explicitly supplied fields in the current grapheme style.
    pub fn with_highlight(mut self, style: HighlightStyle) -> Self {
        if style.attributes.nocombine {
            self.attributes = HighlightAttributes::default();
            self.special = None;
        }
        self.foreground = style.foreground.unwrap_or(self.foreground);
        self.background = style.background.map_or(self.background, |rgb| {
            let alpha = u32::from(100 - style.blend.unwrap_or_default().min(100)) * 255 / 100;
            (rgb << 8) | alpha
        });
        self.special = style.special.or(self.special);
        let previous = self.attributes;
        let next = style.attributes;
        self.attributes = HighlightAttributes {
            bold: previous.bold || next.bold,
            italic: previous.italic || next.italic,
            reverse: previous.reverse || next.reverse,
            standout: previous.standout || next.standout,
            underline: previous.underline || next.underline,
            undercurl: previous.undercurl || next.undercurl,
            underdouble: previous.underdouble || next.underdouble,
            underdotted: previous.underdotted || next.underdotted,
            underdashed: previous.underdashed || next.underdashed,
            strikethrough: previous.strikethrough || next.strikethrough,
            overline: previous.overline || next.overline,
            dim: previous.dim || next.dim,
            conceal: previous.conceal || next.conceal,
            nocombine: false,
        };
        self
    }

    /// Underline this grapheme with a wave in the given RGB color or theme role.
    pub(in crate::grid) fn with_undercurl(mut self, color: u32) -> Self {
        self.attributes.undercurl = true;
        self.special = Some(color);
        self
    }

    /// Resolve foreground and background together so reversal also works for palette roles.
    fn colors(self, theme: Style) -> (u32, u32) {
        let fg = theme.foreground(self.foreground);
        let bg = theme.tint(self.background);
        if self.attributes.reverse || self.attributes.standout {
            let background = if bg == 0 { theme.background() } else { bg };
            (background >> 8, (fg << 8) | 0xff)
        } else {
            (fg, bg)
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
    fn place(&mut self, column: usize, cell: TextCell<'_>, left: usize, paint: CellPaint) -> usize {
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
        write_grapheme(cells, cell.text, |_| paint);
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
        Self::with_paint(text, width, spacing, style, |_| {
            CellPaint::new(color, 0, false)
        })
    }

    /// Preserve fitted grapheme offsets when different bytes carry different Ex styles.
    fn with_paint(
        text: &str,
        width: usize,
        spacing: &TextSpacing,
        style: LabelStyle,
        mut paint: impl FnMut(usize) -> CellPaint,
    ) -> Option<Self> {
        let mut row = FrameRow {
            cells: Vec::new(),
            positions: Vec::new(),
        };
        let mut column = 0;
        for cell in spacing.fitting_cells(text, width) {
            let appearance = paint(cell.byte);
            column = row.place(column, cell, 0, appearance);
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

/// Store one `grapheme` with its `paint` in `cells`, expanding tabs to blanks.
fn write_grapheme(cells: &mut [Cell], grapheme: &str, mut paint: impl FnMut(&Cell) -> CellPaint) {
    let mut text = if grapheme == "\t" { " " } else { grapheme };
    for cell in cells {
        cell.paint = paint(cell);
        cell.text.clear();
        cell.text.push_str(text);
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
    /// Placement of the icon's canvas within the row.
    placement: IconPlacement,
}

/// Determine the size and anchor of an SVG canvas.
#[derive(Clone, Copy, PartialEq, Eq)]
enum IconPlacement {
    /// A font-sized square starting at the column's left edge.
    Default,
    /// Explicit canvas relative to the column's left edge and the row midpoint.
    Custom(Bounds<Pixels>),
    /// Tight canvas ending at the column's right edge, sized as a percentage of the font size.
    RightAligned(u8),
}

/// Snap a logical window coordinate to a device pixel boundary at `scale`.
fn snap_pixel(value: Pixels, scale: f32) -> Pixels {
    px((f32::from(value) * scale).round() / scale)
}

impl RowIcon {
    /// Return the icon's slot at panel `origin`, centered in its row using `metrics`.
    /// Right-aligned icons use the raster dimensions at device `scale` to avoid a second fit.
    fn bounds(&self, origin: Point<Pixels>, metrics: Metrics, scale: f32) -> Bounds<Pixels> {
        let anchor = origin
            + point(
                px(self.column as f32 * metrics.cw),
                px((self.row as f32 + 0.5) * metrics.lh),
            );
        match self.placement {
            IconPlacement::Default => {
                let side = px(metrics.font_size);
                Bounds::new(anchor - point(px(0.), side / 2.), size(side, side))
            }
            IconPlacement::Custom(canvas) => Bounds::new(anchor + canvas.origin, canvas.size),
            IconPlacement::RightAligned(percent) => {
                let edge = (metrics.font_size * f32::from(percent) * 0.01 * scale)
                    .round()
                    .max(1.) as i32;
                let (width, height) = self.icon.raster_dimensions(edge);
                let width = px(width as f32 / scale);
                let height = px(height as f32 / scale);
                let right = snap_pixel(origin.x + px((self.column + 1) as f32 * metrics.cw), scale);
                let top = snap_pixel(anchor.y - height / 2., scale);
                Bounds::new(point(right - width, top), size(width, height))
            }
        }
    }
}

/// One horizontal or vertical line with half-cell coordinates.
#[derive(Clone, Copy, PartialEq, Eq)]
struct GuideSegment {
    /// Start of the line in half columns and half rows.
    from: (usize, usize),
    /// End of the line in the same coordinates, to the right or below `from`.
    to: (usize, usize),
    /// RGB role of a half-opacity rule, aligned by its right edge. Tree guides use their RGBA role.
    rule: Option<u32>,
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
            at.map(|v| snap_pixel(v, scale))
        };
        let from = snap(self.from);
        let to = snap(self.to);
        let pixel = px(scale.recip());
        Bounds::new(
            if self.rule.is_some() {
                from - point(pixel, px(0.))
            } else {
                from
            },
            size((to.x - from.x).max(pixel), (to.y - from.y).max(pixel)),
        )
    }

    /// Resolve the painted RGBA value using the panel's theme `style`.
    fn color(self, style: Style) -> u32 {
        self.rule.map_or_else(
            || style.tint(TREE_GUIDE),
            |role| (style.foreground(role) << 8) | 0x80,
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

/// Area filled by the component background before its cells are drawn.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum ThemeBackground {
    /// Fill the panel within its viewport clip.
    #[default]
    Full,
    /// Leave an interior rectangle to the panels drawn there.
    Excluding(Rect),
    /// Draw only the panel's content and decorations.
    Hidden,
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
    /// Guides and rules painted over the row tints, before the icons.
    guides: Vec<GuideSegment>,
    /// Tapered badge edges painted over row tints.
    chevrons: Vec<RowChevron>,
    /// Path outlines whose geometry stays independent of the shaped rows.
    outlines: Vec<Outline>,
    /// Frame-local pointer columns of the border title and its blank padding.
    title_columns: Range<usize>,
    /// Vertical inset of row tint fills, in logical pixels.
    tint_inset: Pixels,
    /// Optional component that supplies the panel's color overrides.
    theme_component: Option<ThemeComponent>,
    /// Which parts of the panel receive the component background.
    theme_background: ThemeBackground,
    /// Workspace columns within which text graphemes must fit completely to paint.
    whole_grapheme_columns: Option<Range<usize>>,
    /// Retained row and tint that continue for half a column past the text viewport.
    trailing_row_tint: Option<(usize, u32)>,
}

impl Frame {
    /// Allocate `width` by `height` blank transparent cells.
    pub fn new(width: usize, height: usize) -> Self {
        Self::viewport(width, height, 0)
    }

    /// Paint the fixed chrome of `window` with its `component` theme and `active` focus color.
    /// The title painter receives local columns and ink, and returns the end of its title if present.
    pub(in crate::grid) fn grid_window(
        window: GridWindow,
        component: ThemeComponent,
        active: bool,
        draw_title: impl FnOnce(&mut Self, Range<usize>, u32) -> Option<usize>,
    ) -> Self {
        let area = window.bounds();
        let body = window.body();
        let mut frame = Self::new(area.w, area.h);
        frame.set_theme_component(component);
        frame.exclude_theme_background(Rect {
            x: body.x - area.x,
            y: body.y - area.y,
            ..body
        });
        frame.draw_border(active, false, false, draw_title);
        frame
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
            outlines: Vec::new(),
            title_columns: 0..0,
            tint_inset: px(0.),
            theme_component: None,
            theme_background: ThemeBackground::Full,
            whole_grapheme_columns: None,
            trailing_row_tint: None,
        }
    }

    /// Paint whole text graphemes within `columns` measured from the workspace grid origin.
    pub(in crate::grid) fn keep_whole_graphemes(&mut self, columns: Range<usize>) {
        self.whole_grapheme_columns = Some(columns);
    }

    /// Continue the `row` tint past the fixed right viewport edge by half a column.
    pub(in crate::grid) fn extend_row_tint(&mut self, row: usize, color: u32) {
        self.trailing_row_tint = Some((row, color));
    }

    /// Apply the colors of `component` to this frame.
    pub(in crate::grid) fn set_theme_component(&mut self, component: ThemeComponent) {
        self.theme_component = Some(component);
    }

    /// Leave `interior` in frame-local grid coordinates for independent child panels.
    pub(in crate::grid) fn exclude_theme_background(&mut self, interior: Rect) {
        self.theme_background = ThemeBackground::Excluding(interior);
    }

    /// Keep scoped text and decorations while leaving the existing background visible.
    pub(in crate::grid) fn hide_theme_background(&mut self) {
        self.theme_background = ThemeBackground::Hidden;
    }

    /// Apply this frame's color scope to the current window `style`.
    fn theme_style(&self, style: Style) -> Style {
        self.theme_component
            .map_or(style, |component| style.for_component(component))
    }

    /// Write fixed-grid `text` at cell `(x, y)` in `fg`, clipped to `limit` cells.
    /// Return the first column after the written text.
    pub fn text(&mut self, x: usize, y: usize, text: &str, fg: u32, limit: usize) -> usize {
        self.highlighted_text(x, y, text, fg, limit, None)
    }

    /// Write fixed-grid text with an optional highlight `style`, preserving existing cell tints.
    pub(in crate::grid) fn highlighted_text(
        &mut self,
        x: usize,
        y: usize,
        text: &str,
        fg: u32,
        limit: usize,
        style: Option<HighlightStyle>,
    ) -> usize {
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
            write_grapheme(&mut row[column..column + width], ch, |cell| {
                let paint = CellPaint::new(fg, cell.paint.background, false);
                style.map_or(paint, |style| paint.with_highlight(style))
            });
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
        self.place_label(y, slot, text, spacing, style, |_| {
            CellPaint::new(color, 0, false)
        })
    }

    /// Retain one measured label with Ex styles at source byte offsets.
    pub(in crate::grid) fn highlighted_label(
        &mut self,
        y: usize,
        slot: Range<usize>,
        text: &str,
        color: u32,
        spacing: &TextSpacing,
        style_at: impl Fn(usize) -> Option<HighlightStyle>,
    ) -> usize {
        let base = CellPaint::new(color, 0, false);
        self.place_label(y, slot, text, spacing, LabelStyle::Text, |byte| {
            style_at(byte).map_or(base, |style| base.with_highlight(style))
        })
    }

    /// Compose and anchor a fitted label without losing its internal glyph spacing.
    fn place_label(
        &mut self,
        y: usize,
        slot: Range<usize>,
        text: &str,
        spacing: &TextSpacing,
        style: LabelStyle,
        paint: impl FnMut(usize) -> CellPaint,
    ) -> usize {
        let Some(row) = self.rows.get(y) else {
            return slot.start;
        };
        let width = slot.end.min(row.cells.len()).saturating_sub(slot.start) * UNITS_PER_COLUMN;
        let Some(mut label) = Label::with_paint(text, width, spacing, style, paint) else {
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
    /// `tint` fills the trailing blanks. `style` maps byte offsets to foreground, tint and dimming.
    pub fn viewport_line<'a>(
        &mut self,
        y: usize,
        cells: impl Iterator<Item = TextCell<'a>>,
        viewport: Range<usize>,
        tint: u32,
        mut style: impl FnMut(usize) -> (u32, u32, bool),
    ) {
        self.styled_viewport_line(y, cells, viewport, 0, tint, None, |bytes| {
            let (fg, bg, dimmed) = style(bytes.start);
            CellPaint::new(fg, bg, dimmed)
        });
    }

    /// Retain `cells` in row `y` with `indent_units` of leading padding before their measured positions.
    /// Clip to `viewport`, fill blanks with `tint`, and style each grapheme's full byte range.
    /// If supplied, `end_paint` marks one cell at the measured end of the line.
    pub(in crate::grid) fn styled_viewport_line<'a>(
        &mut self,
        y: usize,
        cells: impl Iterator<Item = TextCell<'a>>,
        viewport: Range<usize>,
        indent_units: usize,
        tint: u32,
        end_paint: Option<CellPaint>,
        mut style: impl FnMut(Range<usize>) -> CellPaint,
    ) {
        let row = &mut self.rows[y];
        let left = viewport.start * UNITS_PER_COLUMN;
        let right = viewport.end * UNITS_PER_COLUMN;
        let mut column = 0;
        let leading = indent_units.min(right).saturating_sub(left);
        if leading > 0 {
            column = row.place(
                0,
                TextCell {
                    byte: 0,
                    text: "\t",
                    columns: leading.div_ceil(UNITS_PER_COLUMN),
                    start: left,
                    end: left + leading,
                },
                left,
                CellPaint::new(TEXT, tint, false),
            );
        }
        let mut end = indent_units;
        for mut cell in cells {
            cell.start += indent_units;
            cell.end += indent_units;
            end = cell.end;
            if cell.start >= right {
                break;
            }
            if cell.end <= left {
                continue;
            }
            let appearance = style(cell.byte..cell.byte + cell.text.len());
            column = row.place(column, cell, left, appearance);
        }
        if let Some(paint) = end_paint
            && (left..right).contains(&end)
        {
            column = row.place(
                column,
                TextCell {
                    byte: 0,
                    text: " ",
                    columns: 1,
                    start: end,
                    end: end + UNITS_PER_COLUMN,
                },
                left,
                paint,
            );
        }
        let remaining = ((right - left) as isize - row.x(column)).max(0) as usize;
        let needed = column + remaining.div_ceil(UNITS_PER_COLUMN);
        if needed > row.cells.len() {
            row.cells.resize_with(needed, Cell::default);
        }
        for blank in &mut row.cells[column..] {
            blank.paint.background = tint;
        }
        row.omit_grid_positions();
    }
    /// Apply RGBA `color` to columns `start..end` on `y`.
    pub fn tint(&mut self, y: usize, start: usize, end: usize, color: u32) {
        if let Some(row) = self.rows.get_mut(y).map(|row| &mut row.cells) {
            let end = end.min(row.len());
            for cell in &mut row[start.min(end)..end] {
                cell.paint.background = color;
            }
        }
    }

    /// Inspect retained display style in grid-level rendering tests.
    #[cfg(test)]
    pub(in crate::grid) fn cell_paint_at(&self, row: usize, column: usize) -> CellPaint {
        self.rows[row].cells[column].paint
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
            placement: IconPlacement::Default,
        });
    }

    /// Place a tightly cropped `icon` at the right edge of `column` on `row`.
    /// Size its longer side to `percent` of the font size.
    pub(in crate::grid) fn right_aligned_icon(
        &mut self,
        column: usize,
        row: usize,
        icon: Icon,
        percent: u8,
    ) {
        self.icons.push(RowIcon {
            column,
            row,
            icon,
            placement: IconPlacement::RightAligned(percent),
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
            placement: IconPlacement::Custom(canvas),
        });
    }

    /// Center `hints` on `row`, using `cell_width` and `spacing` to align their captions.
    pub(in crate::grid) fn icon_key_hints<const N: usize>(
        &mut self,
        row: usize,
        cell_width: f32,
        spacing: &TextSpacing,
        hints: [(KeyIcon, &str); N],
    ) {
        let slots = key_hint_slots(self.rows[row].cells.len(), cell_width, spacing, hints);
        for ((key, caption), slot) in hints.into_iter().zip(slots) {
            let start = slot.start + key.columns(cell_width, KEY_HINT_GAP);
            self.key_icon(start, row, key, -key.visible_width - KEY_HINT_GAP);
            self.styled_label(
                row,
                start..slot.end,
                caption,
                TEXT,
                spacing,
                LabelStyle::Dimmed,
            );
        }
    }

    /// Add a trunk at `column` and `row`. `continues` makes it reach the next row.
    /// `branch_before` adds a horizontal arm that stops before the given icon column.
    fn tree_guide(
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
            rule: None,
        });
        if let Some(icon_column) = branch_before {
            self.guides.push(GuideSegment {
                from: (x, middle),
                to: ((x + 2).min(icon_column * 2), middle),
                rule: None,
            });
        }
    }

    /// Draw a one-device-pixel rule on `row` to the left of the `column` boundary at half opacity.
    /// Resolve the RGB `color` from the panel's theme.
    pub(in crate::grid) fn vertical_rule(&mut self, column: usize, row: usize, color: u32) {
        let x = column * 2;
        self.guides.push(GuideSegment {
            from: (x, row * 2),
            to: (x, (row + 1) * 2),
            rule: Some(color),
        });
    }

    /// Draw a rounded border with measured `title` using `spacing` and the focus accent when `active`.
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
        self.draw_border(
            active,
            show_controls,
            include_help,
            |frame, columns, color| Some(draw_title(frame, columns, color)),
        );
    }

    /// Draw the focused or inactive outline and optional corner controls, leaving a gap for a visible title.
    fn draw_border(
        &mut self,
        active: bool,
        show_controls: bool,
        include_help: bool,
        draw_title: impl FnOnce(&mut Self, Range<usize>, u32) -> Option<usize>,
    ) {
        let area = Rect {
            w: self.rows.first().map_or(0, |row| row.cells.len()),
            h: self.rows.len(),
            ..Rect::default()
        };
        if area.w < 2 || area.h < 2 {
            return;
        }
        let color = if active { ACCENT } else { MUTED };
        let controls = show_controls
            .then(|| area.corner_controls(include_help))
            .flatten();
        let columns = area.border_title_columns(controls.as_ref());
        let mut gaps = EdgeGaps::new();
        let start = columns.start;
        if !columns.is_empty()
            && let Some(end) = draw_title(self, columns, color)
            && end > start
        {
            self.title_columns = title_hit_columns(start..end);
            gaps.push(self.title_columns.clone());
        }
        if let Some(controls) = controls {
            let area = controls.area();
            gaps.push(area.x..area.x + area.w);
            if let Some(help) = controls.help {
                self.text(help.x + help.w / 2, 0, "?", GOLD, 1);
            }
        }
        self.outline(area, color, OutlineStyle::Solid, gaps);
    }

    /// Draw a titled grid section within `area` in `color`.
    /// `draw_title` paints within the available columns and returns its last occupied edge.
    pub(in crate::grid) fn section_border(
        &mut self,
        area: Rect,
        color: u32,
        draw_title: impl FnOnce(&mut Self, Range<usize>) -> usize,
    ) {
        if area.w < 2 || area.h < 2 {
            return;
        }
        let local = area.border_title_columns(None);
        let columns = area.x + local.start..area.x + local.end;
        let gap = (!columns.is_empty())
            .then(|| {
                let start = columns.start;
                let end = draw_title(self, columns);
                title_hit_columns(start..end)
            })
            .filter(|gap| !gap.is_empty());
        self.outline(area, color, OutlineStyle::Solid, gap);
    }

    /// Retain a stroke for `area` in `color` and `style`, skipping `gaps` in frame-local top columns.
    pub(in crate::grid) fn outline(
        &mut self,
        area: Rect,
        color: u32,
        style: OutlineStyle,
        gaps: impl IntoIterator<Item = Range<usize>>,
    ) {
        if area.w >= 2 && area.h >= 2 {
            self.outlines.push(Outline {
                area,
                color,
                style,
                top: gaps.into_iter().collect(),
                bottom: EdgeGaps::new(),
                top_offset: px(0.),
                bottom_offset: px(0.),
            });
        }
    }

    /// Move a docked pane's last outline by signed logical pixels `top` and `bottom`.
    /// Its row-zero title follows `top`; the content rows stay on their grid.
    pub(in crate::grid) fn offset_pane_borders(&mut self, top: Pixels, bottom: Pixels) {
        if let Some(outline) = self.outlines.last_mut() {
            outline.top_offset = top;
            outline.bottom_offset = bottom;
        }
    }

    /// Follow the docked pane's top outline when painting its title row.
    fn title_offset(&self, row: usize) -> Pixels {
        if row == 0 {
            self.outlines
                .last()
                .map_or(px(0.), |outline| outline.top_offset)
        } else {
            px(0.)
        }
    }

    /// Reserve frame-local `columns` for a button in the most recently added outline's bottom edge.
    pub(in crate::grid) fn outline_footer_gap(&mut self, columns: Range<usize>) {
        if let Some(outline) = self.outlines.last_mut() {
            outline.bottom.push(columns);
        }
    }

    /// Draw the message outline for `level`, centering the severity heading using `spacing`.
    pub(in crate::grid) fn message_border(&mut self, level: MessageLevel, spacing: &TextSpacing) {
        self.message_border_with_title(level, None, spacing);
    }

    /// Draw the outline and icon for `level`, measuring `title` or the severity label with `spacing`.
    pub(in crate::grid) fn message_border_with_title(
        &mut self,
        level: MessageLevel,
        title: Option<&str>,
        spacing: &TextSpacing,
    ) {
        let width = self.rows.first().map_or(0, |row| row.cells.len());
        let area = Rect {
            w: width,
            h: self.rows.len(),
            ..Rect::default()
        };
        let (border, text) = level.colors();
        let Some((icon, severity)) = level.heading() else {
            self.outline(area, border, OutlineStyle::Dashed, []);
            return;
        };
        let title = title.unwrap_or(severity);
        let heading = MESSAGE_ICON_COLUMNS + spacing.columns(title);
        if width < heading + 4 || area.h < 2 {
            self.outline(area, border, OutlineStyle::Solid, []);
            return;
        }
        let start = (width - heading) / 2;
        self.title_columns = title_hit_columns(start..start + heading);
        self.outline(
            area,
            border,
            OutlineStyle::Solid,
            Some(self.title_columns.clone()),
        );
        self.icon(start, 0, icon);
        self.label(
            0,
            start + MESSAGE_ICON_COLUMNS..start + heading,
            title,
            text,
            spacing,
        );
    }
}

#[cfg(test)]
mod message_tests {
    use super::*;
    use gpui::{FontId, NoopTextSystem, SvgRenderer};
    use std::sync::Arc;

    /// Alert icons and titles are centered in solid rounded outlines with separate text colors.
    #[test]
    fn warning_and_error_frames_center_the_severity_heading() {
        let spacing = TextSpacing::new(Arc::new(NoopTextSystem::new()), FontId(0), 16., 9.);
        let svg = SvgRenderer::new(Arc::new(()));
        for level in [MessageLevel::Warning, MessageLevel::Error] {
            let (border, text) = level.colors();
            assert_ne!(border, text);
            let heading = level.heading_columns(&spacing);
            let width = heading + 10;
            let mut frame = Frame::new(width, 3);
            frame.message_border(level, &spacing);
            frame.label(1, 2..width - 2, "msg", text, &spacing);
            assert_eq!(frame.outlines.len(), 1);
            let outline = &frame.outlines[0];
            assert_eq!(outline.color, border);
            assert_eq!(outline.style, OutlineStyle::Solid);
            assert_eq!(outline.top.as_slice(), Some(4..6 + heading).as_slice());
            assert!(frame.rows.iter().all(|row| {
                row.cells
                    .iter()
                    .all(|cell| cell.text == " " && cell.paint.background == 0)
            }));
            let (icon, title) = level.heading().unwrap();
            assert_eq!(frame.icons.len(), 1);
            assert_eq!((frame.icons[0].column, frame.icons[0].row), (5, 0));
            assert_eq!(frame.icons[0].icon, icon);
            assert!(icon.rasterize(&svg, 20).is_some());
            let heading_label = &frame.labels[0][0];
            assert_eq!(
                heading_label.x,
                (5 + MESSAGE_ICON_COLUMNS) * UNITS_PER_COLUMN
            );
            assert_eq!(
                heading_label
                    .text
                    .cells
                    .iter()
                    .map(|cell| cell.text.as_str())
                    .collect::<String>(),
                title
            );
            assert!(
                heading_label
                    .text
                    .cells
                    .iter()
                    .all(|cell| cell.paint.foreground == text)
            );
            assert!(
                frame.labels[1][0]
                    .text
                    .cells
                    .iter()
                    .all(|cell| cell.paint.foreground == text)
            );
        }
    }
}
