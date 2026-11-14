//! Show valid language results as each active provider replies.

use super::{
    Workspace,
    rows::{Row, Target, row},
};
use crate::grid::windows::editor::EditResult;
use gpui::{Context, Window};
use nanobug::{
    document::Document,
    i18n,
    plugins::{
        package::Provider,
        session::{Action, CallResult, Key, State},
    },
    vim,
};
use nanobug_plugin_sdk::{
    Diagnostic, Edit, Instance, Permission, PluginEvent,
    language::{Completion, Definition},
};
use std::{collections::BTreeMap, path::PathBuf, time::Duration};
use unicode_segmentation::UnicodeSegmentation;

pub(super) struct LanguageResults {
    scope: u64,
    opening: u64,
    revision: u64,
    /// A rename can change the source of a language request without touching its text.
    path: Option<PathBuf>,
    cursor: usize,
    request: u64,
    kind: Provider,
    pending: usize,
    /// The first provider failure, shown if the request produced no usable results.
    error: Option<String>,
    completions: BTreeMap<Completion, Vec<String>>,
    diagnostics: Vec<(String, Diagnostic)>,
    /// Unique destinations in provider reply order.
    definitions: Vec<(String, Definition)>,
}

impl LanguageResults {
    fn matches(&self, doc: &Document) -> bool {
        (doc.opening, doc.revision) == (self.opening, self.revision)
            && self.path == doc.path
            && (self.kind == Provider::Diagnose || doc.cursor == self.cursor)
    }

    /// Merge one response without moving the user's selection to another completion.
    fn receive(
        &mut self,
        source: String,
        doc: Option<&Document>,
        result: Option<CallResult>,
        selection: usize,
    ) -> usize {
        self.pending -= 1;
        let Some(doc) = doc.filter(|doc| self.matches(doc)) else {
            return selection;
        };
        let Some(result) =
            result.filter(|reply| reply.version == Some((self.opening, self.revision)))
        else {
            return selection;
        };
        if self.kind == Provider::Complete {
            let selected = self.completions.keys().nth(selection).cloned();
            let legacy = result
                .output
                .completions
                .into_iter()
                .map(|text| Completion {
                    label: text.clone(),
                    edit: Edit {
                        start: self.cursor,
                        end: self.cursor,
                        text,
                    },
                });
            for mut item in result
                .output
                .completion_items
                .into_iter()
                .chain(legacy)
                .take(80)
            {
                let edit = &mut item.edit;
                if edit.start > self.cursor
                    || edit.end < self.cursor
                    || !doc.text.is_char_boundary(edit.start)
                    || !doc.text.is_char_boundary(edit.end)
                    || edit.text.len() > 2048
                    || edit.text.contains('\0')
                    || item.label.is_empty()
                    || item.label.len() > 2048
                    || item.label.contains('\0')
                {
                    continue;
                }
                item.label = item.label.replace("\r\n", "\n");
                edit.text = edit.text.replace("\r\n", "\n");
                if self.completions.len() >= 200 && !self.completions.contains_key(&item) {
                    continue;
                }
                let sources = self.completions.entry(item).or_default();
                if !sources.contains(&source) {
                    sources.push(source.clone());
                }
            }
            return selected
                .and_then(|selected| self.completions.keys().position(|value| value == &selected))
                .unwrap_or(selection);
        }
        if self.kind == Provider::Definition {
            for item in result.output.definitions.into_iter().take(80) {
                if item.path.is_absolute()
                    && self.definitions.len() < 200
                    && !self.definitions.iter().any(|(_, prior)| *prior == item)
                {
                    self.definitions.push((source.clone(), item));
                }
            }
            return selection;
        }
        self.add_diagnostics(source, doc, result.output.diagnostics);
        selection
    }

    /// Accept only version-matched, bounded diagnostic ranges from one provider.
    fn add_diagnostics(
        &mut self,
        source: String,
        doc: &Document,
        diagnostics: impl IntoIterator<Item = Diagnostic>,
    ) {
        for item in diagnostics.into_iter().take(80) {
            if item.start <= item.end
                && doc.text.is_char_boundary(item.start)
                && doc.text.is_char_boundary(item.end)
                && item.message.len() <= 2048
                && self.diagnostics.len() < 200
                && !self.diagnostics.iter().any(|(name, prior)| {
                    name == &source
                        && prior.start == item.start
                        && prior.end == item.end
                        && prior.message == item.message
                })
            {
                self.diagnostics.push((source.clone(), item));
            }
        }
    }

