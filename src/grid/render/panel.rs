//! Retained row shaping, glyph painting, cursor text and backdrop stamping.

use super::{Frame, FrameRow, LabelStyle, Metrics};
use crate::{
    ui::icons::IconCache,
    ui::{
        backdrop::BackdropRaster,
        text::{UNITS_PER_COLUMN, chinese_base},
        theme::{
            ACCENT, CURSOR_TEXT, GRID_CHINESE_FONT, GRID_RULE_FONT_FAMILY, GRID_TEXT_FONT,
            TREE_GUIDE,
        },
    },
};
use gpui::{
    App, Bounds, ContentMask, Context, Font, FontId, GlyphId, IntoElement, PathBuilder, Pixels,
    PlatformTextSystem, Point, Render, RenderGlyphParams, ShapedGlyph, ShapedLine, Styled,
    TextAlign, TextRun, UnderlineStyle, Window, WindowTextSystem, canvas, fill, font, point, px,
    rgb, rgba,
};
use nanobug::animation::cursor::{Vec2, Wave};
use std::{cell::RefCell, rc::Rc};
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
    fn font(self) -> Font {
        match self {
            Self::Text => GRID_TEXT_FONT.clone(),
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
    /// Base ink color.
    color: u32,
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
}

/// All prepared drawing data for a row.
struct PreparedRow {
    /// Background runs as visual start, width and RGBA color.
    backgrounds: Vec<(Pixels, Pixels, u32)>,
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
    /// Paint the covered character in the theme's cursor text color with byte `alpha` in `window`.
    pub fn paint(&self, alpha: u32, window: &mut Window) {
        let color = rgba((CURSOR_TEXT << 8) | alpha).into();
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
            for &(x, width, color) in &prepared.backgrounds {
                let area = data
                    .frame
                    .tint_bounds(point(bounds.origin.x + x, y), width, metrics.lh);
                raster.fill(area.intersect(&clip), color);
            }
            for run in prepared.runs.iter().chain(&prepared.label_runs) {
                let baseline = point(bounds.origin.x + run.x, y + run.baseline(metrics.lh));
                for shaped in &run.line.runs {
                    for glyph in &shaped.glyphs {
                        raster.glyph(
                            baseline + run.position(glyph, run.cluster(glyph.index)),
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
        for &chevron in &data.frame.chevrons {
            let area = data.frame.chevron_bounds(chevron, bounds.origin, metrics);
            raster.chevron(area, clip, chevron.points_left, chevron.color);
        }
        let scale = window.scale_factor();
        for guide in &data.frame.guides {
            raster.fill(
                guide.bounds(bounds.origin, metrics, scale).intersect(&clip),
                TREE_GUIDE,
            );
        }
        for icon in &data.frame.icons {
            raster.icon(icon.icon, icon.bounds(bounds.origin, metrics), clip);
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
        let cell_x = metrics.spacing_px(row.x(column));
        data.prepare_row(cell.row, window.text_system())?;
        let (run, cluster) = data.cache[cell.row].as_ref()?.cluster_at(column, width)?;
        let origin = cell.origin + point(run.x - cell_x, px(0.));
        let center = Vec2::new(
            f32::from(origin.x) + (cluster as f32 + 0.5) * run.advance,
            f32::from(origin.y) + metrics.lh * 0.5,
        );
        let (_, shift) = sample_waves(&data.waves, center);
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
        let cells = &positioned.cells;
        let cached = self.cache.get_mut(row)?;
        if cached.is_some() {
            return cached.as_ref();
        }
        let mut prepared = PreparedRow {
            backgrounds: Vec::new(),
            runs: Vec::new(),
            label_runs: Vec::new(),
        };
        let mut col = 0;
        while col < cells.len() {
            let start = col;
            let color = cells[col].bg;
            while col < cells.len() && cells[col].bg == color {
                col += 1;
            }
            if color != 0 {
                let x = self.metrics.spacing_px(positioned.x(start));
                let end = self.metrics.spacing_px(positioned.x(col));
                prepared.backgrounds.push((x, end - x, color));
            }
        }
        Self::shape_runs(
            positioned,
            0,
            self.metrics,
            text_system,
            LabelStyle::Text,
            &mut prepared.runs,
        );
        for label in &self.frame.labels[row] {
            Self::shape_runs(
                &label.text,
                label.x as isize,
                self.metrics,
                text_system,
                label.style,
                &mut prepared.label_runs,
            );
        }
        *cached = Some(prepared);
        cached.as_ref()
    }

    /// Shape `row` at `origin` spacing units using `metrics` and `text_system`, appending to `runs`.
    /// `style` selects any font or decoration override for the label.
    fn shape_runs(
        row: &FrameRow,
        origin: isize,
        metrics: Metrics,
        text_system: &WindowTextSystem,
        style: LabelStyle,
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
            let color = cells[col].fg;
            let width = cells[col].text.width().max(1);
            let units = row.x(col + width) - row.x(col);
            let face = GridFace::for_cell(&cells[col].text);
            let mut text = String::new();
            while col < cells.len()
                && cells[col].fg == color
                && !cells[col].text.is_empty()
                && cells[col].text.width().max(1) == width
                && row.x(col + width) - row.x(col) == units
                && GridFace::for_cell(&cells[col].text) == face
            {
                text.push_str(&cells[col].text);
                col += width;
            }
            if text.trim().is_empty() {
                continue;
            }
            let run = TextRun {
                len: text.len(),
                font: match style {
                    LabelStyle::Symbol => font(if cfg!(windows) {
                        "Segoe UI Symbol"
                    } else {
                        GRID_RULE_FONT_FAMILY
                    }),
                    _ => face.font(),
                },
                color: rgb(color).into(),
                underline: (style == LabelStyle::Key).then_some(UnderlineStyle {
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
                color: rgba(0x00000070).into(),
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

    /// Paint rows at `bounds` in `window` using `cx`.
    fn paint(&self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
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
                for run in row.runs.iter().chain(&row.label_runs) {
                    let origin = point(bounds.origin.x + run.x, y);
                    let waves_here = self.waves.iter().any(|wave| {
                        (f32::from(y) + self.metrics.lh * 0.5 - wave.center.y).abs()
                            <= wave.front() + 7.
                    });
                    if waves_here || !run.shifts.is_empty() {
                        let waves = if waves_here {
                            self.waves.as_slice()
                        } else {
                            &[]
                        };
                        paint_glyph_run(run, origin, self.metrics.lh, waves, window);
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
                    window.paint_path(path, rgba(chevron.color));
                }
            }
            let scale = window.scale_factor();
            for guide in &self.frame.guides {
                window.paint_quad(fill(
                    guide.bounds(bounds.origin, self.metrics, scale),
                    rgba(TREE_GUIDE),
                ));
            }
            for icon in &self.frame.icons {
                cx.global::<IconCache>().paint(
                    icon.icon,
                    icon.bounds(bounds.origin, self.metrics),
                    window,
                    cx,
                );
            }
        });
    }
}

/// Return the accumulated tint and bounded glyph displacement of `waves` at `center`.
fn sample_waves(waves: &[Wave], center: Vec2) -> (f32, Point<Pixels>) {
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

/// Paint native glyphs of `run` at `origin` in `window`, using row `height` and any active `waves`.
fn paint_glyph_run(
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
            let cluster = run.cluster(glyph.index);
            let center = Vec2::new(
                f32::from(origin.x) + (cluster as f32 + 0.5) * run.advance,
                f32::from(origin.y) + height * 0.5,
            );
            let (light, shift) = sample_waves(waves, center);
            let at = origin + run.position(glyph, cluster) + point(px(0.), baseline) + shift;
            if glyph.is_emoji {
                let _ = window.paint_emoji(at, shaped.font_id, glyph.id, layout.font_size);
            } else {
                let color =
                    rgb(run.color).blend(rgba((ACCENT << 8) | (light.clamp(0., 1.) * 205.) as u32));
                let _ = window.paint_glyph(
                    at + point(px(0.), px(1.)),
                    shaped.font_id,
                    glyph.id,
                    layout.font_size,
                    rgba(0x00000070).into(),
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
mod tests;
