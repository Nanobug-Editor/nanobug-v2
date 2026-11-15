//! Lay out parsed Markdown in the editor's native text system and clip it to the preview pane.

mod anchors;
mod images;

use super::parser::{
    AlignedText, Alignment, Block, ImageSize, ImageWidth, Inline, InlineStyle, LocatedBlock,
    Markdown,
};
use crate::ui::{
    text::chinese_base,
    theme::{GRID_CHINESE_FONT, MUTED, Style, TEXT, ThemeComponent, style},
};
use anchors::Anchors;
use gpui::{
    AnyElement, App, Bounds, ContentMask, Corners, Font, FontWeight, Hsla, IntoElement, Pixels,
    Point, RenderImage, Size, StrikethroughStyle, Styled, TextAlign, TextRun, UnderlineStyle,
    Window, WindowTextSystem, WrappedLine, canvas, fill, point, px, rgb, rgba, size,
};
pub(super) use images::image_revision;
use images::{ImageState, Images};
use std::{ops::Range, path::Path, rc::Rc, sync::Arc};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;

/// Retained native shapes, positions and hit regions for a single preview width.
pub(in crate::grid::windows::editor) struct PreviewLayout {
    /// Paint-only rules, code backgrounds and table bands in document order.
    decorations: Vec<Decoration>,
    /// Text in ascending vertical order, including individually aligned table cells.
    text: Vec<TextLine>,
    /// Decoded images fitted to the same columns as their enclosing text.
    images: Vec<Picture>,
    /// Link rectangles for decoded images and their loading placeholders.
    image_links: Vec<(Bounds<Pixels>, String)>,
    /// Heading and named HTML destinations measured in preview-local pixels.
    anchors: Anchors,
    /// Increasing source rows paired with their actual vertical layout positions.
    scroll_points: Vec<(f32, f32)>,
    /// Full logical height including the outer Markdown margins.
    height: Pixels,
}

impl PreviewLayout {
    /// Shape `markdown` at the pane's `width` and user font size using `window` and `cx`.
    /// In `split` mode, leave one Chinese character of space inside the pane beyond the divider column.
    /// Resolve relative images from `document_path` and load remote images asynchronously.
    pub(super) fn new(
        markdown: &Markdown,
        width: Pixels,
        split: bool,
        font_size: f32,
        document_path: Option<&Path>,
        window: &mut Window,
        cx: &mut App,
    ) -> Self {
        let em = px(font_size);
        let padding = (em * 1.5).min(width * 0.10);
        let theme = style(cx).for_component(ThemeComponent::Editor);
        let system = Arc::clone(window.text_system());
        let split_inset = split.then(|| {
            let font = system.resolve_font(&GRID_CHINESE_FONT);
            system.advance(font, em, '中').map_or(em, |size| size.width)
        });
        let column = Column::preview(width, em, padding, split_inset);
        Self::shape(
            markdown,
            column,
            padding,
            em,
            theme,
            &system,
            Some(Images::new(document_path, window, cx)),
        )
    }

    /// Shape documentation at `width` using the panel's `font_size`, `theme` and `window`.
    /// The popup supplies padding and displays the alternatives for image references.
    pub(super) fn snippet(
        markdown: &Markdown,
        width: Pixels,
        font_size: f32,
        theme: Style,
        window: &Window,
    ) -> Self {
        Self::shape(
            markdown,
            Column {
                left: px(0.),
                width,
            },
            px(0.),
            px(font_size),
            theme,
            window.text_system(),
            None,
        )
    }

