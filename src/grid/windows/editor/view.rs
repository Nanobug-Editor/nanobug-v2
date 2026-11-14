//! Derive the editor header, line numbers and text from the active Tabpage.

use super::{EditorWindow, TEXT_OVERSCAN, buffer::FOLD_ELLIPSIS};
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

/// Right-facing fold marker, with its visible path inside one character cell.
pub(super) static FOLD_ARROW: Icon = Icon::new(
    include_bytes!("../../../../assets/icons/fold-chevron.svg"),
    1.,
);

/// The same fold marker in amber for a header with a diagnostic.
static DIAGNOSTIC_FOLD_ARROW: Icon = Icon::new(
    include_bytes!("../../../../assets/icons/fold-chevron-warning.svg"),
    1.,
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

    /// Return the text viewport inside `area` after applying the gutter and any requested height.
    pub(in crate::grid) fn text_area(&self, area: Rect) -> Rect {
        let body = Self::body(area);
        let gutter = (self.vim().line_count().to_string().len().max(4) + 2).min(body.w);
        Rect {
            x: body.x + gutter,
            w: body.w - gutter,
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
        let (numbers, text) = self.content_frames(context, |_| None);
        EditorFrames {
            chrome: edit,
            numbers,
            text,
        }
    }

    /// Draw text and line numbers for `context`, using `color_at` for optional syntax colors by byte offset.
    /// The caret row shows its document line number. Other rows show the number of j/k steps to reach them.
    pub(in crate::grid) fn content_frames(
        &self,
        context: WindowFrame<'_>,
        color_at: impl Fn(usize) -> Option<u32>,
    ) -> (Frame, Frame) {
        let buffer = &self.buffers[self.active];
        let vim = &buffer.vim;
        let row = vim.doc.location(vim.doc.cursor).0;
        let caret_display_row = vim.doc.display_row(row);
        let last_display_row = vim
            .doc
            .display_row(vim.line_count() - 1)
            .max(caret_display_row);
        let active = context.selected;
        let selection_tint = context.selection_tint();
        let body = self.text_area(context.area);
        let gutter = body.x - Self::body(context.area).x;
        let first = buffer.scroll.first_cell();
        let mut numbers = Frame::viewport(gutter, body.h + 1, first);
        numbers.set_theme_component(ThemeComponent::Gutter);
        let left = buffer.columns.first_cell() - buffer.left_padding();
        let width = body.w + buffer.left_padding() + TEXT_OVERSCAN;
        let right = left + width;
        let mut text = Frame::viewport(width, body.h + 1, first);
        text.set_theme_component(ThemeComponent::Editor);
        let composition = if active {
            self.composition.text.as_str()
        } else {
            ""
        };
        for visible in 0..=body.h {
            let display_row = first + visible;
            if display_row > last_display_row {
                numbers.text(2, visible, "~", MUTED, 1);
                continue;
            }
            let line = vim.doc.document_row(display_row);
            let distance = display_row.abs_diff(caret_display_row);
            let number = if distance == 0 { line + 1 } else { distance };
            let current = row == line && active;
            if current {
                numbers.tint(visible, 0, gutter, CURRENT);
            }
            let number_color = if current { GOLD } else { MUTED };
            let width = gutter.saturating_sub(2);
            let folded = vim.doc.folded_at(line);
            if folded {
                numbers.text(
                    0,
                    visible,
                    &format!("{number:>width$}"),
                    number_color,
                    width,
                );
                if let Some(column) = Self::fold_arrow_column(gutter) {
                    let icon = if buffer.diagnostic_on_line(line) {
                        DIAGNOSTIC_FOLD_ARROW
                    } else {
                        FOLD_ARROW
                    };
                    numbers.icon(column, visible, icon);
                }
            } else {
                numbers.text(
                    0,
                    visible,
                    &format!("{number:>width$} │"),
                    number_color,
                    gutter,
                );
                if gutter >= 2 && buffer.diagnostic_on_line(line) {
                    numbers.text(gutter - 1, visible, "!", WARNING_TEXT, 1);
                }
            }
            let (mut displayed, preedit) = buffer.displayed_line(line, composition);
            let mut source_len = displayed.len();
            let closing_offset = buffer.folded_brace(line).map(|(offset, brace)| {
                source_len = buffer.fold_header(line, &displayed).len();
                // Append paint-only cells after the real line and any active IME text.
                let displayed = displayed.to_mut();
                displayed.truncate(source_len);
                displayed.push_str(FOLD_ELLIPSIS);
                displayed.push_str(brace);
                offset
            });
            let closing_byte = source_len + FOLD_ELLIPSIS.len();
            let selection = vim.selection_on_line(line);
            let tint = if current { CURRENT } else { 0 };
            let highlights = buffer.highlights();
            let foreground = |offset| {
                let index = highlights.partition_point(|span| span.range.end <= offset);
                highlights
                    .get(index)
                    .filter(|span| span.range.start <= offset)
                    .map(|span| crate::ui::theme::syntax_color(span.kind))
                    .or_else(|| color_at(offset))
                    .unwrap_or(TEXT)
            };
            text.viewport_line(
                visible,
                context.spacing.cells(&displayed),
                left..right,
                tint,
                |byte| {
                    if let Some(offset) = closing_offset.filter(|_| byte >= source_len) {
                        return if byte < closing_byte {
                            (0xffffff, tint, true)
                        } else {
                            (foreground(offset + byte - closing_byte), tint, false)
                        };
                    }
                    if preedit.as_ref().is_some_and(|range| range.contains(&byte)) {
                        (GOLD, PREEDIT, false)
                    } else {
                        let byte = byte
                            - preedit
                                .as_ref()
                                .filter(|range| byte >= range.end)
                                .map_or(0, |range| range.len());
                        let offset = vim.doc.starts[line] + byte;
                        let background = if selection
                            .as_ref()
                            .is_some_and(|range| range.contains(&offset))
                        {
                            selection_tint
                        } else {
                            tint
                        };
                        (foreground(offset), background, false)
                    }
                },
            );
        }

        (numbers, text)
    }
}
