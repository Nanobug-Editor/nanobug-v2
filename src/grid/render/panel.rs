//! Retained row shaping, glyph painting, cursor text and backdrop stamping.

use super::{
    CellPaint, Frame, FrameRow, LabelStyle, Metrics, Rect, ThemeBackground, exposed_regions,
};
use crate::{
    ui::icons::IconCache,
    ui::{
        backdrop::BackdropRaster,
        text::{UNITS_PER_COLUMN, chinese_base},
        theme::{ACCENT, GRID_CHINESE_FONT, GRID_RULE_FONT_FAMILY, GridFont, Style, style},
    },
};
use gpui::{
    App, Bounds, ContentMask, Context, Font, FontId, FontStyle, FontWeight, GlyphId, IntoElement,
    PathBuilder, Pixels, PlatformTextSystem, Point, Render, RenderGlyphParams, ShapedGlyph,
    ShapedLine, Styled, TextAlign, TextRun, UnderlineStyle, Window, WindowTextSystem, canvas, fill,
    font, point, px, rgb, rgba,
};
use nanobug::animation::cursor::{Vec2, Wave};
use std::{cell::RefCell, ops::Range, rc::Rc};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Font choice for one grid cell, also used to split shaping runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GridFace {
    /// Ordinary text and code, with native fallback for other scripts and symbols.
    Text,
    /// Han and East Asian punctuation.
    Chinese,
    /// Connected pane rules and other box-drawing glyphs.
    BoxDrawing,
}

impl GridFace {
    /// Select a face for the grapheme in `text` from its first code point.
    fn for_cell(text: &str) -> Self {
        match text.chars().next() {
            Some('\u{2500}'..='\u{257f}') => Self::BoxDrawing,
            Some(_) if chinese_base(text).is_some() => Self::Chinese,
            _ => Self::Text,
        }
    }

    /// Return the font descriptor for shaping cells in this run.
    fn font(self, grid: GridFont) -> Font {
        match self {
            Self::Text => grid.descriptor(),
            Self::Chinese => GRID_CHINESE_FONT.clone(),
            Self::BoxDrawing => font(GRID_RULE_FONT_FAMILY),
        }
    }
}

/// One prepared glyph run at a character-grid column.
struct Run {
    /// First cell of this text run.
    column: usize,
    /// Visual start relative to the panel's left edge.
    x: Pixels,
    /// Reusable GPUI text layout.
    line: ShapedLine,
    /// Faint shadow preserves contrast without changing the reference's thin glyph shapes.
    shadow: ShapedLine,
    /// Display width of each cluster in logical pixels.
    advance: f32,
    /// Byte offsets of the prepared clusters.
    clusters: Vec<usize>,
    /// Horizontal translations from shaped cluster origins to measured positions.
    shifts: Vec<Pixels>,
    /// Base ink color resolved through the current theme.
    color: u32,
    /// Foreground and shadow opacity selected by the label.
    opacity: f32,
    /// Decorations kept independent of glyph layout for clipped and shifted text runs.
    decoration: Decoration,
}

/// The line style requested by a highlight group.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Underline {
    Single,
    Double,
    Dotted,
    Dashed,
    Curl,
}

/// Text effects drawn in the same retained position in normal and clipped paint paths.
#[derive(Clone, Copy, Default)]
struct Decoration {
    underline: Option<Underline>,
    strike: bool,
    overline: bool,
    special: Option<u32>,
}

impl Decoration {
    /// Resolve a cell's underline, strike and overline using this panel's theme.
    fn from_paint(paint: CellPaint, style: Style) -> Self {
        let attributes = paint.attributes;
        let underline = [
            (attributes.undercurl, Underline::Curl),
            (attributes.underdouble, Underline::Double),
            (attributes.underdotted, Underline::Dotted),
            (attributes.underdashed, Underline::Dashed),
            (attributes.underline, Underline::Single),
        ]
        .into_iter()
        .find_map(|(active, style)| active.then_some(style));
        Self {
            underline,
            strike: attributes.strikethrough,
            overline: attributes.overline,
            special: paint.special.map(|color| style.foreground(color)),
        }
    }
}

impl Run {
    /// Baseline offset from the top of a grid row with logical `height`.
    fn baseline(&self, height: f32) -> Pixels {
        (px(height) + self.line.ascent - self.line.descent) / 2.
    }