    /// Retain geometry for `markdown` using `column`, vertical `padding`, font size `em`,
    /// the `theme`, the text `system`, and any `images` available to this view.
    fn shape<'a>(
        markdown: &Markdown,
        column: Column,
        padding: Pixels,
        em: Pixels,
        theme: Style,
        system: &'a WindowTextSystem,
        images: Option<Images<'a>>,
    ) -> Self {
        let mut builder = LayoutBuilder {
            shapes: Self {
                decorations: Vec::new(),
                text: Vec::new(),
                images: Vec::new(),
                image_links: Vec::new(),
                anchors: Anchors::default(),
                scroll_points: vec![(0., 0.)],
                height: px(0.),
            },
            system,
            colors: Colors::for_theme(theme),
            font: theme.font.descriptor(),
            em,
            images,
        };
        let bottom = builder.blocks(&markdown.blocks, column, padding, builder.colors.body);
        builder.shapes.height = bottom + padding;
        builder
            .shapes
            .scroll_points
            .retain(|&(row, _)| row < markdown.source_rows as f32);
        builder.mark_source(Some(markdown.source_rows), bottom + padding);
        // Cells in the same table row can wrap to different heights. Sort by the
        // actual top of each shaped line before filling the overlap index.
        builder.shapes.text.sort_by_key(|line| line.bounds.top());
        let mut bottom = px(0.);
        for line in &mut builder.shapes.text {
            bottom = bottom.max(line.bounds.bottom());
            line.last_bottom = bottom;
        }
        builder.shapes
    }

    /// Return the full height in logical pixels for clamping vertical scroll.
    pub(in crate::grid::windows::editor) fn height(&self) -> Pixels {
        self.height
    }

    /// Return increasing points as (physical source row, preview position in logical pixels).
    pub(super) fn scroll_points(&self) -> &[(f32, f32)] {
        &self.scroll_points
    }

    /// Resolve a URL `fragment` to a vertical offset in the current laid-out document.
    pub(super) fn anchor_top(&self, fragment: &str) -> Option<usize> {
        if fragment.is_empty() {
            Some(0)
        } else {
            self.anchors
                .top(fragment)
                .map(|top| f32::from(top).floor() as usize)
        }
    }

    /// Resolve a link at a document-local `position` whose y includes the scroll offset.
    pub(in crate::grid::windows::editor) fn link_at(
        &self,
        position: Point<Pixels>,
    ) -> Option<&str> {
        let first = self
            .text
            .partition_point(|line| line.last_bottom <= position.y);
        self.text[first..]
            .iter()
            .take_while(|line| line.bounds.top() <= position.y)
            .find_map(|line| line.link_at(position))
            .or_else(|| {
                self.image_links
                    .iter()
                    .find_map(|(bounds, url)| bounds.contains(&position).then_some(url.as_str()))
            })
    }

    /// Paint the scrolled document in `viewport`, restricted to the unobscured `clip`.
    pub(super) fn element(
        self: &Rc<Self>,
        viewport: Bounds<Pixels>,
        clip: Bounds<Pixels>,
        offset: Pixels,
    ) -> AnyElement {
        self.viewport_element(viewport, clip, offset, true)
    }

    /// Paint a documentation viewport at `offset`, with its background supplied by the popup.
    pub(in crate::grid::windows::editor) fn snippet_element(
        self: &Rc<Self>,
        viewport: Bounds<Pixels>,
        offset: Pixels,
    ) -> AnyElement {
        self.viewport_element(viewport, viewport, offset, false)
    }

    /// Paint retained shapes in `viewport` at vertical `offset`, within `clip`.
    /// With `paint_background`, apply the editor component's background inside the viewport.
    fn viewport_element(
        self: &Rc<Self>,
        viewport: Bounds<Pixels>,
        clip: Bounds<Pixels>,
        offset: Pixels,
        paint_background: bool,
    ) -> AnyElement {
        let layout = Rc::clone(self);
        canvas(
            |_, _, _| (),
            move |bounds, _, window, cx| {
                let clip = clip.intersect(&bounds);
                if clip.is_empty() {
                    return;
                }
                let origin = bounds.origin - point(px(0.), offset);
                let top = clip.top() - origin.y;
                let bottom = clip.bottom() - origin.y;
                window.with_content_mask(Some(ContentMask { bounds: clip }), |window| {
                    if paint_background
                        && let Some(background) = style(cx)
                            .for_component(ThemeComponent::Editor)
                            .component_background()
                    {
                        window.paint_quad(fill(bounds, rgba(background)));
                    }
                    for shape in &layout.decorations {
                        if shape.bounds.top() < bottom && shape.bounds.bottom() > top {
                            window.paint_quad(
                                fill(translated(shape.bounds, origin), shape.color)
                                    .corner_radii(shape.radius),
                            );
                        }
                    }
                    for picture in &layout.images {
                        let bounds = translated(picture.bounds, origin);
                        if !bounds.intersect(&clip).is_empty() {
                            let _ = window.paint_image(
                                clip,
                                bounds,
                                Corners::default(),
                                picture.image.clone(),
                                0,
                                false,
                            );
                        }
                    }
                    let first = layout.text.partition_point(|line| line.last_bottom <= top);
                    for line in layout.text[first..]
                        .iter()
                        .take_while(|line| line.bounds.top() < bottom)
                    {
                        if line.bounds.bottom() > top {
                            line.paint(origin, clip, window, cx);
                        }
                    }
                });
            },
        )
        .absolute()
        .left(viewport.left())
        .top(viewport.top())
        .w(viewport.size.width)
        .h(viewport.size.height)
        .into_any_element()
    }
}

/// Horizontal constraints shared by blocks and table cells.
#[derive(Clone, Copy)]
struct Column {
    /// Left edge in preview-local logical pixels.
    left: Pixels,
    /// Available width in logical pixels.
    width: Pixels,
}

impl Column {
    /// In split mode apply `split_inset` symmetrically to keep centered content at the pane center.
    /// In preview-only mode cap the centered column at 61 `em` and keep `padding` on both sides.
    fn preview(width: Pixels, em: Pixels, padding: Pixels, split_inset: Option<Pixels>) -> Self {
        let content_width = split_inset.map_or_else(
            || (width - 2. * padding).min(em * 61.).max(px(1.)),
            |inset| (width - 2. * inset).max(px(1.)),
        );
        Self {
            left: (width - content_width) / 2.,
            width: content_width,
        }
    }

