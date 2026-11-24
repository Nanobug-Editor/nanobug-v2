//! Resolve the visible plugin diagnostic ranges to underline colors in document bytes.

use crate::ui::theme::{ERROR_UNDERLINE, WARNING_UNDERLINE};
use nanobug_plugin_sdk::{Diagnostic, DiagnosticSeverity};
use std::{
    collections::BTreeMap,
    ops::{Range, RangeInclusive},
};

/// Disjoint ranges per severity allow a binary lookup for each visible grapheme.
#[derive(Default)]
pub(super) struct DiagnosticUnderlines {
    /// Document bytes covered by errors, including a single byte for point diagnostics.
    errors: Vec<Range<usize>>,
    /// Document bytes covered by warnings.
    warnings: Vec<Range<usize>>,
    /// Insertion positions can also mark a blank cell at the end of their logical line.
    points: BTreeMap<usize, u32>,
}

impl DiagnosticUnderlines {
    /// Index current `diagnostics` touching the `visible` source bytes or their end boundary.
    pub(super) fn new<'a>(
        diagnostics: impl Iterator<Item = &'a Diagnostic>,
        visible: Range<usize>,
    ) -> Self {
        let mut marks = Self::default();
        for item in diagnostics {
            if item.end < visible.start || item.start > visible.end {
                continue;
            }
            let (ranges, color) = match item.severity {
                DiagnosticSeverity::Error => (&mut marks.errors, ERROR_UNDERLINE),
                DiagnosticSeverity::Warning => (&mut marks.warnings, WARNING_UNDERLINE),
                _ => continue,
            };
            if item.start == item.end {
                let ink = marks.points.entry(item.start).or_insert(color);
                if color == ERROR_UNDERLINE {
                    *ink = color;
                }
            }
            ranges.push(item.start..item.end.max(item.start.saturating_add(1)));
        }
        for ranges in [&mut marks.errors, &mut marks.warnings] {
            ranges.sort_unstable_by_key(|range| range.start);
            ranges.dedup_by(|next, previous| {
                if next.start <= previous.end {
                    previous.end = previous.end.max(next.end);
                    true
                } else {
                    false
                }
            });
        }
        marks
    }

    /// Return the strongest underline intersecting any byte of a displayed grapheme.
    pub(super) fn color(&self, bytes: Range<usize>) -> Option<u32> {
        [
            (&self.errors, ERROR_UNDERLINE),
            (&self.warnings, WARNING_UNDERLINE),
        ]
        .into_iter()
        .find_map(|(ranges, color)| {
            let index = ranges.partition_point(|range| range.end <= bytes.start);
            ranges
                .get(index)
                .filter(|range| range.start < bytes.end)
                .map(|_| color)
        })
    }

    /// Return the strongest point at the displayed line ending, including a hidden final LF.
    pub(super) fn at_line_end(&self, bytes: RangeInclusive<usize>) -> Option<u32> {
        self.points
            .range(bytes)
            .map(|(_, &ink)| ink)
            .max_by_key(|&ink| ink == ERROR_UNDERLINE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        grid::{
            render::Rect,
            windows::{WindowFrame, editor::EditorWindow},
        },
        ui::text::TextSpacing,
    };
    use gpui::{FontId, NoopTextSystem};
    use nanobug::{document::Document, plugins::session::DocumentDiagnostics};
    use std::sync::Arc;

    /// Preserve syntax colors and measured tab and grapheme widths across diagnostic boundaries.
    #[test]
    fn diagnostic_waves_cover_graphemes_across_lines_and_mark_empty_eof() {
        let source = "a\te\u{301}中\n\n好Z\n";
        let mut doc = Document::new(source);
        doc.path = Some("waves.rs".into());
        let version = (doc.opening, doc.revision);
        let path = doc.path.clone();
        let mut editor = EditorWindow::new(doc);
        let spacing = TextSpacing::new(Arc::new(NoopTextSystem::new()), FontId(0), 16., 9.);
        let context = WindowFrame {
            area: Rect {
                w: 32,
                h: 6,
                ..Default::default()
            },
            selected: true,
            modal_open: false,
            controls_hovered: false,
            spacing: &spacing,
        };
        let paint = |editor: &EditorWindow| editor.content_frames(context, |_| Some(0x336699)).1;
        let base = paint(&editor);
        let diagnostic = |start, end, severity| Diagnostic {
            start,
            end,
            severity,
            message: "test diagnostic".into(),
        };
        let accent = source.find('\u{301}').unwrap();
        let empty_line = source.find("\n\n").unwrap() + 1;
        let diagnostics = vec![
            diagnostic(
                accent,
                accent + '\u{301}'.len_utf8(),
                DiagnosticSeverity::Error,
            ),
            diagnostic(1, source.find('Z').unwrap(), DiagnosticSeverity::Warning),
            diagnostic(0, 1, DiagnosticSeverity::Information),
            diagnostic(empty_line, empty_line, DiagnosticSeverity::Warning),
            diagnostic(source.len(), source.len(), DiagnosticSeverity::Error),
        ];
        editor
            .buffer_mut()
            .set_diagnostics(vec![Arc::new(DocumentDiagnostics {
                path,
                version,
                diagnostics,
            })]);
        let marked = paint(&editor);
        let expected = [
            &[
                None,
                Some(WARNING_UNDERLINE),
                Some(WARNING_UNDERLINE),
                Some(WARNING_UNDERLINE),
                Some(ERROR_UNDERLINE),
                Some(WARNING_UNDERLINE),
                Some(WARNING_UNDERLINE),
                None,
            ][..],
            &[Some(WARNING_UNDERLINE), None][..],
            &[
                Some(WARNING_UNDERLINE),
                Some(WARNING_UNDERLINE),
                None,
                Some(ERROR_UNDERLINE),
                None,
            ][..],
        ];
        for (row, colors) in expected.iter().enumerate() {
            for (column, color) in colors.iter().enumerate() {
                let original = base.cell_paint_at(row, column);
                assert_eq!(
                    marked.cell_paint_at(row, column),
                    color.map_or(original, |color| original.with_undercurl(color)),
                    "row {row}, column {column}",
                );
            }
        }
    }
}
