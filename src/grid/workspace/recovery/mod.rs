//! Checkpoint shared text buffers and reconcile them with the live workspace.

mod commands;
mod store;
#[cfg(all(test, windows))]
mod tests;

use super::Workspace;
use gpui::{Context, Window};
use nanobug::{document::Document, i18n};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use store::{Draft, Store};

/// Bound the interval between drafts during continuous typing as well as idle editing.
const INTERVAL: Duration = Duration::from_secs(1);

/// One run's writer and the revision counter shared by periodic and explicit writes.
#[derive(Default)]
pub(super) struct Recovery {
    writer: Option<Arc<Mutex<Writer>>>,
    unavailable: Option<String>,
    generation: u64,
    reported_error: bool,
}

/// Serializes file publication, cleanup and explicit commands within one workspace.
struct Writer {
    store: Store,
    records: HashMap<u64, Record>,
    generation: u64,
    finished: bool,
}

/// Last checkpointed revision of a logical buffer, with rename and save tracking.
#[derive(Clone, PartialEq, Eq)]
struct Record {
    revision: u64,
    scope: PathBuf,
    path: Option<PathBuf>,
    modified: bool,
}

/// Write only changed drafts and remove completed drafts in one background job.
struct Batch {
    generation: u64,
    writes: Vec<(u64, Record, Draft)>,
    removals: Vec<u64>,
}

impl Recovery {
    /// Open a separate run beneath `root`; `None` denotes an isolated workspace.
    pub(super) fn new(root: Option<anyhow::Result<PathBuf>>) -> Self {
        match root.map(|path| path.and_then(Store::new)) {
            Some(Ok(store)) => Self {
                writer: Some(Arc::new(Mutex::new(Writer {
                    store,
                    records: HashMap::new(),
                    generation: 0,
                    finished: false,
                }))),
                ..Self::default()
            },
            Some(Err(error)) => Self {
                unavailable: Some(format!("{error:#}")),
                ..Self::default()
            },
            None => Self::default(),
        }
    }

    /// Serialize approved shutdown with any in-flight writer and clean this run only.
    pub(super) fn finish(&mut self) {
        if let Some(writer) = self.writer.take() {
            let mut writer = writer.lock().expect("recovery writer");
            writer.finished = true;
            if let Err(error) = writer.store.finish() {
                eprintln!("Could not clean editor recovery data: {error:#}");
            }
        }
    }

    /// Issue an ordering token when an explicit command or background job is prepared.
    fn next_generation(&mut self) -> u64 {
        self.generation += 1;
        self.generation
    }
}

impl Record {
    /// Identify the dirty state of the latest shared `document` in `scope`.
    fn new(scope: &Path, document: &Document) -> Self {
        let (revision, path, modified) = document.recovery_state();
        Self {
            revision,
            scope: scope.to_owned(),
            path,
            modified,
        }
    }
}

impl Writer {
    /// Publish `draft` for `id` and record its metadata after the durable write succeeds.
    fn write(&mut self, id: u64, record: Record, draft: &Draft) -> anyhow::Result<()> {
        self.store.write(id, draft)?;
        self.records.insert(id, record);
        Ok(())
    }

    /// Apply `batch` if an explicit Ex operation or shutdown has yet to supersede it.
    fn publish(&mut self, batch: Batch) -> anyhow::Result<()> {
        if self.finished || batch.generation <= self.generation {
            return Ok(());
        }
        self.generation = batch.generation;
        let mut result = Ok(());
        for (id, record, draft) in batch.writes {
            if let Err(error) = self.write(id, record, &draft) {
                result = Err(error);
            }
        }
        for id in batch.removals {
            match self.store.remove(id) {
                Ok(()) => {
                    self.records.remove(&id);
                }
                Err(error) => result = Err(error),
            }
        }
        result
    }
}

impl Workspace {
    /// Sample on the UI thread and publish atomically off-thread until `window` closes.
    pub(super) fn start_recovery(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(writer) = self.recovery.writer.clone() else {
            return;
        };
        let executor = cx.background_executor().clone();
        cx.spawn_in(window, async move |view, cx| {
            loop {
                executor.timer(INTERVAL).await;
                let Ok(Ok(batch)) =
                    cx.update(|_, cx| view.update(cx, |workspace, _| workspace.recovery_batch()))
                else {
                    break;
                };
                let Some(batch) = batch else { continue };
                let writer = writer.clone();
                let result = executor
                    .spawn(async move { writer.lock().expect("recovery writer").publish(batch) })
                    .await;
                let _ = cx.update(|window, cx| {
                    view.update(cx, |workspace, cx| {
                        if workspace.recovery.writer.is_none() {
                            return;
                        }
                        if let Err(error) = &result
                            && !workspace.recovery.reported_error
                        {
                            workspace.set_status(i18n::format(
                                "ex.recovery_error",
                                &[("error", &format!("{error:#}"))],
                            ));
                            workspace.sync(window, cx);
                        }
                        workspace.recovery.reported_error = result.is_err();
                    })
                });
            }
        })
        .detach();
    }

    /// Prepare only texts with new revisions, covering inactive and detached buffers as well.
    fn recovery_batch(&mut self) -> Option<Batch> {
        let records = self
            .recovery
            .writer
            .as_ref()?
            .try_lock()
            .ok()?
            .records
            .clone();
        let mut retained = HashSet::new();
        let mut writes = Vec::new();
        for project in self.projects.ordered() {
            for (id, buffer) in project.session_buffer_entries() {
                if !buffer.supports_shared_view()
                    || !buffer.vim.swapfile_enabled()
                    || !project.session_buffer_loaded(id)
                {
                    continue;
                }
                let document = &buffer.vim.doc;
                let prior = records.get(&id);
                let record = Record::new(&project.root, document);
                if record.modified && prior != Some(&record) {
                    writes.push((
                        id,
                        record.clone(),
                        Draft {
                            scope: record.scope.clone(),
                            buffer: document.recovery_draft(),
                        },
                    ));
                }
                if record.modified || prior == Some(&record) {
                    retained.insert(id);
                }
            }
        }
        let removals: Vec<_> = records
            .into_keys()
            .filter(|id| !retained.contains(id))
            .collect();
        if writes.is_empty() && removals.is_empty() {
            return None;
        }
        Some(Batch {
            generation: self.recovery.next_generation(),
            writes,
            removals,
        })
    }

    /// Retire resolved checkpoints promptly after a save or an explicit close/discard.
    pub(in crate::grid::workspace) fn prune_resolved_drafts(&mut self) {
        let Some(writer) = self.recovery.writer.clone() else {
            return;
        };
        let generation = self.recovery.next_generation();
        let mut writer = writer.lock().expect("recovery writer");
        // A queued batch from before the save or discard must not recreate an old draft.
        writer.generation = generation;
        let removals: Vec<_> = writer
            .records
            .iter()
            .filter_map(|(&id, prior)| {
                let keep = self.projects.ordered().any(|project| {
                    project.session_buffer_loaded(id)
                        && project.session_buffer(id).is_some_and(|buffer| {
                            buffer.vim.swapfile_enabled() && {
                                let record = Record::new(&project.root, &buffer.vim.doc);
                                record.modified || *prior == record
                            }
                        })
                });
                (!keep).then_some(id)
            })
            .collect();
        for id in removals {
            match writer.store.remove(id) {
                Ok(()) => {
                    writer.records.remove(&id);
                }
                Err(error) => eprintln!("Could not clean editor recovery data: {error:#}"),
            }
        }
    }
}
