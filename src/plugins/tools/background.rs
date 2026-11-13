//! Rebuildable native services receive complete authorized document snapshots.

use super::{PreparedExecutable, Process, Source, command};
use crate::plugins::{pipe::process_private_bytes, session::Project};
use anyhow::{Context, Result, ensure};
use nanobug_plugin_sdk::{DocumentSnapshot, ToolSnapshot, ToolSnapshotReply, receive, send};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{self, BufReader},
    path::PathBuf,
    process::Stdio,
    sync::{Arc, atomic::Ordering, mpsc},
    time::{Duration, Instant},
};

const SYNC_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_DOCUMENTS: usize = 16;
const MAX_DOCUMENT_BYTES: usize = 120 * 1024;
const MAX_RESULT_BYTES: usize = 64 * 1024;
const PRIVATE_LIMIT: u64 = 384 * 1024 * 1024;
const MEMORY_SAMPLE: Duration = Duration::from_secs(1);
const RECLAIM_GRACE: Duration = Duration::from_secs(5);

/// A host-observed service phase, independent of the plugin runtime's phase.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundToolPhase {
    Starting,
    Synchronizing,
    Ready,
    Recovering,
    Suspended,
    Paused,
}

/// Small public status that never exposes raw tool output or document contents.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct BackgroundToolStatus {
    pub phase: BackgroundToolPhase,
    pub pid: Option<u32>,
    pub revision: Option<u64>,
    pub private_bytes: Option<u64>,
    pub memory_paused: bool,
}

impl BackgroundToolStatus {
    pub(in crate::plugins) fn idle(phase: BackgroundToolPhase) -> Self {
        Self {
            phase,
            pid: None,
            revision: None,
            private_bytes: None,
            memory_paused: false,
        }
    }
}

/// Version comparison includes paths so a Save As changes the tool's input.
#[derive(PartialEq, Eq)]
struct DocumentSet {
    documents: Vec<Arc<DocumentSnapshot>>,
    omitted: usize,
}

impl DocumentSet {
    fn snapshot(project: &Project, allowed: bool) -> Self {
        if !allowed {
            return Self {
                documents: Vec::new(),
                omitted: 0,
            };
        }
        let active = project.document.iter();
        let docs = active.chain(project.open_documents.iter().filter(|candidate| {
            project
                .document
                .as_ref()
                .is_none_or(|doc| doc.opening != candidate.opening)
        }));
        let mut documents = Vec::new();
        let mut remaining = MAX_DOCUMENT_BYTES;
        let mut omitted = 0;
        for document in docs {
            if documents.len() == MAX_DOCUMENTS || document.text.len() > remaining {
                omitted += 1;
            } else {
                remaining -= document.text.len();
                documents.push(Arc::clone(document));
            }
        }
        Self { documents, omitted }
    }

    fn same_version(&self, next: &Self) -> bool {
        self.omitted == next.omitted
            && self.documents.len() == next.documents.len()
            && self
                .documents
                .iter()
                .zip(&next.documents)
                .all(|(old, new)| {
                    old.opening == new.opening
                        && old.revision == new.revision
                        && old.path == new.path
                })
    }
}

struct Pending {
    revision: u64,
    documents: DocumentSet,
    deadline: Instant,
}

/// Sample infrequently and offer one bounded chance to release reconstructable state.
struct MemoryPressure {
    sample_at: Instant,
    bytes: Option<u64>,
    reclaim_by: Option<Instant>,
    request_reclaim: bool,
}

impl MemoryPressure {
    fn new() -> Self {
        Self {
            sample_at: Instant::now(),
            bytes: None,
            reclaim_by: None,
            request_reclaim: false,
        }
    }

    fn observe(&mut self, now: Instant, bytes: Option<u64>) -> Option<u64> {
        self.sample_at = now + MEMORY_SAMPLE;
        self.bytes = bytes;
        if bytes.is_none_or(|bytes| bytes <= PRIVATE_LIMIT) {
            self.reclaim_by = None;
            self.request_reclaim = false;
        } else if let Some(deadline) = self.reclaim_by {
            if now >= deadline {
                return bytes;
            }
        } else {
            self.reclaim_by = Some(now + RECLAIM_GRACE);
            self.request_reclaim = true;
        }
        None
    }
}

/// Verify the same native image on any automatic restart, including user-selected tools.
#[derive(Clone, PartialEq, Eq)]
pub(in crate::plugins) struct Executable {
    path: PathBuf,
    digest: [u8; 32],
}

impl Executable {
    fn identify(path: PathBuf) -> Result<Self> {
        let mut hash = Sha256::new();
        io::copy(&mut File::open(&path)?, &mut hash)?;
        Ok(Self {
            path,
            digest: hash.finalize().into(),
        })
    }
}

/// The supervisor holds the exact process and replies received on its pipes.
pub(in crate::plugins) struct BackgroundProcess {
    process: Process,
    pub executable: Executable,
    requests: mpsc::SyncSender<ToolSnapshot>,
    responses: mpsc::Receiver<ToolSnapshotReply>,
    pending: Option<Pending>,
    result: Option<(DocumentSet, ToolSnapshotReply)>,
    memory: MemoryPressure,
    // This field drops after the process has been stopped and reaped.
    _reservation: PreparedExecutable,
}

