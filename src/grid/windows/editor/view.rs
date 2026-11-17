//! Derive the editor header, line numbers and text from the active Tabpage.

use super::{EditorWindow, TEXT_OVERSCAN};
use crate::{
    grid::{
        render::{Frame, Rect},
        windows::WindowFrame,
    },
    ui::{
        icons::Icon,
        theme::{CURRENT, GOLD, MUTED, PREEDIT, TEXT, ThemeComponent, WARNING_TEXT},
    },
};
use nanobug_plugin_sdk::language::Highlight;

/// Empty source columns between the rendered text and the Markdown split divider.
const MARKDOWN_SOURCE_RIGHT_PADDING: usize = 1;
/// Reserve two digits before expanding the gutter for longer documents.
const MIN_LINE_NUMBER_COLUMNS: usize = 2;

/// Resolve the foreground role at document byte `offset` from sorted `highlights`.
fn syntax_foreground(highlights: &[Highlight], offset: usize) -> Option<u32> {
    let index = highlights.partition_point(|span| span.range.end <= offset);
    highlights
        .get(index)
        .filter(|span| span.range.start <= offset)
        .map(|span| crate::ui::theme::syntax_color(span.kind))
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

/// Retained frames used by the editor's fixed chrome and independently clipped text layers.
pub(in crate::grid) struct EditorFrames {
    /// Border with Bufferline and Tabpage labels.
    pub chrome: Frame,
    /// Vertically scrolling line numbers.
    pub numbers: Frame,
    /// Text scrolling on both axes.
    pub text: Frame,
}

impl EditorWindow {
    /// Return the scrollable body inside the bordered grid rectangle `area`.
    pub(in crate::grid) fn body(area: Rect) -> Rect {
        Rect {
            x: area.x + 1,
            y: area.y + 1,
            w: area.w.saturating_sub(2),
            h: area.h.saturating_sub(2),
        }
    }

    /// Return the text viewport within `area`, accounting for the gutter, divider gap and view height.
    pub(in crate::grid) fn text_area(&self, area: Rect) -> Rect {
        let body = self.source_body(area);
        if self.terminal().is_some() {
            return body;
        }
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
        let gutter = (number_width + 2).min(body.w);
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

    /// Index of the clickable fold marker within a `gutter`-cell row, leaving one cell before the text.
    pub(super) fn fold_arrow_column(gutter: usize) -> Option<usize> {
        gutter.checked_sub(2).filter(|&column| column > 0)
    }

    /// Draw the editor in `context`, keeping its header within `visible_cols` of the screen.
    pub(in crate::grid) fn frames(
        &self,
        context: WindowFrame<'_>,
        visible_cols: usize,
    ) -> EditorFrames {
        let mut edit = Frame::new(context.area.w, context.area.h);
        edit.set_theme_component(ThemeComponent::Editor);
        edit.exclude_theme_background(Self::body(Rect {
            x: 0,
            y: 0,
            ..context.area
        }));
        self.header(context.area, context.spacing, visible_cols)
            .paint(&mut edit, context.spacing, context.selected);
        if self.markdown_view_mode() == Some(super::MarkdownViewMode::Split)
            && let Some(preview) = self.markdown_preview_area(context.area)
        {
            let divider = preview.x - context.area.x - 1;
            for row in 1..context.area.h.saturating_sub(1) {
                edit.vertical_rule(divider, row, MUTED);
            }
        }
        let (numbers, text) = self.content_frames(context, |_| None);
        EditorFrames {
            chrome: edit,
            numbers,
            text,
        }
    }

    /// Draw text and line numbers for `context`, using `color_at` for optional syntax colors by byte offset.
    /// Built-in Markdown colors fill gaps left by the provider and `color_at`.
    /// Show line numbers on the first visual row of each line and highlight the caret's visual row.
    pub(in crate::grid) fn content_frames(
        &self,
        context: WindowFrame<'_>,
        color_at: impl Fn(usize) -> Option<u32>,
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
        let gutter = body.x - source.x;
        let number_width = gutter.saturating_sub(2);
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
        let mut numbers = Frame::viewport(gutter, body.h + 1, first);
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
        let foreground = |offset| {
            syntax_foreground(highlights, offset)
                .or_else(|| color_at(offset))
                .or_else(|| {
                    markdown
                        .as_deref()
                        .and_then(|spans| syntax_foreground(spans, offset))
                })
                .unwrap_or(TEXT)
        };
        let mut painted_line = None;
        for visible in 0..=body.h {
            let display_row = first + visible;
            let Some(visual_row) = rows.row(display_row).filter(|row| row.line <= last_line) else {
                numbers.text(
                    number_width.saturating_sub(1),
                    visible,
                    "~",
                    MUTED,
                    number_width.min(1),
                );
                continue;
            };
            let line = visual_row.line;
            let current = display_row == caret_display_row && active;
            if current {
                numbers.tint(visible, 0, gutter, CURRENT);
                if divider_padding > 0 {
                    // The left outline is centered in its border cell. Match its half-column gap.
                    text.extend_row_tint(visible, CURRENT);
                }
            }
            if visual_row
                .segment
                .as_ref()
                .is_none_or(|segment| segment.bytes.start == 0)
            {
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
                    numbers.text(
                        0,
                        visible,
                        &format!("{number:>number_width$}"),
                        number_color,
                        number_width,
                    );
                }
                if let Some(column) = Self::fold_arrow_column(gutter) {
                    let diagnostic = buffer.diagnostic_on_line(line);
                    if vim.doc.folded_at(line) {
                        let icon = if diagnostic {
                            DIAGNOSTIC_FOLD_ARROW
                        } else {
                            FOLD_ARROW
                        };
                        numbers.right_aligned_icon(column, visible, icon, 56);
                    } else if diagnostic {
                        numbers.text(column, visible, "!", WARNING_TEXT, 1);
                    }
                }
            }
            // Several adjacent visual rows can share one composed or folded line.
            if painted_line
                .as_ref()
                .is_none_or(|(previous, _, _, _)| *previous != line)
            {
                painted_line = Some((
                    line,
                    buffer.painted_line(line, composition),
                    vim.selection_on_line(line),
                    vim.search_highlights(vim.doc.line_range(line)),
                ));
            }
            let (_, painted, selection, matches) = painted_line.as_ref().unwrap();
            let (fragment, byte_start, tab_column) =
                visual_row
                    .segment
                    .map_or((painted.text.as_ref(), 0, 0), |segment| {
                        let byte_start = segment.bytes.start;
                        (&painted.text[segment.bytes], byte_start, segment.tab_column)
                    });
            let tint = if current { CURRENT } else { 0 };
            text.viewport_line(
                visible,
                context
                    .spacing
                    .buffer_cells(fragment, byte_start, tab_column, tabstop),
                left..right,
                tint,
                |byte| {
                    if let Some(suffix) = painted
                        .fold_suffix
                        .as_ref()
                        .filter(|suffix| byte >= suffix.start_byte)
                    {
                        return if byte < suffix.closing_byte {
                            (0xffffff, tint, true)
                        } else {
                            (
                                foreground(suffix.closing_offset + byte - suffix.closing_byte),
                                tint,
                                false,
                            )
                        };
                    }
                    if painted
                        .preedit
                        .as_ref()
                        .is_some_and(|range| range.contains(&byte))
                    {
                        (GOLD, PREEDIT, false)
                    } else {
                        let byte = byte
                            - painted
                                .preedit
                                .as_ref()
                                .filter(|range| byte >= range.end)
                                .map_or(0, |range| range.len());
                        let offset = vim.doc.starts[line] + byte;
                        if selection
                            .as_ref()
                            .or(substitution.as_ref())
                            .is_some_and(|range| range.contains(&offset))
                        {
                            (foreground(offset), selection_tint, false)
                        } else if matches
                            .get(matches.partition_point(|hit| hit.end <= offset))
                            .is_some_and(|hit| hit.contains(&offset))
                        {
                            (GOLD, selection_tint, false)
                        } else {
                            (foreground(offset), tint, false)
                        }
                    }
                },
            );
        }

        (numbers, text)
    }
}
