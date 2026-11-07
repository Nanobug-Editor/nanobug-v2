//! Character cells and retained text-row layouts. Unchanged panels do not reshape their text.
use crate::{backdrop::BackdropRaster, explorer::EntryIcon};
use gpui::{
    App, Bounds, ContentMask, Context, FontId, GlyphId, IntoElement, Pixels, PlatformTextSystem,
    Point, Render, RenderGlyphParams, ShapedLine, Styled, TextAlign, TextRun, TransformationMatrix,
    Window, WindowTextSystem, canvas, fill, font, point, px, rgb, rgba, size,
};
use nanobug::{cursor_lifecycle::Wave, cursor_motion::Vec2};
use std::{cell::RefCell, rc::Rc};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Bundled Maple Mono derivative with wider Han ink and the original cell advances.
pub const GRID_FONT_FAMILY: &str = "Nanobug Grid";
/// Foreground color of normal text.
pub const TEXT: u32 = 0xdce7ed;
/// Foreground color of secondary text.
pub const MUTED: u32 = 0x8a9eb3;
/// Focus, mode and cursor accent.
pub const ACCENT: u32 = 0x9df0d4;
/// Warm accent for project names and the active line number.
pub const GOLD: u32 = 0xf2d09b;
/// Shared material tint: approximately 84% opacity (16% transparent), independent of text.
pub const BACKGROUND: u32 = 0x0b1624d6;
/// Selection tint, matching the Glass Editor reference.
pub const SELECTION: u32 = 0x65baa558;
/// Subtle current-line/status tint above the panel's material.
pub const CURRENT: u32 = 0x85bcd522;
/// Side of a file-tree icon in logical pixels.
const ENTRY_ICON_SIZE: f32 = 16.;
/// Corner and horizontal rule cells between each side and its padded label.
const BORDER_LABEL_INSET: usize = 2;
/// The close glyph and its space on each side, in cells.
const CLOSE_LABEL_WIDTH: usize = 3;

/// Common font geometry, shared by grid drawing, pointer hit testing and the cursor.
#[derive(Clone, Copy)]
pub struct Metrics {
    /// Narrow-cell advance in device-aligned logical pixels.
    pub cw: f32,
    /// Row height in logical pixels.
    pub lh: f32,
    /// Font size in logical pixels.
    pub font_size: f32,
    /// Grid origin inside the native client area.
    pub origin: Point<Pixels>,
}

impl Metrics {
    /// Convert character `x`, `y` coordinates to window-local pixels.
    pub fn at(self, x: f32, y: f32) -> Point<Pixels> {
        self.origin + point(px(x * self.cw), px(y * self.lh))
    }
    /// Convert native `point` to nonnegative character coordinates.
    pub fn cell(self, point: Point<Pixels>) -> (usize, usize) {
        (
            ((f32::from(point.x - self.origin.x) / self.cw)
                .floor()
                .max(0.)) as usize,
            ((f32::from(point.y - self.origin.y) / self.lh)
                .floor()
                .max(0.)) as usize,
        )
    }
}

/// Half-open panel rectangle in character cells.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct Rect {
    /// First column.
    pub x: usize,
    /// First row.
    pub y: usize,
    /// Width in cells.
    pub w: usize,
    /// Height in cells.
    pub h: usize,
}

impl Rect {
    /// Whether cell `(x, y)` lies inside this rectangle.
    pub fn contains(self, x: usize, y: usize) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }
    /// Convert this rectangle to pixels using `metrics`.
    pub fn pixels(self, metrics: Metrics) -> Bounds<Pixels> {
        Bounds::new(
            metrics.at(self.x as f32, self.y as f32),
            size(
                px(self.w as f32 * metrics.cw),
                px(self.h as f32 * metrics.lh),
            ),
        )
    }

    /// Return the padded close label's rectangle, mirrored from the title's left inset.
    pub fn close_button(self) -> Option<Self> {
        if self.w <= 2 * (BORDER_LABEL_INSET + CLOSE_LABEL_WIDTH) || self.h < 2 {
            return None;
        }
        Some(Self {
            x: self.x + self.w - BORDER_LABEL_INSET - CLOSE_LABEL_WIDTH,
            y: self.y,
            w: CLOSE_LABEL_WIDTH,
            h: 1,
        })
    }
}

