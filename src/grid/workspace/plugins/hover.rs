//! Keep requested definition details tied to the originating normal-mode caret.

use super::Workspace;
use crate::grid::windows::editor::{
    DefinitionHoverContent, DefinitionHoverPopup, MarkdownViewMode,
};
use crate::grid::workspace::layout::Focus;
use gpui::{App, Context, ScrollEvent, Task, Window, px};
use nanobug::{
    animation::scroll::GridScroll,
    document::Document,
    i18n,
    plugins::{
        package::Provider,
        session::{Action, State},
    },
    vim::Mode,
};
use nanobug_plugin_sdk::{DiagnosticSeverity, PluginEvent};
use std::{path::PathBuf, time::Duration};

/// Number of editor rows moved by a single page key in the hover popup.
const HOVER_SCROLL_ROWS: isize = 4;

/// One hover request and the reply rendered beside the caret at that position.
pub(super) struct DefinitionHover {
    /// Unique request sequence used to discard superseded replies.
    request: u64,
    /// Project and document identity captured before invoking the language provider.
    scope: u64,
    opening: u64,
    revision: u64,
    path: Option<PathBuf>,
    caret: usize,
    /// Server documentation once the request has answered.
    content: Option<DefinitionHoverContent>,
    /// Animated offset and requested destination in editor rows.
    scroll: GridScroll,
    /// Fractional wheel rows retained independently of the editor's wheel input.
    wheel_rows: f32,
    /// Owner of the request for this popup, or the publication displayed for a local diagnostic.
    source: PopupSource,
}

/// Source-specific validity of the details beside the editor caret.
enum PopupSource {
    /// Keep an in-flight definition reply active until the popup is dismissed.
    Definition { _task: Task<()> },
    /// Diagnostics are valid while the buffer has the same publication.
    Diagnostics(u64),
}

impl Workspace {
    /// Return the active normal-mode editor document and its plugin scope when it owns input.
    fn definition_hover_target(&self) -> Option<(u64, &Document)> {
        let project = self.projects.active_project()?;
        let vim = project.editor.vim();
        (!self.modal_open()
            && !self.plugins.surface.focused
            && project.focus == Focus::Editor
            && project.editor.markdown_view_mode() != Some(MarkdownViewMode::Preview)
            && vim.mode == Mode::Normal)
            .then_some((project.plugin_scope, &vim.doc))
    }

    /// Return the pending or visible request while its originating editor and caret are current.
    fn current_definition_hover(&self) -> Option<&DefinitionHover> {
        let hover = self.plugins.definition_hover.as_ref()?;
        let (scope, doc) = self.definition_hover_target()?;
        (hover.scope == scope
            && hover.opening == doc.opening
            && hover.revision == doc.revision
            && hover.path == doc.path
            && match hover.source {
                PopupSource::Diagnostics(epoch) => {
                    self.projects.active().editor.buffer().diagnostic_epoch() == epoch
                }
                PopupSource::Definition { .. } => true,
            }
            && hover.caret == doc.cursor)
            .then_some(hover)
    }

    /// Inspect the returned definition text for the active caret in native tests.
    #[cfg(all(test, windows))]
    pub(in crate::grid::workspace) fn definition_hover_text(&self) -> Option<&str> {
        Some(self.current_definition_hover()?.content.as_ref()?.text())
    }

    /// Measure the active reply through `cx` and fit it next to the caret in `window`.
    pub(in crate::grid::workspace) fn definition_hover_popup(
        &self,
        window: &Window,
        cx: &App,
    ) -> Option<DefinitionHoverPopup> {
        let hover = self.current_definition_hover()?;
        let details = hover.content.as_ref()?;
        let editor = &self.projects.active_project()?.editor;
        editor.definition_hover_popup(
            self.active_editor_area(),
            self.metrics,
            &self.spacing,
            details,
            &hover.scroll,
            window,
            cx,
        )
    }

    /// Scroll the visible popup in `window` by one step in `direction` (-1 or 1).
    /// Return whether the popup consumed the key, including at either boundary.
    pub(in crate::grid::workspace) fn scroll_definition_hover(
        &mut self,
        direction: isize,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(popup) = self.definition_hover_popup(window, cx) else {
            return false;
        };
        self.request_definition_hover_scroll(
            popup.scrolled_first_row(direction * HOVER_SCROLL_ROWS),
            popup.last_first_row(),
            window,
            cx,
        );
        true
    }

