//! Editable command-line history in a temporary grid window.

use super::{Buffer, EditorWindow};
use crate::{
    grid::{
        render::{Frame, GridLayer, LabelStyle, Metrics, Panel, Rect},
        windows::WindowFrame,
    },
    ui::{
        text::{TextSpacing, UNITS_PER_COLUMN},
        theme::{MUTED, TEXT, ThemeComponent},
    },
};
use gpui::{App, AppContext, Entity};
use nanobug::{
    document::Document,
    i18n,
    vim::{Mode, Vim},
};

/// Scratch buffer for one prompt's history and draft.
pub(in crate::grid) struct CommandWindow {
    /// Which prompt accepts the selected line.
    pub(in crate::grid) prompt: char,
    /// In-memory editor, including independent input, undo and selection.
    pub(in crate::grid) editor: EditorWindow,
    /// Border, line numbers and document retained separately for viewport clipping.
    panels: [Entity<Panel>; 3],
    /// Fractional vertical wheel steps between native pointer events.
    pub(in crate::grid) wheel: f32,
    /// Whether a scratch window command follows Ctrl+w.
    pub(in crate::grid) window_prefix: bool,
}

impl CommandWindow {
    /// Copy `prompt` history and draft from `source`, using `metrics` and `cx` for rendering.
    pub(in crate::grid) fn new(
        prompt: char,
        source: &EditorWindow,
        metrics: Metrics,
        cx: &mut App,
    ) -> Self {
        let (text, offset) = source.vim().command_window_text(prompt, source.register());
        let mut doc = Document::new(&text);
        doc.cursor = offset;
        Self {
            prompt,
            editor: EditorWindow::with_buffer(Buffer::with_vim(Vim::scratch(doc))),
            panels: std::array::from_fn(|_| cx.new(|_| Panel::new(metrics))),
            wheel: 0.,
            window_prefix: false,
        }
    }

    /// Return the edited line under the caret and its byte column in that line.
    pub(in crate::grid) fn selected(&self) -> (String, usize) {
        let doc = &self.editor.vim().doc;
        let line = self.editor.buffer().cursor_row();
        (doc.line(line).into(), doc.cursor - doc.starts[line])
    }

    /// Return the full window's content layers for `area`, including their independent clips.
    pub(in crate::grid) fn layers(&self, area: Rect, metrics: Metrics) -> [GridLayer; 3] {
        let [chrome, numbers, text] = &self.panels;
        let [numbers, text] = self.editor.content_layers(area, metrics, numbers, text);
        [
            GridLayer {
                panel: chrome.clone(),
                bounds: area.pixels(metrics),
                clip: None,
            },
            numbers,
            text,
        ]
    }

    /// Retained text panel for caret glyphs and effects.
    pub(in crate::grid) fn text_panel(&self) -> &Entity<Panel> {
        &self.panels[2]
    }

    /// Apply grid font or DPI `metrics` to every retained panel through `cx`.
    pub(in crate::grid) fn set_appearance(&self, metrics: Metrics, cx: &mut App) {
        for panel in &self.panels {
            panel.update(cx, |panel, cx| panel.set_appearance(metrics, cx));
        }
    }

    /// Draw the editable history and its chrome inside `area`, measuring through `spacing`.
    pub(in crate::grid) fn refresh(&self, area: Rect, spacing: &TextSpacing, cx: &mut App) {
        let (numbers, text) = self.editor.content_frames(
            WindowFrame {
                area,
                selected: true,
                modal_open: false,
                controls_hovered: false,
                spacing,
            },
            |_| None,
        );
        let mut chrome = Frame::new(area.w, area.h);
        chrome.set_theme_component(ThemeComponent::Editor);
        let title = if self.prompt == ':' {
            "vim_hint.command_window"
        } else {
            "vim_hint.search_window"
        };
        let title = format!("{} {}", self.prompt, i18n::text(title));
        chrome.border(&title, true, false, spacing);
        if area.h > 2 && area.w > 5 {
            let slot = 2..area.w - 2;
            let text = self.footer();
            let (color, style) = if matches!(self.editor.vim().mode, Mode::Command | Mode::Search) {
                (TEXT, LabelStyle::Text)
            } else {
                (MUTED, LabelStyle::Dimmed)
            };
            chrome.styled_label(area.h - 1, slot, &text, color, spacing, style);
        }
        for (panel, frame) in self.panels.iter().zip([chrome, numbers, text]) {
            panel.update(cx, |panel, cx| panel.set(frame, cx));
        }
    }

    /// Return the footer text for local prompt input or history-window controls.
    fn footer(&self) -> String {
        let vim = self.editor.vim();
        if matches!(vim.mode, Mode::Command | Mode::Search) {
            self.editor.command_text()
        } else if !vim.message.is_empty() {
            vim.message.clone()
        } else {
            i18n::text("command_window.hint").into()
        }
    }

    /// Resolve the scratch caret or its own footer prompt within `area` using `spacing`.
    pub(in crate::grid) fn caret(
        &self,
        area: Rect,
        spacing: &TextSpacing,
    ) -> (f32, f32, f32, bool, bool) {
        if !matches!(self.editor.vim().mode, Mode::Command | Mode::Search) {
            return self.editor.body_caret(area, spacing);
        }
        let width = area.w.saturating_sub(4);
        let text = self.editor.command_caret_text();
        let units = spacing
            .fitting_cells(&text, width * UNITS_PER_COLUMN)
            .last()
            .map_or(0, |cell| cell.end);
        (
            (area.x + 2) as f32 + units as f32 / UNITS_PER_COLUMN as f32,
            (area.y + area.h.saturating_sub(1)) as f32,
            1.,
            true,
            width > 0,
        )
    }
}