/// The grid font's multiplication glyph fitted to the ink height of a capital X.
#[derive(Clone, Copy)]
pub struct CloseGlyph {
    /// Bundled grid face that provides the multiplication glyph.
    font_id: FontId,
    /// Glyph index in the bundled font.
    glyph_id: GlyphId,
    /// Em size needed to match the capital ink height.
    font_size: Pixels,
    /// Offset from the label's horizontal center and top edge to the glyph's baseline.
    offset: Point<Pixels>,
}

impl CloseGlyph {
    /// Measure the bundled `font_id` through `glyphs` and align it to the normal text in `window`.
    /// `metrics` supplies the body font size and grid row height.
    pub fn new(
        metrics: Metrics,
        font_id: FontId,
        glyphs: &dyn PlatformTextSystem,
        window: &Window,
    ) -> Self {
        let glyph_id = glyphs.glyph_for_char(font_id, '×').expect("grid font ×");
        let capital_id = glyphs.glyph_for_char(font_id, 'X').expect("grid font X");
        let cross = glyphs
            .typographic_bounds(font_id, glyph_id)
            .expect("grid font × bounds");
        let capital = glyphs
            .typographic_bounds(font_id, capital_id)
            .expect("grid font X bounds");
        let text_system = window.text_system();
        let units_per_em = text_system.units_per_em(font_id) as f32;
        let body_scale = metrics.font_size / units_per_em;
        let cross_scale = body_scale * capital.size.height / cross.size.height;
        let font_size = px(cross_scale * units_per_em);
        let capital_line = text_system.shape_line(
            "X".into(),
            px(metrics.font_size),
            &[TextRun {
                len: 1,
                font: font(GRID_FONT_FAMILY),
                ..Default::default()
            }],
            Some(px(metrics.cw)),
        );
        let baseline = (px(metrics.lh) + capital_line.ascent - capital_line.descent) / 2.;
        let offset = point(
            px(-cross.center().x * cross_scale),
            baseline + px(cross.origin.y * cross_scale - capital.origin.y * body_scale),
        );
        Self {
            font_id,
            glyph_id,
            font_size,
            offset,
        }
    }

    /// Paint the glyph and its text shadow in the center of the padded button.
    pub fn element(self) -> impl IntoElement {
        canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                let origin = point(bounds.center().x, bounds.origin.y) + self.offset;
                let _ = window.paint_glyph(
                    origin + point(px(0.), px(1.)),
                    self.font_id,
                    self.glyph_id,
                    self.font_size,
                    rgba(0x00000070).into(),
                );
                let _ = window.paint_glyph(
                    origin,
                    self.font_id,
                    self.glyph_id,
                    self.font_size,
                    rgb(GOLD).into(),
                );
            },
        )
        .size_full()
    }
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

/// SVG and tint for one file-tree row, anchored to a grid cell.
#[derive(Clone, PartialEq, Eq)]
struct RowIcon {
    /// Logical column at the icon's left edge.
    column: usize,
    /// Logical row used to center the icon vertically.
    row: usize,
    /// Embedded glyph selected for this entry.
    icon: EntryIcon,
    /// RGB tint shared with the file name.
    color: u32,
}