    /// Cluster index for the glyph at UTF-8 `byte`.
    fn cluster(&self, byte: usize) -> usize {
        self.clusters
            .partition_point(|&start| start <= byte)
            .saturating_sub(1)
    }

    /// Apply the translation for `cluster` to this native `glyph`, retaining its shape.
    fn position(&self, glyph: &ShapedGlyph, cluster: usize) -> Point<Pixels> {
        let shift = self.shifts.get(cluster).copied().unwrap_or_default();
        glyph.position + point(shift, px(0.))
    }

    /// Check whether the entire measured cell of `cluster` at `origin` fits the source `clip`.
    fn cluster_fits(&self, clip: Option<&Range<Pixels>>, origin: Pixels, cluster: usize) -> bool {
        graphemes_fit(
            clip,
            origin + px(cluster as f32 * self.advance),
            self.advance,
        )
    }

    /// Yield the native glyphs and their fonts for the retained grapheme at `cluster`.
    fn cluster_glyphs(&self, cluster: usize) -> impl Iterator<Item = (FontId, &ShapedGlyph)> {
        let start = self.clusters[cluster];
        let end = self
            .clusters
            .get(cluster + 1)
            .copied()
            .unwrap_or(self.line.text.len());
        self.line.runs.iter().flat_map(move |shaped| {
            shaped.glyphs.iter().filter_map(move |glyph| {
                (start..end)
                    .contains(&glyph.index)
                    .then_some((shaped.font_id, glyph))
            })
        })
    }

    /// Emit underline and strike segments at the same measured positions as the text.
    fn paint_decorations(
        &self,
        origin: Point<Pixels>,
        height: f32,
        clip: Option<&Range<Pixels>>,
        mut fill: impl FnMut(Bounds<Pixels>, u32),
    ) {
        let Decoration {
            underline,
            strike,
            overline,
            special,
        } = self.decoration;
        if underline.is_none() && !strike && !overline || self.opacity == 0. {
            return;
        }
        let total = self.clusters.len() as f32;
        let (first, end) = clip.map_or((0., total), |clip| {
            let first = (f32::from(clip.start - origin.x) / self.advance - 0.001).ceil();
            let end = (f32::from(clip.end - origin.x) / self.advance + 0.001).floor();
            (first.max(0.), end.min(total))
        });
        if first >= end {
            return;
        }
        let baseline = self.baseline(height);
        let mut ink = |start: f32, y: Pixels, width: f32, color: u32| {
            fill(
                Bounds {
                    origin: origin + point(px(start), y),
                    size: gpui::size(px(width), px(1.)),
                },
                (color << 8) | (self.opacity * 255.).round() as u32,
            );
        };
        let start = first * self.advance;
        let finish = end * self.advance;
        if strike {
            ink(
                start,
                baseline - self.line.ascent / 3.,
                finish - start,
                self.color,
            );
        }
        if overline {
            ink(
                start,
                (baseline - self.line.ascent).max(px(0.)),
                finish - start,
                self.color,
            );
        }
        let Some(underline) = underline else {
            return;
        };
        let color = special.unwrap_or(self.color);
        let y = (baseline + px(1.)).min(px(height - 2.));
        match underline {
            Underline::Single => ink(start, y, finish - start, color),
            Underline::Double => {
                ink(start, y - px(1.), finish - start, color);
                ink(start, y + px(1.), finish - start, color);
            }
            Underline::Dotted | Underline::Dashed | Underline::Curl => {
                let (period, width) = match underline {
                    Underline::Dotted => (3., 1.),
                    Underline::Dashed => (6., 4.),
                    _ => (2., 2.),
                };
                let mut at = (start / period).floor() * period;
                while at < finish {
                    let left = at.max(start);
                    let right = (at + width).min(finish);
                    if right > left {
                        let shift = if underline == Underline::Curl {
                            (at / period) % 2.
                        } else {
                            0.
                        };
                        ink(left, y + px(shift), right - left, color);
                    }
                    at += period;
                }
            }
        }
    }
}

/// All prepared drawing data for a row.
struct PreparedRow {
    /// Background runs as visual start, width and RGBA color.
    backgrounds: Vec<(Pixels, Pixels, u32)>,
    /// Backgrounds of measured labels aligned with their possibly offset text.
    label_backgrounds: Vec<(Pixels, Pixels, u32)>,
    /// Foreground text runs grouped by color and grapheme width.
    runs: Vec<Run>,
    /// Independently positioned text runs painted above the fixed grid.
    label_runs: Vec<Run>,
}

