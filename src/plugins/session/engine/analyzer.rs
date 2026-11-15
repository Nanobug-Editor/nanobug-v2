//! Bind native language-server sessions to authorized plugin modules and editor revisions.

mod hover;

use super::{Answer, CallResult, Engine, Key, ResultEvent};
use crate::plugins::{
    package::Provider,
    query,
    session::{DocumentDiagnostics, LanguageServerPhase, LanguageServerStatus, State},
    tools::rust_analyzer::{self, Analyzer, Event, ProjectSettings, Request, Server},
};
use nanobug_plugin_sdk::{
    Diagnostic, DocumentSnapshot, Edit, Output, Permission, PluginEvent,
    language::{Completion, Definition},
};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

/// One running server and its versioned documents for a live language module.
pub(super) struct Binding {
    root: PathBuf,
    /// Adapter selected by the current package manifest.
    kind: Server,
    /// Automatic exclusions used to initialize this server generation.
    automatic_exclusions: Arc<[String]>,
    /// Identifies this process even if the Rust module reuses its WASM worker.
    server_id: u64,
    generation: u64,
    server: Option<Analyzer>,
    /// Last phase of this process generation.
    phase: LanguageServerPhase,
    documents: Vec<Arc<DocumentSnapshot>>,
    diagnostics: BTreeMap<u64, Arc<DocumentDiagnostics>>,
    calls: BTreeMap<u64, Call>,
    error: Option<String>,
}

/// A user query awaiting a reply for exactly the supplied snapshot.
struct Call {
    request: Request,
    document: Arc<DocumentSnapshot>,
    offset: usize,
    answer: Answer,
    deadline: Instant,
}