/// One panel's derived character cells and file-tree icons.
#[derive(Clone, PartialEq, Eq)]
pub struct Frame {
    /// Visible rows, each containing the same number of cells.
    rows: Vec<Vec<Cell>>,
    /// Content index of the first visible row.
    first_row: usize,
    /// File-tree icons painted over the cells and retained separately from text shaping.
    icons: Vec<RowIcon>,
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
                vec![
                    Cell {
                        text: " ".into(),
                        fg: TEXT,
                        bg: 0
                    };
                    width
                ];
                height
            ],
            first_row,
            icons: Vec::new(),
        }
    }
    /// Write UTF-8 `text` at cell `(x, y)` in `fg`, clipped to `limit` cells.
    /// Return the first column after the written text.
    pub fn text(&mut self, x: usize, y: usize, text: &str, fg: u32, limit: usize) -> usize {
        let Some(row) = self.rows.get_mut(y) else {
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
            row[column].text = if ch == "\t" { " ".into() } else { ch.into() };
            row[column].fg = fg;
            for cell in &mut row[column + 1..column + width] {
                cell.text = if ch == "\t" {
                    " ".into()
                } else {
                    String::new()
                };
                cell.fg = fg;
            }
            column += width;
        }
        column
    }
    /// Apply RGBA `color` to columns `start..end` on `y`.
    pub fn tint(&mut self, y: usize, start: usize, end: usize, color: u32) {
        if let Some(row) = self.rows.get_mut(y) {
            let end = end.min(row.len());
            for cell in &mut row[start.min(end)..end] {
                cell.bg = color;
            }
        }
    }

    /// Paint `icon` in RGB `color`, left-aligned to `column` and centered within `row`.
    pub fn icon(&mut self, column: usize, row: usize, icon: EntryIcon, color: u32) {
        self.icons.push(RowIcon {
            column,
            row,
            icon,
            color,
        });
    }

    /// Draw a character border with `title` and the focus accent when `active`.
    /// Leave a padded slot for the overlaid glyph when `show_close` is true.
    pub fn border(&mut self, title: &str, active: bool, show_close: bool) {
        let height = self.rows.len();
        let width = self.rows.first().map_or(0, Vec::len);
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
            self.text(BORDER_LABEL_INSET, 0, " ", color, 1);
            let end = self.text(title_start, 0, title, color, title_end - title_start);
            self.text(end, 0, " ", color, 1);
        }
        if show_close && let Some(button) = close {
            self.text(button.x, 0, "   ", color, button.w);
        }
    }
}

/// One prepared glyph run at a character-grid column.
struct Run {
    /// First cell of this text run.
    column: usize,
    /// Reusable GPUI text layout.
    line: ShapedLine,
    /// Faint shadow preserves contrast without changing the reference's thin glyph shapes.
    shadow: ShapedLine,
    /// Display width of each cluster in logical pixels.
    advance: f32,
    /// Byte offsets of the prepared clusters.
    clusters: Vec<usize>,
    /// Base ink color.
    color: u32,
}

impl Run {
    /// Baseline offset from the top of a grid row with logical `height`.
    fn baseline(&self, height: f32) -> Pixels {
        (px(height) + self.line.ascent - self.line.descent) / 2.
    }
}

/// All prepared drawing data for a row.
struct PreparedRow {
    /// Background runs as starting column, cell count and RGBA color.
    backgrounds: Vec<(usize, usize, u32)>,
    /// Foreground text runs grouped by color and grapheme width.
    runs: Vec<Run>,
}

/// Mutable retained rows shared by prepaint and paint closures.
struct PanelData {
    /// Current derived cells.
    frame: Frame,
    /// Font/grid metrics.
    metrics: Metrics,
    /// None marks a changed row whose text must be shaped again.
    cache: Vec<Option<PreparedRow>>,
    /// Current text waves, independent of row shaping.
    waves: Vec<Wave>,
}

/// Independent cached GPUI view for chrome, file manager or editing area.
pub struct Panel {
    /// Retained character cells and text layouts.
    data: Rc<RefCell<PanelData>>,
}

