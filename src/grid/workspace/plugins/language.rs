//! Show valid language results as each active provider replies.

use super::{
    Workspace,
    rows::{Row, Target, row},
};
use crate::grid::{windows::editor::EditResult, workspace::layout::Focus};
use gpui::{Context, KeyDownEvent, Task, Window};
use nanobug::{
    document::Document,
    i18n,
    plugins::{
        completion,
        package::Provider,
        session::{Action, CallResult, Key, State},
    },
    vim,
};
use nanobug_plugin_sdk::{
    Diagnostic, Edit, Instance, Permission, PluginEvent,
    language::{Completion, Definition},
};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use unicode_segmentation::UnicodeSegmentation;

/// Wait for a brief pause in typing before asking completion providers.
const COMPLETION_DELAY: Duration = Duration::from_millis(120);

/// Candidates in provider order, each with the sources that supplied the same edit.
type Candidates = Vec<(Completion, Vec<String>)>;

/// A nonmodal completion request whose edits and selection belong to the active caret.
pub(super) struct CompletionPopup {
    /// Validated replies for the exact document revision and insertion point.
    results: LanguageResults,
    /// Matching candidates rebased to the latest edit while its query is pending.
    retained: Candidates,
    /// The selected candidate's index in the displayed order.
    selection: usize,
    /// Start of the typed prefix for a native word, line or path completion.
    prefix_start: Option<usize>,
    /// Dropping the popup closes the reply receivers for superseded requests.
    replies: Vec<Task<()>>,
}

impl CompletionPopup {
    /// Use current replies as soon as they contain a candidate.
    fn candidates(&self) -> &Candidates {
        if self.results.completions.is_empty() {
            &self.retained
        } else {
            &self.results.completions
        }
    }

    /// Keep compatible candidates for a single Insert edit at the previous caret.
    /// Shift only replacement ends that span that edit and validate them against `doc`.
    fn rebase_after_caret_edit(
        &mut self,
        scope: u64,
        doc: &Document,
        (previous_revision, previous_cursor): (u64, usize),
    ) -> bool {
        let results = &self.results;
        if scope != results.scope
            || doc.opening != results.opening
            || doc.path != results.path
            || previous_revision != results.revision
            || previous_cursor != results.cursor
            || previous_revision.checked_add(1) != Some(doc.revision)
        {
            return false;
        }
        let start = doc.text[..doc.cursor]
            .char_indices()
            .rev()
            .take_while(|(_, ch)| completion::identifier_char(*ch))
            .last()
            .map_or(doc.cursor, |(start, _)| start);
        if start >= previous_cursor.min(doc.cursor) {
            return false;
        }

        let prefix = &doc.text[start..doc.cursor];
        let mut candidates = if self.results.completions.is_empty() {
            std::mem::take(&mut self.retained)
        } else {
            std::mem::take(&mut self.results.completions)
        };
        let selected = candidates
            .get(self.selection)
            .map(|(item, _)| item.label.clone());
        candidates.retain_mut(|(item, _)| {
            let edit = &mut item.edit;
            if edit.start != start || edit.end < previous_cursor {
                return false;
            }
            edit.end = edit.end - previous_cursor + doc.cursor;
            doc.text.is_char_boundary(edit.end)
                && (completion::rank(prefix, &edit.text).is_some()
                    || completion::rank(prefix, &item.label).is_some())
        });
        if candidates.is_empty() {
            return false;
        }
        self.selection = selected
            .and_then(|label| candidates.iter().position(|(item, _)| item.label == label))
            .unwrap_or(0);
        self.retained = candidates;
        self.results.revision = doc.revision;
        self.results.cursor = doc.cursor;
        self.results.request = 0;
        self.replies.clear();
        true
    }
}

/// Choose between the document results list and an editor-owned completion popup.
#[derive(Clone, Copy)]
enum LanguageDisplay {
    List,
    Completion { automatic: bool },
}

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
    completions: Candidates,
    diagnostics: Vec<(String, Diagnostic)>,
    /// Unique destinations in provider reply order.
    definitions: Vec<(String, Definition)>,
}

