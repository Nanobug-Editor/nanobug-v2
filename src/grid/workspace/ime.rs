//! Native input for the document and popup filter, with preedit local to the owning surface.
use super::{Workspace, layout::Focus};
use crate::ui::ime::Composition;
use gpui::{
    Bounds, ClipboardItem, Context, EntityInputHandler, Pixels, Point, UTF16Selection, Window, px,
    size,
};
use nanobug::{animation::scroll::ScrollInput, vim::Mode};
use std::ops::Range;

impl Workspace {
    /// Read preedit from the surface that currently owns native input.
    fn composition(&self) -> &Composition {
        if self.help.is_open() {
            &self.help.composition
        } else {
            &self.projects.active.editor.composition
        }
    }

    /// Update preedit on the surface that currently owns native input.
    fn composition_mut(&mut self) -> &mut Composition {
        if self.help.is_open() {
            &mut self.help.composition
        } else {
            &mut self.projects.active.editor.composition
        }
    }

    /// Whether the active surface accepts native text input at this moment.
    pub(super) fn accepts_native_text(&self) -> bool {
        if self.small_modal_open() {
            false
        } else if self.help.is_open() {
            self.help.is_filtering()
        } else {
            self.projects.active.focus == Focus::Editor
                && matches!(
                    self.projects.active.editor.vim().mode,
                    Mode::Insert | Mode::Command | Mode::Search
                )
        }
    }

    /// Refresh the preedit and candidate anchor in `window` through `cx`.
    fn repaint_preedit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.help.is_open() {
            self.repaint_help(ScrollInput::Advance, window, cx);
        } else {
            self.sync(window, cx);
        }
    }
}

impl EntityInputHandler for Workspace {
    /// Read UTF-16 `range` from the transient composition and report the adjusted returned range.
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        self.composition().text_for_range(range, adjusted)
    }

    /// Expose the native composition selection, or an empty local text-input interval.
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.composition().selection.clone(),
            reversed: false,
        })
    }

    /// Report the current native preedit interval in UTF-16 units.
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        let composition = self.composition();
        (!composition.is_empty()).then(|| 0..composition.text.encode_utf16().count())
    }

    /// Finalize any outstanding native composition when the OS unmarks it.
    fn unmark_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.composition_mut().take();
        if !text.is_empty() {
            self.commit_text(&text, window, cx);
        } else {
            self.repaint_preedit(window, cx);
        }
    }

    /// Commit native `text` once; preedit is discarded before handing text to the modal workspace.
    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.composition_mut().clear();
        self.commit_text(text, window, cx);
    }

    /// Update transient `text` and its UTF-16 `selection`; no document/undo mutation occurs yet.
    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selection: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.accepts_native_text() {
            return;
        }
        self.composition_mut().mark(range, text, selection);
        self.repaint_preedit(window, cx);
    }

    /// Anchor OS candidate UI to the logical input point, independently of visual interpolation.
    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let (x, y, width, _, _) = self.caret();
        Some(Bounds::new(
            self.metrics.at(x, y),
            size(px(self.metrics.cw * width), px(self.metrics.lh)),
        ))
    }

    /// The local composition surface exposes only its current insertion endpoint for native hit tests.
    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.composition().selection.end)
    }

    /// Size of the local native composition surface, measured in UTF-16 units.
    fn text_length_utf16(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        Some(self.composition().text.encode_utf16().count())
    }

    /// IME is enabled for the filter prompt and the editor's text-entry modes.
    fn accepts_text_input(&self, _: &mut Window, _: &mut Context<Self>) -> bool {
        self.accepts_native_text()
    }

    /// Paste literal clipboard text into the current editor or help prompt.
    fn paste(&mut self, item: ClipboardItem, window: &mut Window, cx: &mut Context<Self>) {
        if self.accepts_native_text()
            && let Some(text) = item.text()
        {
            self.composition_mut().clear();
            self.commit_text(&text, window, cx);
        }
    }
}