impl Panel {
    /// Create an empty panel using shared `metrics`; the root paints the material once.
    pub fn new(metrics: Metrics) -> Self {
        Self {
            data: Rc::new(RefCell::new(PanelData {
                frame: Frame::new(0, 0),
                metrics,
                cache: Vec::new(),
                waves: Vec::new(),
            })),
        }
    }
    /// Set the `frame` and notify through `cx`, reshaping rows whose cells changed.
    pub fn set(&mut self, frame: Frame, cx: &mut Context<Self>) {
        if self.data.borrow_mut().set_frame(frame) {
            cx.notify();
        }
    }

    /// Install sampled `waves` without invalidating the prepared rows.
    pub fn set_waves(&mut self, waves: Vec<Wave>, cx: &mut Context<Self>) {
        let mut data = self.data.borrow_mut();
        if data.waves != waves {
            data.waves = waves;
            cx.notify();
        }
    }

    /// Stamp this panel at `bounds` into `raster` within its viewport `clip`.
    /// `window` supplies the same shaped rows used by the sharp canvas.
    pub(crate) fn stamp_backdrop(
        &self,
        bounds: Bounds<Pixels>,
        clip: Bounds<Pixels>,
        raster: &mut BackdropRaster<'_>,
        window: &Window,
    ) {
        let mut data = self.data.borrow_mut();
        data.prepare(window);
        let metrics = data.metrics;
        for (row, prepared) in data.cache.iter().enumerate() {
            let Some(prepared) = prepared else { continue };
            let y = bounds.origin.y + px(row as f32 * metrics.lh);
            for &(column, width, color) in &prepared.backgrounds {
                let area = Bounds::new(
                    point(bounds.origin.x + px(column as f32 * metrics.cw), y),
                    size(px(width as f32 * metrics.cw), px(metrics.lh)),
                );
                raster.fill(area.intersect(&clip), color);
            }
            for run in &prepared.runs {
                let baseline = point(
                    bounds.origin.x + px(run.column as f32 * metrics.cw),
                    y + run.baseline(metrics.lh),
                );
                for shaped in &run.line.runs {
                    for glyph in &shaped.glyphs {
                        raster.glyph(
                            baseline + glyph.position,
                            RenderGlyphParams {
                                font_id: shaped.font_id,
                                glyph_id: glyph.id,
                                font_size: run.line.font_size,
                                subpixel_variant: point(0, 0),
                                scale_factor: 1.,
                                is_emoji: glyph.is_emoji,
                                subpixel_rendering: false,
                                dilation: 0,
                            },
                            run.color,
                            clip,
                        );
                    }
                }
            }
        }
        let row_offset = (metrics.lh - ENTRY_ICON_SIZE) * 0.5;
        for icon in &data.frame.icons {
            let at = bounds.origin
                + point(
                    px(icon.column as f32 * metrics.cw),
                    px(icon.row as f32 * metrics.lh + row_offset),
                );
            raster.icon(icon.icon.svg(), at, ENTRY_ICON_SIZE, icon.color, clip);
        }
    }

    /// Measure the rightmost ink in wide cell `column` of `row`, using the layout in `window`
    /// and the same native glyph system as the painter.
    pub fn glyph_width(
        &self,
        row: usize,
        column: usize,
        window: &Window,
        glyphs: &dyn PlatformTextSystem,
    ) -> Option<f32> {
        self.data
            .borrow_mut()
            .glyph_width(row, column, window.text_system(), glyphs)
    }
}

impl PanelData {
    /// Replace the derived `frame`, preserving prepared layouts for unchanged visible rows.
    /// Return whether the panel needs repainting.
    fn set_frame(&mut self, frame: Frame) -> bool {
        if self.frame == frame {
            return false;
        }
        let shift = frame.first_row.abs_diff(self.frame.first_row);
        if shift > 0 && shift < self.cache.len() {
            if frame.first_row > self.frame.first_row {
                self.cache.rotate_left(shift);
            } else {
                self.cache.rotate_right(shift);
            }
        }
        self.cache.resize_with(frame.rows.len(), || None);
        for (i, row) in frame.rows.iter().enumerate() {
            // Rows wrapped by the rotation have no matching content index in the old viewport.
            let previous = (frame.first_row + i)
                .checked_sub(self.frame.first_row)
                .and_then(|index| self.frame.rows.get(index));
            if previous != Some(row) {
                self.cache[i] = None;
            }
        }
        self.frame = frame;
        true
    }

