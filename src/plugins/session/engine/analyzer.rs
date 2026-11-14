//! Bind native language-server sessions to authorized plugin modules and editor revisions.

use super::{Answer, CallResult, Engine, Key, ResultEvent};
use crate::plugins::{
    package::Provider,
    query,
    session::{DocumentDiagnostics, State},
    tools::rust_analyzer::{self, Analyzer, Event, Request},
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

/// One running server and its versioned documents for a live Rust module.
pub(super) struct Binding {
    root: PathBuf,
    /// Identifies this process even if the Rust module reuses its WASM worker.
    server_id: u64,
    generation: u64,
    server: Option<Analyzer>,
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
    /// Drop servers that lost authorization and sync newly opened or edited Rust documents.
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
        let mut errors = BTreeMap::new();
        for key in keys {
            let project = self.projects[&key.instance.project].clone();
            let generation = self.lifecycle.runtimes[&key.namespace].generation;
            let documents: Vec<_> = project
                .document
                .iter()
                .chain(&project.open_documents)
                .filter(|document| {
                    document.path.as_ref().is_some_and(|path| {
                        path.starts_with(&project.root)
                            && self.lifecycle.wanted[&key].accepts_document(Some(path))
                    })
                })
                .cloned()
                .collect();
            if documents.is_empty() && !old.contains_key(&key) {
                continue;
            }
            let mut binding = old
                .remove(&key)
                .filter(|binding| binding.root == project.root && binding.generation == generation)
                .unwrap_or_else(|| {
                    let server_id = self.next();
                    let finish = self.finish.clone();
                    let owner = key.clone();
                    let root = project.root.clone();
                    let closed =
                        Arc::clone(self.project_flags.entry(key.instance.project).or_default());
                    let result = Analyzer::start(root.clone(), closed, move |event| {
                        let _ = finish.send(ResultEvent::Analyzer(owner.clone(), server_id, event));
                    });
                    let (server, error) = match result {
                        Ok(server) => (Some(server), None),
                        Err(error) => (None, Some(error.to_string())),
                    };
                    Binding {
                        root,
                        server_id,
                        generation,
                        server,
                        documents: Vec::new(),
                        diagnostics: BTreeMap::new(),
                        calls: BTreeMap::new(),
                        error,
                    }
                });
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
                        .try_send(Err("rust-analyzer request timed out".into()));
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
        if self.view.analyzer_errors != errors {
            self.view.analyzer_errors = errors;
            self.changed = true;
        }
    }

    /// Keep native analysis within the module's grants and the project's tool execution policy.
    fn analyzer_allowed(&self, key: &Key) -> bool {
        self.lifecycle
            .wanted
            .get(key)
            .is_some_and(|module| module.rust_analyzer)
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
            _ => return false,
        };
        let Some(reply) = reply else { return false };
        if !self
            .lifecycle
            .wanted
            .get(key)
            .is_some_and(|module| module.rust_analyzer && module.provides.contains(&provider))
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
            return fail(
                "rust-analyzer needs project execution permission. Open the plugin's Modules page."
                    .into(),
            );
        }
        self.sync_analyzers();
        let Some(document) = self.active_document(key).cloned() else {
            return fail("Open a Rust source file to use rust-analyzer.".into());
        };
        let id = self.next();
        let Some(binding) = self.analyzers.get_mut(key) else {
            return fail("rust-analyzer is unavailable for this document.".into());
        };
        let Some(server) = &binding.server else {
            return fail(
                binding
                    .error
                    .clone()
                    .unwrap_or_else(|| "rust-analyzer has stopped.".into()),
            );
        };
        let Some(path) = document.path.as_ref().filter(|_| {
            document.text.is_char_boundary(offset)
                && binding
                    .documents
                    .iter()
                    .any(|open| same_snapshot(open, &document))
        }) else {
            return fail("rust-analyzer needs an open Rust file in this project.".into());
        };
        let deadline = Instant::now() + timeout;
        if let Err(error) = server.request(
            id,
            request,
            path.clone(),
            offset,
            version(&document),
            deadline,
        ) {
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
            Event::Response { id, result } => {
                let Some(call) = binding.calls.remove(&id) else {
                    return;
                };
                let response = if Instant::now() >= call.deadline
                    || !current.as_ref().is_some_and(|doc| {
                        version(doc) == version(&call.document) && doc.path == call.document.path
                    }) {
                    Err("Document changed or rust-analyzer request expired".into())
                } else {
                    result.map(|value| CallResult {
                        generation: binding.generation,
                        version: Some(version(&call.document)),
                        output: match call.request {
                            Request::Completion => Output {
                                completion_items: completions(
                                    &call.document.text,
                                    call.offset,
                                    &value,
                                ),
                                ..Output::default()
                            },
                            Request::Definition => Output {
                                definitions: definitions(&value),
                                ..Output::default()
                            },
                        },
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
                .try_send(Err("rust-analyzer session closed".into()));
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

/// Keep only completion candidates the editor can apply as one plain-text edit.
fn completions(text: &str, offset: usize, value: &Value) -> Vec<Completion> {
    let items = value.as_array().or_else(|| value["items"].as_array());
    items
        .into_iter()
        .flatten()
        .take(512)
        .filter_map(|item| {
            if item["insertTextFormat"].as_u64() == Some(2)
                || item["additionalTextEdits"]
                    .as_array()
                    .is_some_and(|edits| !edits.is_empty())
            {
                return None;
            }
            let label = item["label"].as_str()?;
            let (start, end, replacement) = if let Some(edit) = item.get("textEdit") {
                let selection = edit.get("range").or_else(|| edit.get("replace"))?;
                let (start, end) = range(text, selection)?;
                (start, end, edit["newText"].as_str()?)
            } else {
                let ident = |c: char| c.is_alphanumeric() || c == '_';
                let before = text.get(..offset)?;
                let after = text.get(offset..)?;
                let start = before.trim_end_matches(ident).len();
                let end = text.len() - after.trim_start_matches(ident).len();
                (start, end, item["insertText"].as_str().unwrap_or(label))
            };
            (start <= offset && offset <= end && label.len() <= 2048 && replacement.len() <= 2048)
                .then(|| Completion {
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