    /// Inset the two sides independently, retaining a positive shaping width in narrow panes.
    fn inset(self, left: Pixels, right: Pixels) -> Self {
        Self {
            left: self.left + left,
            width: (self.width - left - right).max(px(1.)),
        }
    }

    /// Return the rectangle spanning this column between `top` and `bottom`.
    fn bounds(self, top: Pixels, bottom: Pixels) -> Bounds<Pixels> {
        Bounds::new(point(self.left, top), size(self.width, bottom - top))
    }
}

/// A retained solid decoration beneath the text layer.
struct Decoration {
    /// Preview-local geometry.
    bounds: Bounds<Pixels>,
    /// Logical corner radius.
    radius: Pixels,
    /// Native fill.
    color: Hsla,
}

/// A scaled local image in its retained preview rectangle.
struct Picture {
    /// Preview-local image bounds preserving its source aspect ratio.
    bounds: Bounds<Pixels>,
    /// Decoded first frame, retained while the layout can reference the atlas entry.
    image: Arc<RenderImage>,
}

/// A native wrapped text line and link spans within its UTF-8 text.
struct TextLine {
    /// Preview-local rectangle, including all visual wraps.
    bounds: Bounds<Pixels>,
    /// Maximum bottom of this line and preceding lines in the paint index.
    last_bottom: Pixels,
    /// Layout and decorations created by the native shaper.
    line: WrappedLine,
    /// Height of each visual line.
    line_height: Pixels,
    /// Alignment within the available width, including per-column table alignment.
    align: TextAlign,
    /// Destination of each clickable byte range.
    links: Vec<(Range<usize>, String)>,
}

impl TextLine {
    /// Paint the shaped line relative to `origin`, intersecting the pane `clip` and cell bounds.
    fn paint(
        &self,
        origin: Point<Pixels>,
        clip: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let bounds = translated(self.bounds, origin);
        let mask = bounds.intersect(&clip);
        if mask.is_empty() {
            return;
        }
        window.with_content_mask(Some(ContentMask { bounds: mask }), |window| {
            let _ = self.line.paint_background(
                bounds.origin,
                self.line_height,
                self.align,
                Some(bounds),
                window,
                cx,
            );
            let _ = self.line.paint(
                bounds.origin,
                self.line_height,
                self.align,
                Some(bounds),
                window,
                cx,
            );
        });
    }

    /// Map a preview-local `position` back to a byte in the aligned wrapped line.
    fn link_at(&self, position: Point<Pixels>) -> Option<&str> {
        if self.links.is_empty()
            || !self.bounds.contains(&position)
            || position.y >= self.bounds.bottom()
        {
            return None;
        }
        let mut local = position - self.bounds.origin;
        if self.align != TextAlign::Left {
            let boundaries = self.line.wrap_boundaries();
            let row = ((local.y / self.line_height) as usize).min(boundaries.len());
            let boundary_x = |index: usize| {
                let wrap = &boundaries[index];
                self.line.runs()[wrap.run_ix].glyphs[wrap.glyph_ix]
                    .position
                    .x
            };
            let left = if row == 0 {
                px(0.)
            } else {
                boundary_x(row - 1)
            };
            let right = if row == boundaries.len() {
                self.line.unwrapped_layout.width
            } else {
                boundary_x(row)
            };
            let align_space = self.bounds.size.width - (right - left);
            local.x -= match self.align {
                TextAlign::Left => px(0.),
                TextAlign::Center => align_space / 2.,
                TextAlign::Right => align_space,
            };
        }
        let index = self.line.index_for_position(local, self.line_height).ok()?;
        self.links
            .iter()
            .find_map(|(bytes, url)| bytes.contains(&index).then_some(url.as_str()))
    }
}

/// Markdown colors resolved once when its width or editor palette changes.
struct Colors {
    /// Main foreground from the editor's configured theme.
    body: Hsla,
    /// Secondary foreground for quotes and unavailable image text.
    muted: Hsla,
    /// Standard GitHub link blue for the selected color scheme.
    link: Hsla,
    /// Subtle fill for code and alternating table rows.
    shade: Hsla,
    /// Rules, quote bars and table borders.
    edge: Hsla,
}

impl Colors {
    /// Keep text on the editor material and adapt the GitHub-inspired accents to `theme`.
    fn for_theme(theme: Style) -> Self {
        let (link, shade, edge) = if theme.light {
            (0x0969da, 0x818b981d, 0x57606a42)
        } else {
            (0x58a6ff, 0x6e768127, 0x8b949e59)
        };
        Self {
            body: rgb(theme.foreground(TEXT)).into(),
            muted: rgb(theme.foreground(MUTED)).into(),
            link: rgb(link).into(),
            shade: rgba(shade).into(),
            edge: rgba(edge).into(),
        }
    }
}