    /// Shape invalidated rows against the current native text system in `window`.
    fn prepare(&mut self, window: &Window) {
        for row in 0..self.frame.rows.len() {
            self.prepare_row(row, window.text_system());
        }
    }

    /// Get the cached layout of `row`, preparing it with `text_system` on first use.
    fn prepare_row(&mut self, row: usize, text_system: &WindowTextSystem) -> Option<&PreparedRow> {
        let cells = self.frame.rows.get(row)?;
        let cached = self.cache.get_mut(row)?;
        if cached.is_some() {
            return cached.as_ref();
        }
        let mut prepared = PreparedRow {
            backgrounds: Vec::new(),
            runs: Vec::new(),
        };
        let mut col = 0;
        while col < cells.len() {
            let start = col;
            let color = cells[col].bg;
            while col < cells.len() && cells[col].bg == color {
                col += 1;
            }
            if color != 0 {
                prepared.backgrounds.push((start, col - start, color));
            }
        }
        col = 0;
        while col < cells.len() {
            if cells[col].text.is_empty() {
                col += 1;
                continue;
            }
            let start = col;
            let color = cells[col].fg;
            let width = cells[col].text.width().max(1);
            let mut text = String::new();
            while col < cells.len()
                && cells[col].fg == color
                && !cells[col].text.is_empty()
                && cells[col].text.width().max(1) == width
            {
                text.push_str(&cells[col].text);
                col += width;
            }
            if text.trim().is_empty() {
                continue;
            }
            let run = TextRun {
                len: text.len(),
                font: font(GRID_FONT_FAMILY),
                color: rgb(color).into(),
                ..Default::default()
            };
            let clusters = text
                .grapheme_indices(true)
                .map(|(index, _)| index)
                .collect();
            let shared: gpui::SharedString = text.into();
            let shadow_run = TextRun {
                color: rgba(0x00000070).into(),
                ..run.clone()
            };
            let advance = Some(px(self.metrics.cw * width as f32));
            let shadow = text_system.shape_line(
                shared.clone(),
                px(self.metrics.font_size),
                &[shadow_run],
                advance,
            );
            let line = text_system.shape_line(shared, px(self.metrics.font_size), &[run], advance);
            prepared.runs.push(Run {
                column: start,
                line,
                shadow,
                advance: self.metrics.cw * width as f32,
                clusters,
                color,
            });
        }
        *cached = Some(prepared);
        cached.as_ref()
    }

    /// Return the ink extent from the start of wide cell `(row, column)` in logical pixels.
    /// `text_system` supplies the painted layout and `glyphs` supplies its native bounds.
    fn glyph_width(
        &mut self,
        row: usize,
        column: usize,
        text_system: &WindowTextSystem,
        glyphs: &dyn PlatformTextSystem,
    ) -> Option<f32> {
        let text = &self.frame.rows.get(row)?.get(column)?.text;
        let columns = text.width();
        if columns < 2 || text.trim().is_empty() {
            return None;
        }
        let cell_width = self.metrics.cw;
        let layout = self.prepare_row(row, text_system)?;
        let run = layout.runs.iter().rfind(|run| run.column <= column)?;
        let offset = column - run.column;
        let (start, cluster) = run.line.text.grapheme_indices(true).nth(offset / columns)?;
        let bytes = start..start + cluster.len();
        let mut right = f32::NEG_INFINITY;
        for shaped in &run.line.runs {
            for glyph in shaped
                .glyphs
                .iter()
                .filter(|glyph| bytes.contains(&glyph.index))
            {
                if glyph.is_emoji {
                    return None;
                }
                let bounds = glyphs.typographic_bounds(shaped.font_id, glyph.id).ok()?;
                if bounds.size.width > 0. {
                    let scale = f32::from(run.line.font_size)
                        / text_system.units_per_em(shaped.font_id) as f32;
                    right = right.max(
                        f32::from(glyph.position.x) + (bounds.origin.x + bounds.size.width) * scale,
                    );
                }
            }
        }
        let width = right - offset as f32 * cell_width;
        (width.is_finite() && width > 0.).then_some(width)
    }