impl Engine {
    /// Drop servers that lost authorization and sync newly opened or edited source documents.
    pub(super) fn sync_analyzers(&mut self) {
        let keys: Vec<_> = self
            .lifecycle
            .wanted
            .keys()
            .filter(|key| self.analyzer_allowed(key))
            .cloned()
            .collect();
        let mut old = std::mem::take(&mut self.analyzers);
        let mut visible = BTreeMap::new();
        let mut statuses = BTreeMap::<u64, LanguageServerStatus>::new();
        let mut errors = BTreeMap::new();
        for key in keys {
            let project = self.projects[&key.instance.project].clone();
            let server_kind = self.analyzer_kind(&key).expect("selected language server");
            let generation = self.lifecycle.runtimes[&key.namespace].generation;
            let starts_early = self.rules.start_language_server(&key);
            // A different open SQL buffer may select a different dialect than the active one.
            let overlays = (!server_kind.is_sql()).then_some(&project.open_documents);
            let documents: Vec<_> = project
                .document
                .iter()
                .chain(overlays.into_iter().flatten())
                .filter(|document| {
                    document.path.as_ref().is_some_and(|path| {
                        path.starts_with(&project.root)
                            && (self.lifecycle.wanted[&key].accepts_document(Some(path))
                                || server_kind.accepts_project_overlay(path))
                    })
                })
                .cloned()
                .collect();
            if !starts_early && documents.is_empty() && !old.contains_key(&key) {
                continue;
            }
            let automatic_exclusions = self.rules.language_server_exclude_dirs(&key);
            let retained = old.remove(&key).filter(|binding| {
                binding.root == project.root
                    && binding.generation == generation
                    && binding.kind == server_kind
                    && binding.automatic_exclusions.as_ref() == automatic_exclusions
            });
            let mut binding = match retained {
                Some(binding) => binding,
                None => {
                    let automatic_exclusions = Arc::from(automatic_exclusions);
                    let settings = ProjectSettings {
                        plugin_id: self.records[&key.namespace].release.manifest.id.clone(),
                        automatic_exclusions: Arc::clone(&automatic_exclusions),
                    };
                    let server_id = self.next();
                    let finish = self.finish.clone();
                    let owner = key.clone();
                    let root = project.root.clone();
                    let closed =
                        Arc::clone(self.project_flags.entry(key.instance.project).or_default());
                    let result = Analyzer::start(
                        server_kind,
                        root.clone(),
                        Some(settings),
                        closed,
                        move |event| {
                            let _ =
                                finish.send(ResultEvent::Analyzer(owner.clone(), server_id, event));
                        },
                    );
                    let (server, phase, error) = match result {
                        Ok(server) => (Some(server), LanguageServerPhase::Starting, None),
                        Err(error) => (None, LanguageServerPhase::Failed, Some(error.to_string())),
                    };
                    Binding {
                        root,
                        kind: server_kind,
                        automatic_exclusions,
                        server_id,
                        generation,
                        server,
                        phase,
                        documents: Vec::new(),
                        diagnostics: BTreeMap::new(),
                        calls: BTreeMap::new(),
                        error,
                    }
                }
            };
            if !same_documents(&binding.documents, &documents) {
                if let Some(server) = &binding.server {
                    server.sync(documents.clone());
                }
                binding.diagnostics.retain(|opening, entry| {
                    documents.iter().any(|doc| {
                        doc.opening == *opening
                            && version(doc) == entry.version
                            && doc.path.as_ref() == Some(&entry.path)
                    })
                });
                binding.documents = documents;
            }
            binding.calls.retain(|_, call| {
                if call.answer.is_closed() {
                    return false;
                }
                if Instant::now() >= call.deadline {
                    let _ = call
                        .answer
                        .try_send(Err("Language server request timed out".into()));
                    return false;
                }
                true
            });
            if let Some(diagnostic) = project.document.as_ref().and_then(|document| {
                binding.diagnostics.get(&document.opening).filter(|entry| {
                    entry.version == version(document)
                        && document.path.as_ref() == Some(&entry.path)
                })
            }) {
                visible.insert(key.clone(), Arc::clone(diagnostic));
            }
            if let Some(error) = &binding.error {
                errors.insert(key.clone(), error.clone());
            }
            let current_document = project.document.as_ref().and_then(|document| {
                let path = document.path.as_ref()?;
                (path.starts_with(&project.root)
                    && self.lifecycle.wanted[&key].accepts_document(Some(path)))
                .then_some((document.opening, path))
            });
            // A server for the selected document takes priority over a project startup.
            if (starts_early || current_document.is_some())
                && statuses
                    .get(&project.id)
                    .is_none_or(|status| status.document.is_none() && current_document.is_some())
            {
                statuses.insert(
                    project.id,
                    LanguageServerStatus {
                        document: current_document.map(|(opening, path)| (opening, path.clone())),
                        name: binding.kind.name(),
                        phase: binding.phase,
                    },
                );
            }
            self.analyzers.insert(key, binding);
        }
        self.changed |= self.view.document_diagnostics.len() != visible.len()
            || visible.iter().any(|(key, value)| {
                self.view
                    .document_diagnostics
                    .get(key)
                    .is_none_or(|previous| !Arc::ptr_eq(previous, value))
            });
        self.view.document_diagnostics = visible;
        if self.view.language_servers != statuses {
            self.view.language_servers = statuses;
            self.changed = true;
        }
        if self.view.analyzer_errors != errors {
            self.view.analyzer_errors = errors;
            self.changed = true;
        }
    }

    /// Keep native analysis within the module's grants and the project's tool execution policy.
    fn analyzer_allowed(&self, key: &Key) -> bool {
        self.analyzer_kind(key).is_some()
            && self.view.modules.get(key) == Some(&State::Ready)
            && !self.installation.busy(&key.namespace)
            && !self.is_draining(key.instance.project)
            && self.records.get(&key.namespace).is_some_and(|record| {
                record.enabled
                    && record.grants.contains(&Permission::ReadDocument)
                    && record.grants.contains(&Permission::RunTool)
            })
            && self
                .projects
                .get(&key.instance.project)
                .is_some_and(|project| {
                    self.project_execution
                        .get(&query::project_key(&project.root))
                        == Some(&true)
                })
    }

    /// Resolve the server declared by the current module, including the legacy Rust flag.
    fn analyzer_kind(&self, key: &Key) -> Option<Server> {
        let module = self.lifecycle.wanted.get(key)?;
        module
            .language_server
            .or(module.rust_analyzer.then_some(Server::RustAnalyzer))
    }