impl PreparedRow {
    /// Find the run and cluster of a grapheme at `column` that occupies `width` cells.
    fn cluster_at(&self, column: usize, width: usize) -> Option<(&Run, usize)> {
        let run = self.runs.iter().rfind(|run| run.column <= column)?;
        let cluster = (column - run.column) / width;
        (cluster < run.clusters.len()).then_some((run, cluster))
    }
}

/// The grid cell to recolor when the animated cursor head covers it.
#[derive(Clone, Copy, PartialEq)]
pub struct CursorCell {
    /// Row relative to the first retained row in the text panel.
    pub row: usize,
    /// Horizontal start in visual columns relative to the text panel.
    pub column: f32,
    /// Window position of the cell's top-left corner.
    pub origin: Point<Pixels>,
}

/// A cursor cell's shaped glyphs at their current window positions.
pub struct CursorText {
    /// Font size used by the underlying prepared row.
    font_size: Pixels,
    /// Native fonts, glyph IDs and baseline positions for this character.
    glyphs: Vec<(FontId, GlyphId, Point<Pixels>)>,
}

impl CursorText {
    /// Paint the covered character in RGB `ink` with byte `alpha` in `window`.
    pub fn paint(&self, ink: u32, alpha: u32, window: &mut Window) {
        let color = rgba((ink << 8) | alpha).into();
        for &(font, glyph, origin) in &self.glyphs {
            let _ = window.paint_glyph(origin, font, glyph, self.font_size, color);
        }
    }
}

/// Mutable retained rows shared by prepaint and paint closures.
struct PanelData {
    /// Current derived cells.
    frame: Frame,
    /// Font/grid metrics.
    metrics: Metrics,
    /// Editor style used when resolving cell roles and shaping the row.
    style: Style,
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
    /// Inspect the text actually retained for each label on a row in native rendering tests.
    #[cfg(all(test, windows))]
    pub(in crate::grid) fn row_labels(&self, row: usize) -> Vec<String> {
        self.data.borrow().frame.labels[row]
            .iter()
            .map(|label| {
                label
                    .text
                    .cells
                    .iter()
                    .map(|cell| cell.text.as_str())
                    .collect()
            })
            .collect()
    }

    /// Create an empty panel using shared `metrics`; the root paints the material once.
    pub fn new(metrics: Metrics) -> Self {
        Self {
            data: Rc::new(RefCell::new(PanelData {
                frame: Frame::new(0, 0),
                metrics,
                style: Style::default(),
                cache: Vec::new(),
                waves: Vec::new(),
            })),
        }
    }

    /// Test the border title and its adjacent padding within the panel's grid area.
    pub(in crate::grid) fn title_at(
        &self,
        area: Rect,
        position: Point<Pixels>,
        metrics: Metrics,
    ) -> bool {
        let data = self.data.borrow();
        let columns = &data.frame.title_columns;
        let title = Rect {
            x: area.x + columns.start,
            y: area.y,
            w: columns.len(),
            h: 1,
        };
        area.w > 0
            && area.h > 0
            && !columns.is_empty()
            && title
                .pixels(metrics)
                .contains(&(position - point(px(0.), data.frame.title_offset(0))))
    }
    /// Set the `frame` and notify through `cx`, reshaping rows whose cells changed.
    pub fn set(&mut self, frame: Frame, cx: &mut Context<Self>) {
        let next_style = frame.theme_style(style(cx));
        let mut data = self.data.borrow_mut();
        let changed = data.set_frame(frame);
        if data.set_style(next_style) || changed {
            cx.notify();
        }
    }

    /// Repaint retained rows if their geometry, font or palette changed.
    pub fn set_appearance(&mut self, metrics: Metrics, cx: &mut Context<Self>) {
        let style = style(cx);
        let mut data = self.data.borrow_mut();
        let style = data.frame.theme_style(style);
        if data.metrics != metrics || data.style != style {
            data.metrics = metrics;
            data.style = style;
            data.cache.iter_mut().for_each(|row| *row = None);
            cx.notify();
        }
    }

