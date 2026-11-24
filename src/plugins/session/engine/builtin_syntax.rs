//! Schedule lexical Tree-sitter highlighting for visible editor documents.

use super::{Backoff, Engine, ResultEvent};
use crate::plugins::session::{DocumentSyntax, builtin_syntax_grammar};
use nanobug_plugin_sdk::{
    DocumentSnapshot,
    document_wire::MAX_DOCUMENT_BYTES,
    language::{DocumentFeatures, HIGHLIGHT_QUERY_PREFIX, SyntaxQuery},
};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Instant};

/// Path and grammar distinguish snapshots even if the text revision does not change.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Stamp {
    project: u64,
    version: (u64, u64),
    path: PathBuf,
    grammar: &'static str,
}

impl Stamp {
    fn key(&self) -> (u64, u64) {
        (self.project, self.version.0)
    }

    fn matches(&self, syntax: &DocumentSyntax) -> bool {
        self.version == syntax.version
            && self.path == syntax.path
            && syntax.grammar == Some(self.grammar)
    }
}

/// Track the latest visible snapshots while at most one lexical job uses the syntax worker.
#[derive(Default)]
pub(super) struct State {
    pub(super) visible: Vec<Arc<DocumentSnapshot>>,
    running: bool,
    retries: BTreeMap<(u64, u64), (Stamp, Backoff)>,
    /// Give another visible pane a turn if the last request was superseded while typing.
    last_started: Option<(u64, u64)>,
}

impl Engine {
    /// Select a bundled grammar for a named, bounded buffer in the active project.
    fn builtin_stamp(&self, project: u64, doc: &DocumentSnapshot) -> Option<Stamp> {
        if doc.text.len() > MAX_DOCUMENT_BYTES {
            return None;
        }
        let path = doc.path.as_ref()?;
        let grammar =
            builtin_syntax_grammar(Some(path), self.document_filetype(project, doc.opening))?;
        Some(Stamp {
            project,
            version: (doc.opening, doc.revision),
            path: path.clone(),
            grammar,
        })
    }

    /// Keep current results and coalesce incoming edits before choosing the next outstanding pane.
    pub(super) fn sync_builtin_syntax(&mut self) {
        let mut desired = BTreeMap::new();
        if let Some(project) = self.active.and_then(|id| self.projects.get(&id)) {
            for doc in project.document.iter().chain(&self.builtin_syntax.visible) {
                if let Some(stamp) = self.builtin_stamp(project.id, doc) {
                    desired.entry(stamp.key()).or_insert((stamp, doc));
                }
            }
        }
        let before = self.view.builtin_syntax.len();
        self.view.builtin_syntax.retain(|key, syntax| {
            desired
                .get(key)
                .is_some_and(|(stamp, _)| stamp.matches(syntax))
        });
        self.changed |= self.view.builtin_syntax.len() != before;
        self.builtin_syntax.retries.retain(|key, (previous, _)| {
            desired.get(key).is_some_and(|(stamp, _)| stamp == previous)
        });
        if self.builtin_syntax.running {
            return;
        }
        let focused = self.active.and_then(|id| {
            self.projects
                .get(&id)?
                .document
                .as_ref()
                .map(|doc| (id, doc.opening))
        });
        let now = Instant::now();
        let Some((stamp, doc)) = desired
            .into_values()
            .filter(|(stamp, _)| {
                let key = stamp.key();
                !self.view.builtin_syntax.contains_key(&key)
                    && self
                        .builtin_syntax
                        .retries
                        .get(&key)
                        .is_none_or(|(_, retry)| retry.ready(now))
            })
            .min_by_key(|(stamp, _)| {
                let key = Some(stamp.key());
                (key == self.builtin_syntax.last_started, key != focused)
            })
        else {
            return;
        };
        let doc = Arc::clone(doc);
        let key = stamp.key();
        let id = self.next();
        let worker = Arc::clone(&self.syntax);
        let finish = self.finish.clone();
        if self.syntax_io.submit(move || {
            let data = worker
                .lock()
                .unwrap()
                .parse(
                    id,
                    stamp.grammar.into(),
                    doc.as_ref().clone(),
                    Some(HIGHLIGHT_QUERY_PREFIX.into()),
                )
                .ok()
                .and_then(|value| serde_json::from_value::<SyntaxQuery>(value).ok())
                .filter(|query| (query.opening, query.revision) == stamp.version)
                .map(|query| DocumentSyntax {
                    grammar: Some(stamp.grammar),
                    version: stamp.version,
                    path: stamp.path.clone(),
                    highlights: DocumentFeatures::from_captures(&doc.text, query.captures)
                        .highlights,
                });
            let _ = finish.send(ResultEvent::BuiltinSyntax(stamp, data));
        }) {
            self.builtin_syntax.running = true;
            self.builtin_syntax.last_started = Some(key);
        }
    }

    /// Publish only if the exact snapshot still belongs to a visible pane.
    pub(super) fn finish_builtin_syntax(&mut self, stamp: Stamp, data: Option<DocumentSyntax>) {
        self.builtin_syntax.running = false;
        let key = stamp.key();
        if self.active != Some(stamp.project)
            || !self.projects.get(&stamp.project).is_some_and(|project| {
                project
                    .document
                    .iter()
                    .chain(&self.builtin_syntax.visible)
                    .any(|doc| {
                        doc.opening == stamp.version.0
                            && self.builtin_stamp(stamp.project, doc).as_ref() == Some(&stamp)
                    })
            })
        {
            return;
        }
        if let Some(data) = data {
            self.builtin_syntax.retries.remove(&key);
            self.view.builtin_syntax.insert(key, Arc::new(data));
            self.changed = true;
        } else {
            self.builtin_syntax
                .retries
                .entry(key)
                .or_insert((stamp, Backoff::default()))
                .1
                .failed(Instant::now());
        }
    }
}