impl LanguageResults {
    /// Whether this list can be navigated with Insert-mode completion controls.
    pub(super) fn is_completion(&self) -> bool {
        self.kind == Provider::Complete
    }

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
            let prefix = completion_prefix(doc);
            let selected = self
                .completions
                .get(selection)
                .map(|(item, _)| item.clone());
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
                .filter(|item| {
                    item.label.len() <= 2048
                        && item.edit.text.len() <= 2048
                        && candidate_rank(prefix, item).is_some()
                })
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
                let space = self.completions.len() < 200;
                if let Some((_, sources)) = self
                    .completions
                    .iter_mut()
                    .find(|(prior, _)| *prior == item)
                {
                    if !sources.contains(&source) {
                        sources.push(source.clone());
                    }
                } else if space {
                    self.completions.push((item, vec![source.clone()]));
                }
            }
            self.completions
                .sort_by_cached_key(|(item, _)| candidate_rank(prefix, item));
            return selected
                .and_then(|selected| {
                    self.completions
                        .iter()
                        .position(|(item, _)| *item == selected)
                })
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
    /// Return the active identifier prefix used to highlight matching menu characters.
    pub(in crate::grid::workspace) fn completion_query(&self) -> &str {
        self.completion_target().map_or("", |(_, doc)| {
            self.plugins
                .completion
                .as_ref()
                .filter(|popup| popup.results.matches(doc))
                .and_then(|popup| popup.prefix_start)
                .and_then(|start| doc.text.get(start..doc.cursor))
                .unwrap_or_else(|| completion_prefix(doc))
        })
    }

    /// Return the document and project scope only while the main editor can show completion.
    fn completion_target(&self) -> Option<(u64, &Document)> {
        let project = self.projects.active_project()?;
        let editor = &project.editor;
        let vim = editor.vim();
        (!self.modal_open()
            && !self.plugins.owns_input()
            && project.focus == Focus::Editor
            && vim.mode.is_insert()
            && vim.accepts_text()
            && editor.composition().is_empty())
        .then_some((project.plugin_scope, &vim.doc))
    }

    /// Return the ordered candidate labels and selected row for the current editor snapshot.
    pub(in crate::grid::workspace) fn completion_items(&self) -> Option<(Vec<&str>, usize)> {
        let (scope, doc) = self.completion_target()?;
        let popup = self.plugins.completion.as_ref()?;
        (scope == popup.results.scope
            && popup.results.matches(doc)
            && !popup.candidates().is_empty())
        .then(|| {
            (
                popup
                    .candidates()
                    .iter()
                    .map(|(item, _)| item.label.as_str())
                    .collect(),
                popup.selection,
            )
        })
    }

    /// Cancel pending completion and remove visible candidates without changing editor mode.
    pub(in crate::grid::workspace) fn dismiss_completion(&mut self, cx: &mut Context<Self>) {
        self.plugins.completion_delay = None;
        if self.plugins.completion.take().is_some() {
            cx.notify();
        }
    }

    /// Display native Insert matches ordered in `values`, replacing text from `start` to the caret.
    /// `reverse` begins selection from the final item. `cx` invalidates the popup layer.
    pub(in crate::grid::workspace) fn show_insert_candidates(
        &mut self,
        start: usize,
        values: Vec<String>,
        reverse: bool,
        cx: &mut Context<Self>,
    ) {
        self.dismiss_completion(cx);
        let Some((scope, doc)) = self.completion_target() else {
            return;
        };
        if values.is_empty() || start > doc.cursor || !doc.text.is_char_boundary(start) {
            return;
        }
        let selection = if reverse { values.len() - 1 } else { 0 };
        let completions = values
            .into_iter()
            .map(|text| {
                (
                    Completion {
                        label: text.clone(),
                        edit: Edit {
                            start,
                            end: doc.cursor,
                            text,
                        },
                    },
                    Vec::new(),
                )
            })
            .collect();
        self.plugins.completion = Some(CompletionPopup {
            results: LanguageResults {
                scope,
                opening: doc.opening,
                revision: doc.revision,
                path: doc.path.clone(),
                cursor: doc.cursor,
                request: 0,
                kind: Provider::Complete,
                pending: 0,
                error: None,
                completions,
                diagnostics: Vec::new(),
                definitions: Vec::new(),
            },
            retained: Vec::new(),
            selection,
            prefix_start: Some(start),
            replies: Vec::new(),
        });
        cx.notify();
    }

    /// Debounce completion after an insertion or backspace at the prior revision and caret.
    pub(in crate::grid::workspace) fn refresh_automatic_completion(
        &mut self,
        previous: (u64, usize),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((_, doc)) = self.completion_target() else {
            self.dismiss_completion(cx);
            return;
        };
        let eligible = doc.text[..doc.cursor]
            .chars()
            .next_back()
            .is_some_and(|before| {
                completion::identifier_char(before) || ".:>(<@%/#$-'\"".contains(before)
            });
        if doc.path.is_none() || !eligible {
            self.dismiss_completion(cx);
            return;
        }
        // Only Insert's literal commit or plain Backspace changes a single range at the caret.
        let retain = self.projects.active().editor.vim().mode == vim::Mode::Insert;
        let mut popup = self.plugins.completion.take();
        let was_visible = popup
            .as_ref()
            .is_some_and(|popup| !popup.candidates().is_empty());
        let (scope, doc) = self.completion_target().expect("eligible editor");
        if let Some(current) = popup.as_mut() {
            if !retain
                || current.prefix_start.is_some()
                || !current.rebase_after_caret_edit(scope, doc, previous)
            {
                popup = None;
            }
        }
        let stamp = (
            scope,
            doc.opening,
            doc.revision,
            doc.cursor,
            doc.path.clone(),
        );
        let delay = cx.background_executor().timer(COMPLETION_DELAY);
        self.plugins.completion = popup;
        if was_visible {
            cx.notify();
        }
        self.plugins.completion_delay = Some(cx.spawn_in(window, async move |view, cx| {
            delay.await;
            let _ = cx.update(|window, cx| {
                view.update(cx, |workspace, cx| {
                    workspace.plugins.completion_delay = None;
                    if workspace.completion_target().is_some_and(|(scope, doc)| {
                        (scope, doc.opening, doc.revision, doc.cursor, &doc.path)
                            == (stamp.0, stamp.1, stamp.2, stamp.3, &stamp.4)
                    }) {
                        workspace.request_language(
                            Provider::Complete,
                            LanguageDisplay::Completion { automatic: true },
                            window,
                            cx,
                        );
                    }
                })
            });
        }));
    }

    /// Route candidate navigation and acceptance while ordinary typing stays in the editor.
    pub(in crate::grid::workspace) fn completion_keydown(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let key = &event.keystroke;
        let ctrl = key.modifiers.control
            && !key.modifiers.shift
            && !key.modifiers.alt
            && !key.modifiers.platform
            && !key.modifiers.function;
        if ctrl
            && matches!(key.key.as_str(), "e" | "z")
            && (self.plugins.completion.is_some() || self.plugins.completion_delay.is_some())
        {
            self.dismiss_completion(cx);
            return true;
        }
        let chord = (ctrl, key.modifiers.modified(), key.key.as_str());
        let movement = match chord {
            (true, _, "n") | (false, false, "down") => Some(1),
            (true, _, "p") | (false, false, "up") => Some(-1),
            _ => None,
        };
        let accept = matches!(chord, (true, _, "y") | (false, false, "enter" | "tab"));
        if (accept || movement.is_some())
            && let Some((count, selected)) = self
                .completion_menu(window)
                .map(|(_, items, selected)| (items.len(), selected))
                .or_else(|| {
                    // Explicit native completion chords work even when the popup cannot fit.
                    if !ctrl || self.plugins.completion.as_ref()?.prefix_start.is_none() {
                        return None;
                    }
                    self.completion_items()
                        .map(|(items, selected)| (items.len(), selected))
                })
        {
            if accept {
                if !event.is_held {
                    self.accept_completion_at(selected, window, cx);
                }
                return true;
            }
            if let Some(movement) = movement
                && let Some(popup) = &mut self.plugins.completion
            {
                popup.selection =
                    (selected as isize + movement).rem_euclid(count as isize) as usize;
                cx.notify();
            }
            return true;
        }
        // A native text commit or plain Backspace updates the list after the edit lands.
        if key.modifiers.control
            || key.modifiers.alt
            || key.modifiers.platform
            || key.modifiers.function
            || !(key
                .key_char
                .as_ref()
                .is_some_and(|text| !text.chars().any(char::is_control))
                || key.key.chars().count() == 1
                || matches!(key.key.as_str(), "space" | "backspace"))
        {
            self.dismiss_completion(cx);
        }
        false
    }

    /// Apply the candidate at `index` only while its snapshot and editor mode still match.
    pub(in crate::grid::workspace) fn accept_completion_at(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let edit = self.completion_target().and_then(|(scope, doc)| {
            let popup = self.plugins.completion.as_ref()?;
            (scope == popup.results.scope && popup.results.matches(doc))
                .then(|| popup.candidates().get(index))
                .flatten()
                .map(|(item, _)| item.edit.clone())
        });
        self.dismiss_completion(cx);
        if let Some(edit) = edit {
            self.run_vim(
                |vim, _| {
                    vim.apply_completion(edit.start..edit.end, &edit.text);
                    vim::Action::None
                },
                window,
                cx,
            );
            self.sync(window, cx);
        }
    }

    /// Request document results from the command or the Insert-mode completion shortcut.
    pub(in crate::grid::workspace) fn plugin_language(
        &mut self,
        kind: Provider,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dismiss_completion(cx);
        let display = if kind == Provider::Complete && self.completion_target().is_some() {
            LanguageDisplay::Completion { automatic: false }
        } else {
            LanguageDisplay::List
        };
        self.request_language(kind, display, window, cx);
    }

    /// Find ready plugins for `kind` on `path` in `scope`, retaining the runtime generation.
    pub(super) fn language_providers(
        &self,
        kind: Provider,
        scope: u64,
        path: Option<&Path>,
    ) -> Vec<(Key, String, u64)> {
        let mut providers = Vec::new();
        for record in self.plugins.view.installed.iter() {
            if !record.enabled || !record.grants.contains(&Permission::ReadDocument) {
                continue;
            }
            let manifest = &record.release.manifest;
            for module in &manifest.modules {
                if !module.provides.contains(&kind) || !module.accepts_document(path) {
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
        providers
    }

    /// Query ready providers and publish their replies to the requested editor surface.
    fn request_language(
        &mut self,
        kind: Provider,
        display: LanguageDisplay,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let automatic = matches!(display, LanguageDisplay::Completion { automatic: true });
        let definition = kind == Provider::Definition;
        if definition {
            self.plugins.pending_definition = None;
        }
        self.sync_plugin_projects();
        let Some(project) = self.projects.active_project() else {
            return;
        };
        let doc = &project.editor.vim().doc;
        if doc.text.len() > nanobug_plugin_sdk::document_wire::MAX_DOCUMENT_BYTES {
            if automatic {
                self.dismiss_completion(cx);
            } else {
                self.set_status(i18n::text("editor.plugin_document_too_large").into());
            }
            return;
        }
        let (scope, opening, revision, cursor) =
            (project.plugin_scope, doc.opening, doc.revision, doc.cursor);
        let mut providers = self.language_providers(kind, scope, doc.path.as_deref());
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
            if automatic {
                self.dismiss_completion(cx);
            } else {
                self.set_status("No active language provider".into());
            }
            return;
        }
        self.plugins.request_id += 1;
        let request = self.plugins.request_id;
        let popup = matches!(display, LanguageDisplay::Completion { .. });
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
            completions: Vec::new(),
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
        if popup {
            let mut prior = self.plugins.completion.take();
            if let Some(prior) = prior.as_mut() {
                prior.results = results;
            } else {
                prior = Some(CompletionPopup {
                    results,
                    retained: Vec::new(),
                    selection: 0,
                    prefix_start: None,
                    replies: Vec::new(),
                });
            }
            self.plugins.completion = prior;
        } else if definition {
            self.plugins.pending_definition = Some(results);
        } else {
            self.plugins.outline = None;
            self.plugins.language = Some(results);
            self.open_document_list();
        }
        for (key, name, generation) in providers {
            let (reply, receive) = async_channel::bounded(1);
            let event = match kind {
                Provider::Complete => PluginEvent::Complete { offset: cursor },
                Provider::Definition => PluginEvent::Definition { offset: cursor },
                _ => PluginEvent::Diagnose,
            };
            let invocation = Action::Invoke {
                key: key.clone(),
                event,
                reply: Some(reply),
                timeout: Duration::from_secs(if kind == Provider::Diagnose { 3 } else { 15 }),
            };
            if popup || definition {
                // Editor-owned queries leave plugin manager feedback alone.
                if let Some(session) = &self.plugins.session {
                    let _ = session.send(invocation);
                }
            } else if !self.plugins.send(invocation) {
                if let Some(results) = &mut self.plugins.language {
                    results.pending -= 1;
                    results
                        .error
                        .get_or_insert_with(|| "Plugin request queue is busy".into());
                }
                continue;
            }
            let reply_task = cx.spawn_in(window, async move |workspace, cx| {
                let result = receive
                    .recv()
                    .await
                    .unwrap_or_else(|_| Err("Plugin request stopped".into()));
                let _ = cx.update(|window, cx| {
                    workspace.update(cx, |workspace, cx| {
                        let popup_current = !popup || workspace.completion_target().is_some();
                        let definition_current = !definition
                            || !workspace.modal_open()
                                && !workspace.plugins.surface.focused
                                && workspace.projects.active_project().is_some_and(|project| {
                                    project.focus == Focus::Editor
                                        && project.editor.vim().mode == vim::Mode::Normal
                                });
                        let (results, selection, mut retained) = if popup {
                            let Some(popup) = &mut workspace.plugins.completion else {
                                return;
                            };
                            (
                                &mut popup.results,
                                &mut popup.selection,
                                Some(&mut popup.retained),
                            )
                        } else {
                            let results = if definition {
                                &mut workspace.plugins.pending_definition
                            } else {
                                &mut workspace.plugins.language
                            };
                            let Some(results) = results else {
                                return;
                            };
                            (results, &mut workspace.plugins.manager.selection, None)
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
                        let current = popup_current
                            && definition_current
                            && doc.is_some_and(|doc| results.matches(doc));
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
                        *selection = results.receive(name, doc, result, *selection);
                        if let Some(retained) = retained.as_mut()
                            && !retained.is_empty()
                            && !results.completions.is_empty()
                        {
                            let selected = retained.get(*selection);
                            *selection = selected
                                .and_then(|(selected, _)| {
                                    results
                                        .completions
                                        .iter()
                                        .position(|(item, _)| item.label == selected.label)
                                })
                                .unwrap_or(0);
                            retained.clear();
                        }
                        if popup {
                            if results.pending == 0
                                && results.completions.is_empty()
                                && retained.is_none_or(|prior| prior.is_empty())
                            {
                                let error = results.error.take();
                                workspace.dismiss_completion(cx);
                                if !automatic && current {
                                    workspace.set_status(error.unwrap_or_else(|| {
                                        i18n::text("editor.no_completions").into()
                                    }));
                                    workspace.sync(window, cx);
                                }
                            } else {
                                cx.notify();
                            }
                            return;
                        }
                        if definition {
                            if results.pending == 0 {
                                workspace.finish_definition(current, window, cx);
                            }
                            return;
                        }
                        workspace.plugin_refresh(window, cx);
                    })
                });
            });
            if let Some(popup) = self.plugins.completion.as_mut().filter(|_| popup) {
                popup.replies.push(reply_task);
            } else {
                reply_task.detach();
            }
        }
        if !popup && !definition {
            self.plugin_refresh(window, cx);
        } else if popup && !automatic {
            cx.notify();
        }
    }

    /// Present a completed definition query only while its originating editor is still active.
    fn finish_definition(&mut self, current: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(results) = self.plugins.pending_definition.take().filter(|_| current) else {
            return;
        };
        match results.definitions.as_slice() {
            [] => {
                let empty = results.empty_label();
                self.set_status(results.error.unwrap_or_else(|| empty.into()));
                self.sync(window, cx);
            }
            [(_, destination)] => {
                let destination = destination.clone();
                self.plugins.language = Some(results);
                self.use_definition(destination, window, cx);
            }
            _ => {
                self.plugins.outline = None;
                self.plugins.language = Some(results);
                self.open_document_list();
                self.plugin_refresh(window, cx);
            }
        }
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
                        vim.apply_completion(edit.start..edit.end, &edit.text);
                        if vim.doc.revision != revision {
                            changed_path = vim.doc.path.clone();
                        }
                    }
                    let doc = &vim.doc;
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
        if current
            && offered
            && self
                .jump_to_file(&definition.path, true, |doc| {
                    definition_offset(doc, &definition)
                })
                .is_ok_and(|visited| visited)
        {
            self.sync(window, cx);
            return;
        }
        self.set_status(i18n::text("editor.definition_unavailable").into());
        self.sync(window, cx);
    }
}

/// Extract the identifier immediately before the current caret.
fn completion_prefix(doc: &Document) -> &str {
    let before = &doc.text[..doc.cursor];
    &before[before.trim_end_matches(completion::identifier_char).len()..]
}

/// Score the display label and the replacement, which can have different provider formatting.
fn candidate_rank(query: &str, item: &Completion) -> Option<(u8, usize, usize)> {
    completion::rank(query, &item.label)
        .into_iter()
        .chain(completion::rank(query, &item.edit.text))
        .min()
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
    fn retained_completion_replaces_the_current_identifier_after_typing() {
        let mut doc = Document::new("Result<()>");
        doc.cursor = 2;
        let mut popup = CompletionPopup {
            results: LanguageResults {
                scope: 1,
                opening: doc.opening,
                revision: doc.revision,
                path: doc.path.clone(),
                cursor: doc.cursor,
                request: 1,
                kind: Provider::Complete,
                pending: 0,
                error: None,
                completions: ["Render", "Result"]
                    .into_iter()
                    .map(|label| {
                        (
                            Completion {
                                label: label.into(),
                                edit: Edit {
                                    start: 0,
                                    end: 6,
                                    text: label.into(),
                                },
                            },
                            vec!["Rust".into()],
                        )
                    })
                    .collect(),
                diagnostics: Vec::new(),
                definitions: Vec::new(),
            },
            retained: Vec::new(),
            selection: 1,
            prefix_start: None,
            replies: Vec::new(),
        };
        let previous = (doc.revision, doc.cursor);
        doc.replace(2..2, "s", true);
        assert!(popup.rebase_after_caret_edit(1, &doc, previous));
        assert!(popup.results.matches(&doc));
        assert_eq!(popup.candidates().len(), 1);
        let edit = &popup.candidates()[popup.selection].0.edit;
        doc.replace(edit.start..edit.end, &edit.text, true);
        assert_eq!(doc.text, "Result<()>");
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
            completions: Vec::new(),
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
        assert_eq!(results.completions[0].1, ["First"]);
        results.receive("Failed".into(), Some(&doc), None, 0);
        let selection = results.receive(
            "Second".into(),
            Some(&doc),
            Some(completions(&doc, &["Earlier in sort order", "文字\n"])),
            0,
        );
        assert_eq!(results.completions[selection].1, ["First", "Second"]);
        assert_eq!(results.completions[selection].0.label, "文字\n");

        let late = completions(&doc, &["old reply"]);
        doc.replace(0..0, "New ", true);
        results.receive("Late".into(), Some(&doc), Some(late), selection);
        assert_eq!(results.pending, 0);
        assert!(
            !results
                .completions
                .iter()
                .any(|(item, _)| item.label == "old reply")
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
            completions: Vec::new(),
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
