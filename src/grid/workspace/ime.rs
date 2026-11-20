//! Native input for the document and popup filter, with preedit local to the owning surface.

#[cfg(windows)]
mod windows;

use super::{Workspace, layout::Focus};
use crate::{
    grid::windows::{editor::MarkdownViewMode, start::StartPage},
    ui::ime::Composition,
};
use gpui::{
    Bounds, ClipboardItem, Context, EntityInputHandler, Pixels, Point, UTF16Selection, Window, px,
    size,
};
use nanobug::animation::scroll::ScrollInput;
use std::ops::Range;

impl Workspace {
    /// Clear preedit when the active surface stops accepting text and sync the system IME.
    pub(super) fn sync_text_input(&mut self, window: &Window, cx: &mut Context<Self>) {
        if !self.accepts_native_text() {
            if let Some(settings) = &mut self.settings {
                settings.composition.clear();
            }
            if let Some(history) = &mut self.command_window {
                history.editor.composition_mut().clear();
            }
            self.plugins.composition.clear();
            if let Some(project) = self.projects.active_project_mut() {
                project.editor.composition_mut().clear();
            }
            if let Some(page) = self.projects.start_mut()
                && let Some(picker) = &mut page.picker
            {
                picker.composition.clear();
            }
            self.help.composition.clear();
            if let Some(session) = &mut self.file_picker {
                session.state.composition.clear();
                if let Some(editor) = session.focused_editor_mut() {
                    editor.composition_mut().clear();
                }
            }
            #[cfg(feature = "plugin-probe")]
            if let Some(probe) = &mut self.probe {
                probe.composition.clear();
            }
        }
        #[cfg(windows)]
        self.sync_native_ime(window, cx, false);
        #[cfg(not(windows))]
        let _ = (window, cx);
    }

    /// Read preedit from the surface that currently owns native input.
    fn composition(&self) -> Option<&Composition> {
        if let Some(settings) = &self.settings {
            return settings.accepts_text().then_some(&settings.composition);
        }
        if self.vim_menu.is_some() {
            return None;
        }
        if self.terminal_keys_active() {
            return self
                .accepts_native_text()
                .then_some(self.projects.active().editor.composition());
        }
        if let Some(history) = &self.command_window {
            return history
                .editor
                .vim()
                .accepts_text()
                .then_some(history.editor.composition());
        }
        if self.plugins.owns_input() {
            return self
                .plugins
                .accepts_text()
                .then_some(&self.plugins.composition);
        }
        #[cfg(feature = "plugin-probe")]
        if self.probe_has_focus() {
            return self.probe.as_ref().map(|probe| &probe.composition);
        }
        if self.help.is_open() {
            Some(&self.help.composition)
        } else if let Some(session) = &self.file_picker {
            if let Some(editor) = session.focused_editor() {
                return editor.vim().accepts_text().then_some(editor.composition());
            }
            session
                .state
                .prompt
                .as_ref()
                .map(|_| &session.state.composition)
        } else if let Some(page) = self.projects.start() {
            page.picker
                .as_ref()
                .filter(|picker| picker.filter_focused())
                .map(|picker| &picker.composition)
        } else {
            self.projects
                .active_project()
                .filter(|project| {
                    project.editor.markdown_view_mode() != Some(MarkdownViewMode::Preview)
                })
                .map(|project| project.editor.composition())
        }
    }

    /// Update preedit on the surface that currently owns native input.
    fn composition_mut(&mut self) -> Option<&mut Composition> {
        if self.settings.is_some() {
            return self
                .settings
                .as_mut()
                .filter(|s| s.accepts_text())
                .map(|s| &mut s.composition);
        }
        if self.vim_menu.is_some() {
            return None;
        }
        if self.terminal_keys_active() {
            return self
                .accepts_native_text()
                .then(|| self.projects.active_mut().editor.composition_mut());
        }
        if let Some(history) = &mut self.command_window {
            return history
                .editor
                .vim()
                .accepts_text()
                .then(|| history.editor.composition_mut());
        }
        if self.plugins.owns_input() {
            return self
                .plugins
                .accepts_text()
                .then_some(&mut self.plugins.composition);
        }
        #[cfg(feature = "plugin-probe")]
        if self.probe_has_focus() {
            return self.probe.as_mut().map(|probe| &mut probe.composition);
        }
        if self.help.is_open() {
            Some(&mut self.help.composition)
        } else if let Some(session) = &mut self.file_picker {
            if session.previewing_file() {
                let editor = session.focused_editor_mut()?;
                return editor
                    .vim()
                    .accepts_text()
                    .then_some(editor.composition_mut());
            }
            session
                .state
                .prompt
                .as_ref()
                .map(|_| &mut session.state.composition)
        } else if self.projects.start().is_some() {
            self.projects
                .start_mut()?
                .picker
                .as_mut()
                .filter(|picker| picker.filter_focused())
                .map(|picker| &mut picker.composition)
        } else {
            self.projects
                .active_project_mut()
                .filter(|project| {
                    project.editor.markdown_view_mode() != Some(MarkdownViewMode::Preview)
                })
                .map(|project| project.editor.composition_mut())
        }
    }