    #[cfg(all(test, windows))]
    pub(crate) fn metrics(&self) -> Metrics {
        self.data.borrow().metrics
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
    /// `window` supplies the text system and `cx` supplies the current window palette.
    pub(crate) fn stamp_backdrop(
        &self,
        bounds: Bounds<Pixels>,
        clip: Bounds<Pixels>,
        raster: &mut BackdropRaster<'_>,
        window: &Window,
        cx: &App,
    ) {
        let mut data = self.data.borrow_mut();
        let scoped = data.frame.theme_style(style(cx));
        data.set_style(scoped);
        data.prepare(window);
        let metrics = data.metrics;
        let source_clip = data.horizontal_grapheme_clip(window);
        data.paint_theme_background(bounds, |area, color| {
            raster.fill(area.intersect(&clip), color)
        });
        if let Some((area, color)) = data.trailing_tint(bounds, source_clip.as_ref()) {
            raster.fill(area.intersect(&clip), color);
        }
        let clip = text_bounds(clip, source_clip.as_ref());
        for outline in &data.frame.outlines {
            if let Some(path) = outline.path(bounds.origin, metrics) {
                let color = (data.style.foreground(outline.color) << 8) | 0xff;
                raster.path(&path, clip, color);
            }
        }
        for (row, prepared) in data.cache.iter().enumerate() {
            let Some(prepared) = prepared else { continue };
            let y = bounds.origin.y + px(row as f32 * metrics.lh);
            for &(x, width, color) in &prepared.backgrounds {
                let area = data
                    .frame
                    .tint_bounds(point(bounds.origin.x + x, y), width, metrics.lh);
                raster.fill(area.intersect(&clip), color);
            }
            let y = y + data.frame.title_offset(row);
            for &(x, width, color) in &prepared.label_backgrounds {
                let area = data
                    .frame
                    .tint_bounds(point(bounds.origin.x + x, y), width, metrics.lh);
                raster.fill(area.intersect(&clip), color);
            }
            for run in prepared.runs.iter().chain(&prepared.label_runs) {
                let origin = point(bounds.origin.x + run.x, y);
                let baseline = origin + point(px(0.), run.baseline(metrics.lh));
                run.paint_decorations(origin, metrics.lh, source_clip.as_ref(), |area, color| {
                    raster.fill(area.intersect(&clip), color);
                });
                for shaped in &run.line.runs {
                    for glyph in &shaped.glyphs {
                        let cluster = run.cluster(glyph.index);
                        if !run.cluster_fits(source_clip.as_ref(), baseline.x, cluster) {
                            continue;
                        }
                        raster.glyph(
                            baseline + run.position(glyph, cluster),
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
                            (run.color << 8) | (run.opacity * 255.).round() as u32,
                            clip,
                        );
                    }
                }
            }
        }
        for &chevron in &data.frame.chevrons {
            let area = data.frame.chevron_bounds(chevron, bounds.origin, metrics);
            raster.chevron(
                area,
                clip,
                chevron.points_left,
                data.style.tint(chevron.color),
            );
        }
        let scale = window.scale_factor();
        for guide in &data.frame.guides {
            raster.fill(
                guide.bounds(bounds.origin, metrics, scale).intersect(&clip),
                guide.color(data.style),
            );
        }
        for icon in &data.frame.icons {
            let origin = bounds.origin + point(px(0.), data.frame.title_offset(icon.row));
            raster.icon(icon.icon, icon.bounds(origin, metrics, scale), clip);
        }
    }

    /// Measure the rightmost ink of the cell at `x` visual columns in `row`, using `window`
    /// and the same native glyph system as the painter.
    pub fn glyph_width(
        &self,
        row: usize,
        x: f32,
        window: &Window,
        glyphs: &dyn PlatformTextSystem,
    ) -> Option<f32> {
        let mut data = self.data.borrow_mut();
        let units = (x * UNITS_PER_COLUMN as f32).round() as isize;
        let column = data.frame.rows.get(row)?.column_at(units)?;
        data.glyph_width(row, column, window.text_system(), glyphs)
    }

    /// Resolve `cell` from the retained layout and text waves used to paint it in `window`.
    pub fn cursor_text(&self, cell: CursorCell, window: &Window) -> Option<CursorText> {
        let mut data = self.data.borrow_mut();
        let units = (cell.column * UNITS_PER_COLUMN as f32).round() as isize;
        let row = data.frame.rows.get(cell.row)?;
        let column = row.column_at(units)?;
        let text = &row.cells.get(column)?.text;
        if text.trim().is_empty() {
            return None;
        }
        let width = text.width().max(1);
        let metrics = data.metrics;
        let source_clip = data.horizontal_grapheme_clip(window);
        let cell_x = metrics.spacing_px(row.x(column));
        data.prepare_row(cell.row, window.text_system())?;
        let (run, cluster) = data.cache[cell.row].as_ref()?.cluster_at(column, width)?;
        let origin = cell.origin + point(run.x - cell_x, px(0.));
        let center = Vec2::new(
            f32::from(origin.x) + (cluster as f32 + 0.5) * run.advance,
            f32::from(origin.y) + metrics.lh * 0.5,
        );
        let (_, shift) = sample_waves(&data.waves, center);
        if !run.cluster_fits(source_clip.as_ref(), origin.x + shift.x, cluster) {
            return None;
        }
        let baseline = origin + point(px(0.), run.baseline(metrics.lh)) + shift;
        Some(CursorText {
            font_size: run.line.font_size,
            glyphs: run
                .cluster_glyphs(cluster)
                .map(|(font, glyph)| (font, glyph.id, baseline + run.position(glyph, cluster)))
                .collect(),
        })
    }
}

impl PanelData {
    /// Invalidate the prepared rows when their resolved `style` changes.
    fn set_style(&mut self, style: Style) -> bool {
        if self.style == style {
            return false;
        }
        self.style = style;
        self.cache.iter_mut().for_each(|row| *row = None);
        true
    }

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
            let unchanged = (frame.first_row + i)
                .checked_sub(self.frame.first_row)
                .is_some_and(|index| {
                    self.frame.rows.get(index) == Some(row)
                        && self.frame.labels.get(index) == Some(&frame.labels[i])
                });
            if !unchanged {
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
        let positioned = self.frame.rows.get(row)?;
        let cached = self.cache.get_mut(row)?;
        if cached.is_some() {
            return cached.as_ref();
        }
        let mut prepared = PreparedRow {
            backgrounds: Vec::new(),
            label_backgrounds: Vec::new(),
            runs: Vec::new(),
            label_runs: Vec::new(),
        };
        Self::row_backgrounds(
            positioned,
            0,
            self.metrics,
            self.style,
            &mut prepared.backgrounds,
        );
        Self::shape_runs(
            positioned,
            0,
            self.metrics,
            text_system,
            LabelStyle::Text,
            self.style,
            &mut prepared.runs,
        );
        for label in &self.frame.labels[row] {
            Self::row_backgrounds(
                &label.text,
                label.x as isize,
                self.metrics,
                self.style,
                &mut prepared.label_backgrounds,
            );
            Self::shape_runs(
                &label.text,
                label.x as isize,
                self.metrics,
                text_system,
                label.style,
                self.style,
                &mut prepared.label_runs,
            );
        }
        *cached = Some(prepared);
        cached.as_ref()
    }

