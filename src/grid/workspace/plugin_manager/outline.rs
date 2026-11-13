//! Present the current document's symbols and apply editor folding commands.

use super::{
    Workspace,
    rows::{Row, Target, row},
};
use gpui::{Context, Window};
use nanobug::{i18n, plugins::session::State};
use nanobug_plugin_sdk::language::Symbol;

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
    /// Apply the latest eligible provider result to each buffer that already has or can receive one.
    pub(in crate::grid::workspace) fn refresh_document_presentation(&mut self) -> bool {
        let active_scope = self
            .projects
            .active_project()
            .map(|project| project.plugin_scope);
        let view = &self.plugins.view;
        let mut changed = false;
        for project in self.projects.iter_mut() {
            let scope = project.plugin_scope;
            let active_buffer = project.editor.active;
            for (index, buffer) in project.editor.buffers.iter_mut().enumerate() {
                let doc = &buffer.vim.doc;
                let provider = view
                    .document_presentations
                    .iter()
                    .find(|(key, data)| {
                        key.instance.project == scope
                            && data.version == (doc.opening, doc.revision)
                            && view.modules.get(*key) == Some(&State::Ready)
                    })
                    .map(|(key, data)| (key.clone(), data.clone()));
                if provider.is_none()
                    && buffer
                        .presentation_owner()
                        .is_some_and(|(key, generation)| {
                            view.modules.get(key) == Some(&State::Ready)
                                && view
                                    .runtime_status
                                    .get(&key.namespace)
                                    .is_some_and(|status| status.generation == generation)
                                && view.installed.iter().any(|record| {
                                    record.release.manifest.identity().namespace() == key.namespace
                                        && record.release.manifest.modules.iter().any(|module| {
                                            module.id == key.instance.module
                                                && module.accepts_document(doc.path.as_deref())
                                        })
                                })
                        })
                {
                    continue;
                }
                changed |= buffer.set_presentation(provider)
                    && Some(scope) == active_scope
                    && index == active_buffer;
            }
        }
        changed
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
        let buffer = &project.editor.buffers[project.editor.active];
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
            let buffer = &mut project.editor.buffers[project.editor.active];
            let doc = &buffer.vim.doc;
            if (doc.opening, doc.revision) == outline.version
                && buffer
                    .symbols()
                    .iter()
                    .any(|symbol| symbol.offset == offset)
            {
                let offset = super::language::cursor_boundary(doc, offset);
                buffer.vim.set_cursor(offset);
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
        if project.editor.buffers[project.editor.active].toggle_fold() {
            self.last_cursor_space = None;
            self.sync(window, cx);
        } else {
            self.set_status(i18n::text("editor.no_fold").into());
        }
    }
}
