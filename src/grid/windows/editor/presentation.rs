//! Keep provider results and fold state with their retained editor buffers.
use super::EditorWindow;
use nanobug::plugins::session::{DocumentDiagnostics, DocumentSyntax, State, View};
use std::{collections::BTreeMap, sync::Arc};

impl EditorWindow {
    /// Apply syntax and the project's diagnostics grouped by shared buffer to this pane.
    pub(in crate::grid) fn sync_presentations(
        &mut self,
        scope: u64,
        view: &View,
        focused: bool,
        diagnostics: &BTreeMap<u64, Vec<Arc<DocumentDiagnostics>>>,
    ) -> bool {
        let mut changed = false;
        let active_buffer = self.active;
        for (index, buffer) in self.buffers.iter_mut().enumerate() {
            let active = index == active_buffer;
            let incoming = diagnostics
                .get(&buffer.vim.doc.buffer_number())
                .cloned()
                .unwrap_or_default();
            changed |= buffer.set_diagnostics(incoming) && active;
            let doc = &buffer.vim.doc;
            let filetype = buffer.vim.filetype_override();
            let matches = |data: &DocumentSyntax| {
                data.version == (doc.opening, doc.revision) && doc.path.as_ref() == Some(&data.path)
            };
            let grammar = nanobug::plugins::session::builtin_syntax_grammar(
                doc.path.as_deref(),
                filetype.as_deref(),
            );
            let native = view
                .builtin_syntax
                .get(&(scope, doc.opening))
                .filter(|data| data.grammar == grammar && matches(data))
                .cloned()
                .or_else(|| {
                    buffer.syntax_layers[0]
                        .as_ref()
                        .filter(|data| data.grammar == grammar && matches(data))
                        .cloned()
                });
            changed |= replace_syntax(&mut buffer.syntax_layers[0], native) && active;
            if !focused {
                continue;
            }
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
            let semantic = view
                .semantic_syntax
                .iter()
                .find(|(key, data)| key.instance.project == scope && matches(data))
                .map(|(_, data)| data.clone());
            changed |= replace_syntax(&mut buffer.syntax_layers[1], semantic) && active;
            if keep_folds {
                buffer.retain_highlights();
            } else {
                changed |= buffer.set_presentation(provider) && active;
            }
        }
        changed
    }
}

/// Replace a lexical or semantic layer only when its published allocation changes.
fn replace_syntax(
    slot: &mut Option<Arc<DocumentSyntax>>,
    next: Option<Arc<DocumentSyntax>>,
) -> bool {
    if matches!((&*slot, &next), (None, None))
        || slot
            .as_ref()
            .zip(next.as_ref())
            .is_some_and(|(a, b)| Arc::ptr_eq(a, b))
    {
        return false;
    }
    *slot = next;
    true
}
