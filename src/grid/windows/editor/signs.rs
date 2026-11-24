//! Allocate and paint a sign column beside the existing line numbers and fold affordance.

use crate::{
    grid::render::{CellPaint, Frame},
    ui::theme::MUTED,
};
use nanobug::{
    document::SignPlacement,
    vim::{HighlightStyle, InputMappings},
};
use std::ops::Range;

/// Neovim's default sign column holds one glyph occupying at most two cells.
const SIGN_WIDTH: usize = 2;
/// Reserve one extra cell for diagnostics when the sign column is present.
const DIAGNOSTIC_WIDTH: usize = 1;
/// The fold arrow and following space stay at the right edge of the gutter.
const FOLD_WIDTH: usize = 2;

/// Shared widths for the viewport, line-number painter and fold hit testing.
pub(super) struct EditorGutter {
    /// Total width after clipping to the available body.
    pub(super) width: usize,
    /// Cells for the line number.
    pub(super) number: Range<usize>,
    /// Dedicated diagnostic cell when a sign column is present.
    pub(super) diagnostic: Option<usize>,
}

impl EditorGutter {
    /// Keep at least one text cell when adding the sign and diagnostic columns.
    pub(super) fn new(number_width: usize, available: usize, has_signs: bool) -> Self {
        let compact = number_width + FOLD_WIDTH;
        let expanded = compact + SIGN_WIDTH + DIAGNOSTIC_WIDTH;
        if has_signs && expanded < available {
            Self {
                width: expanded,
                number: SIGN_WIDTH..SIGN_WIDTH + number_width,
                diagnostic: Some(SIGN_WIDTH + number_width),
            }
        } else {
            let width = compact.min(available);
            Self {
                width,
                number: 0..width.saturating_sub(FOLD_WIDTH),
                diagnostic: None,
            }
        }
    }