/// Typography for one text block or table cell.
#[derive(Clone, Copy)]
struct Typography {
    /// Active foreground, inherited by nested blocks.
    foreground: Hsla,
    /// Logical font size.
    size: Pixels,
    /// Height reserved for each visual line.
    line_height: Pixels,
    /// Apply the renderer's semibold face to headings and table headers.
    bold: bool,
    /// Alignment within the available column.
    align: TextAlign,
}

impl Typography {
    /// Use normal-weight, left-aligned body text at `size` and `foreground`.
    fn body(size: Pixels, foreground: Hsla) -> Self {
        Self {
            foreground,
            size,
            line_height: size * 1.5,
            bold: false,
            align: TextAlign::Left,
        }
    }

    /// Apply the parsed horizontal `alignment` without changing the text metrics.
    fn aligned(self, alignment: Alignment) -> Self {
        Self {
            align: alignment.into(),
            ..self
        }
    }
}

impl From<Alignment> for TextAlign {
    /// Convert a parsed `alignment` into the native text shaper's alignment.
    fn from(alignment: Alignment) -> Self {
        match alignment {
            Alignment::Left => Self::Left,
            Alignment::Center => Self::Center,
            Alignment::Right => Self::Right,
        }
    }
}

/// Native text runs and click ranges assembled from Markdown spans.
#[derive(Default)]
struct InlineLine {
    /// Text for one source line before width-dependent wrapping.
    text: String,
    /// Font and style of each consecutive UTF-8 run.
    runs: Vec<TextRun>,
    /// The UTF-8 span of each link mapped to its destination.
    links: Vec<(Range<usize>, String)>,
    /// Named HTML destinations mapped from offsets in the text to wrapped rows.
    anchors: Vec<(usize, String)>,
}

/// Accumulates retained geometry while shaping blocks from top to bottom.
struct LayoutBuilder<'a> {
    /// Paintable output and link hit data.
    shapes: PreviewLayout,
    /// Native shaper associated with the active window.
    system: &'a WindowTextSystem,
    /// Current Markdown palette.
    colors: Colors,
    /// The configured editor face for Latin text and code.
    font: Font,
    /// Base body font size in logical pixels.
    em: Pixels,
    /// Resources for file previews that can resolve image references.
    images: Option<Images<'a>>,
}

