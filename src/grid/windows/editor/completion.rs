//! Lay out and draw the completion menu alongside a visible editor caret.

use super::EditorWindow;
use crate::{
    grid::render::{Metrics, Rect},
    ui::{
        text::TextSpacing,
        theme::{ACCENT, GOLD, MUTED, SELECTION, TEXT, ThemeComponent, style},
    },
};
use gpui::{
    AnyElement, BorderStyle, Bounds, ContentMask, IntoElement, Pixels, Point, Styled, TextAlign,
    TextRun, canvas, fill, point, px, quad, rgb, rgba, size,
};
use std::{borrow::Cow, ops::Range};

/// Limit the overlay to a short list so the file stays visible while typing.
const MAX_ROWS: usize = 10;
/// Keep long signatures from spanning the whole editor.
const MAX_COLUMNS: usize = 64;

/// The popup's visible slice and window-local bounds, shared by painting and pointer input.
pub(in crate::grid) struct CompletionMenu {
    /// Full bounds of the painted rows, including their horizontal padding.
    bounds: Bounds<Pixels>,
    /// Candidate indices represented by the visible rows.
    rows: Range<usize>,
    /// Number of text columns between the padding cells.
    columns: usize,
}

impl EditorWindow {
    /// Anchor completions for `labels` and `selected` near the caret in `area`.
    /// Restrict the menu to the visible editor text area inside the native `viewport`.
    pub(in crate::grid) fn completion_menu(
        &self,
        area: Rect,
        metrics: Metrics,
        spacing: &TextSpacing,
        viewport: Bounds<Pixels>,
        labels: &[&str],
        selected: usize,
    ) -> Option<CompletionMenu> {
        let (x, y, _, insert, visible) = self.body_caret(area, spacing);
        if !insert || !visible || labels.is_empty() {
            return None;
        }
        let body = self.text_area(area).pixels(metrics).intersect(&viewport);
        let caret = metrics.at(x, y);
        let row_height = px(metrics.lh);
        if !body.contains(&caret) || body.bottom() < caret.y + row_height {
            return None;
        }

        let max_rows = labels.len().min(MAX_ROWS);
        let above = (f32::from(caret.y - body.top()) / metrics.lh).floor() as usize;
        let below = (f32::from(body.bottom() - caret.y - row_height) / metrics.lh).floor() as usize;
        let opens_below = below >= max_rows || below >= above;
        let row_count = max_rows.min(if opens_below { below } else { above });
        let max_columns = (f32::from(body.size.width) / metrics.cw).floor() as usize;
        let text_limit = max_columns.min(MAX_COLUMNS).saturating_sub(2);
        if row_count == 0 || text_limit == 0 {
            return None;
        }

        let first = selected
            .saturating_sub(row_count - 1)
            .min(labels.len() - row_count);
        let rows = first..first + row_count;
        // Keep the same width while arrow keys scroll the visible rows.
        let mut columns = 1;
        for label in labels {
            let label = visible_label(label);
            let width = spacing.columns(&spacing.ellipsize(&label, text_limit));
            columns = columns.max(width.min(text_limit));
            if columns == text_limit {
                break;
            }
        }
        let width = px((columns + 2) as f32 * metrics.cw);
        let height = row_height * row_count as f32;
        let left = caret.x.min(body.right() - width).max(body.left());
        let top = if opens_below {
            caret.y + row_height
        } else {
            caret.y - height
        };
        Some(CompletionMenu {
            bounds: Bounds::new(point(left, top), size(width, height)),
            rows,
            columns,
        })
    }
}

impl CompletionMenu {
    /// Return the window-local cutout used by the editor text layer and pointer hit testing.
    pub(in crate::grid) fn bounds(&self) -> Bounds<Pixels> {
        self.bounds
    }

    /// Return the absolute candidate index under `position` using the row height in `metrics`.
    pub(in crate::grid) fn hit(&self, position: Point<Pixels>, metrics: Metrics) -> Option<usize> {
        if !self.bounds.contains(&position) {
            return None;
        }
        let row = (f32::from(position.y - self.bounds.top()) / metrics.lh).floor() as usize;
        (row < self.rows.len()).then_some(self.rows.start + row)
    }