    /// Handle declared language queries in the host, leaving unrelated plugin events on WASM.
    pub(super) fn invoke_analyzer(
        &mut self,
        key: &Key,
        event: &PluginEvent,
        reply: Option<&Answer>,
        timeout: Duration,
    ) -> bool {
        let (provider, request, offset) = match *event {
            PluginEvent::Complete { offset } => (Provider::Complete, Request::Completion, offset),
            PluginEvent::Definition { offset } => {
                (Provider::Definition, Request::Definition, offset)
            }
            PluginEvent::Hover { offset } => (Provider::Hover, Request::Hover, offset),
            _ => return false,
        };
        let Some(reply) = reply else { return false };
        let Some(server) = self.analyzer_kind(key) else {
            return false;
        };
        if !self
            .lifecycle
            .wanted
            .get(key)
            .is_some_and(|module| module.provides.contains(&provider))
        {
            return false;
        }
        let fail = |error| {
            if provider == Provider::Complete {
                return false;
            }
            let _ = reply.try_send(Err(error));
            true
        };
        if !self.analyzer_allowed(key) {
            return fail(format!(
                "{} needs project execution permission. Open the plugin's Modules page.",
                server.name()
            ));
        }
        self.sync_analyzers();
        let Some(document) = self.active_document(key).cloned() else {
            return fail(format!(
                "Open a supported source file to use {}.",
                server.name()
            ));
        };
        let id = self.next();
        let Some(binding) = self.analyzers.get_mut(key) else {
            return fail(format!(
                "{} is unavailable for this document.",
                server.name()
            ));
        };
        let Some(server) = &binding.server else {
            return fail(
                binding
                    .error
                    .clone()
                    .unwrap_or_else(|| "Language server has stopped.".into()),
            );
        };
        let Some(path) = document
            .path
            .as_ref()
            .filter(|_| document.text.is_char_boundary(offset))
        else {
            return fail("The language server needs an open file in this project.".into());
        };
        let deadline = Instant::now() + timeout;
        let queued = if binding
            .documents
            .iter()
            .any(|open| same_snapshot(open, &document))
        {
            server.request(
                id,
                request,
                path.clone(),
                offset,
                version(&document),
                deadline,
            )
        } else if binding.kind == Server::RustAnalyzer
            && !path.starts_with(&binding.root)
            && binding.kind.language_id(path).is_some()
            && matches!(request, Request::Definition | Request::Hover)
        {
            // rust-analyzer can navigate indexed dependencies without receiving an editor overlay.
            server.request_indexed(id, request, document.clone(), offset, deadline)
        } else {
            return fail("The language server needs an open file in this project.".into());
        };
        if let Err(error) = queued {
            return fail(error);
        }
        binding.calls.insert(
            id,
            Call {
                request,
                document,
                offset,
                answer: reply.clone(),
                deadline,
            },
        );
        true
    }