impl BackgroundProcess {
    /// Resolve and spawn on a bounded worker before passing ownership to the supervisor.
    pub(in crate::plugins) fn start(
        source: &Source,
        args: &[String],
        expected: Option<&Executable>,
    ) -> Result<Self> {
        super::check_args(args)?;
        let reservation = source.executable()?;
        let executable = Executable::identify(reservation.path().to_path_buf())?;
        ensure!(
            expected.is_none_or(|expected| *expected == executable),
            "Background tool executable changed. Restart it explicitly."
        );
        ensure!(
            !source.closed.load(Ordering::Acquire),
            "Background tool launch was cancelled"
        );
        Self::spawn(executable, &source.project, args, reservation)
    }

    fn spawn(
        executable: Executable,
        directory: &std::path::Path,
        args: &[String],
        reservation: PreparedExecutable,
    ) -> Result<Self> {
        let mut launch = command(&executable.path, directory);
        let mut process = Process::own(
            launch
                .args(args)
                .stdin(Stdio::piped())
                .spawn()
                .context("Start native background tool")?,
        )?;
        let mut input = process
            .child
            .stdin
            .take()
            .context("Background tool input pipe unavailable")?;
        let mut output = BufReader::new(
            process
                .child
                .stdout
                .take()
                .context("Background tool output pipe unavailable")?,
        );
        let (requests, queue) = mpsc::sync_channel(1);
        let (replies, responses) = mpsc::sync_channel::<ToolSnapshotReply>(2);
        std::thread::spawn(move || {
            while let Ok(message) = queue.recv() {
                if send(&mut input, &message).is_err() {
                    break;
                }
            }
        });
        std::thread::spawn(move || {
            while let Ok(Some(response)) = receive::<ToolSnapshotReply>(&mut output) {
                if replies.send(response).is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            process,
            executable,
            requests,
            responses,
            pending: None,
            result: None,
            memory: MemoryPressure::new(),
            _reservation: reservation,
        })
    }

    pub(in crate::plugins) fn acknowledged_snapshot(&self) -> bool {
        self.result.is_some()
    }

    pub(in crate::plugins) fn sample_memory(&mut self, now: Instant) -> Option<u64> {
        if now < self.memory.sample_at {
            return None;
        }
        self.memory
            .observe(now, process_private_bytes(&self.process.child))
    }

    pub(in crate::plugins) fn reclaiming(&self) -> Option<u64> {
        self.memory.reclaim_by.and(self.memory.bytes)
    }

    /// Send the latest full input only when the previous exchange has settled.
    pub(in crate::plugins) fn advance(
        &mut self,
        project: &Project,
        read_document: bool,
        revision: u64,
        now: Instant,
    ) -> Result<()> {
        if let Some(status) = self.process.child.try_wait()? {
            anyhow::bail!("Background tool exited ({status})");
        }
        match self.responses.try_recv() {
            Ok(response) => {
                let pending = self
                    .pending
                    .take()
                    .context("Unsolicited background tool result")?;
                ensure!(
                    pending.revision == response.revision,
                    "Background tool acknowledged the wrong revision"
                );
                ensure!(
                    serde_json::to_vec(&response.result)?.len() <= MAX_RESULT_BYTES,
                    "Background tool result exceeds the result limit"
                );
                self.result = Some((pending.documents, response));
            }
            Err(mpsc::TryRecvError::Disconnected) => anyhow::bail!("Background tool pipe closed"),
            Err(mpsc::TryRecvError::Empty) => {}
        }
        if let Some(pending) = &self.pending {
            ensure!(
                now < pending.deadline,
                "Background tool synchronization timed out"
            );
            return Ok(());
        }
        let documents = DocumentSet::snapshot(project, read_document);
        if !self.memory.request_reclaim
            && self
                .result
                .as_ref()
                .is_some_and(|(last, _)| last.same_version(&documents))
        {
            return Ok(());
        }
        self.requests
            .try_send(ToolSnapshot {
                protocol: 1,
                revision,
                documents: documents
                    .documents
                    .iter()
                    .map(|doc| doc.as_ref().clone())
                    .collect(),
                omitted_documents: documents.omitted,
                discard_rebuildable_cache: self.memory.request_reclaim,
            })
            .map_err(|_| anyhow::anyhow!("Background tool input is unavailable"))?;
        self.memory.request_reclaim = false;
        self.pending = Some(Pending {
            revision,
            documents,
            deadline: now + SYNC_TIMEOUT,
        });
        Ok(())
    }

    /// Old results remain private until a matching current snapshot has been processed.
    pub(in crate::plugins) fn current(
        &self,
        project: &Project,
        read_document: bool,
    ) -> (BackgroundToolStatus, Option<&Value>) {
        let current = DocumentSet::snapshot(project, read_document);
        let ready = self
            .result
            .as_ref()
            .filter(|(documents, _)| documents.same_version(&current))
            .map(|(_, reply)| reply);
        (
            BackgroundToolStatus {
                phase: if ready.is_some() {
                    BackgroundToolPhase::Ready
                } else {
                    BackgroundToolPhase::Synchronizing
                },
                pid: Some(self.process.child.id()),
                revision: ready.map(|reply| reply.revision),
                private_bytes: self.memory.bytes,
                memory_paused: false,
            },
            ready.map(|reply| &reply.result),
        )
    }
}
