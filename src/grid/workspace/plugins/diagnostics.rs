//! Resolve live plugin publications to editor buffers before building diagnostic views.

use super::Workspace;
use crate::grid::workspace::{projects::Project, quickfix::Entry};
use nanobug::{
    document::Document,
    plugins::session::{DocumentDiagnostics, Key, State, View},
};
use nanobug_plugin_sdk::DiagnosticSeverity;
use std::{collections::BTreeMap, sync::Arc};

/// Yield only the results whose provider, source opening, path and revision still match.
pub(super) fn current_results<'a>(
    project: &'a Project,
    view: &'a View,
) -> impl Iterator<Item = (&'a Key, &'a Document, &'a Arc<DocumentDiagnostics>)> {
    view.document_diagnostics
        .iter()
        .chain(&view.guest_diagnostics)
        .filter_map(move |((key, opening), data)| {
            if key.instance.project != project.plugin_scope
                || view.modules.get(key) != Some(&State::Ready)
            {
                return None;
            }
            let (pane, index) = project.document_owner(*opening)?;
            let doc = &project.editor_by_id(pane)?.buffers()[index].vim.doc;
            (data.version == (*opening, doc.revision) && data.path == doc.path)
                .then_some((key, doc, data))
        })
}

impl Workspace {
    /// Route diagnostic Ex bindings through the same windows and commands as their mappings.
    pub(in crate::grid::workspace) fn diagnostic_command(
        &mut self,
        command: nanobug::vim::DiagnosticCommand,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        use nanobug::vim::{
            DiagnosticCommand as D, DiagnosticList as L, ListVisibility as V, QuickfixCommand as Q,
        };
        match command {
            D::Line => self.show_line_diagnostics(window, cx),
            D::Enable(enabled) => self.set_diagnostic_display(enabled, window, cx),
            D::Export {
                location,
                open,
                severity,
                title,
            } => self.set_diagnostic_list(location, open, severity, title, window, cx),
            D::View(list @ (L::Buffer | L::Project), visibility) => {
                let buffer = list == L::Buffer;
                match visibility {
                    V::Open => self.show_diagnostics(buffer, window, cx),
                    V::Close => self.close_diagnostics(buffer, window, cx),
                    V::Toggle => self.toggle_diagnostics(buffer, window, cx),
                }
            }
            D::View(list, visibility) => {
                let location = list == L::Location;
                match visibility {
                    V::Open => self.quickfix_command(location, Q::Open(None), window, cx),
                    V::Close => self.quickfix_command(location, Q::Close, window, cx),
                    V::Toggle => self.toggle_quickfix(location, window, cx),
                }
            }
        }
    }

    /// Collect the active project's known diagnostics, optionally restricted to a shared buffer
    /// number. Store source byte columns so tabs and wide characters do not move jump targets.
    pub(in crate::grid::workspace) fn diagnostic_entries(
        &self,
        current_buffer: Option<u64>,
    ) -> Vec<Entry> {
        let Some(project) = self.projects.active_project() else {
            return Vec::new();
        };
        let view = &self.plugins.view;
        let sources: BTreeMap<_, _> = view
            .installed
            .iter()
            .map(|record| {
                let manifest = &record.release.manifest;
                (manifest.identity().namespace(), &manifest.name)
            })
            .collect();
        let mut entries = Vec::new();
        for (key, doc, data) in current_results(project, view) {
            let buffer = doc.buffer_number();
            if current_buffer.is_some_and(|owner| buffer != owner) {
                continue;
            }
            let source = sources.get(&key.namespace).map_or_else(
                || key.instance.module.clone(),
                |name| format!("{name}/{}", key.instance.module),
            );
            for diagnostic in &data.diagnostics {
                let offset = diagnostic.start;
                if !doc.text.is_char_boundary(offset) {
                    continue;
                }
                let line = doc.starts.partition_point(|&start| start <= offset) - 1;
                entries.push(Entry {
                    buffer,
                    path: doc.path.clone(),
                    revision: doc.revision,
                    offset,
                    line,
                    column: offset - doc.starts[line],
                    severity: diagnostic.severity,
                    message: diagnostic.message.clone(),
                    source: source.clone(),
                });
            }
        }
        // Stable file and byte ordering is shared by the picker and both navigation lists.
        entries.sort_by(|a, b| {
            let order = |level| match level {
                DiagnosticSeverity::Error => 0,
                DiagnosticSeverity::Warning => 1,
                DiagnosticSeverity::Information => 2,
                DiagnosticSeverity::Hint => 3,
            };
            a.path
                .cmp(&b.path)
                .then_with(|| a.buffer.cmp(&b.buffer))
                .then_with(|| a.offset.cmp(&b.offset))
                .then_with(|| order(a.severity).cmp(&order(b.severity)))
                .then_with(|| a.source.cmp(&b.source))
                .then_with(|| a.message.cmp(&b.message))
        });
        entries.dedup_by(|a, b| {
            a.buffer == b.buffer
                && a.offset == b.offset
                && a.severity == b.severity
                && a.source == b.source
                && a.message == b.message
        });
        entries
    }
}
