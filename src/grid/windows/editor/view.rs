//! Derive the editor header, line numbers and text from the active Tabpage.

use super::{
    EditorWindow, TEXT_OVERSCAN,
    diagnostics::DiagnosticUnderlines,
    signs::{EditorGutter, RowSigns},
};
use crate::{
    grid::{
        render::{CellPaint, Frame, Rect},
        windows::WindowFrame,
    },
    ui::{
        components::grid_window::GridWindow,
        icons::Icon,
        theme::{
            CURRENT, FOLDED, FOLDED_CURRENT, GOLD, MUTED, PREEDIT, TEXT, ThemeComponent,
            WARNING_TEXT, syntax_color,
        },
    },
};
use nanobug::vim::InputMappings;
use nanobug_plugin_sdk::language::{Highlight, SyntaxKind};

/// Empty source columns between the rendered text and the Markdown split divider.
const MARKDOWN_SOURCE_RIGHT_PADDING: usize = 1;
/// Reserve two digits before expanding the gutter for longer documents.
const MIN_LINE_NUMBER_COLUMNS: usize = 2;

/// Find the syntax role at document byte `offset` from sorted `highlights`.
fn syntax_span(highlights: &[Highlight], offset: usize) -> Option<Highlight> {
    let index = highlights.partition_point(|span| span.range.end <= offset);
    highlights
        .get(index)
        .filter(|span| span.range.start <= offset)
        .copied()
}

/// Right-facing fold marker with a tight crop at the arrow tip.
pub(super) static FOLD_ARROW: Icon = Icon::new(
    include_bytes!("../../../../assets/icons/fold-chevron.svg"),
    8.8 / 13.4,
);

/// The same fold marker in amber for a header with a diagnostic.
static DIAGNOSTIC_FOLD_ARROW: Icon = Icon::new(
    include_bytes!("../../../../assets/icons/fold-chevron-warning.svg"),
    8.8 / 13.4,
);

/// Fixed chrome and independently clipped content, as frames or their retained panels.
pub(in crate::grid) struct EditorLayers<T> {
    /// Border with Bufferline and Tabpage labels.
    pub chrome: T,
    /// Vertically scrolling line numbers.
    pub numbers: T,
    /// Text scrolling on both axes.
    pub text: T,
}

impl EditorWindow {
    /// Lay out an editor in `area` with file tabs on the title border and no fixed interior rows.
    pub(in crate::grid) fn grid_window(area: Rect) -> GridWindow {
        GridWindow::new(area, 0, 0)
    }

    /// Return the scrollable body inside the bordered grid rectangle `area`.
    pub(in crate::grid) fn body(area: Rect) -> Rect {
        Self::grid_window(area).body()
    }

    /// Return the text viewport within `area`, accounting for the gutter, divider gap and view height.
    pub(in crate::grid) fn text_area(&self, area: Rect) -> Rect {
        let body = self.source_body(area);
        if self.terminal().is_some() {
            return body;
        }
        let gutter = self.gutter(body.w).width;
        let width = body.w - gutter;
        let padding = if self.markdown_view_mode() == Some(super::MarkdownViewMode::Split) {
            MARKDOWN_SOURCE_RIGHT_PADDING.min(width.saturating_sub(1))
        } else {
            0
        };
        Rect {
            x: body.x + gutter,
            w: width - padding,
            h: self.vim().view_height(body.h),
            ..body
        }
    }

    /// Resolve the current buffer's line-number and sign columns inside the source viewport.
    fn gutter(&self, available: usize) -> EditorGutter {
        let options = self.window_options();
        let number_width = if options.number || options.relativenumber {
            self.vim()
                .line_count()
                .to_string()
                .len()
                .max(MIN_LINE_NUMBER_COLUMNS)
        } else {
            1 // Retain the fold and diagnostic gutter without reserving digits.
        };
        EditorGutter::new(number_width, available, self.vim().doc.has_signs())
    }

    /// Index of the clickable fold marker within a `gutter`-cell row, leaving one cell before the text.
    pub(super) fn fold_arrow_column(gutter: usize) -> Option<usize> {
        gutter.checked_sub(2).filter(|&column| column > 0)
    }

    /// Draw the editor in `context`, keeping its header within `visible_cols` of the screen.
    pub(in crate::grid) fn frames(
        &self,
        context: WindowFrame<'_>,
        visible_cols: usize,
        mappings: &InputMappings,
    ) -> EditorLayers<Frame> {
        let window = Self::grid_window(context.area);
        let mut edit = Frame::grid_window(
            window,
            ThemeComponent::Editor,
            context.selected,
            |frame, _, _| {
                self.header(context.area, context.spacing, visible_cols)
                    .paint(frame, context.spacing)
            },
        );
        if self.markdown_view_mode() == Some(super::MarkdownViewMode::Split)
            && let Some(preview) = self.markdown_preview_area(context.area)
        {
            let divider = preview.x - context.area.x - 1;
            let body = window.body();
            let first = body.y - context.area.y;
            for row in first..first + body.h {
                edit.vertical_rule(divider, row, MUTED);
            }
        }
        let (numbers, text) = self.content_frames_with_mappings(context, |_| None, mappings);
        EditorLayers {
            chrome: edit,
            numbers,
            text,
        }
    }

