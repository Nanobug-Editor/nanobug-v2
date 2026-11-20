//! Keep provider results and fold state with their retained editor buffers.
use super::EditorWindow;
use nanobug::plugins::session::{State, View};
impl EditorWindow {
    /// Refresh results for `scope` from `view`; report whether the displayed buffer changed.
    pub(in crate::grid) fn sync_presentations(&mut self, scope: u64, view: &View) -> bool {
        let mut changed = false;
        let active_buffer = self.active;
        for (index, buffer) in self.buffers.iter_mut().enumerate() {
            let doc = &buffer.vim.doc;
            let filetype = buffer.vim.filetype_override();
            let diagnostics = view
                .document_diagnostics
                .iter()
                .filter(|(key, data)| {
                    key.instance.project == scope
                        && data.version == (doc.opening, doc.revision)
                        && doc.path.as_ref() == Some(&data.path)
                        && view.modules.get(*key) == Some(&State::Ready)
                })
                .map(|(_, data)| data.clone())
                .collect();
            let provider = view
                .document_presentations
                .iter()
                .filter(|(key, data)| {
                    key.instance.project == scope
                        && data.version == (doc.opening, doc.revision)
                        && view.modules.get(*key) == Some(&State::Ready)
                })
                .min_by_key(|(key, _)| {
                    // An explicit match takes priority over an old package's path match.
                    !filetype.as_ref().is_some_and(|filetype| {
                        view.installed.iter().any(|record| {
                            let manifest = &record.release.manifest;
                            manifest.identity().namespace() == key.namespace
                                && manifest.modules.iter().any(|module| {
                                    module.id == key.instance.module
                                        && module.document_filetypes.contains(filetype)
                                })
                        })
                    })
                })
                .map(|(key, data)| (key.clone(), data.clone()));
            let active = index == active_buffer;
            let keep_folds = provider.is_none()
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
                                            && module.accepts_filetype(
                                                doc.path.as_deref(),
                                                filetype.as_deref(),
                                            )
                                    })
                            })
                    });
            let matches = |data: &nanobug::plugins::session::DocumentSyntax| {
                data.version == (doc.opening, doc.revision) && doc.path.as_ref() == Some(&data.path)
            };
            let grammar = nanobug::plugins::session::builtin_syntax_grammar(
                doc.path.as_deref(),
                filetype.as_deref(),
            );
            let native = view
                .builtin_syntax
                .as_ref()
                .filter(|(id, data)| *id == scope && data.grammar == grammar && matches(data))
                .map(|(_, data)| data.clone())
                .or_else(|| {
                    buffer.syntax_layers[0]
                        .as_ref()
                        .filter(|data| data.grammar == grammar && matches(data))
                        .cloned()
                });
            let semantic = view
                .semantic_syntax
                .iter()
                .find(|(key, data)| key.instance.project == scope && matches(data))
                .map(|(_, data)| data.clone());
            for (slot, next) in buffer.syntax_layers.iter_mut().zip([native, semantic]) {
                if !matches!((&*slot, &next), (None, None))
                    && !slot
                        .as_ref()
                        .zip(next.as_ref())
                        .is_some_and(|(a, b)| std::sync::Arc::ptr_eq(a, b))
                {
                    *slot = next;
                    changed |= active;
                }
            }
            changed |= buffer.set_diagnostics(diagnostics) && active;
            if keep_folds {
                buffer.retain_highlights();
            } else {
                changed |= buffer.set_presentation(provider) && active;
            }
        }
        changed
    }
}