    /// Convert a legacy insertion `text` into an edit at the request's cursor.
    pub(super) fn insertion(&self, text: String) -> Edit {
        Edit {
            start: self.cursor,
            end: self.cursor,
            text,
        }
    }

    pub(super) fn title(&self) -> &'static str {
        i18n::text(match self.kind {
            Provider::Complete => "editor.completions",
            Provider::Definition => "editor.definitions",
            _ => "editor.diagnostics",
        })
    }

    /// Whether selecting a result jumps to an existing document range.
    pub(super) fn jumps_to_result(&self) -> bool {
        matches!(self.kind, Provider::Diagnose | Provider::Definition)
    }

    /// Empty-list feedback for this provider request.
    pub(super) fn empty_label(&self) -> &'static str {
        i18n::text(match self.kind {
            Provider::Complete => "editor.no_completions",
            Provider::Definition => "editor.no_definitions",
            _ => "editor.no_diagnostics",
        })
    }

    pub(super) fn rows(&self) -> Vec<Row> {
        let mut rows = self
            .completions
            .iter()
            .map(|(item, sources)| {
                let target = if item.edit.start == self.cursor
                    && item.edit.end == self.cursor
                    && item.edit.text == item.label
                {
                    Target::Completion(item.edit.text.clone())
                } else {
                    Target::CompletionEdit(item.edit.clone())
                };
                row(&item.label, sources.join(", "), target)
            })
            .collect::<Vec<_>>();
        rows.extend(self.diagnostics.iter().map(|(source, item)| {
            row(
                format!("{}  ·  {source}", item.message),
                format!("Bytes {}–{}", item.start, item.end),
                Target::Diagnostic(item.start),
            )
        }));
        rows.extend(self.definitions.iter().map(|(source, item)| {
            row(
                format!("{}  ·  {}", item.path.display(), u64::from(item.line) + 1),
                source,
                Target::Definition(item.clone()),
            )
        }));
        if self.pending > 0 {
            rows.push(row("Waiting for providers…", "", Target::None));
        }
        if rows.is_empty()
            && let Some(error) = &self.error
        {
            rows.push(row(error, "", Target::None));
        }
        rows
    }
}

