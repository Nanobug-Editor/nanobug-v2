//! Derive the editor border, line numbers and text from this window's active buffer.

use super::{EditorWindow, TEXT_OVERSCAN};
use crate::{
    grid::{
        render::{Frame, Rect},
        windows::WindowFrame,
    },
    ui::theme::{CURRENT, GOLD, MUTED, TEXT},
};
use nanobug::i18n;
use std::path::Path;

/// Retained frames used by the editor's fixed chrome and independently clipped text layers.
pub(in crate::grid) struct EditorFrames {
    /// Border and file title.
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

    /// Return the text viewport inside `area`, excluding the fixed horizontal line-number gutter.
    pub(in crate::grid) fn text_area(&self, area: Rect) -> Rect {
        let body = Self::body(area);
        let gutter = (self.vim().doc.starts.len().to_string().len().max(4) + 2).min(body.w);
        Rect {
            x: body.x + gutter,
            w: body.w - gutter,
            ..body
        }
    }

    /// Format the active filename relative to `project`, including the modified marker.
    pub(in crate::grid) fn title(&self, project: &Path) -> String {
        let doc = &self.vim().doc;
        let filename = doc
            .path
            .as_deref()
            .map(|path| {
                path.strip_prefix(project)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .into_owned()
            })
            .unwrap_or_else(|| i18n::text("editor.untitled").into());
        let modified = if doc.modified() { " [+]" } else { "" };
        format!("{filename}{modified}")
    }

    /// Build display frames for `context`, naming the active file relative to `project`.
    pub(in crate::grid) fn frames(&self, context: WindowFrame<'_>, project: &Path) -> EditorFrames {
        let buffer = &self.buffers[self.active];
        let vim = &buffer.vim;
        let row = vim.doc.location(vim.doc.cursor).0;
        let active = context.selected;
        let selection_tint = context.selection_tint();
        let mut edit = Frame::new(context.area.w, context.area.h);
        context.border(&mut edit, &self.title(project));
        let body = self.text_area(context.area);
        let gutter = body.x - Self::body(context.area).x;
        let first = buffer.scroll.first_cell();
        let mut numbers = Frame::viewport(gutter, body.h + 1, first);
        let left = buffer.columns.first_cell() - buffer.left_padding();
        let width = body.w + buffer.left_padding() + TEXT_OVERSCAN;
        let right = left + width;
        let mut text = Frame::viewport(width, body.h + 1, first);
        let selection = vim.selection();
        let composition = if active {
            self.composition.text.as_str()
        } else {
            ""
        };
        for visible in 0..=body.h {
            let line = first + visible;
            if line >= vim.doc.starts.len() {
                numbers.text(2, visible, "~", MUTED, 1);
                continue;
            }
            let current = row == line && active;
            if current {
                numbers.tint(visible, 0, gutter, CURRENT);
            }
            numbers.text(
                0,
                visible,
                &format!("{:>width$} │", line + 1, width = gutter.saturating_sub(2)),
                if current { GOLD } else { MUTED },
                gutter,
            );
            let (displayed, preedit) = buffer.displayed_line(line, composition);
            let tint = if current { CURRENT } else { 0 };
            text.viewport_line(
                visible,
                context.spacing.cells(&displayed),
                left..right,
                tint,
                |byte| {
                    if preedit.as_ref().is_some_and(|range| range.contains(&byte)) {
                        (GOLD, 0xc9a3f251)
                    } else if selection
                        .as_ref()
                        .is_some_and(|range| range.contains(&(vim.doc.starts[line] + byte)))
                    {
                        (TEXT, selection_tint)
                    } else {
                        (TEXT, tint)
                    }
                },
            );
        }

        EditorFrames {
            chrome: edit,
            numbers,
            text,
        }
    }
}
