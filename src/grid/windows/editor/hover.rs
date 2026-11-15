//! Fit rendered Markdown documentation next to the editor caret.

use super::{
    EditorWindow,
    markdown::{MarkdownSnippet, PreviewLayout},
};
use crate::{
    grid::render::{Frame, LabelStyle, Metrics, OutlineStyle, Rect, outline_path},
    ui::{
        text::TextSpacing,
        theme::{MUTED, TEXT, ThemeComponent, style},
    },
};
use gpui::{
    AnyElement, App, Bounds, IntoElement, Pixels, Point, Size, Styled, Window, canvas, fill, point,
    px, rgb, rgba, size,
};
use nanobug::{animation::scroll::GridScroll, i18n, vim::Mode};
use std::{cell::Cell, rc::Rc};

/// Leave a quarter of a text row above the body and below the footer for the outline.
const EDGE_ROWS: f32 = 0.25;
/// Limit the documentation body independently of the space for the footer and outline.
const MAX_CONTENT_ROWS: usize = 13;
/// Maximum popup width in grid columns, including both edges.
const MAX_COLUMNS: usize = 76;

/// One server reply with its retained Markdown layout and preferred popup width.
pub(in crate::grid) struct DefinitionHoverContent {
    /// Parsed documentation and the layouts reused by the popup.
    markdown: MarkdownSnippet,
    /// Preferred content columns at the most recently measured width and font size.
    preferred: Cell<Option<(WidthKey, usize)>>,
}

/// Inputs to the grid's preferred-width measurement for the source Markdown.
#[derive(Clone, Copy, PartialEq)]
struct WidthKey {
    /// Available content columns.
    columns: usize,
    /// Logical width of a grid column.
    cell_width: f32,
    /// Logical size used to measure the bundled text faces.
    font_size: f32,
}

impl DefinitionHoverContent {
    /// Retain the server's `text` as Markdown and defer measuring it until layout.
    pub(in crate::grid) fn new(text: String) -> Self {
        Self {
            markdown: MarkdownSnippet::new(text),
            preferred: Cell::new(None),
        }
    }

    /// Return the original server text for the native integration test.
    #[cfg(all(test, windows))]
    pub(in crate::grid) fn text(&self) -> &str {
        self.markdown.text()
    }

    /// Estimate the natural width from the longest source line using `spacing` and the cap in `key`.
    fn preferred_columns(&self, key: WidthKey, spacing: &TextSpacing) -> usize {
        if let Some((_, width)) = self.preferred.get().filter(|(cached, _)| *cached == key) {
            return width;
        }
        let mut width = 0;
        for line in self.markdown.text().lines() {
            width = width.max(spacing.columns(line.trim_end())).min(key.columns);
            if width == key.columns {
                break;
            }
        }
        self.preferred.set(Some((key, width)));
        width
    }
}

/// A fitted definition detail panel and its window-local cutout.
pub(in crate::grid) struct DefinitionHoverPopup {
    /// Overall bounds for painting and clipping editor text behind the panel.
    bounds: Bounds<Pixels>,
    /// Measured Markdown shared with the reply cache.
    layout: Rc<PreviewLayout>,
    /// Window-local body that clips Markdown before it reaches the footer.
    body: Bounds<Pixels>,
    /// Maximum physical offset expressed in editor rows, including a partial last row.
    scroll_limit: f32,
    /// Animated visual offset and logical destination in editor rows.
    position: f32,
    target: usize,
    /// Width of the frame in grid columns, including its side rules.
    columns: usize,
    /// Keyboard hint selected according to whether the content overflows.
    hint: &'static str,
}