    /// Merge successive cells of the same background in grid or measured-label coordinates.
    fn row_backgrounds(
        row: &FrameRow,
        origin: isize,
        metrics: Metrics,
        style: Style,
        output: &mut Vec<(Pixels, Pixels, u32)>,
    ) {
        let key = |paint: CellPaint| {
            if paint.attributes.reverse || paint.attributes.standout {
                (paint.foreground, true)
            } else {
                (paint.background, false)
            }
        };
        let cells = &row.cells;
        let mut col = 0;
        while col < cells.len() {
            let start = col;
            let paint = cells[col].paint;
            let bg = key(paint);
            while col < cells.len() && key(cells[col].paint) == bg {
                col += 1;
            }
            let color = paint.colors(style).1;
            if color != 0 {
                let x = metrics.spacing_px(origin + row.x(start));
                let end = metrics.spacing_px(origin + row.x(col));
                output.push((x, end - x, color));
            }
        }
    }

    /// Shape `row` at `origin` spacing units using `metrics` and `text_system`, appending to `runs`.
    /// The label selects its font, opacity or decoration. The theme supplies the colors.
    fn shape_runs(
        row: &FrameRow,
        origin: isize,
        metrics: Metrics,
        text_system: &WindowTextSystem,
        label: LabelStyle,
        style: Style,
        runs: &mut Vec<Run>,
    ) {
        let cells = &row.cells;
        let mut col = 0;
        while col < cells.len() {
            if cells[col].text.is_empty() {
                col += 1;
                continue;
            }
            let start = col;
            let paint = cells[col].paint;
            let dimmed = paint.dimmed;
            let same_ink = |other: CellPaint| {
                paint.foreground == other.foreground
                    && paint.dimmed == other.dimmed
                    && paint.attributes == other.attributes
                    && paint.special == other.special
                    && (!(paint.attributes.reverse || paint.attributes.standout)
                        || paint.background == other.background)
            };
            let opacity = if paint.attributes.conceal {
                0.
            } else if dimmed || paint.attributes.dim || label == LabelStyle::Dimmed {
                0.5
            } else {
                1.
            };
            let width = cells[col].text.width().max(1);
            let units = row.x(col + width) - row.x(col);
            let face = GridFace::for_cell(&cells[col].text);
            let mut text = String::new();
            while col < cells.len()
                && same_ink(cells[col].paint)
                && !cells[col].text.is_empty()
                && cells[col].text.width().max(1) == width
                && row.x(col + width) - row.x(col) == units
                && GridFace::for_cell(&cells[col].text) == face
            {
                text.push_str(&cells[col].text);
                col += width;
            }
            let decoration = Decoration::from_paint(paint, style);
            if opacity == 0.
                || text.trim().is_empty()
                    && !decoration.strike
                    && !decoration.overline
                    && decoration.underline.is_none()
            {
                continue;
            }
            let color = paint.colors(style).0;
            let mut font = match label {
                LabelStyle::Symbol => font(if cfg!(windows) {
                    "Segoe UI Symbol"
                } else {
                    GRID_RULE_FONT_FAMILY
                }),
                _ => face.font(style.font),
            };
            if paint.attributes.bold {
                font.weight = FontWeight::BOLD;
            }
            if paint.attributes.italic {
                font.style = FontStyle::Italic;
            }
            let run = TextRun {
                len: text.len(),
                font,
                color: rgb(color).opacity(opacity).into(),
                underline: (label == LabelStyle::Key).then_some(UnderlineStyle {
                    thickness: px(1.),
                    color: None,
                    wavy: false,
                }),
                ..Default::default()
            };
            let clusters = text
                .grapheme_indices(true)
                .map(|(index, _)| index)
                .collect();
            let shared: gpui::SharedString = text.into();
            let shadow_run = TextRun {
                color: rgba(style.shadow()).opacity(opacity).into(),
                underline: None,
                ..run.clone()
            };
            let advance = metrics.spacing_px(units);
            let shadow = text_system.shape_line(
                shared.clone(),
                px(metrics.font_size),
                &[shadow_run],
                Some(advance),
            );
            let line = text_system.shape_line(shared, px(metrics.font_size), &[run], Some(advance));
            let mut run = Run {
                column: start,
                x: metrics.spacing_px(origin + row.x(start)),
                line,
                shadow,
                advance: f32::from(advance),
                clusters,
                shifts: Vec::new(),
                color,
                opacity,
                decoration,
            };
            if units != (width * UNITS_PER_COLUMN) as isize {
                // Native shaping can leave small advance differences. Move each whole cluster
                // to its measured start and keep relative offsets of marks within the cluster.
                let mut shifts = vec![None; run.clusters.len()];
                for glyph in run.line.runs.iter().flat_map(|shaped| &shaped.glyphs) {
                    let cluster = run.cluster(glyph.index);
                    shifts[cluster].get_or_insert(advance * cluster as f32 - glyph.position.x);
                }
                run.shifts = shifts.into_iter().map(Option::unwrap_or_default).collect();
            }
            runs.push(run);
        }
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
        let text = &self.frame.rows.get(row)?.cells.get(column)?.text;
        let columns = text.width();
        if columns < 2 || text.trim().is_empty() {
            return None;
        }
        let layout = self.prepare_row(row, text_system)?;
        let (run, cluster) = layout.cluster_at(column, columns)?;
        let mut right = f32::NEG_INFINITY;
        for (font, glyph) in run.cluster_glyphs(cluster) {
            if glyph.is_emoji {
                return None;
            }
            let bounds = glyphs.typographic_bounds(font, glyph.id).ok()?;
            if bounds.size.width > 0. {
                let scale = f32::from(run.line.font_size) / text_system.units_per_em(font) as f32;
                right = right.max(
                    f32::from(run.position(glyph, cluster).x)
                        + (bounds.origin.x + bounds.size.width) * scale,
                );
            }
        }
        let width = right - cluster as f32 * run.advance;
        (width.is_finite() && width > 0.).then_some(width)
    }