    /// Paint rows at `bounds` in `window` using `cx`.
    fn paint(&self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for (i, row) in self.cache.iter().enumerate() {
                let Some(row) = row else { continue };
                let y = bounds.origin.y + px(i as f32 * self.metrics.lh);
                for &(col, width, color) in &row.backgrounds {
                    window.paint_quad(fill(
                        Bounds::new(
                            point(bounds.origin.x + px(col as f32 * self.metrics.cw), y),
                            size(px(width as f32 * self.metrics.cw), px(self.metrics.lh)),
                        ),
                        rgba(color),
                    ));
                }
                for run in &row.runs {
                    let origin =
                        point(bounds.origin.x + px(run.column as f32 * self.metrics.cw), y);
                    if self.waves.iter().any(|wave| {
                        (f32::from(y) + self.metrics.lh * 0.5 - wave.center.y).abs()
                            <= wave.front() + 7.
                    }) {
                        paint_wave_run(run, origin, self.metrics.lh, &self.waves, window);
                        continue;
                    }
                    let _ = run.shadow.paint(
                        origin + point(px(0.), px(1.)),
                        px(self.metrics.lh),
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
                    let _ = run.line.paint(
                        point(bounds.origin.x + px(run.column as f32 * self.metrics.cw), y),
                        px(self.metrics.lh),
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
                }
            }
            let icon_size = px(ENTRY_ICON_SIZE);
            let row_offset = (self.metrics.lh - ENTRY_ICON_SIZE) * 0.5;
            for icon in &self.frame.icons {
                let icon_bounds = Bounds::new(
                    bounds.origin
                        + point(
                            px(icon.column as f32 * self.metrics.cw),
                            px(icon.row as f32 * self.metrics.lh + row_offset),
                        ),
                    size(icon_size, icon_size),
                );
                let (key, data) = icon.icon.svg();
                let _ = window.paint_svg(
                    icon_bounds,
                    key.into(),
                    Some(data),
                    TransformationMatrix::unit(),
                    rgb(icon.color).into(),
                    cx,
                );
            }
        });
    }
}

/// Paint the cached glyphs of `run` at `origin`, sampling each cluster once per wave position.
fn paint_wave_run(
    run: &Run,
    origin: Point<Pixels>,
    height: f32,
    waves: &[Wave],
    window: &mut Window,
) {
    let layout = &run.line;
    let baseline = run.baseline(height);
    for shaped in &layout.runs {
        for glyph in &shaped.glyphs {
            let cluster = run
                .clusters
                .partition_point(|&start| start <= glyph.index)
                .saturating_sub(1);
            let center = Vec2::new(
                f32::from(origin.x) + (cluster as f32 + 0.5) * run.advance,
                f32::from(origin.y) + height * 0.5,
            );
            let (light, shift) =
                waves
                    .iter()
                    .fold((0., Vec2::default()), |(light, shift), wave| {
                        let (value, offset) = wave.sample(center);
                        (
                            light + value,
                            Vec2::new(shift.x + offset.x, shift.y + offset.y),
                        )
                    });
            let at = origin
                + glyph.position
                + point(
                    px(shift.x.clamp(-12., 12.)),
                    baseline + px(shift.y.clamp(-12., 12.)),
                );
            if glyph.is_emoji {
                let _ = window.paint_emoji(at, shaped.font_id, glyph.id, layout.font_size);
            } else {
                let _ = window.paint_glyph(
                    at + point(px(0.), px(1.)),
                    shaped.font_id,
                    glyph.id,
                    layout.font_size,
                    rgba(0x00000070).into(),
                );
                let color =
                    rgb(run.color).blend(rgba((ACCENT << 8) | (light.clamp(0., 1.) * 205.) as u32));
                let _ = window.paint_glyph(
                    at,
                    shaped.font_id,
                    glyph.id,
                    layout.font_size,
                    color.into(),
                );
            }
        }
    }
}