impl EditorWindow {
    /// Fit `details` at the current `scroll` position next to the Normal-mode caret in `area`.
    /// Measure using `metrics`, `spacing` and the native text system from `window` and `cx`.
    pub(in crate::grid) fn definition_hover_popup(
        &self,
        area: Rect,
        metrics: Metrics,
        spacing: &TextSpacing,
        details: &DefinitionHoverContent,
        scroll: &GridScroll,
        window: &Window,
        cx: &App,
    ) -> Option<DefinitionHoverPopup> {
        let (x, y, _, _, visible) = self.body_caret(area, spacing);
        if self.vim().mode != Mode::Normal || !visible {
            return None;
        }
        let viewport = Bounds::new(point(px(0.), px(0.)), window.viewport_size());
        let body = self.text_area(area).pixels(metrics).intersect(&viewport);
        let caret = metrics.at(x, y);
        let row_height = px(metrics.lh);
        if !body.contains(&caret) || body.bottom() < caret.y + row_height {
            return None;
        }

        let above = caret.y - body.top();
        let below = body.bottom() - caret.y - row_height;
        let chrome_height = row_height * (1. + EDGE_ROWS * 2.);
        let content_rows = ((f32::from(above.max(below) - chrome_height) / metrics.lh) as usize)
            .min(MAX_CONTENT_ROWS);
        let body_columns = (f32::from(body.size.width) / metrics.cw).floor() as usize;
        let text_limit = body_columns.min(MAX_COLUMNS).saturating_sub(2);
        if content_rows == 0 || text_limit < 4 {
            return None;
        }

        let text_width = details.preferred_columns(
            WidthKey {
                columns: text_limit,
                cell_width: metrics.cw,
                font_size: metrics.font_size,
            },
            spacing,
        );
        let close_hint = i18n::text("editor.hover_close_hint");
        let scroll_hint = i18n::text("editor.hover_scroll_hint");
        let max_height = row_height * content_rows as f32;
        let content_columns = text_width.max(spacing.columns(close_hint)).min(text_limit);
        let body_width = px(content_columns as f32 * metrics.cw);
        let layout = details
            .markdown
            .layout(body_width, metrics.font_size, window, cx);
        if layout.height() <= px(0.) {
            return None;
        }
        let body_height = layout.height().min(max_height);
        let scroll_limit = f32::from(layout.height() - body_height) / metrics.lh;
        let last_first_row = scroll_limit.ceil() as usize;
        let hint = if last_first_row > 0 {
            scroll_hint
        } else {
            close_hint
        };
        let columns = content_columns.max(spacing.columns(hint)).min(text_limit) + 2;
        let width = px(columns as f32 * metrics.cw);
        let bounds = popup_bounds(
            body,
            caret,
            row_height,
            size(width, body_height + chrome_height),
        );
        Some(DefinitionHoverPopup {
            bounds,
            layout,
            body: Bounds::new(
                bounds.origin + point(px(metrics.cw), row_height * EDGE_ROWS),
                size(body_width, body_height),
            ),
            scroll_limit,
            position: scroll.position().min(last_first_row as f32),
            target: scroll.target().min(last_first_row),
            columns,
            hint,
        })
    }
}

/// Fit a popup of `size` inside the editor `body`, below the `caret` and its `row_height` if possible.
fn popup_bounds(
    body: Bounds<Pixels>,
    caret: Point<Pixels>,
    row_height: Pixels,
    size: Size<Pixels>,
) -> Bounds<Pixels> {
    let left = caret.x.min(body.right() - size.width).max(body.left());
    let below = caret.y + row_height;
    let top = if body.bottom() - below >= size.height {
        below
    } else {
        caret.y - size.height
    };
    Bounds::new(point(left, top), size)
}

impl DefinitionHoverPopup {
    /// Move the destination by `rows`, clamped to the first and last available view.
    pub(in crate::grid) fn scrolled_first_row(&self, rows: isize) -> usize {
        self.target
            .saturating_add_signed(rows)
            .min(self.last_first_row())
    }

    /// Return the destination row requested by the page keys.
    #[cfg(all(test, windows))]
    pub(in crate::grid) fn scroll_target(&self) -> usize {
        self.target
    }

    /// Return the animated position in rows for native verification.
    #[cfg(all(test, windows))]
    pub(in crate::grid) fn scroll_position(&self) -> f64 {
        f64::from(self.position)
    }

    /// Return the last offset that still fills the viewport.
    pub(in crate::grid) fn last_first_row(&self) -> usize {
        self.scroll_limit.ceil() as usize
    }

    /// Return the window-local cutout shared by the detail panel and the underlying editor text.
    pub(in crate::grid) fn bounds(&self) -> Bounds<Pixels> {
        self.bounds
    }

