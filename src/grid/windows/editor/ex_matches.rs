//! Retain the three window-local Ex match slots and project them onto visible source bytes.

use nanobug::vim::{ExMatch, HighlightStyle, InputMappings};
use std::ops::Range;

/// Matched byte ranges and current group style for one slot of the visible editor text.
pub(super) struct VisibleMatch {
    hits: Vec<Range<usize>>,
    style: HighlightStyle,
}

impl VisibleMatch {
    /// Return the group's style if a rendered character starts inside this match.
    pub(super) fn at(&self, offset: usize) -> Option<HighlightStyle> {
        self.hits
            .get(self.hits.partition_point(|hit| hit.end <= offset))
            .is_some_and(|hit| hit.contains(&offset))
            .then_some(self.style)
    }
}

/// Resolve current colors and match only the source slice needed by this frame.
pub(super) fn visible_matches(
    slots: &[Option<ExMatch>; 3],
    mappings: &InputMappings,
    text: &str,
    visible: Range<usize>,
) -> [Option<VisibleMatch>; 3] {
    let source = &text[visible.clone()];
    slots.each_ref().map(|slot| {
        let rule = slot.as_ref()?;
        let style = mappings.highlight_style(&rule.group)?;
        let hits = rule
            .regex
            .find_iter(source)
            .map(|hit| {
                let width = if hit.is_empty() {
                    source[hit.end()..].chars().next().map_or(0, char::len_utf8)
                } else {
                    0
                };
                visible.start + hit.start()..visible.start + hit.end() + width
            })
            .collect();
        Some(VisibleMatch { hits, style })
    })
}

#[cfg(test)]
mod tests {
    use super::super::EditorWindow;
    use crate::{
        grid::{render::Rect, windows::WindowFrame},
        ui::{text::TextSpacing, theme::TEXT},
    };
    use gpui::{FontId, NoopTextSystem};
    use nanobug::{document::Document, vim::Action};
    use std::sync::Arc;

    /// Apply Ex through the same typed model result consumed by the native workspace.
    fn ex(editor: &mut EditorWindow, command: &str) -> Result<(), String> {
        let action = editor
            .edit(|vim, register| vim.execute_ex_line(command, register))
            .action;
        assert_eq!(action, Action::None);
        if editor.vim().ex_failed() {
            Err(editor.vim().message.clone())
        } else {
            Ok(())
        }
    }

    /// Numbered slots remain local to a pane, resolve styles live and follow buffer changes.
    #[test]
    fn match_highlights_keep_window_scope_and_slot_priority() -> Result<(), String> {
        let mut editor = EditorWindow::new(Document::new("TODO todo\n"));
        let document = editor.vim().doc.shared_view();
        let mut other = EditorWindow::new(document);
        other.set_input_mappings(editor.register.input_mappings().clone());
        let spacing = TextSpacing::new(Arc::new(NoopTextSystem::new()), FontId(0), 16., 9.);
        let context = WindowFrame {
            area: Rect {
                w: 24,
                h: 6,
                ..Rect::default()
            },
            selected: false,
            modal_open: false,
            controls_hovered: false,
            spacing: &spacing,
        };
        let paint = |pane: &EditorWindow, column| {
            pane.content_frames(context, |_| None)
                .1
                .cell_paint_at(0, column)
        };
        ex(
            &mut editor,
            "highlight First guifg=#ff0011 gui=bold,underline",
        )?;
        ex(&mut editor, "highlight Second guifg=#2288ff guibg=#334455")?;
        ex(&mut editor, "2match Second /TODO/")?;
        ex(&mut editor, "match First /TODO/")?;
        assert_eq!(paint(&editor, 0).foreground, 0xff0011);
        assert_eq!(paint(&editor, 5).foreground, TEXT);
        assert_eq!(paint(&other, 0).foreground, TEXT);

        ex(&mut editor, "highlight First guifg=#101112")?;
        assert_eq!(paint(&editor, 0).foreground, 0x101112);
        assert!(ex(&mut editor, "match MissingGroup /TODO/").is_err());
        assert_eq!(paint(&editor, 0).foreground, 0x2288ff);
        assert_eq!(paint(&editor, 0).background, 0x334455ff);

        let new_buffer = super::super::Buffer::new(Document::new("a TODO"));
        let target = new_buffer.id;
        editor.push_buffer(new_buffer);
        assert!(editor.select_buffer(target, 0.));
        assert_eq!(paint(&editor, 0).foreground, TEXT);
        assert_eq!(paint(&editor, 2).foreground, 0x2288ff);
        ex(&mut editor, "2match none")?;
        assert_eq!(paint(&editor, 2).foreground, TEXT);
        Ok(())
    }
}