impl Render for Panel {
    /// Draw this panel with a single retained canvas and cached row layouts.
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let prepare = self.data.clone();
        let paint = self.data.clone();
        canvas(
            move |_, window, _| prepare.borrow_mut().prepare(window),
            move |bounds, _, window, cx| paint.borrow().paint(bounds, window, cx),
        )
        .size_full()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// Scrolling one row should only prepare the newly exposed row and any changed content.
    #[test]
    fn scrolled_panel_keeps_prepared_rows_that_stay_visible() {
        let height = 40;
        let make_frame = |first| {
            let mut frame = Frame::viewport(24, height, first);
            for row in 0..height {
                frame.text(0, row, &format!("Line {}", first + row), TEXT, 24);
            }
            frame
        };
        let glyphs = Arc::new(gpui::NoopTextSystem::new());
        let text_system = WindowTextSystem::new(Arc::new(gpui::TextSystem::new(glyphs)));
        let panel = Panel::new(Metrics {
            cw: 8.,
            lh: 20.,
            font_size: 16.,
            origin: point(px(0.), px(0.)),
        });
        let mut data = panel.data.borrow_mut();
        data.set_frame(make_frame(0));
        for row in 0..height {
            data.prepare_row(row, &text_system).unwrap();
        }

        let mut next = make_frame(1);
        next.tint(3, 0, 24, CURRENT);
        data.set_frame(next);
        assert_eq!(data.cache.iter().flatten().count(), height - 2);
        assert!(data.cache[3].is_none());
        assert!(data.cache[height - 1].is_none());
        assert!(
            data.cache[0].as_ref().unwrap().runs[0]
                .line
                .text
                .starts_with("Line 1 ")
        );

        for row in 0..height {
            data.prepare_row(row, &text_system).unwrap();
        }
        data.set_frame(make_frame(0));
        assert_eq!(data.cache.iter().flatten().count(), height - 2);
        assert!(data.cache[0].is_none());
        assert!(data.cache[4].is_none());
    }

    /// Wide cells use the cached glyph's ink extent, including later glyphs in the same run.
    #[test]
    fn wide_cursor_uses_ink_without_counting_grid_padding() {
        let glyphs = Arc::new(gpui::NoopTextSystem::new());
        let text_system = WindowTextSystem::new(Arc::new(gpui::TextSystem::new(glyphs.clone())));
        let metrics = Metrics {
            cw: 8.,
            lh: 20.,
            font_size: 16.,
            origin: point(px(0.), px(0.)),
        };
        let mut frame = Frame::new(7, 1);
        frame.text(0, 0, "a定位\u{3000}", TEXT, 7);
        let mut data = PanelData {
            frame,
            metrics,
            cache: vec![None],
            waves: Vec::new(),
        };
        for column in [1, 3] {
            let ink = data
                .glyph_width(0, column, &text_system, glyphs.as_ref())
                .unwrap();
            assert!(
                (ink - 7.136).abs() < 0.01,
                "unexpected width {ink} at column {column}"
            );
        }
        assert_eq!(data.glyph_width(0, 0, &text_system, glyphs.as_ref()), None);
        assert_eq!(data.glyph_width(0, 5, &text_system, glyphs.as_ref()), None);
    }
}