    /// Whether the active surface accepts native text input at this moment.
    pub(super) fn accepts_native_text(&self) -> bool {
        if let Some(settings) = &self.settings {
            return settings.accepts_text();
        }
        if self.vim_menu.is_some() {
            return false;
        }
        if self.terminal_keys_active() {
            return self
                .projects
                .active()
                .editor
                .terminal()
                .is_some_and(|terminal| terminal.accepts_text());
        }
        if let Some(history) = &self.command_window {
            return history.editor.vim().accepts_text();
        }
        if self.plugins.owns_input() {
            return self.plugins.accepts_text();
        }
        #[cfg(feature = "plugin-probe")]
        if self.probe_has_focus() {
            return true;
        }
        if self.small_modal_open() {
            false
        } else if self.help.is_open() {
            self.help.is_filtering()
        } else if let Some(session) = &self.file_picker {
            session.state.prompt.is_some()
                || session
                    .focused_editor()
                    .is_some_and(|editor| editor.vim().accepts_text())
        } else if let Some(page) = self.projects.start() {
            page.picker
                .as_ref()
                .is_some_and(|picker| picker.filter_focused())
        } else {
            self.projects.active_project().is_some_and(|project| {
                project.focus == Focus::Editor
                    && project.editor.markdown_view_mode() != Some(MarkdownViewMode::Preview)
                    && project.editor.vim().accepts_text()
            })
        }
    }

    /// Refresh the preedit and candidate anchor in `window` through `cx`.
    fn repaint_preedit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.is_some() {
            self.repaint_settings(window, cx);
            return;
        }
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
        self.composition()?.text_for_range(range, adjusted)
    }

    /// Expose the native composition selection, or an empty local text-input interval.
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.composition()?.selection.clone(),
            reversed: false,
        })
    }

    /// Report the current native preedit interval in UTF-16 units.
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        let composition = self.composition()?;
        (!composition.is_empty()).then(|| 0..composition.text.encode_utf16().count())
    }

    /// Finalize any outstanding native composition when the OS unmarks it.
    fn unmark_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(composition) = self.composition_mut() else {
            return;
        };
        let text = composition.take();
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
        if let Some(composition) = self.composition_mut() {
            composition.clear();
            self.commit_text(text, window, cx);
        }
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
        if self.accepts_native_text()
            && let Some(composition) = self.composition_mut()
        {
            composition.mark(range, text, selection);
            // Keep candidate navigation with the OS while it owns a preedit.
            if !text.is_empty() {
                self.dismiss_completion(cx);
            }
            self.trace_plugin_preedit(text.encode_utf16().count());
            #[cfg(feature = "plugin-probe")]
            if self.probe_has_focus()
                && let Some(client) = self.probe.as_ref().and_then(|probe| probe.client.as_ref())
            {
                client.mark(
                    "draft_preedit",
                    serde_json::json!({ "units": text.encode_utf16().count() }),
                );
            }
            self.repaint_preedit(window, cx);
        }
    }

    /// Anchor OS candidate UI to the logical input point, independently of visual interpolation.
    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        if let Some(settings) = &self.settings {
            if !settings.accepts_text() {
                return None;
            }
            let (x, y) = settings.caret(self.settings_area(), &self.spacing)?;
            return Some(Bounds::new(
                self.metrics.at(x, y),
                size(px(self.metrics.cw), px(self.metrics.lh)),
            ));
        }
        if let Some(bounds) = self.canvas_input_caret_bounds() {
            return Some(bounds);
        }
        if !self.plugins.owns_input()
            && self.file_picker.is_none()
            && let (Some(page), Some(layout)) = (self.projects.start(), self.start_layout())
        {
            let metrics = StartPage::content_metrics(self.metrics);
            let (caret, width) = page.caret(layout, metrics, &self.spacing);
            return Some(Bounds::new(
                caret.origin,
                size(px(metrics.cw * width), px(metrics.lh)),
            ));
        }
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
        Some(self.composition()?.selection.end)
    }

    /// Size of the local native composition surface, measured in UTF-16 units.
    fn text_length_utf16(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        Some(self.composition()?.text.encode_utf16().count())
    }

    /// IME is enabled for the filter prompt and the editor's text-entry modes.
    fn accepts_text_input(&self, _: &mut Window, _: &mut Context<Self>) -> bool {
        self.accepts_native_text()
    }

    /// Paste literal clipboard text into the current editor or help prompt.
    fn paste(&mut self, item: ClipboardItem, window: &mut Window, cx: &mut Context<Self>) {
        if self.accepts_native_text()
            && let Some(text) = item.text()
            && let Some(composition) = self.composition_mut()
        {
            composition.clear();
            self.commit_pasted_text(&text, window, cx);
        }
    }
}

#[cfg(all(test, windows))]
mod tests;