    /// Fill the scoped background in `bounds`, leaving child panels to draw their own material.
    /// `fill` receives the nonoverlapping rectangles and the resolved RGBA color.
    fn paint_theme_background(
        &self,
        bounds: Bounds<Pixels>,
        mut fill: impl FnMut(Bounds<Pixels>, u32),
    ) {
        let Some(color) = self.style.component_background() else {
            return;
        };
        match self.frame.theme_background {
            ThemeBackground::Hidden => {}
            ThemeBackground::Full => fill(bounds, color),
            ThemeBackground::Excluding(interior) => {
                let interior = interior.pixels(Metrics {
                    origin: bounds.origin,
                    ..self.metrics
                });
                for area in exposed_regions(bounds, interior) {
                    if !area.is_empty() {
                        fill(area, color);
                    }
                }
            }
        }
    }

    /// Convert the source clip to device-snapped horizontal bounds in `window`.
    fn horizontal_grapheme_clip(&self, window: &Window) -> Option<Range<Pixels>> {
        let columns = self.frame.whole_grapheme_columns.as_ref()?;
        let snap = |column| {
            window
                .pixel_snap_point(self.metrics.at(column as f32, 0.))
                .x
        };
        Some(snap(columns.start).max(px(0.))..snap(columns.end).min(window.viewport_size().width))
    }