    /// Draw the visible `labels` and `selected` index using the editor's font and color roles.
    pub(in crate::grid) fn element(
        self,
        labels: &[&str],
        query: &str,
        selected: usize,
        metrics: Metrics,
        spacing: &TextSpacing,
    ) -> AnyElement {
        let visible: Vec<_> = labels[self.rows.clone()]
            .iter()
            .map(|label| {
                let source = visible_label(label);
                let label = spacing.ellipsize(&source, self.columns);
                let visible_bytes = if label == source {
                    label.len()
                } else {
                    label.len() - '…'.len_utf8()
                };
                let matches = nanobug::plugins::completion::matched_chars(query, &source)
                    .unwrap_or_default()
                    .into_iter()
                    .take_while(|range| range.end <= visible_bytes)
                    .collect::<Vec<_>>();
                (gpui::SharedString::from(label.into_owned()), matches)
            })
            .collect();
        let selected = selected.saturating_sub(self.rows.start);
        let area = self.bounds;
        canvas(
            |_, _, _| (),
            move |bounds, _, window, cx| {
                let appearance = style(cx).for_component(ThemeComponent::Editor);
                let font = appearance.font.descriptor();
                let height = px(metrics.lh);
                let pad = px(metrics.cw);
                window.with_content_mask(Some(ContentMask { bounds }), |window| {
                    for (index, (label, matches)) in visible.into_iter().enumerate() {
                        let active = index == selected;
                        let origin = bounds.origin + point(px(0.), height * index as f32);
                        if active {
                            let row = Bounds::new(origin, size(bounds.size.width, height));
                            window.paint_quad(fill(row, rgba(appearance.tint(SELECTION))));
                        }
                        let color = rgb(appearance.foreground(if active { ACCENT } else { TEXT }));
                        let matched = rgb(appearance.foreground(GOLD));
                        let runs = label_runs(label.len(), &matches, |len, is_match| TextRun {
                            len,
                            font: font.clone(),
                            color: if is_match { matched } else { color }.into(),
                            ..Default::default()
                        });
                        let line = window.text_system().shape_line(
                            label.clone(),
                            px(metrics.font_size),
                            &runs,
                            None,
                        );
                        let _ = line.paint(
                            origin + point(pad, px(0.)),
                            height,
                            TextAlign::Left,
                            None,
                            window,
                            cx,
                        );
                    }
                    window.paint_quad(quad(
                        bounds,
                        px(0.),
                        rgba(0),
                        px(1.),
                        rgba((appearance.foreground(MUTED) << 8) | 0x66),
                        BorderStyle::Solid,
                    ));
                });
            },
        )
        .absolute()
        .left(area.left())
        .top(area.top())
        .w(area.size.width)
        .h(area.size.height)
        .into_any_element()
    }
}

/// Cover a label with runs for the matched byte ranges and the intervening normal text.
fn label_runs<T>(
    len: usize,
    matches: &[Range<usize>],
    mut run: impl FnMut(usize, bool) -> T,
) -> Vec<T> {
    let mut runs = Vec::new();
    let mut offset = 0;
    for matched in matches {
        if offset < matched.start {
            runs.push(run(matched.start - offset, false));
        }
        runs.push(run(matched.len(), true));
        offset = matched.end;
    }
    if offset < len {
        runs.push(run(len - offset, false));
    }
    runs
}

/// Use the first nonempty line in `label`, normalizing control characters for native text drawing.
fn visible_label(label: &str) -> Cow<'_, str> {
    let line = label
        .split(['\n', '\r', '\u{2028}', '\u{2029}'])
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    if line.chars().any(char::is_control) {
        Cow::Owned(
            line.chars()
                .map(|ch| if ch.is_control() { ' ' } else { ch })
                .collect(),
        )
    } else {
        Cow::Borrowed(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{FontId, NoopTextSystem};
    use nanobug::document::Document;
    use std::sync::Arc;

    #[test]
    fn candidate_runs_highlight_only_matched_characters() {
        let matched = nanobug::plugins::completion::matched_chars("pbf", "PathBuf").unwrap();
        assert_eq!(
            label_runs("PathBuf".len(), &matched, |len, matched| (len, matched)),
            [(1, true), (3, false), (1, true), (1, false), (1, true)]
        );
    }

    /// A long list near the lower-right edge flips up, fits, and keeps the chosen row clickable.
    #[test]
    fn completion_flips_above_caret_and_keeps_the_selected_candidate_visible() {
        let mut doc = Document::new("a\nb\nc\nd\n0123456789");
        doc.cursor = doc.starts[4];
        let mut editor = EditorWindow::new(doc);
        editor.edit(|vim, register| vim.key("A", register));
        let metrics = Metrics {
            cw: 9.,
            lh: 20.,
            font_size: 16.,
            origin: point(px(0.), px(0.)),
        };
        let spacing = TextSpacing::new(Arc::new(NoopTextSystem::new()), FontId(0), 16., 9.);
        let area = Rect {
            w: 20,
            h: 7,
            ..Rect::default()
        };
        let mut labels = ["candidate"; 12];
        labels[11] = "candidate_with_a_long_signature()";
        let first = editor
            .completion_menu(area, metrics, &spacing, area.pixels(metrics), &labels, 0)
            .unwrap();
        let menu = editor
            .completion_menu(area, metrics, &spacing, area.pixels(metrics), &labels, 11)
            .unwrap();
        let body = editor.text_area(area).pixels(metrics);
        let bounds = menu.bounds();
        let (x, y, ..) = editor.body_caret(area, &spacing);
        assert_eq!(bounds.bottom(), metrics.at(x, y).y);
        assert_eq!(bounds.left(), body.left());
        assert_eq!(bounds.right(), body.right());
        assert_eq!(bounds, first.bounds());
        assert_eq!(menu.rows, 8..12);
        assert_eq!(
            menu.hit(point(bounds.center().x, bounds.bottom() - px(10.)), metrics),
            Some(11),
        );
        assert_eq!(visible_label("\n\tfirst\tline\r\nsecond"), "first line");
    }
}