impl Workspace {
    pub(in crate::grid::workspace) fn plugin_language(
        &mut self,
        kind: Provider,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sync_plugin_projects();
        let Some(project) = self.projects.active_project() else {
            return;
        };
        let doc = &project.editor.vim().doc;
        if doc.text.len() > nanobug_plugin_sdk::document_wire::MAX_DOCUMENT_BYTES {
            self.set_status(i18n::text("editor.plugin_document_too_large").into());
            return;
        }
        let (scope, opening, revision, cursor) =
            (project.plugin_scope, doc.opening, doc.revision, doc.cursor);
        let mut providers = Vec::new();
        for record in self.plugins.view.installed.iter() {
            if !record.enabled || !record.grants.contains(&Permission::ReadDocument) {
                continue;
            }
            let manifest = &record.release.manifest;
            for module in &manifest.modules {
                if !module.provides.contains(&kind) || !module.accepts_document(doc.path.as_deref())
                {
                    continue;
                }
                let key = Key {
                    namespace: manifest.identity().namespace(),
                    instance: Instance {
                        project: scope,
                        module: module.id.clone(),
                    },
                };
                if self.plugins.view.modules.get(&key) == Some(&State::Ready)
                    && let Some(status) = self.plugins.view.runtime_status.get(&key.namespace)
                {
                    providers.push((
                        key,
                        format!("{} ({}) · {}", manifest.name, manifest.id, module.id),
                        status.generation,
                    ));
                }
            }
        }
        if providers.is_empty() {
            if kind == Provider::Definition {
                let mut moved = false;
                self.projects.active_mut().editor.edit(|vim, _| {
                    moved = vim.local_declaration();
                    nanobug::vim::Action::None
                });
                if moved {
                    self.sync(window, cx);
                    return;
                }
            }
            self.set_status("No active language provider".into());
            return;
        }
        self.plugins.request_id += 1;
        let request = self.plugins.request_id;
        self.plugins.outline = None;
        let mut results = LanguageResults {
            scope,
            opening,
            revision,
            path: doc.path.clone(),
            cursor,
            request,
            kind,
            pending: 0,
            error: None,
            completions: BTreeMap::new(),
            diagnostics: Vec::new(),
            definitions: Vec::new(),
        };
        if kind == Provider::Diagnose {
            // Prefer current push diagnostics from each provider with a live result.
            providers.retain(|(key, source, _)| {
                let Some(live) = self
                    .plugins
                    .view
                    .document_diagnostics
                    .get(key)
                    .filter(|live| {
                        live.version == (opening, revision) && doc.path.as_ref() == Some(&live.path)
                    })
                else {
                    return true;
                };
                results.add_diagnostics(source.clone(), doc, live.diagnostics.iter().cloned());
                false
            });
        }
        results.pending = providers.len();
        self.plugins.language = Some(results);
        self.open_document_list();
        for (key, name, generation) in providers {
            let (reply, receive) = async_channel::bounded(1);
            let event = match kind {
                Provider::Complete => PluginEvent::Complete { offset: cursor },
                Provider::Definition => PluginEvent::Definition { offset: cursor },
                _ => PluginEvent::Diagnose,
            };
            if !self.plugins.send(Action::Invoke {
                key: key.clone(),
                event,
                reply: Some(reply),
                timeout: Duration::from_secs(if kind == Provider::Diagnose { 3 } else { 15 }),
            }) {
                if let Some(results) = &mut self.plugins.language {
                    results.pending -= 1;
                    results
                        .error
                        .get_or_insert_with(|| "Plugin request queue is busy".into());
                }
                continue;
            }
            cx.spawn_in(window, async move |workspace, cx| {
                let result = receive
                    .recv()
                    .await
                    .unwrap_or_else(|_| Err("Plugin request stopped".into()));
                let _ = cx.update(|window, cx| {
                    workspace.update(cx, |workspace, cx| {
                        let Some(results) = &mut workspace.plugins.language else {
                            return;
                        };
                        if results.request != request {
                            return;
                        }
                        let live = workspace.plugins.view.modules.get(&key) == Some(&State::Ready)
                            && workspace
                                .plugins
                                .view
                                .runtime_status
                                .get(&key.namespace)
                                .is_some_and(|status| status.generation == generation);
                        let doc = workspace
                            .projects
                            .active_project()
                            .filter(|project| project.plugin_scope == scope)
                            .map(|project| &project.editor.vim().doc);
                        let current = doc.is_some_and(|doc| results.matches(doc));
                        let result = match result {
                            Ok(reply)
                                if live
                                    && current
                                    && reply.generation == generation
                                    && reply.version
                                        == Some((results.opening, results.revision)) =>
                            {
                                if !reply.output.message.is_empty() {
                                    results.error.get_or_insert(reply.output.message.clone());
                                }
                                Some(reply)
                            }
                            Err(error) if live && current => {
                                let summary = error.lines().next().unwrap_or_default();
                                results.error.get_or_insert_with(|| {
                                    if summary.is_empty() {
                                        i18n::text("editor.language_provider_failed").into()
                                    } else {
                                        summary.chars().take(300).collect()
                                    }
                                });
                                None
                            }
                            _ => None,
                        };
                        workspace.plugins.manager.selection =
                            results.receive(name, doc, result, workspace.plugins.manager.selection);
                        if current
                            && results.pending == 0
                            && let [(_, definition)] = results.definitions.as_slice()
                        {
                            let definition = definition.clone();
                            workspace.use_definition(definition, window, cx);
                            return;
                        }
                        workspace.plugin_refresh(window, cx);
                    })
                });
            })
            .detach();
        }
        self.plugin_refresh(window, cx);
    }