impl LayoutBuilder<'_> {
    /// Associate a `row` with its laid-out `y`, skipping collapsed or repeated positions.
    fn mark_source(&mut self, row: Option<usize>, y: Pixels) {
        let points = &mut self.shapes.scroll_points;
        let y = f32::from(y);
        if let Some(row) = row.map(|row| row as f32)
            && points
                .last()
                .is_none_or(|&(last_row, last_y)| row > last_row && y > last_y)
        {
            points.push((row, y));
        }
    }

    /// Lay out consecutive `blocks` in `column` starting at `y` and return their bottom.
    fn blocks(
        &mut self,
        blocks: &[LocatedBlock],
        column: Column,
        mut y: Pixels,
        foreground: Hsla,
    ) -> Pixels {
        let mut after_content = false;
        for LocatedBlock { block, source_row } in blocks {
            let anchors_only = matches!(block, Block::Paragraph(AlignedText { inlines, .. })
                if inlines.iter().all(|inline| matches!(inline, Inline::Anchor(_))));
            if after_content && !anchors_only {
                y += if matches!(block, Block::Heading { .. }) {
                    self.em * 1.5
                } else {
                    self.em
                };
            }
            if !anchors_only {
                self.mark_source(*source_row, y);
            }
            y = self.block(block, column, y, foreground);
            after_content |= !anchors_only;
        }
        y
    }

    /// Lay out `block` in `column` at `y`, inheriting the surrounding text `foreground`.
    fn block(&mut self, block: &Block, column: Column, y: Pixels, foreground: Hsla) -> Pixels {
        let body = Typography::body(self.em, foreground);
        match block {
            Block::Heading {
                level,
                inlines,
                id,
                alignment,
            } => {
                self.shapes.anchors.heading(inlines, id.as_deref(), y);
                let scale = match level {
                    1 => 2.,
                    2 => 1.5,
                    3 => 1.25,
                    4 => 1.,
                    5 => 0.875,
                    _ => 0.85,
                };
                let heading = Typography {
                    size: self.em * scale,
                    line_height: self.em * scale * 1.25,
                    bold: true,
                    foreground: if *level >= 6 {
                        self.colors.muted
                    } else {
                        foreground
                    },
                    ..body.aligned(*alignment)
                };
                let mut bottom = self.inlines(inlines, column, y, heading);
                if *level <= 2 {
                    bottom += self.em * 0.3;
                    self.decoration(column, bottom, bottom + px(1.), self.colors.edge, px(0.));
                    bottom += px(1.);
                }
                bottom
            }
            Block::Paragraph(text) => {
                self.inlines(&text.inlines, column, y, body.aligned(text.alignment))
            }
            Block::Code {
                content,
                alignment,
                line_sources,
            } => self.code(content, line_sources, column, y, body.aligned(*alignment)),
            Block::Quote(blocks) => {
                let inset = self.em.min(column.width * 0.25);
                let bottom = self
                    .blocks(blocks, column.inset(inset, px(0.)), y, self.colors.muted)
                    .max(y + body.line_height);
                self.decoration(
                    Column {
                        width: px(3.).min(inset),
                        ..column
                    },
                    y,
                    bottom,
                    self.colors.edge,
                    px(0.),
                );
                bottom
            }
            Block::List { start, items } => self.list(*start, items, column, y, body),
            Block::Rule => {
                self.decoration(column, y, y + px(3.), self.colors.edge, px(0.));
                y + px(3.)
            }
            Block::Table {
                headings,
                rows,
                row_sources,
            } => self.table(headings, rows, row_sources, column, y, body),
        }
    }

    /// Paint `code` in `column` at `y` using `type_style`, recording the actual `line_sources`.
    fn code(
        &mut self,
        code: &str,
        line_sources: &[Option<usize>],
        column: Column,
        y: Pixels,
        type_style: Typography,
    ) -> Pixels {
        let side_pad = self.em.min(column.width * 0.16);
        let top_pad = self.em * 0.75;
        let text_column = column.inset(side_pad, side_pad);
        let plain = InlineStyle::default();
        let mut bottom = y + top_pad;
        let mut lines = code.lines().peekable();
        if lines.peek().is_none() {
            bottom += type_style.line_height;
        }
        for (index, line) in lines.enumerate() {
            self.mark_source(line_sources.get(index).copied().flatten(), bottom);
            let mut shaped = InlineLine::default();
            self.append(
                &mut shaped,
                &expand_code_tabs(line),
                &plain,
                type_style,
                false,
            );
            bottom = self.shape(shaped, text_column, bottom, type_style);
        }
        bottom += top_pad;
        self.decoration(column, y, bottom, self.colors.shade, px(6.));
        bottom
    }

    /// Draw one marker per item and align its recursively rendered blocks with a hanging indent.
    fn list(
        &mut self,
        start: Option<u64>,
        items: &[Vec<LocatedBlock>],
        column: Column,
        mut y: Pixels,
        type_style: Typography,
    ) -> Pixels {
        let last = start.map(|start| start.saturating_add(items.len().saturating_sub(1) as u64));
        let digits = last.map_or(1, |last| last.to_string().len());
        let indent = (self.em * (1.05 + digits as f32 * 0.55)).min(column.width * 0.4);
        let marker_column = Column {
            width: (indent - self.em * 0.4).max(px(1.)),
            ..column
        };
        let item_column = column.inset(indent, px(0.));
        for (index, item) in items.iter().enumerate() {
            if index > 0 {
                y += self.em * 0.25;
            }
            if !matches!(item.first().map(|item| &item.block), Some(Block::Paragraph(AlignedText { inlines, .. }))
                if matches!(inlines.first(), Some(Inline::Checkbox(_))))
            {
                let marker = start.map_or_else(
                    || "•".to_owned(),
                    |start| format!("{}.", start.saturating_add(index as u64)),
                );
                self.inlines(
                    &[Inline::Text {
                        content: marker,
                        style: InlineStyle::default(),
                    }],
                    marker_column,
                    y,
                    Typography {
                        align: TextAlign::Right,
                        ..type_style
                    },
                );
            }
            y = self
                .blocks(item, item_column, y, type_style.foreground)
                .max(y + type_style.line_height);
        }
        y
    }

    /// Shape `headings` and `rows` at `y` in fitted columns using each cell's alignment.
    /// The full `column` limits their width, `type_style` sets typography, and `row_sources` locates each row.
    fn table(
        &mut self,
        headings: &[AlignedText],
        rows: &[Vec<AlignedText>],
        row_sources: &[Option<usize>],
        column: Column,
        y: Pixels,
        type_style: Typography,
    ) -> Pixels {
        let count = rows
            .iter()
            .map(Vec::len)
            .max()
            .unwrap_or(0)
            .max(headings.len());
        if count == 0 {
            return y;
        }
        let mut weights = vec![6.; count];
        let table_rows = || {
            (!headings.is_empty())
                .then_some(headings)
                .into_iter()
                .chain(rows.iter().map(Vec::as_slice))
        };
        for row in table_rows() {
            for (weight, cell) in weights.iter_mut().zip(row.iter()) {
                let preferred = cell
                    .inlines
                    .iter()
                    .map(inline_columns)
                    .sum::<usize>()
                    .clamp(6, 42) as f32;
                *weight = preferred.max(*weight);
            }
        }
        let points: f32 = weights.iter().sum();
        let columns = weights
            .into_iter()
            .scan(column.left, |left, weight| {
                let width = column.width * (weight / points);
                let cell_column = Column { left: *left, width };
                *left += width;
                Some(cell_column)
            })
            .collect::<Vec<_>>();
        let mut top = y + px(1.);
        self.decoration(column, y, top, self.colors.edge, px(0.));
        for (index, row) in table_rows().enumerate() {
            self.mark_source(row_sources.get(index).copied().flatten(), top);
            let padding_y = self.em * 0.35;
            let mut bottom = top + type_style.line_height + 2. * padding_y;
            for (index_in_row, cell_column) in columns.iter().copied().enumerate() {
                let pad_x = (self.em * 0.65).min(cell_column.width * 0.18);
                let Some(cell) = row.get(index_in_row) else {
                    continue;
                };
                let text_bottom = self.inlines(
                    &cell.inlines,
                    cell_column.inset(pad_x, pad_x),
                    top + padding_y,
                    Typography {
                        bold: index == 0 && !headings.is_empty(),
                        ..type_style.aligned(cell.alignment)
                    },
                );
                bottom = bottom.max(text_bottom + padding_y);
            }
            if index % 2 == 0 {
                self.decoration(column, top, bottom, self.colors.shade, px(0.));
            }
            self.decoration(column, bottom, bottom + px(1.), self.colors.edge, px(0.));
            top = bottom + px(1.);
        }
        for left in columns
            .iter()
            .map(|col| col.left)
            .chain([column.left + column.width - px(1.)])
        {
            self.decoration(
                Column {
                    left,
                    width: px(1.),
                },
                y,
                top,
                self.colors.edge,
                px(0.),
            );
        }
        top
    }

    /// Convert `inlines` to shaped text, breaking paragraphs at explicit Markdown and HTML breaks.
    fn inlines(
        &mut self,
        inlines: &[Inline],
        column: Column,
        mut y: Pixels,
        type_style: Typography,
    ) -> Pixels {
        let mut line = InlineLine::default();
        let mut after_image = false;
        let plain = InlineStyle::default();
        for inline in inlines {
            match inline {
                Inline::Text { content, style } => {
                    for (index, part) in content.split('\n').enumerate() {
                        if index > 0 {
                            y = self.shape(std::mem::take(&mut line), column, y, type_style);
                        }
                        self.append(&mut line, part, style, type_style, false);
                    }
                    after_image = false;
                }
                Inline::Break => {
                    if !after_image {
                        y = self.shape(std::mem::take(&mut line), column, y, type_style);
                    }
                    after_image = false;
                }
                Inline::Image {
                    alt,
                    url,
                    link,
                    size,
                    alignment,
                } => {
                    let image_state = self
                        .images
                        .as_mut()
                        .zip(url.as_deref())
                        .map_or(ImageState::Failed, |(images, url)| images.get(url));
                    let image = match image_state {
                        ImageState::Ready(image) => Some(image),
                        ImageState::Pending => None,
                        ImageState::Failed => {
                            let fallback = if alt.is_empty() {
                                url.as_deref().unwrap_or("")
                            } else {
                                alt
                            };
                            self.append(
                                &mut line,
                                &fallback.replace(['\r', '\n'], " "),
                                &InlineStyle {
                                    link: link.clone(),
                                    ..InlineStyle::default()
                                },
                                type_style,
                                true,
                            );
                            after_image = false;
                            continue;
                        }
                    };
                    if !line.text.trim().is_empty() || !line.anchors.is_empty() {
                        y = self.shape(std::mem::take(&mut line), column, y, type_style);
                    } else {
                        line = InlineLine::default();
                    }
                    let align = alignment.map_or(type_style.align, TextAlign::from);
                    y = self.image(image, *size, link.as_deref(), column, y, align);
                    after_image = true;
                }
                Inline::Anchor(id) => {
                    if after_image {
                        self.shapes.anchors.insert(id.clone(), y);
                    } else {
                        line.anchors.push((line.text.len(), id.clone()));
                    }
                }
                Inline::Checkbox(checked) => {
                    let marker = if *checked { "☑ " } else { "☐ " };
                    self.append(&mut line, marker, &plain, type_style, false);
                    after_image = false;
                }
            }
        }
        if after_image {
            y
        } else {
            self.shape(line, column, y, type_style)
        }
    }

    /// Fit an `image` or placeholder using its `requested` size and optional `link`.
    /// Position it within `column` at `y` using `align`.
    fn image(
        &mut self,
        image: Option<Arc<RenderImage>>,
        requested: ImageSize,
        link: Option<&str>,
        column: Column,
        y: Pixels,
        align: TextAlign,
    ) -> Pixels {
        let natural = image.as_ref().map_or(
            size((self.em * 8.).min(column.width), self.em * 1.5),
            |image| {
                let dimensions = image.size(0);
                size(
                    self.em * (dimensions.width.0 as f32 / 16.),
                    self.em * (dimensions.height.0 as f32 / 16.),
                )
            },
        );
        let Size { width, height } = fit_image_size(requested, natural, column.width);
        if width <= px(0.) || height <= px(0.) {
            return y;
        }
        let free_width = column.width - width;
        let left = column.left
            + match align {
                TextAlign::Left => px(0.),
                TextAlign::Center => free_width / 2.,
                TextAlign::Right => free_width,
            };
        let bounds = Bounds::new(point(left, y), size(width, height));
        if let Some(link) = link {
            self.shapes.image_links.push((bounds, link.into()));
        }
        if let Some(image) = image {
            self.shapes.images.push(Picture { bounds, image });
        } else {
            self.shapes.decorations.push(Decoration {
                bounds,
                color: self.colors.shade,
                radius: px(4.),
            });
        }
        y + height
    }

    /// Append one `content` fragment using its Markdown `style` over the block typography.
    fn append(
        &self,
        line: &mut InlineLine,
        content: &str,
        style: &InlineStyle,
        type_style: Typography,
        muted: bool,
    ) {
        if content.is_empty() {
            return;
        }
        let start = line.text.len();
        line.text.push_str(content);
        if let Some(url) = &style.link {
            line.links.push((start..line.text.len(), url.clone()));
        }
        let mut font = self.font.clone();
        if style.bold || type_style.bold {
            font.weight = FontWeight::SEMIBOLD;
        }
        if style.italic || muted {
            font = font.italic();
        }
        let thickness = (self.em * 0.06).max(px(1.));
        let run = TextRun {
            len: 0,
            font,
            color: if style.link.is_some() {
                self.colors.link
            } else if muted {
                self.colors.muted
            } else {
                type_style.foreground
            },
            background_color: style.code.then_some(self.colors.shade),
            underline: style.link.is_some().then_some(UnderlineStyle {
                thickness,
                color: None,
                wavy: false,
            }),
            strikethrough: style.strike.then_some(StrikethroughStyle {
                thickness,
                color: None,
            }),
        };
        append_editor_runs(&mut line.runs, content, &run);
    }

    /// Shape an assembled `line` in `column` and reserve the space required by its wraps.
    fn shape(
        &mut self,
        line: InlineLine,
        column: Column,
        y: Pixels,
        type_style: Typography,
    ) -> Pixels {
        let text_is_empty = line.text.is_empty();
        if !text_is_empty
            && let Ok(mut shaped) = self.system.shape_text(
                line.text.into(),
                type_style.size,
                &line.runs,
                Some(column.width.max(px(1.))),
                None,
            )
            && let Some(shaped) = shaped.pop()
        {
            for (index, id) in line.anchors {
                let offset = shaped
                    .position_for_index(index, type_style.line_height)
                    .map_or(px(0.), |position| position.y);
                self.shapes.anchors.insert(id, y + offset);
            }
            let bottom = y + shaped.size(type_style.line_height).height;
            self.shapes.text.push(TextLine {
                bounds: column.bounds(y, bottom),
                last_bottom: bottom,
                line: shaped,
                line_height: type_style.line_height,
                align: type_style.align,
                links: line.links,
            });
            bottom
        } else {
            let anchors_only = text_is_empty && !line.anchors.is_empty();
            for (_, id) in line.anchors {
                self.shapes.anchors.insert(id, y);
            }
            if anchors_only {
                y
            } else {
                y + type_style.line_height
            }
        }
    }

    /// Place a solid decoration across `column` from `top` to `bottom`.
    fn decoration(
        &mut self,
        column: Column,
        top: Pixels,
        bottom: Pixels,
        color: Hsla,
        radius: Pixels,
    ) {
        self.shapes.decorations.push(Decoration {
            bounds: column.bounds(top, bottom),
            color,
            radius,
        });
    }
}

