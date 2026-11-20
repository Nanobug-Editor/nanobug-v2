//! Coalesce native syntax jobs for formats without an installable language provider.
use super::{Backoff, Engine, ResultEvent};
use crate::plugins::session::{DocumentSyntax, builtin_syntax_grammar};
use nanobug_plugin_sdk::{
    DocumentSnapshot,
    language::{DocumentFeatures, HIGHLIGHT_QUERY_PREFIX, SyntaxQuery},
};
use std::{path::PathBuf, sync::Arc, time::Instant};

/// Path and explicit grammar are part of the identity even when the contents do not change.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Stamp {
    project: u64,
    version: (u64, u64),
    path: PathBuf,
    grammar: &'static str,
}

/// Only one native job occupies the worker queue; typing replaces the desired revision.
#[derive(Default)]
pub(super) struct State {
    running: bool,
    last: Option<Stamp>,
    /// A transient worker failure retries the same revision on the session's bounded schedule.
    retry: Backoff,
}

impl Engine {
    fn builtin_document(&self) -> Option<(u64, &Arc<DocumentSnapshot>, &'static str)> {
        let project = self.active?;
        let doc = self.projects.get(&project)?.document.as_ref()?;
        if doc.text.len() > 120 * 1024 {
            return None;
        }
        let grammar = builtin_syntax_grammar(
            doc.path.as_deref(),
            self.document_filetype(project, doc.opening),
        )?;
        Some((project, doc, grammar))
    }

    pub(super) fn sync_builtin_syntax(&mut self) {
        let Some((project, doc, grammar)) = self.builtin_document() else {
            self.builtin_syntax.last = None;
            self.changed |= self.view.builtin_syntax.take().is_some();
            return;
        };
        let version = (doc.opening, doc.revision);
        let Some(path) = &doc.path else { return };
        let same = self.builtin_syntax.last.as_ref().is_some_and(|s| {
            s.project == project && s.version == version && s.grammar == grammar && &s.path == path
        });
        if same
            && (self.builtin_syntax.retry.resume.is_none()
                || !self.builtin_syntax.retry.ready(Instant::now()))
        {
            return;
        }
        let stamp = Stamp {
            project,
            version,
            path: path.clone(),
            grammar,
        };
        let doc = Arc::clone(doc);
        self.changed |= self.view.builtin_syntax.take().is_some();
        if self.builtin_syntax.running {
            return;
        }
        let started = stamp.clone();
        let id = self.next();
        let worker = Arc::clone(&self.syntax);
        let finish = self.finish.clone();
        if self.syntax_io.submit(move || {
            let result = worker
                .lock()
                .unwrap()
                .parse(
                    id,
                    grammar.into(),
                    doc.as_ref().clone(),
                    Some(HIGHLIGHT_QUERY_PREFIX.into()),
                )
                .ok()
                .and_then(|value| serde_json::from_value::<SyntaxQuery>(value).ok())
                .filter(|query| (query.opening, query.revision) == version)
                .map(|query| DocumentSyntax {
                    grammar: Some(grammar),
                    version,
                    path: started.path.clone(),
                    highlights: DocumentFeatures::from_captures(&doc.text, query.captures)
                        .highlights,
                });
            let _ = finish.send(ResultEvent::BuiltinSyntax(started, result));
        }) {
            if !same {
                self.builtin_syntax.retry = Backoff::default();
            }
            self.builtin_syntax.last = Some(stamp);
            self.builtin_syntax.running = true;
        }
    }

    pub(super) fn finish_builtin_syntax(&mut self, stamp: Stamp, data: Option<DocumentSyntax>) {
        self.builtin_syntax.running = false;
        if self
            .builtin_document()
            .is_some_and(|(project, doc, grammar)| {
                project == stamp.project
                    && grammar == stamp.grammar
                    && (doc.opening, doc.revision) == stamp.version
                    && doc.path.as_ref() == Some(&stamp.path)
            })
        {
            if data.is_some() {
                self.builtin_syntax.retry = Backoff::default();
            } else {
                self.builtin_syntax.retry.failed(Instant::now());
            }
            self.view.builtin_syntax = data.map(|d| (stamp.project, Arc::new(d)));
            self.changed = true;
        }
    }
}