    /// Draw text and line numbers for `context`, using `color_at` for optional syntax colors by byte offset.
    /// Built-in Markdown colors fill gaps left by the provider and `color_at`.
    /// Show line numbers on the first visual row of each line, tinting folded lines and the active row.
    pub(in crate::grid) fn content_frames(
        &self,
        context: WindowFrame<'_>,
        color_at: impl Fn(usize) -> Option<u32>,
    ) -> (Frame, Frame) {
        self.content_frames_with_mappings(context, color_at, self.register.input_mappings())
    }

    /// Resolve shared Ex styles while keeping each rendering pane's own match slots.
    fn content_frames_with_mappings(
        &self,
        context: WindowFrame<'_>,
        color_at: impl Fn(usize) -> Option<u32>,
        mappings: &InputMappings,
    ) -> (Frame, Frame) {
        let buffer = &self.buffers[self.active];
        if let Some(terminal) = &buffer.terminal {
            let composition = if context.selected {
                self.composition.text.as_str()
            } else {
                ""
            };
            return (
                Frame::new(0, 0),
                terminal.frame(self.text_area(context.area), composition),
            );
        }
        let vim = &buffer.vim;
        let caret_line = buffer.cursor_row();
        let last_line = (vim.line_count() - 1).max(caret_line);
        let caret_logical_row = vim.doc.display_row(caret_line);
        let active = context.selected;
        let selection_tint = context.selection_tint();
        let options = self.window_options();
        let tabstop = vim.doc.tabstop();
        let substitution = vim.substitution_match();
        let body = self.text_area(context.area);
        let source = self.source_body(context.area);
        let gutter = self.gutter(source.w);
        let number_width = gutter.number.len();
        let divider_padding = source.x + source.w - (body.x + body.w);
        if body.h == 0 {
            return (Frame::new(0, 0), Frame::new(0, 0));
        }
        let composition = self.composition.text.as_str();
        let rows = self.visual_rows(body.w, context.spacing);
        let caret_display_row = if rows.is_wrapped() {
            buffer.visual_caret(&rows, context.spacing, composition).0
        } else {
            caret_logical_row
        };
        let first = buffer.scroll.first_cell();
        let mut numbers = Frame::viewport(gutter.width, body.h + 1, first);
        numbers.set_theme_component(ThemeComponent::Gutter);
        let (left, left_padding) = if rows.is_wrapped() {
            (0, 0)
        } else {
            (
                buffer.columns.first_cell() - buffer.left_padding(),
                buffer.left_padding(),
            )
        };
        let width = body.w + left_padding + TEXT_OVERSCAN;
        let right = left + width;
        let mut text = Frame::viewport(width, body.h + 1, first);
        text.set_theme_component(ThemeComponent::Editor);
        text.keep_whole_graphemes(body.x..body.x + body.w);
        let highlights = buffer.highlights();
        let markdown = buffer.markdown_highlights();
        let [native, semantic] = buffer.syntax_layers.each_ref().map(|layer| {
            layer
                .as_ref()
                .filter(|d| {
                    d.version == (vim.doc.opening, vim.doc.revision)
                        && vim.doc.path.as_ref() == Some(&d.path)
                })
                .map_or(&[][..], |d| d.highlights.as_slice())
        });
        let base = CellPaint::new(TEXT, 0, false);
        let normal = (!active)
            .then(|| mappings.highlight_override("NormalNC"))
            .flatten()
            .or_else(|| mappings.highlight_override("Normal"))
            .map_or(base, |style| base.with_highlight(style));
        let search = mappings.highlight_override("Search");
        let visual = mappings.highlight_override("Visual");
        let syntax = mappings.syntax_styles();
        let source_styles = |offset, tint| {
            let mut paint = normal;
            if tint != 0 {
                paint.background = tint;
            }
            let lexical = syntax_span(highlights, offset).or_else(|| syntax_span(native, offset));
            let semantic = syntax_span(semantic, offset);
            let (kind, fallback) = if let Some(kind) = semantic
                .map(|mut s| {
                    if let Some(finer) = lexical
                        && std::iter::successors(finer.kind.parent(), |k| k.parent())
                            .any(|k| k == s.kind)
                    {
                        s.kind = finer.kind;
                    }
                    s
                })
                .or(lexical)
            {
                (Some(kind), None)
            } else if let Some(color) = color_at(offset) {
                (None, Some(color))
            } else {
                (
                    markdown
                        .as_deref()
                        .and_then(|spans| syntax_span(spans, offset)),
                    None,
                )
            };
            if let Some(span) = kind {
                let kind = span.kind;
                if let Some(style) = syntax[kind as usize] {
                    paint = paint.with_highlight(style);
                } else {
                    paint.foreground = syntax_color(kind);
                }
                for (bit, modifier) in nanobug_plugin_sdk::language::SyntaxModifiers::FLAGS {
                    if span.modifiers.0 & bit != 0 {
                        let style = syntax[modifier as usize].unwrap_or_else(|| {
                            let mut s = nanobug::vim::HighlightStyle::default();
                            s.attributes.strikethrough = modifier == SyntaxKind::Deprecated;
                            s
                        });
                        paint = paint.with_highlight(style);
                    }
                }
            } else if let Some(color) = fallback {
                paint.foreground = color;
            }
            if let Some(style) = buffer.message_highlight(offset) {
                paint = paint.with_highlight(style);
            }
            paint
        };
        let doc = &vim.doc;
        let signs = doc.signs();
        let sign_column = gutter
            .diagnostic
            .and_then(|_| mappings.highlight_override("SignColumn"));
        let cursor_sign_column = gutter.diagnostic.and_then(|_| {
            mappings
                .highlight_override("CursorLineSign")
                .or(sign_column)
                .or_else(|| mappings.highlight_style("CursorLineSign"))
        });
        let visible_start = rows
            .row(first)
            .map_or(0, |row| doc.starts[row.line.saturating_sub(1)]);
        let visible_end = rows
            .row((first + body.h).min(rows.len().saturating_sub(1)))
            .map_or(doc.text.len(), |row| {
                let end = doc.folded_end_row(row.line).unwrap_or(row.line);
                doc.starts.get(end + 2).copied().unwrap_or(doc.text.len())
            });
        let underlines = if self.diagnostic_marks {
            DiagnosticUnderlines::new(buffer.diagnostics(), visible_start..visible_end)
        } else {
            DiagnosticUnderlines::default()
        };
        let slot_matches = super::ex_matches::visible_matches(
            self.register.ex_matches(),
            mappings,
            &doc.text,
            visible_start..visible_end,
        );
        let ex_style = |offset| {
            slot_matches
                .iter()
                .flatten()
                .find_map(|rule| rule.at(offset))
        };
        let mut painted_line = None;
        for visible in 0..=body.h {
            let display_row = first + visible;
            let Some(visual_row) = rows.row(display_row).filter(|row| row.line <= last_line) else {
                gutter.paint_sign(&mut numbers, visible, None, sign_column);
                numbers.text(
                    gutter.number.end.saturating_sub(1),
                    visible,
                    "~",
                    MUTED,
                    number_width.min(1),
                );
                if normal.background != 0 {
                    text.tint(visible, 0, width, normal.background);
                }
                continue;
            };
            let line = visual_row.line;
            let current = display_row == caret_display_row && active;
            let folded = doc.folded_at(line);
            let row_tint = match (folded, current) {
                (true, true) => FOLDED_CURRENT,
                (true, false) => FOLDED,
                (false, true) => CURRENT,
                (false, false) => 0,
            };
            if row_tint != 0 {
                numbers.tint(visible, 0, gutter.width, row_tint);
            }
            // Several adjacent visual rows can share one composed or folded line.
            if painted_line
                .as_ref()
                .is_none_or(|(previous, _, _, _, _)| *previous != line)
            {
                painted_line = Some((
                    line,
                    buffer.painted_line(line, composition),
                    vim.selection_on_line(line),
                    vim.search_highlights(vim.doc.line_range(line)),
                    RowSigns::new(&signs, line, line == caret_line && active, mappings),
                ));
            }
            let (_, painted, selection, matches, signs_on_line) = painted_line.as_ref().unwrap();
            let first_segment = visual_row
                .segment
                .as_ref()
                .is_none_or(|segment| segment.bytes.start == 0);
            let column_style = if line == caret_line && active {
                cursor_sign_column
            } else {
                sign_column
            };
            gutter.paint_sign(
                &mut numbers,
                visible,
                first_segment.then_some(signs_on_line),
                column_style,
            );
            if first_segment {
                let distance = vim.doc.display_row(line).abs_diff(caret_logical_row);
                if options.number || options.relativenumber {
                    let number = if options.relativenumber && (!options.number || distance != 0) {
                        distance
                    } else {
                        line + 1
                    };
                    let number_color = if line == caret_line && active {
                        GOLD
                    } else {
                        MUTED
                    };
                    numbers.highlighted_text(
                        gutter.number.start,
                        visible,
                        &format!("{number:>number_width$}"),
                        number_color,
                        number_width,
                        signs_on_line.number,
                    );
                }
                if let Some(column) = Self::fold_arrow_column(gutter.width) {
                    let diagnostic =
                        self.diagnostic_marks && buffer.line_diagnostics(line).next().is_some();
                    if diagnostic && let Some(diagnostic_column) = gutter.diagnostic {
                        numbers.text(diagnostic_column, visible, "!", WARNING_TEXT, 1);
                    }
                    if folded {
                        let icon = if diagnostic && gutter.diagnostic.is_none() {
                            DIAGNOSTIC_FOLD_ARROW
                        } else {
                            FOLD_ARROW
                        };
                        numbers.right_aligned_icon(column, visible, icon, 56);
                    } else if diagnostic && gutter.diagnostic.is_none() {
                        numbers.text(column, visible, "!", WARNING_TEXT, 1);
                    }
                }
            }
            let (fragment, byte_start, tab_column, indent_units) =
                visual_row
                    .segment
                    .map_or((painted.text.as_ref(), 0, 0, 0), |segment| {
                        let byte_start = segment.bytes.start;
                        (
                            &painted.text[segment.bytes],
                            byte_start,
                            segment.tab_column,
                            segment.indent_units,
                        )
                    });
            let tint = if row_tint == 0 {
                normal.background
            } else {
                row_tint
            };
            let tint = signs_on_line.line.map_or(tint, |style| {
                CellPaint::new(TEXT, tint, false)
                    .with_highlight(style)
                    .background
            });
            if row_tint != 0 && divider_padding > 0 {
                // Match the half-column gap between the left outline and the gutter tint.
                text.extend_row_tint(visible, tint);
            }
            let decorate = |paint: CellPaint| {
                signs_on_line
                    .line
                    .map_or(paint, |style| paint.with_highlight(style))
            };
            let underline = |paint: CellPaint, offset, len| {
                underlines
                    .color(offset..offset + len)
                    .map_or(paint, |ink| paint.with_undercurl(ink))
            };
            let end_paint = if painted.fold_suffix.is_none()
                && byte_start + fragment.len() == painted.text.len()
            {
                let line_end = doc.line_range(line).end;
                let end = if line == last_line {
                    doc.text.len()
                } else {
                    line_end
                };
                underlines
                    .at_line_end(line_end..=end)
                    .map(|ink| CellPaint::new(TEXT, tint, false).with_undercurl(ink))
            } else {
                None
            };
            text.styled_viewport_line(
                visible,
                context
                    .spacing
                    .buffer_cells(fragment, byte_start, tab_column, tabstop),
                left..right,
                indent_units,
                tint,
                end_paint,
                |bytes| {
                    let byte = bytes.start;
                    if let Some(suffix) = painted
                        .fold_suffix
                        .as_ref()
                        .filter(|suffix| byte >= suffix.start_byte)
                    {
                        return if byte < suffix.closing_byte {
                            CellPaint::new(0xffffff, tint, true)
                        } else {
                            let offset = suffix.closing_offset + byte - suffix.closing_byte;
                            let paint = decorate(source_styles(offset, tint));
                            let paint =
                                ex_style(offset).map_or(paint, |style| paint.with_highlight(style));
                            underline(paint, offset, bytes.len())
                        };
                    }
                    if painted
                        .preedit
                        .as_ref()
                        .is_some_and(|range| range.contains(&byte))
                    {
                        CellPaint::new(GOLD, PREEDIT, false)
                    } else {
                        let byte = byte
                            - painted
                                .preedit
                                .as_ref()
                                .filter(|range| byte >= range.end)
                                .map_or(0, |range| range.len());
                        let offset = vim.doc.starts[line] + byte;
                        let paint = decorate(source_styles(offset, tint));
                        let matched = ex_style(offset);
                        let paint = if selection
                            .as_ref()
                            .or(substitution.as_ref())
                            .is_some_and(|range| range.contains(&offset))
                        {
                            let mut paint =
                                matched.map_or(paint, |style| paint.with_highlight(style));
                            if let Some(style) = visual {
                                paint.with_highlight(style)
                            } else {
                                paint.background = selection_tint;
                                paint
                            }
                        } else if let Some(style) = matched {
                            paint.with_highlight(style)
                        } else if matches
                            .get(matches.partition_point(|hit| hit.end <= offset))
                            .is_some_and(|hit| hit.contains(&offset))
                        {
                            search.map_or_else(
                                || CellPaint::new(GOLD, selection_tint, false),
                                |style| paint.with_highlight(style),
                            )
                        } else {
                            paint
                        };
                        underline(paint, offset, bytes.len())
                    }
                },
            );
        }

        (numbers, text)
    }
}