    /// Consume wheel `event` inside the visible popup in `window`, including at either boundary.
    /// Retain partial vertical steps for this popup only and schedule movement through `cx`.
    pub(in crate::grid::workspace) fn wheel_definition_hover(
        &mut self,
        event: &ScrollEvent,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(popup) = self
            .definition_hover_popup(window, cx)
            .filter(|popup| popup.bounds().contains(&event.position))
        else {
            return false;
        };
        let hover = self
            .plugins
            .definition_hover
            .as_mut()
            .expect("visible hover");
        let row_height = self.metrics.lh;
        hover.wheel_rows -= f32::from(event.delta.pixel_delta(px(row_height)).y) / row_height;
        let rows = hover.wheel_rows.trunc() as isize;
        hover.wheel_rows -= rows as f32;
        let last = popup.last_first_row();
        let target = popup.scrolled_first_row(rows);
        if (target == 0 && hover.wheel_rows < 0.) || (target == last && hover.wheel_rows > 0.) {
            hover.wheel_rows = 0.;
        }
        self.request_definition_hover_scroll(target, last, window, cx);
        true
    }

    /// Animate to `target` within `last` using the scroll-frame scheduler for `window` and `cx`.
    fn request_definition_hover_scroll(
        &mut self,
        target: usize,
        last: usize,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let hover = self
            .plugins
            .definition_hover
            .as_mut()
            .expect("visible hover");
        if hover.scroll.target() != target {
            hover
                .scroll
                .request(target, last, self.scroll_epoch.elapsed().as_secs_f64());
            self.schedule_scroll(window, cx);
            cx.notify();
        }
    }

    /// Return whether the current definition popup needs another scroll frame.
    pub(in crate::grid::workspace) fn definition_hover_scroll_active(&self) -> bool {
        self.current_definition_hover()
            .is_some_and(|hover| hover.scroll.is_active())
    }

    /// Move the current popup toward its target as part of the workspace's scroll frame.
    pub(in crate::grid::workspace) fn advance_definition_hover_scroll(&mut self) {
        if self.definition_hover_scroll_active() {
            let now = self.scroll_epoch.elapsed().as_secs_f64();
            self.plugins
                .definition_hover
                .as_mut()
                .expect("active hover")
                .scroll
                .advance(now);
        }
    }

    /// Cancel the query and remove its panel. Return whether it belonged to the active caret.
    pub(in crate::grid::workspace) fn dismiss_definition_hover(
        &mut self,
        cx: &mut Context<Self>,
    ) -> bool {
        let current = self.current_definition_hover().is_some();
        if self.plugins.definition_hover.take().is_some() {
            cx.notify();
        }
        current
    }

    /// Ask the active hover provider for details at the caret in `window` through `cx`.
    pub(in crate::grid::workspace) fn plugin_definition_hover(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dismiss_definition_hover(cx);
        self.sync_plugin_projects();
        let Some((scope, doc)) = self.definition_hover_target() else {
            return;
        };
        if doc.text.len() > nanobug_plugin_sdk::document_wire::MAX_DOCUMENT_BYTES {
            self.set_status(i18n::text("editor.plugin_document_too_large").into());
            self.sync(window, cx);
            return;
        }
        let filetype = self.projects.active().editor.vim().filetype_override();
        let Some((key, _, generation)) = self
            .language_providers(
                Provider::Hover,
                scope,
                doc.path.as_deref(),
                filetype.as_deref(),
            )
            .into_iter()
            .next()
        else {
            self.set_status(i18n::text("editor.no_definition_details").into());
            self.sync(window, cx);
            return;
        };
        let (opening, revision, caret, path) =
            (doc.opening, doc.revision, doc.cursor, doc.path.clone());
        self.plugins.request_id += 1;
        let request = self.plugins.request_id;
        let (reply, receive) = async_channel::bounded(1);
        let action = Action::Invoke {
            key: key.clone(),
            event: PluginEvent::Hover { offset: caret },
            reply: Some(reply),
            timeout: Duration::from_secs(15),
        };
        if self
            .plugins
            .session
            .as_ref()
            .is_none_or(|session| session.send(action).is_err())
        {
            self.set_status(i18n::text("editor.language_provider_failed").into());
            self.sync(window, cx);
            return;
        }
        let task = cx.spawn_in(window, async move |workspace, cx| {
            let reply = receive.recv().await.ok();
            let _ = cx.update(|window, cx| {
                workspace.update(cx, |workspace, cx| {
                    if workspace
                        .current_definition_hover()
                        .filter(|hover| matches!(hover.source, PopupSource::Definition { .. }))
                        .map(|hover| hover.request)
                        != Some(request)
                    {
                        return;
                    }
                    let live = workspace.plugins.view.modules.get(&key) == Some(&State::Ready)
                        && workspace
                            .plugins
                            .view
                            .runtime_status
                            .get(&key.namespace)
                            .is_some_and(|status| status.generation == generation);
                    if let Some(text) = reply
                        .and_then(Result::ok)
                        .filter(|reply| {
                            live && reply.generation == generation
                                && reply.version == Some((opening, revision))
                        })
                        .and_then(|reply| reply.output.hover)
                        .filter(|text| !text.trim().is_empty())
                    {
                        workspace.plugins.definition_hover.as_mut().unwrap().content =
                            Some(DefinitionHoverContent::new(text));
                        cx.notify();
                    } else {
                        workspace.dismiss_definition_hover(cx);
                        workspace.set_status(i18n::text("editor.no_definition_details").into());
                        workspace.sync(window, cx);
                    }
                })
            });
        });
        self.plugins.definition_hover = Some(DefinitionHover {
            request,
            scope,
            opening,
            revision,
            path,
            caret,
            content: None,
            scroll: GridScroll::default(),
            wheel_rows: 0.,
            source: PopupSource::Definition { _task: task },
        });
    }