    pub(super) fn use_language_result(
        &mut self,
        completion: Option<Edit>,
        offset: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(results) = self.plugins.language.take() else {
            return;
        };
        let Some(project) = self.projects.active_project_mut() else {
            return;
        };
        let mut changed_path = None;
        if project.plugin_scope == results.scope {
            project.editor.edit(|vim, _| {
                let doc = &mut vim.doc;
                if results.matches(doc) {
                    if let Some(edit) = completion {
                        let revision = doc.revision;
                        doc.replace(edit.start..edit.end, &edit.text, true);
                        if doc.revision != revision {
                            changed_path = doc.path.clone();
                        }
                    }
                    let target = cursor_boundary(doc, offset.unwrap_or(doc.cursor));
                    vim.set_cursor(target);
                }
                vim::Action::None
            });
        }
        self.plugins.open = false;
        self.apply_edit(
            EditResult {
                action: vim::Action::None,
                changed_path,
            },
            window,
            cx,
        );
        self.sync(window, cx);
    }

    /// Open an offered destination only while the originating request still matches the editor.
    pub(super) fn use_definition(
        &mut self,
        definition: Definition,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(results) = self.plugins.language.take() else {
            return;
        };
        self.plugins.open = false;
        let current = self.projects.active_project().is_some_and(|project| {
            project.plugin_scope == results.scope && results.matches(&project.editor.vim().doc)
        });
        let offered = results
            .definitions
            .iter()
            .any(|(_, offered)| *offered == definition);
        if current && offered {
            let from = results.path.as_deref();
            let destination = if from == Some(definition.path.as_path()) {
                // The current buffer can still hold the definition after its file was removed.
                Some(definition.path.clone())
            } else {
                definition
                    .path
                    .canonicalize()
                    .ok()
                    .filter(|path| path.is_file())
            };
            if let Some(path) = destination {
                if from != Some(&path) {
                    self.open(path.clone(), window, cx);
                }
                let project = self.projects.active_mut();
                let buffer = project.editor.buffer();
                let doc = &buffer.vim.doc;
                if doc.path.as_ref() == Some(&path)
                    && let Some(target) = definition_offset(doc, &definition)
                {
                    project.editor.jump_to(target);
                    self.sync(window, cx);
                    return;
                }
            }
        }
        self.set_status(i18n::text("editor.definition_unavailable").into());
        self.sync(window, cx);
    }
}

/// Map an LSP target to a grapheme boundary using the retained destination text.
fn definition_offset(doc: &Document, item: &Definition) -> Option<usize> {
    let row = usize::try_from(item.line).ok()?;
    doc.starts.get(row)?;
    let range = doc.line_range(row);
    let target = usize::try_from(item.character).ok()?;
    let mut width = 0;
    for (byte, scalar) in doc.text[range.clone()].char_indices() {
        if width == target {
            return Some(cursor_boundary(doc, range.start + byte));
        }
        width += scalar.len_utf16();
    }
    (width == target).then(|| cursor_boundary(doc, range.end))
}