/// Use the declared dimensions in `requested`, deriving an omitted side from the `natural` ratio.
/// Fit the resulting rectangle proportionally into the available `column_width`.
fn fit_image_size(
    requested: ImageSize,
    natural: Size<Pixels>,
    column_width: Pixels,
) -> Size<Pixels> {
    let available = f64::from(f32::from(column_width));
    let natural_width = f64::from(f32::from(natural.width));
    let natural_height = f64::from(f32::from(natural.height));
    let width = requested.width.map(|width| match width {
        ImageWidth::Pixels(width) => f64::from(width),
        ImageWidth::Percent(percent) => available * f64::from(percent) / 100.,
    });
    let height = requested.height.map(f64::from);
    let (width, height) = match (width, height) {
        (Some(width), Some(height)) => (width, height),
        (Some(width), None) => (width, width * natural_height / natural_width),
        (None, Some(height)) => (height * natural_width / natural_height, height),
        (None, None) => (natural_width, natural_height),
    };
    if width > available {
        size(column_width, px((height * available / width) as f32))
    } else {
        size(px(width as f32), px(height as f32))
    }
}

/// Append consecutive graphemes from nonempty `content` to `runs`.
/// `style` supplies the editor's selected Latin face and Markdown text styling.
fn append_editor_runs(runs: &mut Vec<TextRun>, content: &str, style: &TextRun) {
    let mut append = |len, chinese| {
        let font = if chinese {
            Font {
                weight: style.font.weight,
                style: style.font.style,
                ..GRID_CHINESE_FONT.clone()
            }
        } else {
            style.font.clone()
        };
        runs.push(TextRun {
            len,
            font,
            ..style.clone()
        });
    };
    let mut start = 0;
    let mut chinese = chinese_base(content).is_some();
    for (byte, grapheme) in content.grapheme_indices(true).skip(1) {
        let next_chinese = chinese_base(grapheme).is_some();
        if next_chinese != chinese {
            append(byte - start, chinese);
            start = byte;
            chinese = next_chinese;
        }
    }
    append(content.len() - start, chinese);
}

