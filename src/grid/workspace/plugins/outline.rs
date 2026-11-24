//! Present the current document's symbols and apply editor folding commands.

use super::{
    Workspace,
    rows::{Row, Target, row},
};
use gpui::{Context, Window};
use nanobug::i18n;
use nanobug_plugin_sdk::language::Symbol;
use std::collections::BTreeMap;

/// Clickable outline bound to the version and provider that produced the displayed symbols.
pub(super) struct Outline {
    /// Project tab owning this popup.
    scope: u64,
    /// Document opening and revision used to build the rows.
    version: (u64, u64),
    /// Source-ordered symbols captured when the outline opens.
    symbols: Vec<Symbol>,
}

impl Outline {
    /// Reuse the plugin list's native key handling and pointer targets.
    pub(super) fn rows(&self) -> Vec<Row> {
        self.symbols
            .iter()
            .map(|symbol| row(&symbol.name, &symbol.kind, Target::Symbol(symbol.offset)))
            .collect()
    }
}

impl Workspace {
    /// Apply language results to each current-page pane and repaint when a displayed buffer changes.
    pub(in crate::grid::workspace) fn refresh_document_presentation(&mut self) -> bool {
        let active_scope = self
            .projects
            .active_project()
            .map(|project| project.plugin_scope);
        let view = &self.plugins.view;
        let mut changed = false;
        for project in self.projects.iter_mut() {
            let scope = project.plugin_scope;
            let visible = Some(scope) == active_scope;
            let focused = project.active_editor_id();
            // Only an opening still owned by this buffer can lend its diagnostics to a peer view.
            let mut by_buffer = BTreeMap::new();
            for (_, doc, data) in super::diagnostics::current_results(project, view) {
                by_buffer
                    .entry(doc.buffer_number())
                    .or_insert_with(Vec::new)
                    .push(data.clone());
            }
            for id in project.splits().ids() {
                if let Some(editor) = project.editor_by_id_mut(id) {
                    changed |=
                        editor.set_diagnostic_marks(self.plugins.diagnostic_marks) && visible;
                    changed |= editor.sync_presentations(scope, view, id == focused, &by_buffer)
                        && visible;
                }
            }
        }
        self.refresh_diagnostic_lists() || changed
    }

    /// Open a keyboard-selectable outline for the active document.
    pub(in crate::grid::workspace) fn plugin_outline(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.projects.active_project() else {
            return;
        };
        let buffer = project.editor.buffer();
        if buffer.symbols().is_empty() {
            self.set_status(i18n::text("editor.no_outline").into());
            return;
        }
        self.plugins.outline = Some(Outline {
            scope: project.plugin_scope,
            version: (buffer.vim.doc.opening, buffer.vim.doc.revision),
            symbols: buffer.symbols().to_vec(),
        });
        self.plugins.language = None;
        self.open_document_list();
        self.plugin_refresh(window, cx);
    }

    /// Reveal the selected byte `offset` through `window` and `cx` while the outline matches.
    pub(super) fn jump_to_symbol(
        &mut self,
        offset: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(outline) = self.plugins.outline.take() else {
            return;
        };
        if let Some(project) = self.projects.active_project_mut()
            && project.plugin_scope == outline.scope
        {
            let buffer = project.editor.buffer();
            let doc = &buffer.vim.doc;
            if (doc.opening, doc.revision) == outline.version
                && buffer
                    .symbols()
                    .iter()
                    .any(|symbol| symbol.offset == offset)
            {
                let offset = super::language::cursor_boundary(doc, offset);
                project.editor.edit(|vim, _| {
                    let row = vim.doc.location(offset).0;
                    vim.doc.reveal_row(row);
                    vim.set_cursor(offset);
                    nanobug::vim::Action::None
                });
            }
        }
        self.plugins.open = false;
        self.sync(window, cx);
    }

    /// Collapse or expand the nearest fold in the current editor buffer.
    pub(in crate::grid::workspace) fn plugin_fold(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.projects.active_project_mut() else {
            return;
        };
        if project.editor.toggle_fold() {
            self.last_cursor_space = None;
            self.sync(window, cx);
        } else {
            self.set_status(i18n::text("editor.no_fold").into());
        }
    }
}