    /// Reject events from replaced servers and translate LSP ranges against the originating text.
    pub(super) fn analyzer_event(&mut self, key: &Key, server_id: u64, event: Event) {
        if !self.analyzer_allowed(key) {
            return;
        }
        let current = self.active_document(key).cloned();
        let Some(binding) = self
            .analyzers
            .get_mut(key)
            .filter(|binding| binding.server_id == server_id)
        else {
            return;
        };
        match event {
            Event::Phase(phase) => binding.phase = phase,
            Event::Response { id, result } => {
                let Some(call) = binding.calls.remove(&id) else {
                    return;
                };
                let now = Instant::now();
                let current_matches = current.as_ref().is_some_and(|doc| {
                    version(doc) == version(&call.document) && doc.path == call.document.path
                });
                let response = if now >= call.deadline || !current_matches {
                    Err("Document changed or language server request expired".into())
                } else {
                    let output = match call.request {
                        Request::Completion => {
                            let items = result
                                .ok()
                                .map(|value| completions(&call.document.text, call.offset, &value))
                                .unwrap_or_default();
                            if items.is_empty() {
                                // Diagnostic-only or failing servers leave completion to WASM.
                                self.invoke(
                                    key.clone(),
                                    PluginEvent::Complete {
                                        offset: call.offset,
                                    },
                                    Some(call.answer),
                                    call.deadline.saturating_duration_since(now),
                                );
                                return;
                            }
                            Ok(Output {
                                completion_items: items,
                                ..Output::default()
                            })
                        }
                        Request::Definition => result.map(|value| Output {
                            definitions: definitions(&value),
                            ..Output::default()
                        }),
                        Request::Hover => result.map(|value| Output {
                            hover: hover::details(&value),
                            ..Output::default()
                        }),
                    };
                    output.map(|output| CallResult {
                        generation: binding.generation,
                        version: Some(version(&call.document)),
                        output,
                    })
                };
                let _ = call.answer.try_send(response);
            }
            Event::Diagnostics { snapshot, items } => {
                if self.lifecycle.wanted[key]
                    .provides
                    .contains(&Provider::Diagnose)
                    && binding
                        .documents
                        .iter()
                        .any(|doc| same_snapshot(doc, &snapshot))
                    && let Some(path) = &snapshot.path
                {
                    binding.diagnostics.insert(
                        snapshot.opening,
                        Arc::new(DocumentDiagnostics {
                            version: version(&snapshot),
                            path: path.clone(),
                            diagnostics: diagnostics(&snapshot.text, &items),
                        }),
                    );
                }
            }
            Event::Failed(error) => {
                binding.server.take();
                binding.phase = LanguageServerPhase::Failed;
                binding.diagnostics.clear();
                for (_, call) in std::mem::take(&mut binding.calls) {
                    let _ = call.answer.try_send(Err(error.clone()));
                }
                binding.error = Some(error.clone());
                self.log(&key.namespace, &error);
            }
        }
    }
}

impl Drop for Binding {
    /// Release queries promptly when the module stops owning its language server.
    fn drop(&mut self) {
        for (_, call) in std::mem::take(&mut self.calls) {
            let _ = call
                .answer
                .try_send(Err("Language server session closed".into()));
        }
    }
}

fn version(doc: &DocumentSnapshot) -> (u64, u64) {
    (doc.opening, doc.revision)
}

fn same_documents(a: &[Arc<DocumentSnapshot>], b: &[Arc<DocumentSnapshot>]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same_snapshot(a, b))
}

/// Foreground and background snapshots may have different allocations for the same buffer revision.
fn same_snapshot(a: &DocumentSnapshot, b: &DocumentSnapshot) -> bool {
    version(a) == version(b) && a.path == b.path
}

/// Read an LSP range in UTF-16 units and return validated UTF-8 byte offsets.
fn range(text: &str, value: &Value) -> Option<(usize, usize)> {
    let start = rust_analyzer::offset_at(text, value.get("start")?)?;
    let end = rust_analyzer::offset_at(text, value.get("end")?)?;
    (start <= end).then_some((start, end))
}

/// Rank the server's suggestions at `offset` in `text`, then keep applicable plain-text edits.
fn completions(text: &str, offset: usize, value: &Value) -> Vec<Completion> {
    let (Some(before), Some(after), Some(items)) = (
        text.get(..offset),
        text.get(offset..),
        value.as_array().or_else(|| value["items"].as_array()),
    ) else {
        return Vec::new();
    };
    let ident = crate::plugins::completion::identifier_char;
    let word_start = before.trim_end_matches(ident).len();
    let word_end = text.len() - after.trim_start_matches(ident).len();
    let prefix = &before[word_start..];

    // A server can send a complete list in arbitrary order. Apply the display
    // limits after ranking so later prefix matches still reach the editor.
    let mut ranked: Vec<_> = items
        .iter()
        .filter_map(|item| {
            let label = item["label"].as_str().filter(|label| label.len() <= 2048)?;
            if item["insertTextFormat"].as_u64() == Some(2)
                || item["additionalTextEdits"]
                    .as_array()
                    .is_some_and(|edits| !edits.is_empty())
            {
                return None;
            }
            let filter = item["filterText"]
                .as_str()
                .filter(|filter| !filter.is_empty() && filter.len() <= 2048)
                .unwrap_or(label);
            let order = item["sortText"].as_str().unwrap_or(label);
            Some((
                crate::plugins::completion::rank(prefix, filter)?,
                order,
                label,
                item,
            ))
        })
        .collect();
    ranked.sort_by_key(|(rank, order, ..)| (*rank, *order));

    ranked
        .into_iter()
        .take(512)
        .filter_map(|(_, _, label, item)| {
            let (start, end, replacement) = if let Some(edit) = item.get("textEdit") {
                let selection = edit.get("range").or_else(|| edit.get("replace"))?;
                let (start, end) = range(text, selection)?;
                (start, end, edit["newText"].as_str()?)
            } else {
                (
                    word_start,
                    word_end,
                    item["insertText"].as_str().unwrap_or(label),
                )
            };
            (start <= offset && offset <= end && replacement.len() <= 2048).then(|| Completion {
                label: label.into(),
                edit: Edit {
                    start,
                    end,
                    text: replacement.into(),
                },
            })
        })
        .take(80)
        .collect()
}