    /// Position the trailing tint against the fixed text clip and follow its row in `bounds`.
    fn trailing_tint(
        &self,
        bounds: Bounds<Pixels>,
        source_clip: Option<&Range<Pixels>>,
    ) -> Option<(Bounds<Pixels>, u32)> {
        let (row, color) = self.frame.trailing_row_tint?;
        let start = source_clip?.end;
        let y = bounds.top() + px(row as f32 * self.metrics.lh);
        let area =
            self.frame
                .tint_bounds(point(start, y), px(self.metrics.cw * 0.5), self.metrics.lh);
        Some((area, self.style.tint(color)))
    }

    /// Paint rows at `bounds` in `window` using `cx`.
    fn paint(&self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        let source_clip = self.horizontal_grapheme_clip(window);
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            self.paint_theme_background(bounds, |area, color| {
                window.paint_quad(fill(area, rgba(color)));
            });
            if let Some((area, color)) = self.trailing_tint(bounds, source_clip.as_ref()) {
                window.paint_quad(fill(area, rgba(color)));
            }
        });
        let clip = text_bounds(bounds, source_clip.as_ref());
        window.with_content_mask(Some(ContentMask { bounds: clip }), |window| {
            for outline in &self.frame.outlines {
                if let Some(path) = outline.path(bounds.origin, self.metrics) {
                    window.paint_path(path, rgb(self.style.foreground(outline.color)));
                }
            }
            for (i, row) in self.cache.iter().enumerate() {
                let Some(row) = row else { continue };
                let y = bounds.origin.y + px(i as f32 * self.metrics.lh);
                for &(x, width, color) in &row.backgrounds {
                    window.paint_quad(fill(
                        self.frame.tint_bounds(
                            point(bounds.origin.x + x, y),
                            width,
                            self.metrics.lh,
                        ),
                        rgba(color),
                    ));
                }
                let y = y + self.frame.title_offset(i);
                for &(x, width, color) in &row.label_backgrounds {
                    window.paint_quad(fill(
                        self.frame.tint_bounds(
                            point(bounds.origin.x + x, y),
                            width,
                            self.metrics.lh,
                        ),
                        rgba(color),
                    ));
                }
                for run in row.runs.iter().chain(&row.label_runs) {
                    let origin = point(bounds.origin.x + run.x, y);
                    run.paint_decorations(
                        origin,
                        self.metrics.lh,
                        source_clip.as_ref(),
                        |area, color| {
                            window.paint_quad(fill(area, rgba(color)));
                        },
                    );
                    let waves_here = self.waves.iter().any(|wave| {
                        (f32::from(y) + self.metrics.lh * 0.5 - wave.center.y).abs()
                            <= wave.front() + 7.
                    });
                    let run_fits = graphemes_fit(
                        source_clip.as_ref(),
                        origin.x,
                        run.clusters.len() as f32 * run.advance,
                    );
                    if waves_here || !run.shifts.is_empty() || !run_fits {
                        let waves = if waves_here {
                            self.waves.as_slice()
                        } else {
                            &[]
                        };
                        paint_glyph_run(
                            run,
                            origin,
                            self.metrics.lh,
                            waves,
                            self.style,
                            source_clip.as_ref(),
                            window,
                        );
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
                        origin,
                        px(self.metrics.lh),
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
                }
            }
            for &chevron in &self.frame.chevrons {
                let area = self
                    .frame
                    .chevron_bounds(chevron, bounds.origin, self.metrics);
                let (base, tip) = if chevron.points_left {
                    (area.right(), area.left())
                } else {
                    (area.left(), area.right())
                };
                let mut path = PathBuilder::fill();
                path.move_to(point(base, area.top()));
                path.line_to(point(tip, area.center().y));
                path.line_to(point(base, area.bottom()));
                path.close();
                if let Ok(path) = path.build() {
                    window.paint_path(path, rgba(self.style.tint(chevron.color)));
                }
            }
            let scale = window.scale_factor();
            for guide in &self.frame.guides {
                window.paint_quad(fill(
                    guide.bounds(bounds.origin, self.metrics, scale),
                    rgba(guide.color(self.style)),
                ));
            }
            for icon in &self.frame.icons {
                let origin = bounds.origin + point(px(0.), self.frame.title_offset(icon.row));
                cx.global::<IconCache>().paint(
                    icon.icon,
                    icon.bounds(origin, self.metrics, scale),
                    window,
                    cx,
                );
            }
        });
    }
}