    /// Paint the glyph and the sign-column background without covering the line number or fold arrow.
    pub(super) fn paint_sign(
        &self,
        frame: &mut Frame,
        row: usize,
        sign: Option<&RowSigns>,
        column_style: Option<HighlightStyle>,
    ) {
        if self.number.start == 0 {
            return;
        }
        if let Some(style) = column_style {
            let paint = CellPaint::new(MUTED, 0, false).with_highlight(style);
            if paint.background != 0 {
                frame.tint(row, 0, SIGN_WIDTH, paint.background);
            }
        }
        if let Some(sign) = sign
            && let Some(text) = &sign.text
        {
            let foreground = column_style
                .and_then(|style| style.foreground)
                .unwrap_or(MUTED);
            frame.highlighted_text(
                0,
                row,
                text,
                foreground,
                SIGN_WIDTH,
                sign.text_style.or(column_style),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        grid::{
            render::{Metrics, Rect},
            windows::{
                WindowFrame,
                editor::{EditorWindow, view::FOLD_ARROW},
            },
        },
        ui::{
            text::TextSpacing,
            theme::{ThemeComponent, WARNING_TEXT},
        },
    };
    use gpui::{FontId, NoopTextSystem, point, px};
    use nanobug::{
        document::Document, plugins::session::DocumentDiagnostics, vim::SignDefinitionUpdate,
    };
    use nanobug_plugin_sdk::Diagnostic;
    use std::sync::Arc;

    /// Render one shared gutter in two widths, preserving glyph priority and the fold click target.
    #[test]
    fn sign_gutter_shares_placements_and_keeps_diagnostics_and_folds() -> Result<(), String> {
        let mappings = InputMappings::default();
        let foreground = |rgb| HighlightStyle {
            foreground: Some(rgb),
            ..Default::default()
        };
        mappings.set_highlight_palette([
            ("Glyph", foreground(0x123456)),
            ("Number", foreground(0x234567)),
            (
                "Line",
                HighlightStyle {
                    background: Some(0x345678),
                    ..Default::default()
                },
            ),
            ("CurrentGlyph", foreground(0x456789)),
        ]);
        mappings.define_sign(
            "Low",
            SignDefinitionUpdate {
                text: Some("Lo".into()),
                numhl: Some("Number".into()),
                linehl: Some("Line".into()),
                ..Default::default()
            },
        )?;
        mappings.define_sign(
            "High",
            SignDefinitionUpdate {
                text: Some("Hi".into()),
                texthl: Some("Glyph".into()),
                culhl: Some("CurrentGlyph".into()),
                ..Default::default()
            },
        )?;
        let mut doc = Document::new("fn main() {\n    work();\n}\ntail");
        doc.path = Some("sample.rs".into());
        doc.collapse_rows(1..3);
        for (id, name, priority) in [(1, "Low", 10), (2, "High", 20)] {
            assert!(doc.place_sign(SignPlacement {
                id,
                name: name.into(),
                row: 0,
                group: String::new(),
                priority
            }));
        }
        let diagnostics = DocumentDiagnostics {
            version: (doc.opening, doc.revision),
            path: doc.path.clone(),
            diagnostics: vec![Diagnostic {
                start: 0,
                end: 2,
                severity: nanobug_plugin_sdk::DiagnosticSeverity::Warning,
                message: "diagnostic".into(),
            }],
        };
        let mut other = EditorWindow::new(doc.shared_view());
        let mut editor = EditorWindow::new(doc);
        editor.set_input_mappings(mappings.clone());
        other.set_input_mappings(mappings.clone());
        // Fold state is a view preference. Give both views a folded header for this comparison.
        other.buffer_mut().vim.doc.collapse_rows(1..3);
        let peer_diagnostics = DocumentDiagnostics {
            version: (other.vim().doc.opening, other.vim().doc.revision),
            ..diagnostics.clone()
        };
        editor
            .buffer_mut()
            .set_diagnostics(vec![Arc::new(diagnostics)]);
        other
            .buffer_mut()
            .set_diagnostics(vec![Arc::new(peer_diagnostics)]);

        let spacing = TextSpacing::new(Arc::new(NoopTextSystem::new()), FontId(0), 16., 9.);
        let area = Rect {
            w: 28,
            h: 6,
            ..Default::default()
        };
        let context = WindowFrame {
            area,
            selected: false,
            modal_open: false,
            controls_hovered: false,
            spacing: &spacing,
        };
        let mut expected = Frame::viewport(7, 5, 0);
        expected.set_theme_component(ThemeComponent::Gutter);
        expected.text(0, 0, "Hi", 0x123456, 2);
        expected.text(2, 0, " 1", 0x234567, 2);
        expected.text(4, 0, "!", WARNING_TEXT, 1);
        expected.right_aligned_icon(5, 0, FOLD_ARROW, 56);
        expected.text(2, 1, " 1", MUTED, 2);
        for row in 2..5 {
            expected.text(3, row, "~", MUTED, 1);
        }

        let (gutter, text) = editor.content_frames(context, |_| None);
        assert!(gutter == expected);
        assert_eq!(text.cell_paint_at(0, 0).background, 0x345678ff);
        assert_eq!(text.cell_paint_at(0, 16).background, 0x345678ff);
        let narrower = WindowFrame {
            area: Rect { w: 14, ..area },
            ..context
        };
        assert!(other.content_frames(narrower, |_| None).0 == expected);
        let active = WindowFrame {
            selected: true,
            ..context
        };
        assert_eq!(
            editor
                .content_frames(active, |_| None)
                .0
                .cell_paint_at(0, 0)
                .foreground,
            0x456789
        );

        let metrics = Metrics {
            cw: 9.,
            lh: 21.,
            font_size: 16.,
            origin: point(px(3.), px(7.)),
        };
        let text = editor.text_area(area);
        let y = text.y as f32 + 0.5;
        assert!(!editor.open_fold_at_position(
            metrics.at(text.x as f32 - 2.5, y),
            area,
            metrics,
            &spacing
        ));
        assert!(editor.open_fold_at_position(
            metrics.at(text.x as f32 - 1.5, y),
            area,
            metrics,
            &spacing
        ));

        assert!(editor.buffer_mut().vim.doc.place_sign(SignPlacement {
            id: 2,
            name: "High".into(),
            row: 3,
            group: String::new(),
            priority: 20
        }));
        assert_eq!(
            other
                .content_frames(narrower, |_| None)
                .0
                .cell_paint_at(1, 0)
                .foreground,
            0x123456
        );
        Ok(())
    }
}

/// Independently selected display attributes from the signs on one logical row.
#[derive(Default)]
pub(super) struct RowSigns {
    /// First text-bearing sign by priority, ignoring stored GUI icon paths.
    text: Option<String>,
    /// Style selected from the glyph's `culhl` or `texthl` group.
    text_style: Option<HighlightStyle>,
    /// Highest priority `numhl` style for this line.
    pub(super) number: Option<HighlightStyle>,
    /// Highest priority `linehl` style for this line.
    pub(super) line: Option<HighlightStyle>,
}

impl RowSigns {
    /// Select the glyph and each independent highlight from priority-ordered buffer `signs`.
    pub(super) fn new(
        signs: &[SignPlacement],
        row: usize,
        current: bool,
        mappings: &InputMappings,
    ) -> Self {
        let mut result = Self::default();
        let first = signs.partition_point(|sign| sign.row < row);
        for placement in signs[first..].iter().take_while(|sign| sign.row == row) {
            let Some(definition) = mappings.sign_render_definition(&placement.name) else {
                continue;
            };
            if result.text.is_none()
                && definition
                    .text
                    .as_ref()
                    .is_some_and(|text| !text.is_empty())
            {
                result.text = definition.text;
                result.text_style = current
                    .then(|| definition.culhl.as_deref())
                    .flatten()
                    .or(definition.texthl.as_deref())
                    .and_then(|group| mappings.highlight_style(group));
            }
            if result.number.is_none() {
                result.number = definition
                    .numhl
                    .as_deref()
                    .and_then(|group| mappings.highlight_style(group));
            }
            if result.line.is_none() {
                result.line = definition
                    .linehl
                    .as_deref()
                    .and_then(|group| mappings.highlight_style(group));
            }
            if result.text.is_some() && result.number.is_some() && result.line.is_some() {
                break;
            }
        }
        result
    }
}