/// Normalize both LSP Location and LocationLink to local file positions.
fn definitions(result: &Value) -> Vec<Definition> {
    let items = result
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_else(|| std::slice::from_ref(result));
    let mut definitions = Vec::new();
    for item in items.iter().take(80) {
        let location = || {
            let uri = item
                .get("targetUri")
                .or_else(|| item.get("uri"))?
                .as_str()?;
            let path = rust_analyzer::uri_path(uri)?;
            let position = item
                .get("targetSelectionRange")
                .or_else(|| item.get("range"))?
                .get("start")?;
            Some(Definition {
                path,
                line: u32::try_from(position["line"].as_u64()?).ok()?,
                character: u32::try_from(position["character"].as_u64()?).ok()?,
            })
        };
        if let Some(definition) =
            location().filter(|def| Path::is_absolute(&def.path) && !definitions.contains(def))
        {
            definitions.push(definition);
        }
    }
    definitions
}

/// Keep bounded warnings and errors with text ranges in the current snapshot.
fn diagnostics(text: &str, items: &[Value]) -> Vec<Diagnostic> {
    items
        .iter()
        .take(512)
        .filter_map(|item| {
            if item["severity"].as_u64().is_some_and(|level| level > 2) {
                return None;
            }
            let (start, end) = range(text, &item["range"])?;
            let message = item["message"]
                .as_str()?
                .chars()
                .filter(|ch| !ch.is_control() || *ch == '\n')
                .take(500)
                .collect::<String>();
            (!message.is_empty()).then_some(Diagnostic {
                start,
                end,
                message,
            })
        })
        .take(200)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A server may send unfiltered symbols before the visible prefix match.
    #[test]
    fn prefix_matches_survive_completion_limits_and_replace_the_entire_token() {
        let text = "fn example() -> Result<String, ()>";
        let start = text.find("Result").unwrap();
        let end = start + "Result".len();
        let cursor = start + "Res".len();
        let mut items: Vec<_> = (0..530)
            .map(|n| json!({"label": format!("OtherType{n}")}))
            .collect();
        items.push(json!({
            "label": "unrelated display label",
            "filterText": "ResultAlias",
            "sortText": "1",
        }));
        items.push(json!({
            "label": "Result",
            "filterText": "Result",
            "sortText": "0",
            "textEdit": {
                "newText": "Result",
                "insert": {
                    "start": {"line": 0, "character": start},
                    "end": {"line": 0, "character": cursor},
                },
                "replace": {
                    "start": {"line": 0, "character": start},
                    "end": {"line": 0, "character": end},
                },
            },
        }));
        items.push(json!({"label": "Request", "sortText": "2"}));
        items.push(json!({"label": "REST", "sortText": "0"}));
        items.push(json!({"label": "Register", "sortText": "0"}));

        let displayed = completions(text, cursor, &json!({"items": items}));
        let labels: Vec<_> = displayed.iter().map(|item| item.label.as_str()).collect();
        assert!(labels.starts_with(&["Result", "unrelated display label", "REST", "Register"]));
        assert_eq!(
            displayed[0].edit,
            Edit {
                start,
                end,
                text: "Result".into()
            }
        );
    }
}