/// Restrict ordinary row content to the text viewport, keeping any trailing tint separate.
fn text_bounds(bounds: Bounds<Pixels>, source_clip: Option<&Range<Pixels>>) -> Bounds<Pixels> {
    source_clip.map_or(bounds, |clip| {
        bounds.intersect(&Bounds::from_corners(
            point(clip.start, bounds.top()),
            point(clip.end.max(clip.start), bounds.bottom()),
        ))
    })
}

/// Whether the span of `width` logical pixels at `x` fits inside an optional horizontal `clip`.
fn graphemes_fit(clip: Option<&Range<Pixels>>, x: Pixels, width: f32) -> bool {
    // Fixed-point advances are converted separately from the snapped panel origin.
    let tolerance = px(0.001);
    clip.is_none_or(|clip| x + tolerance >= clip.start && x + px(width) <= clip.end + tolerance)
}

/// Return the accumulated tint and bounded glyph displacement of `waves` at `center`.
pub(in crate::grid) fn sample_waves(waves: &[Wave], center: Vec2) -> (f32, Point<Pixels>) {
    let (light, shift) = waves
        .iter()
        .fold((0., Vec2::default()), |(light, shift), wave| {
            let (value, offset) = wave.sample(center);
            (
                light + value,
                Vec2::new(shift.x + offset.x, shift.y + offset.y),
            )
        });
    (
        light,
        point(px(shift.x.clamp(-12., 12.)), px(shift.y.clamp(-12., 12.))),
    )
}

/// Paint native glyphs of `run` at `origin` in `window` using its row `height` and theme `style`.
/// Apply any active `waves` and keep whole graphemes inside the horizontal source `clip`.
fn paint_glyph_run(
    run: &Run,
    origin: Point<Pixels>,
    height: f32,
    waves: &[Wave],
    style: Style,
    clip: Option<&Range<Pixels>>,
    window: &mut Window,
) {
    let layout = &run.line;
    let baseline = run.baseline(height);
    for shaped in &layout.runs {
        for glyph in &shaped.glyphs {
            let cluster = run.cluster(glyph.index);
            let center = Vec2::new(
                f32::from(origin.x) + (cluster as f32 + 0.5) * run.advance,
                f32::from(origin.y) + height * 0.5,
            );
            let (light, shift) = sample_waves(waves, center);
            if !run.cluster_fits(clip, origin.x + shift.x, cluster) {
                continue;
            }
            let at = origin + run.position(glyph, cluster) + point(px(0.), baseline) + shift;
            if glyph.is_emoji {
                let _ = window.paint_emoji(at, shaped.font_id, glyph.id, layout.font_size);
            } else {
                let color = rgb(run.color).opacity(run.opacity).blend(rgba(
                    (style.foreground(ACCENT) << 8) | (light.clamp(0., 1.) * 205.) as u32,
                ));
                let _ = window.paint_glyph(
                    at + point(px(0.), px(1.)),
                    shaped.font_id,
                    glyph.id,
                    layout.font_size,
                    rgba(style.shadow()).opacity(run.opacity).into(),
                );
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
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut data = self.data.borrow_mut();
        let scoped = data.frame.theme_style(style(cx));
        data.set_style(scoped);
        drop(data);
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
mod tests;