    /// Show every diagnostic on the current editor line in a scrollable popup at the caret.
    pub(in crate::grid::workspace) fn show_line_diagnostics(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dismiss_definition_hover(cx);
        let Some((scope, doc)) = self.definition_hover_target() else {
            return;
        };
        let buffer = self.projects.active().editor.buffer();
        let mut items = buffer
            .line_diagnostics(buffer.cursor_row())
            .collect::<Vec<_>>();
        items.sort_by_key(|item| (item.severity as u8, item.start));
        if items.is_empty() {
            self.set_status(i18n::text("editor.no_diagnostics").into());
            self.sync(window, cx);
            return;
        }
        let text = items
            .iter()
            .map(|item| {
                let label = i18n::text(match item.severity {
                    DiagnosticSeverity::Error => "message.error",
                    DiagnosticSeverity::Warning => "message.warning",
                    DiagnosticSeverity::Information => "editor.diagnostic_info",
                    DiagnosticSeverity::Hint => "editor.diagnostic_hint",
                });
                format!("{label} · {}", item.message)
            })
            .collect::<Vec<_>>()
            .join("\n");
        let epoch = buffer.diagnostic_epoch();
        let hover = DefinitionHover {
            request: self.plugins.request_id,
            scope,
            opening: doc.opening,
            revision: doc.revision,
            path: doc.path.clone(),
            caret: doc.cursor,
            content: Some(DefinitionHoverContent::plain(text)),
            scroll: GridScroll::default(),
            wheel_rows: 0.,
            source: PopupSource::Diagnostics(epoch),
        };
        self.plugins.definition_hover = Some(hover);
        cx.notify();
    }

    /// Move to a current diagnostic matching `severity` and show its line's messages.
    pub(in crate::grid::workspace) fn jump_diagnostic(
        &mut self,
        forward: bool,
        edge: bool,
        count: usize,
        severity: Option<DiagnosticSeverity>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let project = self.projects.active_mut();
        if let Some(offset) = project
            .editor
            .buffer()
            .diagnostic_target(forward, edge, count, severity)
        {
            project.editor.jump_to(offset);
            project.status.clear();
            self.show_line_diagnostics(window, cx);
        } else {
            self.set_status(i18n::text("editor.no_diagnostics").into());
        }
    }

    /// Set the workspace diagnostic display to `enabled`, preserving data for manual queries.
    pub(in crate::grid::workspace) fn set_diagnostic_display(
        &mut self,
        enabled: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.plugins.diagnostic_marks = enabled;
        self.dismiss_definition_hover(cx);
        self.set_status(
            i18n::text(if enabled {
                "editor.diagnostics_shown"
            } else {
                "editor.diagnostics_hidden"
            })
            .into(),
        );
        self.sync(window, cx);
    }

    /// Toggle diagnostic decorations in every editor pane.
    pub(in crate::grid::workspace) fn toggle_diagnostic_display(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_diagnostic_display(!self.plugins.diagnostic_marks, window, cx);
    }
}