/// Estimate the intrinsic table width contribution of one inline using Unicode display columns.
fn inline_columns(inline: &Inline) -> usize {
    match inline {
        Inline::Text { content, .. } | Inline::Image { alt: content, .. } => {
            content.chars().map(|ch| ch.width().unwrap_or(0)).sum()
        }
        Inline::Break | Inline::Anchor(_) => 0,
        Inline::Checkbox(_) => 2,
    }
}

/// Expand code tabs to common four-column stops before passing the line to the native shaper.
fn expand_code_tabs(text: &str) -> String {
    let mut column = 0;
    let mut expanded = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch == '\t' {
            let count = 4 - column % 4;
            expanded.extend(std::iter::repeat_n(' ', count));
            column += count;
        } else {
            expanded.push(ch);
            column += ch.width().unwrap_or(0);
        }
    }
    expanded
}

/// Move preview-local `bounds` by the scrolled window `origin`.
fn translated(bounds: Bounds<Pixels>, origin: Point<Pixels>) -> Bounds<Pixels> {
    Bounds::new(bounds.origin + origin, bounds.size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::GridFont;

    /// Keep the measured split inset centered across pane widths and allow narrow panes to shrink it.
    #[test]
    fn split_preview_reserves_one_chinese_advance_inside_the_pane() {
        let wide = px(1400.);
        let em = px(16.);
        let padding = px(24.);
        let advance = px(15.5);
        for width in [px(360.), wide] {
            let split = Column::preview(width, em, padding, Some(advance));
            assert_eq!(split.left, advance);
            assert_eq!(split.width, width - 2. * advance);
        }
        let narrow = Column::preview(px(8.), em, padding, Some(advance));
        assert_eq!(narrow.left, px(3.5));
        assert_eq!(narrow.width, px(1.));
        let standalone = Column::preview(wide, em, padding, None);
        assert_eq!(standalone.width, em * 61.);
        assert_eq!(standalone.left, (wide - standalone.width) / 2.);
    }

    /// Fit explicit pixel and percentage sizes while preserving the appropriate aspect ratio.
    #[test]
    fn markdown_image_size_uses_declared_or_intrinsic_ratio() {
        let dimensions = |width, height| {
            fit_image_size(
                ImageSize { width, height },
                size(px(80.), px(40.)),
                px(120.),
            )
        };
        let pixels = |width| Some(ImageWidth::Pixels(width));
        assert_eq!(dimensions(pixels(52.), Some(65.)), size(px(52.), px(65.)));
        assert_eq!(dimensions(pixels(36.), None), size(px(36.), px(18.)));
        assert_eq!(dimensions(None, Some(23.)), size(px(46.), px(23.)));
        assert_eq!(
            dimensions(pixels(600.), Some(300.)),
            size(px(120.), px(60.))
        );
        assert_eq!(
            dimensions(Some(ImageWidth::Percent(50.)), None),
            size(px(60.), px(30.))
        );
        assert_eq!(dimensions(pixels(0.), None), size(px(0.), px(0.)));
    }

    /// Match editor font selection without losing Markdown styling or splitting graphemes.
    #[test]
    fn preview_uses_the_configured_editor_faces_for_mixed_text() {
        let latin = "Latin e\u{301} ";
        let chinese = "中文字，汉\u{e0100}";
        let suffix = " code";
        let text = format!("{latin}{chinese}{suffix}");
        for configured in [GridFont::RecMono, GridFont::MapleMono] {
            let style = TextRun {
                font: configured.descriptor().bold().italic(),
                background_color: Some(rgb(0x123456).into()),
                ..TextRun::default()
            };
            let mut runs = Vec::new();
            append_editor_runs(&mut runs, &text, &style);
            assert_eq!(runs.len(), 3);
            assert_eq!(
                runs.iter().map(|run| run.len).collect::<Vec<_>>(),
                [latin.len(), chinese.len(), suffix.len()]
            );
            assert_eq!(runs[0].font, style.font);
            assert_eq!(runs[1].font, GRID_CHINESE_FONT.clone().bold().italic());
            assert_eq!(runs[2].font, style.font);
            assert!(
                runs.iter()
                    .all(|run| run.background_color == style.background_color)
            );
        }
    }
}