    /// Paint the fixed popup material and rounded border using the editor's `metrics` and theme.
    pub(in crate::grid) fn chrome_element(&self, metrics: Metrics) -> AnyElement {
        let panel = self.bounds;
        let inset = point(px(metrics.cw * 0.5), px(metrics.lh * EDGE_ROWS * 0.5));
        canvas(
            |_, _, _| (),
            move |bounds, _, window, cx| {
                let theme = style(cx).for_component(ThemeComponent::Editor);
                if let Some(background) = theme.component_background() {
                    window.paint_quad(fill(bounds, rgba(background)));
                }
                let outline =
                    Bounds::from_corners(bounds.origin + inset, bounds.bottom_right() - inset);
                if let Some(path) =
                    outline_path(outline, OutlineStyle::Solid, metrics.font_size, &[], &[])
                {
                    window.paint_path(path, rgb(theme.foreground(MUTED)));
                }
            },
        )
        .absolute()
        .left(panel.left())
        .top(panel.top())
        .w(panel.size.width)
        .h(panel.size.height)
        .into_any_element()
    }

    /// Return the window-local hint row immediately above the bottom padding, using `row_height`.
    pub(in crate::grid) fn footer_bounds(&self, row_height: f32) -> Bounds<Pixels> {
        Bounds::new(
            point(
                self.bounds.left(),
                self.bounds.bottom() - px(row_height * (1. + EDGE_ROWS)),
            ),
            size(self.bounds.size.width, px(row_height)),
        )
    }

    /// Build the fixed hint row with grid `spacing`; the chrome owns the shared background.
    pub(in crate::grid) fn footer_frame(&self, spacing: &TextSpacing) -> Frame {
        let mut frame = Frame::new(self.columns, 1);
        frame.set_theme_component(ThemeComponent::Editor);
        frame.hide_theme_background();
        let hint = spacing.ellipsize(self.hint, self.columns - 2);
        frame.styled_label(
            0,
            1..self.columns - 1,
            &hint,
            TEXT,
            spacing,
            LabelStyle::Dimmed,
        );
        frame
    }

    /// Return the visible window-local Markdown body, above the footer.
    pub(in crate::grid) fn body_bounds(&self) -> Bounds<Pixels> {
        self.body
    }

    /// Convert the animated row position to pixels using the editor's `row_height`.
    pub(in crate::grid) fn body_offset(&self, row_height: f32) -> Pixels {
        px(rendered_scroll_rows(self.position, self.scroll_limit) * row_height)
    }

    /// Paint the retained Markdown body through its own clip using the editor's `row_height`.
    pub(in crate::grid) fn body_element(&self, row_height: f32) -> AnyElement {
        self.layout
            .snippet_element(self.body, self.body_offset(row_height))
    }

    /// Resolve a visible Markdown link at window-local `position` using the editor's `row_height`.
    pub(in crate::grid) fn link_at(
        &self,
        position: Point<Pixels>,
        row_height: f32,
    ) -> Option<&str> {
        if !self.body.contains(&position) {
            return None;
        }
        self.layout
            .link_at(position - self.body.origin + point(px(0.), self.body_offset(row_height)))
    }
}

/// Preserve whole-row steps and use the last animation step to reach a partial final row smoothly.
fn rendered_scroll_rows(position: f32, limit: f32) -> f32 {
    let row = position.floor().min(limit);
    row + position.fract() * (limit - row).min(1.)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Keep the documentation popup within the editor and free of the caret.
    #[test]
    fn definition_details_flip_and_limit_content_without_covering_the_caret() {
        let body = Bounds::new(point(px(9.), px(20.)), size(px(270.), px(160.)));
        let caret = point(px(204.), px(160.));
        let panel = popup_bounds(body, caret, px(20.), size(px(180.), px(130.)));
        assert_eq!(panel.bottom(), caret.y);
        assert_eq!(panel.right(), body.right());
        assert!(panel.left() >= body.left() && panel.top() >= body.top());

        let caret = point(px(50.), px(25.));
        let panel = popup_bounds(body, caret, px(20.), size(px(120.), px(45.)));
        assert_eq!(panel.top(), px(45.));
        assert_eq!(panel.left(), caret.x);
    }

    /// The last row of scroll animation reaches the content edge without exposing blank space.
    #[test]
    fn definition_details_reach_the_last_partial_row() {
        assert_eq!(rendered_scroll_rows(4., 9.5), 4.);
        assert_eq!(rendered_scroll_rows(8.5, 9.5), 8.5);
        assert_eq!(rendered_scroll_rows(9.5, 9.5), 9.25);
        assert_eq!(rendered_scroll_rows(10., 9.5), 9.5);
    }
}