/// Diagnostics use code-point offsets. The editor caret remains at a grapheme boundary.
pub(super) fn cursor_boundary(doc: &Document, requested: usize) -> usize {
    let row = doc
        .starts
        .partition_point(|&start| start <= requested)
        .saturating_sub(1);
    let line = doc.line_range(row);
    if requested >= line.end {
        return line.end;
    }
    doc.text[line.clone()]
        .grapheme_indices(true)
        .map(|(start, _)| line.start + start)
        .take_while(|&start| start <= requested)
        .last()
        .unwrap_or(line.start)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nanobug_plugin_sdk::Output;

    fn completions(doc: &Document, values: &[&str]) -> CallResult {
        CallResult {
            generation: 1,
            version: Some((doc.opening, doc.revision)),
            output: Output {
                completions: values.iter().map(|value| (*value).into()).collect(),
                ..Output::default()
            },
        }
    }

    #[test]
    fn fast_completion_is_visible_while_other_providers_are_pending() {
        let mut doc = Document::new("你好👩‍💻");
        let mut results = LanguageResults {
            scope: 1,
            opening: doc.opening,
            revision: doc.revision,
            path: doc.path.clone(),
            cursor: doc.cursor,
            request: 1,
            kind: Provider::Complete,
            pending: 4,
            error: None,
            completions: BTreeMap::new(),
            diagnostics: Vec::new(),
            definitions: Vec::new(),
        };
        results.receive(
            "First".into(),
            Some(&doc),
            Some(completions(&doc, &["文字\r\n", "文字\n"])),
            0,
        );
        assert_eq!(results.pending, 3);
        assert_eq!(results.completions.values().next().unwrap(), &["First"]);
        results.receive("Failed".into(), Some(&doc), None, 0);
        let selection = results.receive(
            "Second".into(),
            Some(&doc),
            Some(completions(&doc, &["Earlier in sort order", "文字\n"])),
            0,
        );
        assert_eq!(
            results.completions.values().nth(selection).unwrap(),
            &["First", "Second"]
        );
        assert_eq!(
            results.completions.keys().nth(selection).unwrap().label,
            "文字\n"
        );

        let late = completions(&doc, &["old reply"]);
        doc.replace(0..0, "New ", true);
        results.receive("Late".into(), Some(&doc), Some(late), selection);
        assert_eq!(results.pending, 0);
        assert!(
            !results
                .completions
                .keys()
                .any(|item| item.label == "old reply")
        );
        assert!(!results.matches(&doc));
    }

    #[test]
    fn diagnostics_move_the_caret_to_whole_combined_characters() {
        let doc = Document::new("前e\u{301}👩‍💻\n\t末");
        assert_eq!(cursor_boundary(&doc, "前e".len()), "前".len());
        assert_eq!(
            cursor_boundary(&doc, "前e\u{301}👩".len()),
            "前e\u{301}".len()
        );
        assert_eq!(cursor_boundary(&doc, doc.text.len()), doc.text.len());
    }

    #[test]
    fn definition_columns_count_utf16_and_land_at_grapheme_boundaries() {
        let doc = Document::new("before\n前👩‍💻e\u{301}x");
        let mut item = Definition {
            path: "file.rs".into(),
            line: 1,
            character: 1,
        };
        assert_eq!(definition_offset(&doc, &item), Some("before\n前".len()));
        item.character = 2;
        assert_eq!(definition_offset(&doc, &item), None);
        item.character = 7;
        assert_eq!(definition_offset(&doc, &item), Some("before\n前👩‍💻".len()));
        item.character = 8;
        assert_eq!(
            definition_offset(&doc, &item),
            Some("before\n前👩‍💻e\u{301}".len())
        );
        item.line = 100;
        assert_eq!(definition_offset(&doc, &item), None);
    }

    #[test]
    fn diagnostics_keep_sources_separate_as_providers_finish() {
        let doc = Document::new("前后");
        let mut results = LanguageResults {
            scope: 1,
            opening: doc.opening,
            revision: doc.revision,
            path: doc.path.clone(),
            cursor: doc.cursor,
            request: 1,
            kind: Provider::Diagnose,
            pending: 3,
            error: None,
            completions: BTreeMap::new(),
            diagnostics: Vec::new(),
            definitions: Vec::new(),
        };
        let warning = Diagnostic {
            start: 0,
            end: "前".len(),
            message: "Inspect this".into(),
        };
        let response = || CallResult {
            generation: 1,
            version: Some((doc.opening, doc.revision)),
            output: Output {
                diagnostics: vec![
                    warning.clone(),
                    warning.clone(),
                    Diagnostic {
                        start: 1,
                        ..warning.clone()
                    },
                ],
                ..Output::default()
            },
        };
        results.receive(
            "Check (fast) · main".into(),
            Some(&doc),
            Some(response()),
            0,
        );
        assert_eq!(results.diagnostics.len(), 1);
        assert_eq!(results.pending, 2);
        results.receive("Failed".into(), Some(&doc), None, 0);
        results.receive(
            "Check (slow) · main".into(),
            Some(&doc),
            Some(response()),
            0,
        );
        assert_eq!(results.pending, 0);
        assert_eq!(results.diagnostics.len(), 2);
        assert_ne!(results.diagnostics[0].0, results.diagnostics[1].0);
    }
}
