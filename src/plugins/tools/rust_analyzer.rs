//! Project-owned language servers using a bounded LSP transport.

mod backend;
mod backend_exclusions;
#[cfg(test)]
mod backend_tests;
mod frontend_exclusions;
#[cfg(test)]
#[path = "rust_analyzer/tests.rs"]
mod real_server_tests;
mod scan_exclusions;
mod servers;
mod sql;
mod vue;
pub(in crate::plugins) use scan_exclusions::ProjectSettings;
use scan_exclusions::ScanExclusions;
pub use servers::Server;

use crate::plugins::session::LanguageServerPhase;
use anyhow::{Context, Result, ensure};
use nanobug_plugin_sdk::{DocumentSnapshot, document_wire::MAX_DOCUMENT_BYTES};
use serde_json::{Map, Value, json};
use std::{
    collections::BTreeMap,
    io::{self, BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError},
    },
    thread,
    time::{Duration, Instant},
};
use url::Url;

const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_HEADER_BYTES: u64 = 4096;
const INITIALIZE_WAIT: Duration = Duration::from_secs(12);
const TICK: Duration = Duration::from_millis(15);
/// Let edits settle before requesting diagnostics, and keep the number in flight bounded.
const DIAGNOSTIC_IDLE: Duration = Duration::from_millis(250);
const MAX_DIAGNOSTIC_REQUESTS: usize = 4;

/// Only these read-only LSP operations are available to editor callers.
#[derive(Clone, Copy)]
pub(in crate::plugins) enum Request {
    Completion,
    Definition,
    Hover,
}

impl Request {
    /// Map the editor request to its LSP method.
    fn method(self) -> &'static str {
        match self {
            Self::Completion => "textDocument/completion",
            Self::Definition => "textDocument/definition",
            Self::Hover => "textDocument/hover",
        }
    }
}

/// Transport results forwarded to the owning editor session.
pub(in crate::plugins) enum Event {
    /// The server changed phase after its handshake or background work.
    Phase(LanguageServerPhase),
    /// A reply for the original editor request ID.
    Response {
        id: u64,
        result: std::result::Result<Value, String>,
    },
    /// A list of diagnostics computed for precisely this text snapshot.
    Diagnostics {
        snapshot: Arc<DocumentSnapshot>,
        items: Vec<Value>,
    },
    /// The session has stopped accepting requests.
    Failed(String),
}

/// A request bound to the document revision and deadline chosen by the editor.
struct Query {
    id: u64,
    kind: Request,
    uri: String,
    offset: usize,
    source: QuerySource,
    deadline: Instant,
}

/// Position source for an editor overlay or a Rust dependency already indexed from disk.
enum QuerySource {
    /// Opening and revision of the synchronized buffer.
    Overlay((u64, u64)),
    /// Snapshot used to verify the file and calculate a position without sending its text.
    Indexed(Arc<DocumentSnapshot>),
}

type Snapshots = Arc<Mutex<Option<Vec<Arc<DocumentSnapshot>>>>>;

/// Nonblocking handle for a project's LSP process.
pub(in crate::plugins) struct Analyzer {
    snapshots: Snapshots,
    queries: SyncSender<Query>,
    stop: Arc<AtomicBool>,
}

impl Analyzer {
    /// Start a worker with `root` as its workspace folder.
    /// Read the owning plugin's settings on the worker when `settings` is supplied.
    /// The worker stops when the handle is dropped or the shared project flag `closed` is set.
    /// Deliver events on the worker thread through the nonblocking callback `event`.
    pub(in crate::plugins) fn start(
        server: Server,
        root: PathBuf,
        settings: Option<ProjectSettings>,
        closed: Arc<AtomicBool>,
        event: impl Fn(Event) + Send + 'static,
    ) -> io::Result<Self> {
        let snapshots = Snapshots::default();
        let stop = Arc::new(AtomicBool::new(false));
        let (queries, incoming) = mpsc::sync_channel(24);
        let latest = snapshots.clone();
        let stopped = stop.clone();
        thread::Builder::new()
            .name(format!("nanobug-{}", server.name()))
            .spawn(move || {
                let cancelled =
                    || closed.load(Ordering::Acquire) || stopped.load(Ordering::Acquire);
                if !cancelled()
                    && let Err(error) = run(
                        server,
                        &root,
                        settings.as_ref(),
                        latest,
                        incoming,
                        cancelled,
                        &event,
                    )
                    && !cancelled()
                {
                    event(Event::Failed(format!("{error:#}")));
                }
            })?;
        Ok(Self {
            snapshots,
            queries,
            stop,
        })
    }

    /// Replace the set of open snapshots for this project. Superseded edits can be coalesced.
    pub(in crate::plugins) fn sync(&self, documents: Vec<Arc<DocumentSnapshot>>) {
        *self.snapshots.lock().unwrap() = Some(documents);
    }

    /// Queue a position query for the exact `version` at UTF-8 byte `offset` in `path`.
    pub(in crate::plugins) fn request(
        &self,
        id: u64,
        kind: Request,
        path: PathBuf,
        offset: usize,
        version: (u64, u64),
        deadline: Instant,
    ) -> std::result::Result<(), String> {
        let uri = file_uri(&path).ok_or("Unsupported language server document path")?;
        self.enqueue(Query {
            id,
            kind,
            uri,
            offset,
            source: QuerySource::Overlay(version),
            deadline,
        })
    }

    /// Queue read-only navigation in an indexed Rust `document` without opening an overlay.
    /// The worker verifies the disk text before sending a position at `offset`.
    pub(in crate::plugins) fn request_indexed(
        &self,
        id: u64,
        kind: Request,
        document: Arc<DocumentSnapshot>,
        offset: usize,
        deadline: Instant,
    ) -> std::result::Result<(), String> {
        let uri = document
            .path
            .as_deref()
            .and_then(file_uri)
            .ok_or("Unsupported language server document path")?;
        self.enqueue(Query {
            id,
            kind,
            uri,
            offset,
            source: QuerySource::Indexed(document),
            deadline,
        })
    }

    /// Queue `query` without blocking the session thread.
    fn enqueue(&self, query: Query) -> std::result::Result<(), String> {
        self.queries.try_send(query).map_err(|error| match error {
            mpsc::TrySendError::Full(_) => "Language server request queue is busy".into(),
            mpsc::TrySendError::Disconnected(_) => "Language server is stopped".into(),
        })
    }
}

impl Drop for Analyzer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

/// Snapshot and strictly increasing LSP version for an open document URI.
struct Open {
    snapshot: Arc<DocumentSnapshot>,
    version: i32,
    /// Next pull request for this document version, if the server supports pull diagnostics.
    diagnose_at: Option<Instant>,
}

/// A diagnostic pull tied to the precise version that was open when it was sent.
struct DiagnosticRequest {
    uri: String,
    version: i32,
    deadline: Instant,
}

/// Local state shared by the initialize handshake, document changes and requests.
struct Client {
    server: Server,
    /// Static completion support reported by initialize.
    completion: bool,
    /// Static definition support reported by initialize.
    definition: bool,
    /// Static symbol detail support reported by initialize.
    hover: bool,
    /// Whether changes carry a range for servers using incremental synchronization.
    incremental: bool,
    /// Some servers publish diagnostics only via a client request.
    pull_diagnostics: bool,
    diagnostic_serial: u64,
    diagnostic_requests: BTreeMap<String, DiagnosticRequest>,
    writer: SyncSender<Value>,
    documents: BTreeMap<String, Open>,
    /// Global within a server session so old diagnostics cannot match a reopened file.
    next_version: i32,
    pending: BTreeMap<u64, Query>,
    /// Latest Rust status for presentation since the LSP handshake.
    rust_quiescent: bool,
    workspace: Value,
    options: Value,
    /// Native settings returned to workspace configuration requests.
    configuration: Map<String, Value>,
    /// Project scan exclusions also honored at the editor's document boundary.
    exclusions: Arc<ScanExclusions>,
}

/// A native server and its bounded LSP pipe queues for this project session.
struct Endpoint {
    _process: super::Process,
    writer: SyncSender<Value>,
    incoming: Receiver<Result<Value>>,
}

/// Start a server with bounded pipe I/O and an owned process group.
fn start_endpoint(mut command: Command, name: &str) -> Result<Endpoint> {
    command.stdin(Stdio::piped());
    let mut process = super::Process::own(
        command
            .spawn()
            .with_context(|| format!("Could not start {name}"))?,
    )?;
    let stdin = process
        .child
        .stdin
        .take()
        .context("Language server input pipe")?;
    let stdout = process
        .child
        .stdout
        .take()
        .context("Language server output pipe")?;
    // Blocking OS pipe I/O stays off the supervisor so a stopped project can always be reaped.
    let (writer, outgoing) = mpsc::sync_channel::<Value>(96);
    let (send, incoming) = mpsc::sync_channel(24);
    let errors = send.clone();
    thread::Builder::new()
        .name(format!("nanobug-{name}-input"))
        .spawn(move || {
            let mut stdin = stdin;
            for message in outgoing {
                if let Err(error) = write_message(&mut stdin, &message) {
                    let _ = errors.try_send(Err(error));
                    break;
                }
            }
        })?;
    thread::Builder::new()
        .name(format!("nanobug-{name}-output"))
        .spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let message = read_message(&mut reader);
                let done = message.is_err();
                if send.send(message).is_err() || done {
                    break;
                }
            }
        })?;
    Ok(Endpoint {
        _process: process,
        writer,
        incoming,
    })
}

/// Interleave server I/O, document changes, and deadlines until the project closes.
fn run(
    server: Server,
    root: &Path,
    settings: Option<&ProjectSettings>,
    snapshots: Snapshots,
    queries: Receiver<Query>,
    cancelled: impl Fn() -> bool,
    event: &impl Fn(Event),
) -> Result<()> {
    let root_uri = file_uri(root).context("Unsupported language server project path")?;
    if cancelled() {
        return Ok(());
    }
    let workspace = json!([{"uri": root_uri, "name": root.file_name().and_then(|n| n.to_str()).unwrap_or("Project")}]);
    let exclusions = Arc::new(ScanExclusions::load(root, settings)?);
    let mut vue = (server == Server::Vue)
        .then(|| vue::Bridge::start(root, &root_uri, &workspace, exclusions.clone()))
        .transpose()?;
    let primary = if vue.is_some() {
        Server::Typescript
    } else {
        server
    };
    let mut options = match &vue {
        Some(bridge) => bridge.typescript_options(),
        None => server.options(root, exclusions.directories()),
    };
    let mut command = server.command(root)?;
    let mut configuration = Map::new();
    if !exclusions.directories().is_empty() {
        frontend_exclusions::apply(
            primary,
            root,
            exclusions.directories(),
            &mut command,
            &mut options,
            &mut configuration,
        )?;
        backend_exclusions::apply(
            primary,
            root,
            exclusions.directories(),
            &mut command,
            &mut options,
            &mut configuration,
        )?;
    }
    let endpoint = start_endpoint(command, primary.name())?;
    let mut client = Client::new(server, endpoint.writer.clone(), workspace, options);
    client.exclusions = exclusions;
    client.configuration = configuration;
    client.send(initialize(
        root_uri,
        &client.workspace,
        &client.options,
        server == Server::RustAnalyzer,
    ))?;
    let started = Instant::now();
    let mut initialized = false;
    let mut rust_loaded = false;
    let mut phase = LanguageServerPhase::Starting;
    let mut deferred: Vec<Query> = Vec::new();
    let mut sql_changes = None;
    while !cancelled() {
        if let Some(bridge) = &mut vue {
            bridge.poll(&client, event)?;
        }
        // Bound batches so a continuous stream of server notifications cannot starve editor input.
        for _ in 0..24 {
            match endpoint.incoming.try_recv() {
                Ok(Ok(message))
                    if !initialized
                        && message["id"].as_u64() == Some(0)
                        && message.get("method").is_none() =>
                {
                    client.initialized(&message)?;
                    initialized = true;
                }
                Ok(Ok(message)) => {
                    if let Some(bridge) = &mut vue
                        && bridge.handle_primary(&mut client, &message, event)?
                    {
                        continue;
                    }
                    client.on_message(message, event)?;
                }
                Ok(Err(error)) if !initialized => {
                    return Err(error).context("Language server did not initialize");
                }
                Ok(Err(error)) => return Err(error),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => anyhow::bail!("Language server output ended"),
            }
        }
        let now = Instant::now();
        if !initialized && now.duration_since(started) >= INITIALIZE_WAIT {
            anyhow::bail!("Language server initialization timed out");
        }
        let endpoints_initialized = initialized && vue.as_ref().is_none_or(vue::Bridge::ready);
        let next_phase = client.phase(endpoints_initialized);
        if next_phase != phase {
            phase = next_phase;
            event(Event::Phase(phase));
        }
        // Sync documents before requests as soon as the transport can accept them.
        if endpoints_initialized {
            let changed = snapshots.lock().unwrap().take();
            if let Some(changed) = changed {
                if let Some(bridge) = &mut vue {
                    bridge.sync(root, &changed)?;
                }
                if server.sqlfluff_dialect().is_some() {
                    sql_changes = Some((now + DIAGNOSTIC_IDLE, changed));
                } else {
                    client.sync(root, changed)?;
                }
            }
            // SQLFluff launches a linter for every open notification. Coalesce rapid edits.
            if sql_changes.as_ref().is_some_and(|(due, _)| now >= *due)
                && let Some((_, documents)) = sql_changes.take()
            {
                client.sync(root, documents)?;
            }
            // Rust can answer completion and navigation before it can diagnose the loaded project.
            rust_loaded |= client.rust_quiescent || now.duration_since(started) >= INITIALIZE_WAIT;
            if server != Server::RustAnalyzer || rust_loaded {
                client.request_diagnostics(now)?;
            }
        }
        client.expire(now, event)?;
        for query in queries.try_iter().take(24) {
            if deferred.len() + client.pending.len() + vue.as_ref().map_or(0, vue::Bridge::pending)
                >= 24
            {
                respond(
                    query.id,
                    Err("Language server request queue is busy".into()),
                    event,
                );
            } else {
                deferred.push(query);
            }
        }
        let mut next = Vec::new();
        for query in deferred.drain(..) {
            if now >= query.deadline {
                respond(
                    query.id,
                    Err("Language server request timed out".into()),
                    event,
                );
            } else if endpoints_initialized {
                client.request(query, event)?;
            } else {
                next.push(query);
            }
        }
        deferred = next;
        thread::sleep(TICK);
    }
    Ok(())
}

/// Initialize an endpoint for `root_uri` with its workspace folders and server `options`.
/// Enable the Rust diagnostic refresh protocol when `refresh_diagnostics` is set.
fn initialize(
    root_uri: String,
    workspace: &Value,
    options: &Value,
    refresh_diagnostics: bool,
) -> Value {
    let mut message = json!({
        "jsonrpc": "2.0", "id": 0, "method": "initialize",
        "params": {
            "processId": std::process::id(),
            "clientInfo": {"name": "Nanobug"},
            "rootUri": root_uri,
            "workspaceFolders": workspace,
            "initializationOptions": options,
            "capabilities": {
                "general": {"positionEncodings": ["utf-16"]},
                "workspace": {"configuration": true, "workspaceFolders": true},
                "window": {"workDoneProgress": true},
                "textDocument": {
                    "diagnostic": {"dynamicRegistration": false, "relatedDocumentSupport": false},
                    "publishDiagnostics": {"versionSupport": true},
                    "completion": {"completionItem": {"snippetSupport": false, "insertReplaceSupport": true}},
                    "definition": {"linkSupport": true},
                    "hover": {"contentFormat": ["markdown", "plaintext"]}
                },
                "experimental": {"serverStatusNotification": true}
            }
        }
    });
    if refresh_diagnostics {
        message["params"]["capabilities"]["workspace"]["diagnostics"] =
            json!({"refreshSupport": true});
    }
    message
}

impl Client {
    /// Track capabilities, overlays and in-flight requests for one server.
    fn new(server: Server, writer: SyncSender<Value>, workspace: Value, options: Value) -> Self {
        Self {
            server,
            completion: false,
            definition: false,
            hover: false,
            incremental: false,
            pull_diagnostics: false,
            diagnostic_serial: 0,
            diagnostic_requests: BTreeMap::new(),
            writer,
            documents: BTreeMap::new(),
            next_version: 0,
            pending: BTreeMap::new(),
            rust_quiescent: false,
            workspace,
            options,
            configuration: Map::new(),
            exclusions: Arc::default(),
        }
    }

    /// Validate the encoding and save the capabilities reported by this process.
    fn initialized(&mut self, message: &Value) -> Result<()> {
        if message.get("error").is_some() {
            anyhow::bail!(
                "Language server rejected initialization: {}",
                response_error(&message["error"])
            );
        }
        let capabilities = &message["result"]["capabilities"];
        let encoding = capabilities["positionEncoding"]
            .as_str()
            .unwrap_or("utf-16");
        ensure!(
            encoding == "utf-16",
            "Language server negotiated unsupported position encoding"
        );
        // VS Code's HTML and CSS servers accept plain completion requests even when they hide
        // the capability for clients that do not advertise snippet support.
        self.completion = matches!(self.server, Server::Html | Server::Css)
            || advertised(&capabilities["completionProvider"]);
        self.definition = advertised(&capabilities["definitionProvider"]);
        self.hover = advertised(&capabilities["hoverProvider"]);
        let sync = &capabilities["textDocumentSync"];
        self.incremental = self.server != Server::RustAnalyzer
            && sync.as_u64().or_else(|| sync["change"].as_u64()) == Some(2);
        self.pull_diagnostics = self.server.uses_typescript_diagnostics()
            || advertised(&capabilities["diagnosticProvider"]);
        self.notify("initialized", json!({}))?;
        if !self.configuration.is_empty() {
            self.notify(
                "workspace/didChangeConfiguration",
                json!({"settings": self.configuration}),
            )?;
        }
        Ok(())
    }

    /// Describe progress once all endpoints required by this session have initialized.
    fn phase(&self, endpoints_initialized: bool) -> LanguageServerPhase {
        if !endpoints_initialized {
            LanguageServerPhase::Starting
        } else if self.server == Server::RustAnalyzer && !self.rust_quiescent {
            LanguageServerPhase::Indexing
        } else {
            LanguageServerPhase::Ready
        }
    }
}

impl Client {
    /// Queue one outbound message without letting a stalled server block project shutdown.
    fn send(&self, message: Value) -> Result<()> {
        self.writer
            .try_send(message)
            .context("Language server input is no longer accepting messages")
    }

    /// Send a client notification with no reply ID.
    fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}))
    }

    /// Close missing URIs and send whole-document updates only for new revisions.
    fn sync(&mut self, root: &Path, documents: Vec<Arc<DocumentSnapshot>>) -> Result<()> {
        let sqlfluff = self.server.sqlfluff_dialect().is_some();
        let documents: BTreeMap<_, _> = documents
            .into_iter()
            .filter(|doc| doc.text.len() <= MAX_DOCUMENT_BYTES)
            .filter_map(|doc| {
                let path = doc.path.as_deref()?;
                if self.exclusions.contains(path) {
                    return None;
                }
                let uri = if sqlfluff {
                    sql::snapshot_uri(root, path, &doc)?
                } else {
                    file_uri(path)?
                };
                Some((uri, (self.server.language_id(path)?, doc)))
            })
            .collect();
        for (uri, open) in &self.documents {
            if documents
                .get(uri)
                .is_none_or(|(_, doc)| doc.opening != open.snapshot.opening)
            {
                self.notify(
                    "textDocument/didClose",
                    json!({"textDocument": {"uri": uri}}),
                )?;
            }
        }
        self.documents.retain(|uri, open| {
            documents
                .get(uri)
                .is_some_and(|(_, doc)| doc.opening == open.snapshot.opening)
        });
        for (uri, (language, snapshot)) in documents {
            let previous = self.documents.get(&uri);
            if previous.is_some_and(|open| open.snapshot.revision == snapshot.revision) {
                continue;
            }
            self.next_version = self
                .next_version
                .checked_add(1)
                .context("Language server document version overflowed")?;
            let version = self.next_version;
            if let Some(previous) = previous {
                let change = if self.incremental {
                    let old = &previous.snapshot.text;
                    json!({"text": snapshot.text, "range": {
                        "start": {"line": 0, "character": 0},
                        "end": position_at(old, old.len())
                    }})
                } else {
                    json!({"text": snapshot.text})
                };
                self.notify(
                    "textDocument/didChange",
                    json!({
                        "textDocument": {"uri": uri, "version": version},
                        "contentChanges": [change]
                    }),
                )?;
            } else {
                self.notify("textDocument/didOpen", json!({
                    "textDocument": {"uri": uri, "version": version, "languageId": language, "text": snapshot.text}
                }))?;
            }
            self.documents.insert(
                uri,
                Open {
                    snapshot,
                    version,
                    diagnose_at: self
                        .pull_diagnostics
                        .then(|| Instant::now() + DIAGNOSTIC_IDLE),
                },
            );
        }
        Ok(())
    }

    /// Dispatch `query` against its current overlay or a matching indexed file on disk.
    fn request(&mut self, query: Query, event: &impl Fn(Event)) -> Result<()> {
        let supported = match query.kind {
            Request::Completion => self.completion,
            Request::Definition => self.definition,
            Request::Hover => self.hover,
        };
        if !supported || self.exclusions.contains_uri(&query.uri) {
            respond(query.id, Ok(Value::Null), event);
            return Ok(());
        }
        let position = match &query.source {
            QuerySource::Overlay(_) => self
                .current(&query)
                .ok_or("Document changed before analysis".into())
                .and_then(|open| {
                    position_at(&open.snapshot.text, query.offset)
                        .ok_or("Invalid document position".into())
                }),
            QuerySource::Indexed(document) => indexed_position(document, query.offset),
        };
        let position = match position {
            Ok(position) => position,
            Err(error) => {
                respond(query.id, Err(error), event);
                return Ok(());
            }
        };
        self.send(json!({
            "jsonrpc": "2.0", "id": query.id, "method": query.kind.method(),
            "params": {"textDocument": {"uri": query.uri}, "position": position}
        }))?;
        self.pending.insert(query.id, query);
        Ok(())
    }

    /// Find the open snapshot with the exact editor revision for a query.
    fn current(&self, query: &Query) -> Option<&Open> {
        let QuerySource::Overlay(version) = &query.source else {
            return None;
        };
        self.documents
            .get(&query.uri)
            .filter(|open| (open.snapshot.opening, open.snapshot.revision) == *version)
    }

    /// Check synchronized overlays here. The session checks every reply's editor revision.
    fn overlay_changed(&self, query: &Query) -> bool {
        matches!(query.source, QuerySource::Overlay(_)) && self.current(query).is_none()
    }

    /// Query only settled documents and cancel pulls whose document changed or timed out.
    fn request_diagnostics(&mut self, now: Instant) -> Result<()> {
        let expired: Vec<_> = self
            .diagnostic_requests
            .iter()
            .filter(|(_, request)| {
                now >= request.deadline
                    || self
                        .documents
                        .get(&request.uri)
                        .is_none_or(|open| open.version != request.version)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            self.diagnostic_requests.remove(&id);
            self.notify("$/cancelRequest", json!({"id": id}))?;
        }
        let slots = MAX_DIAGNOSTIC_REQUESTS - self.diagnostic_requests.len();
        let documents: Vec<_> = self
            .documents
            .iter_mut()
            .filter(|(uri, _)| {
                !self
                    .diagnostic_requests
                    .values()
                    .any(|request| &request.uri == *uri)
            })
            .filter_map(|(uri, open)| {
                (open.diagnose_at? <= now).then(|| {
                    open.diagnose_at = None;
                    (uri.clone(), open.version)
                })
            })
            .take(slots)
            .collect();
        for (uri, version) in documents {
            self.diagnostic_serial += 1;
            let id = format!("diagnostics-{}", self.diagnostic_serial);
            let (method, params) = if self.server.uses_typescript_diagnostics() {
                // The TypeScript server pushes diagnostics without a document version.
                // Request semantic diagnostics over its tsserver bridge so they remain bound.
                (
                    "workspace/executeCommand",
                    json!({
                        "command": "typescript.tsserverRequest",
                        "arguments": ["semanticDiagnosticsSync", {"file": uri}, {"expectsResult": true}]
                    }),
                )
            } else {
                (
                    "textDocument/diagnostic",
                    json!({"textDocument": {"uri": uri}}),
                )
            };
            self.send(json!({
                "jsonrpc": "2.0", "id": id, "method": method, "params": params
            }))?;
            self.diagnostic_requests.insert(
                id,
                DiagnosticRequest {
                    uri,
                    version,
                    deadline: now + INITIALIZE_WAIT,
                },
            );
        }
        Ok(())
    }

    /// Cancel queries when their deadline or original snapshot is no longer current.
    fn expire(&mut self, now: Instant, event: &impl Fn(Event)) -> Result<()> {
        let expired: Vec<_> = self
            .pending
            .values()
            .filter_map(|query| {
                if now >= query.deadline {
                    Some((query.id, "Language server request timed out"))
                } else if self.overlay_changed(query) {
                    Some((query.id, "Document changed during analysis"))
                } else {
                    None
                }
            })
            .collect();
        for (id, error) in expired {
            self.pending.remove(&id);
            self.notify("$/cancelRequest", json!({"id": id}))?;
            respond(id, Err(error.into()), event);
        }
        Ok(())
    }

    /// Route notifications, answer server requests and deliver revision-bound replies.
    fn on_message(&mut self, mut message: Value, event: &impl Fn(Event)) -> Result<()> {
        if let Some(method) = message["method"].as_str() {
            if let Some(id) = message.get("id") {
                // A dependency change can invalidate diagnostics on unchanged Rust buffers.
                if method == "workspace/diagnostic/refresh" && self.pull_diagnostics {
                    let due = Instant::now() + DIAGNOSTIC_IDLE;
                    for open in self.documents.values_mut() {
                        open.diagnose_at = Some(due);
                    }
                }
                self.answer_server(id, method, &message["params"])?;
            } else if method == "experimental/serverStatus" && self.server == Server::RustAnalyzer {
                if let Some(quiescent) = message["params"]["quiescent"].as_bool() {
                    self.rust_quiescent = quiescent;
                }
            } else if method == "textDocument/publishDiagnostics" {
                let params = &mut message["params"];
                let sqlfluff = self.server.sqlfluff_dialect().is_some();
                if let Some(uri) = params["uri"].as_str()
                    && let Some(uri) = if sqlfluff {
                        Some(uri.to_owned())
                    } else {
                        uri_path(uri).as_deref().and_then(file_uri)
                    }
                    && let Some(open) = self.documents.get(&uri)
                    && (params["version"].as_i64() == Some(i64::from(open.version))
                        || sqlfluff && params["version"].is_null())
                    && let Value::Array(mut items) = params["diagnostics"].take()
                {
                    items.truncate(512);
                    if sqlfluff {
                        sql::diagnostic_positions(&open.snapshot, &mut items);
                    }
                    event(Event::Diagnostics {
                        snapshot: open.snapshot.clone(),
                        items,
                    });
                }
            }
            return Ok(());
        }
        if let Some(id) = message["id"].as_str()
            && let Some(request) = self.diagnostic_requests.remove(id)
            && let Some(open) = self.documents.get(&request.uri)
            && open.version == request.version
            && Instant::now() < request.deadline
            && let Some(mut items) = diagnostic_items(self.server, &mut message["result"])
        {
            items.truncate(512);
            event(Event::Diagnostics {
                snapshot: open.snapshot.clone(),
                items,
            });
        }
        if let Some(id) = message["id"].as_u64()
            && let Some(query) = self.pending.remove(&id)
        {
            let result = if self.overlay_changed(&query) {
                Err("Document changed during analysis".into())
            } else if Instant::now() >= query.deadline {
                Err("Language server request timed out".into())
            } else if let Some(error) = message.get("error") {
                Err(response_error(error))
            } else {
                let mut result = message["result"].take();
                if matches!(query.kind, Request::Definition) {
                    self.exclusions.filter_definitions(&mut result);
                }
                Ok(result)
            };
            respond(id, result, event);
        }
        Ok(())
    }

    /// Reply only to supported protocol requests. No filesystem mutation is delegated to the server.
    fn answer_server(&self, id: &Value, method: &str, params: &Value) -> Result<()> {
        let result = match method {
            "window/workDoneProgress/create"
            | "window/showMessageRequest"
            | "workspace/diagnostic/refresh" => Value::Null,
            "workspace/workspaceFolders" => self.workspace.clone(),
            "workspace/configuration" => Value::Array(
                params["items"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|item| {
                        let section = item["section"].as_str().unwrap_or_default();
                        if section == "rust-analyzer" {
                            self.options.clone()
                        } else if let Some(section) = section.strip_prefix("rust-analyzer.") {
                            section
                                .split('.')
                                .fold(&self.options, |value, key| &value[key])
                                .clone()
                        } else {
                            self.workspace_configuration(section)
                        }
                    })
                    .collect(),
            ),
            _ => {
                return self.send(json!({
                    "jsonrpc": "2.0", "id": id,
                    "error": {"code": -32601, "message": "Unsupported client method"}
                }));
            }
        };
        self.send(json!({"jsonrpc": "2.0", "id": id, "result": result}))
    }

    /// Resolve a native settings `section`, including dotted child sections queried by Pyright.
    fn workspace_configuration(&self, section: &str) -> Value {
        if section.is_empty() && !self.configuration.is_empty() {
            return Value::Object(self.configuration.clone());
        }
        if let Some(value) = self.configuration.get(section) {
            return value.clone();
        }
        let mut keys = section.split('.');
        if let Some(value) = keys.next().and_then(|key| self.configuration.get(key)) {
            return keys.fold(value, |value, key| &value[key]).clone();
        }
        if self.server == Server::Css {
            json!({})
        } else {
            Value::Null
        }
    }
}

/// LSP capabilities use true or an options object to enable a feature.
fn advertised(value: &Value) -> bool {
    value.as_bool() == Some(true) || value.is_object()
}

/// Normalize the TypeScript bridge's one-based line and UTF-16 column to LSP diagnostics.
fn diagnostic_items(server: Server, result: &mut Value) -> Option<Vec<Value>> {
    if !server.uses_typescript_diagnostics() {
        return match result["items"].take() {
            Value::Array(items) => Some(items),
            _ => None,
        };
    }
    let position = |value: &Value| -> Option<Value> {
        Some(json!({
            "line": value["line"].as_u64()?.checked_sub(1)?,
            "character": value["offset"].as_u64()?.checked_sub(1)?,
        }))
    };
    let items = result["body"]
        .as_array()?
        .iter()
        .take(512)
        .filter_map(|item| {
            Some(json!({
                "range": {"start": position(&item["start"])?, "end": position(&item["end"])?},
                "message": item["text"].as_str()?,
                "code": item["code"],
                "severity": match item["category"].as_str() {
                    Some("error") => 1,
                    Some("warning") => 2,
                    _ => 3,
                }
            }))
        })
        .collect::<Vec<_>>();
    // An empty semantic list must leave the parser's syntax diagnostics available.
    (!items.is_empty()).then_some(items)
}

/// Publish a single result associated with the editor's original `id`.
fn respond(id: u64, result: std::result::Result<Value, String>, event: &impl Fn(Event)) {
    event(Event::Response { id, result });
}

/// Keep diagnostic payloads out of protocol error messages and cap the displayed text.
fn response_error(error: &Value) -> String {
    let message: String = error["message"]
        .as_str()
        .unwrap_or("request failed")
        .chars()
        .filter(|ch| !ch.is_control())
        .take(200)
        .collect();
    message
}

/// Encode a native absolute file path for LSP.
pub(in crate::plugins) fn file_uri(path: &Path) -> Option<String> {
    let mut uri: String = Url::from_file_path(path).ok()?.into();
    // rust-analyzer may publish a lowercase drive letter even for a canonical editor path.
    if cfg!(windows)
        && uri.starts_with("file:///")
        && let Some(drive) = uri.get_mut(8..10)
        && drive.as_bytes()[0].is_ascii_alphabetic()
        && drive.as_bytes()[1] == b':'
    {
        drive.make_ascii_lowercase();
    }
    Some(uri)
}

/// Accept only file URIs and recover the editor's native path.
pub(in crate::plugins) fn uri_path(uri: &str) -> Option<PathBuf> {
    let uri = Url::parse(uri).ok()?;
    if uri.scheme() != "file" || uri.query().is_some() || uri.fragment().is_some() {
        return None;
    }
    let path = uri.to_file_path().ok()?;
    (!path.as_os_str().as_encoded_bytes().contains(&0)).then_some(path)
}

/// Verify the saved text of `document` before calculating a position at `offset` for Rust.
fn indexed_position(
    document: &DocumentSnapshot,
    offset: usize,
) -> std::result::Result<Value, String> {
    let path = document
        .path
        .as_deref()
        .ok_or("External Rust source has no path")?;
    // The editor stores LF text. The same text on disk can contain CRLF and a UTF-8 BOM.
    let data = super::super::package::read_limited(path, MAX_DOCUMENT_BYTES as u64 * 2 + 3)
        .map_err(|error| format!("Could not read external Rust source: {error:#}"))?;
    let text = std::str::from_utf8(&data).map_err(|_| "External Rust source must be UTF-8")?;
    let without_bom = text.strip_prefix('\u{feff}');
    let text = without_bom.unwrap_or(text);
    if text != document.text && text.replace("\r\n", "\n") != document.text {
        return Err("External Rust source on disk differs from the open buffer.".into());
    }
    // On the first line, Rust's disk index includes the BOM that the editor did not retain.
    let mut position = position_at(&document.text, offset).ok_or("Invalid document position")?;
    if without_bom.is_some()
        && position["line"].as_u64() == Some(0)
        && let Some(character) = position["character"].as_u64()
    {
        position["character"] = (character + 1).into();
    }
    Ok(position)
}

/// Convert a UTF-8 byte offset into a zero-based LSP UTF-16 position.
fn position_at(text: &str, offset: usize) -> Option<Value> {
    let prefix = text.get(..offset)?;
    let line = prefix
        .as_bytes()
        .iter()
        .filter(|&&byte| byte == b'\n')
        .count();
    let column = prefix.rsplit('\n').next()?.encode_utf16().count();
    Some(json!({"line": line, "character": column}))
}

/// Convert a zero-based LSP UTF-16 position to a UTF-8 byte boundary in `text`.
pub(in crate::plugins) fn offset_at(text: &str, position: &Value) -> Option<usize> {
    let line = usize::try_from(position["line"].as_u64()?).ok()?;
    let column = usize::try_from(position["character"].as_u64()?).ok()?;
    let mut lines = text.split('\n');
    let start = (&mut lines)
        .take(line)
        .map(|line| line.len() + 1)
        .sum::<usize>();
    let line = lines.next()?;
    let line = line.strip_suffix('\r').unwrap_or(line);
    let mut units = 0;
    for (offset, ch) in line.char_indices() {
        if units == column {
            return Some(start + offset);
        }
        units += ch.len_utf16();
        if units > column {
            return None;
        }
    }
    (units == column).then_some(start + line.len())
}

/// Read one LSP message with bounded header and body sizes.
fn read_message(reader: &mut impl BufRead) -> Result<Value> {
    let mut length = None;
    let mut remaining = MAX_HEADER_BYTES;
    loop {
        let mut line = String::new();
        let read = reader.by_ref().take(remaining).read_line(&mut line)?;
        ensure!(read > 0, "Language server output ended");
        ensure!(
            line.ends_with("\r\n"),
            "Invalid rust-analyzer message header"
        );
        remaining -= read as u64;
        if line == "\r\n" {
            break;
        }
        let (name, value) = line
            .split_once(':')
            .context("Invalid rust-analyzer header field")?;
        if name.eq_ignore_ascii_case("content-length") {
            ensure!(length.is_none(), "Duplicate rust-analyzer message length");
            length = Some(value.trim().parse::<usize>()?);
        }
    }
    let length = length.context("Missing rust-analyzer message length")?;
    ensure!(
        length <= MAX_MESSAGE_BYTES,
        "rust-analyzer message exceeds size limit"
    );
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    serde_json::from_slice(&body).context("Invalid rust-analyzer JSON")
}

/// Write a single JSON message with its byte length before flushing the native pipe.
fn write_message(writer: &mut impl Write, message: &Value) -> Result<()> {
    let data = serde_json::to_vec(message)?;
    ensure!(
        data.len() <= MAX_MESSAGE_BYTES,
        "rust-analyzer message exceeds size limit"
    );
    write!(writer, "Content-Length: {}\r\n\r\n", data.len())?;
    writer.write_all(&data)?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hover uses negotiated content formats and ignores a reply to a superseded snapshot.
    #[test]
    fn hover_query_uses_utf16_and_rejects_stale_reply() -> Result<()> {
        let project = tempfile::tempdir()?;
        let root = project.path().canonicalize()?;
        let path = root.join("main.rs");
        let uri = file_uri(&path).unwrap();
        let initialize = initialize(uri.clone(), &Value::Null, &Value::Null, false);
        assert_eq!(
            initialize["params"]["capabilities"]["textDocument"]["hover"]["contentFormat"],
            json!(["markdown", "plaintext"])
        );
        let (sender, receiver) = mpsc::sync_channel(8);
        let mut client = Client::new(Server::RustAnalyzer, sender, Value::Null, Value::Null);
        client.initialized(&json!({"result": {"capabilities": {"hoverProvider": true}}}))?;
        assert_eq!(receiver.try_recv()?["method"], "initialized");
        let snapshot = |revision| {
            Arc::new(DocumentSnapshot {
                opening: 1,
                revision,
                path: Some(path.clone()),
                text: "🌱details".into(),
            })
        };
        client.sync(&root, vec![snapshot(1)])?;
        assert_eq!(receiver.try_recv()?["method"], "textDocument/didOpen");
        client.request(
            Query {
                id: 1,
                kind: Request::Hover,
                uri,
                offset: "🌱".len(),
                source: QuerySource::Overlay((1, 1)),
                deadline: Instant::now() + Duration::from_secs(5),
            },
            &|_| panic!("hover should reach the server"),
        )?;
        let request = receiver.try_recv()?;
        assert_eq!(request["method"], "textDocument/hover");
        assert_eq!(
            request["params"]["position"],
            json!({"line":0, "character":2})
        );
        client.sync(&root, vec![snapshot(2)])?;
        assert_eq!(receiver.try_recv()?["method"], "textDocument/didChange");
        let (events, transport) = mpsc::channel();
        client.on_message(
            json!({"id":1, "result": {"contents":"old details"}}),
            &|event| events.send(event).unwrap(),
        )?;
        assert!(matches!(
            transport.try_recv()?,
            Event::Response {
                id: 1,
                result: Err(_)
            }
        ));
        Ok(())
    }

    /// An indexed Rust query sends a verified disk position and keeps the source outside LSP overlays.
    #[test]
    fn indexed_rust_definition_uses_disk_positions_without_syncing_text() -> Result<()> {
        let project = tempfile::tempdir()?;
        let path = project.path().join("lib.rs");
        std::fs::write(&path, "\u{feff}🌱details\r\nnext\r\n")?;
        let document = Arc::new(DocumentSnapshot {
            opening: 1,
            revision: 0,
            path: Some(path.clone()),
            text: "🌱details\nnext\n".into(),
        });
        assert_eq!(
            indexed_position(&document, "🌱details\n".len()).unwrap(),
            json!({"line": 1, "character": 0})
        );

        let (send, receive) = mpsc::sync_channel(4);
        let mut client = Client::new(Server::RustAnalyzer, send, Value::Null, Value::Null);
        client.definition = true;
        let uri = file_uri(&path).unwrap();
        let query = |id, document| Query {
            id,
            kind: Request::Definition,
            uri: uri.clone(),
            offset: "🌱".len(),
            source: QuerySource::Indexed(document),
            deadline: Instant::now() + Duration::from_secs(5),
        };
        let (events, replies) = mpsc::channel();
        let emit = |event| events.send(event).unwrap();
        client.request(query(1, document.clone()), &emit)?;
        assert_eq!(
            receive.try_recv()?,
            json!({
                "jsonrpc": "2.0", "id": 1, "method": "textDocument/definition",
                "params": {"textDocument": {"uri": uri}, "position": {"line": 0, "character": 3}}
            })
        );
        client.expire(Instant::now(), &emit)?;
        assert!(receive.try_recv().is_err());
        client.on_message(json!({"id": 1, "result": []}), &emit)?;
        assert!(matches!(
            replies.try_recv()?,
            Event::Response {
                id: 1,
                result: Ok(_)
            }
        ));

        let mut dirty = document.as_ref().clone();
        dirty.text.push_str("modified");
        client.request(query(2, Arc::new(dirty)), &emit)?;
        assert!(receive.try_recv().is_err());
        assert!(
            matches!(replies.try_recv()?, Event::Response { id: 2, result: Err(error) } if error.contains("differs from the open buffer"))
        );
        Ok(())
    }

    /// Both sources protect the document boundary even when a server scans its own workspace.
    #[test]
    fn language_server_transport_omits_both_project_exclusions() -> Result<()> {
        use std::fs;

        let project = tempfile::tempdir()?;
        let root = project.path().canonicalize()?;
        let setting_dir = root.join(".nb/plugin-setting");
        fs::create_dir_all(&setting_dir)?;
        for (server, id, file) in [
            (Server::Html, "html", "page.html"),
            (Server::Kotlin, "kotlin", "Main.kt"),
            (Server::SqlH2, "h2", "query.sql"),
        ] {
            let manual = format!("{id}-manual");
            fs::write(
                setting_dir.join(format!("{id}.toml")),
                format!("excludeDirs = [\"{manual}\"]"),
            )?;
            let settings = ProjectSettings {
                plugin_id: id.into(),
                automatic_exclusions: Arc::from(["auto/cache".into()]),
            };
            let (sender, receiver) = mpsc::sync_channel(8);
            let mut client = Client::new(server, sender, Value::Null, Value::Null);
            client.exclusions = Arc::new(ScanExclusions::load(&root, Some(&settings))?);
            client.definition = true;
            let snapshot = |directory: &str, opening| {
                Arc::new(DocumentSnapshot {
                    opening,
                    revision: 0,
                    path: Some(root.join(directory).join(file)),
                    text: "x".into(),
                })
            };
            let auto = snapshot("auto/cache", 1);
            let manual = snapshot(&manual, 2);
            let regular = snapshot("auto/cache-sibling", 3);
            client.sync(&root, vec![auto.clone(), manual.clone(), regular.clone()])?;
            let regular_uri = file_uri(regular.path.as_ref().unwrap()).unwrap();
            assert_eq!(
                receiver.try_recv()?["params"]["textDocument"]["uri"],
                regular_uri
            );
            assert!(
                receiver.try_recv().is_err(),
                "{id} synchronized an excluded document"
            );

            for excluded in [auto, manual] {
                let excluded_uri = file_uri(excluded.path.as_ref().unwrap()).unwrap();
                client.request(
                    Query {
                        id: excluded.opening,
                        kind: Request::Definition,
                        uri: excluded_uri.clone(),
                        offset: 0,
                        source: QuerySource::Overlay((excluded.opening, 0)),
                        deadline: Instant::now() + Duration::from_secs(1),
                    },
                    &|event| {
                        assert!(matches!(
                            event,
                            Event::Response {
                                result: Ok(Value::Null),
                                ..
                            }
                        ))
                    },
                )?;
                assert!(
                    receiver.try_recv().is_err(),
                    "{id} queried an excluded document"
                );
                let mut definitions = json!([{"targetUri": excluded_uri}, {"uri": regular_uri}]);
                client.exclusions.filter_definitions(&mut definitions);
                assert_eq!(definitions, json!([{"uri": regular_uri}]));
            }
        }
        Ok(())
    }

    /// Rust reports indexing until it sends true quiescence, and can start indexing again.
    #[test]
    fn rust_phase_follows_server_status_after_handshake() -> Result<()> {
        use LanguageServerPhase::{Indexing, Ready, Starting};

        let (writer, _) = mpsc::sync_channel(1);
        let mut client = Client::new(Server::RustAnalyzer, writer, Value::Null, Value::Null);
        assert_eq!(client.phase(false), Starting);
        assert_eq!(client.phase(true), Indexing);
        for (quiescent, phase) in [
            (Value::Bool(false), Indexing),
            (Value::Null, Indexing),
            (Value::Bool(true), Ready),
            (Value::Bool(false), Indexing),
            (Value::Bool(true), Ready),
        ] {
            client.on_message(
                json!({"method": "experimental/serverStatus", "params": {"quiescent": quiescent}}),
                &|_| unreachable!("phase is published by the worker"),
            )?;
            assert_eq!(client.phase(true), phase);
        }
        Ok(())
    }

    /// Verify surrogate pairs and the empty last line can be round-tripped without truncation.
    #[test]
    fn position_round_trips_utf16() {
        let text = "行\na😀b\n";
        for offset in (0..=text.len()).filter(|&offset| text.is_char_boundary(offset)) {
            assert_eq!(
                offset_at(text, &position_at(text, offset).unwrap()),
                Some(offset)
            );
        }
        assert_eq!(offset_at(text, &json!({"line": 1, "character": 2})), None);
        assert_eq!(offset_at(text, &json!({"line": 3, "character": 0})), None);
    }

    /// Preserve Rust source filenames containing URI punctuation or Unicode.
    #[test]
    fn file_uris_escape_special_names() -> Result<()> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("空 格#?.rs");
        let uri = file_uri(&path).context("fixture URI")?;
        assert!(uri.ends_with("%E7%A9%BA%20%E6%A0%BC%23%3F.rs"));
        assert_eq!(
            file_uri(&uri_path(&uri).context("decoded fixture path")?),
            Some(uri)
        );
        assert!(uri_path("https://example.com/target.rs").is_none());
        Ok(())
    }

    /// Length is measured in bytes and oversized messages are rejected before allocation.
    #[test]
    fn lsp_messages_are_length_framed() -> Result<()> {
        let mut wire = Vec::new();
        let message = json!({"value": "字😀"});
        write_message(&mut wire, &message)?;
        assert_eq!(read_message(&mut wire.as_slice())?, message);
        let invalid = format!("Content-Length: {}\r\n\r\n", MAX_MESSAGE_BYTES + 1);
        assert!(read_message(&mut invalid.as_bytes()).is_err());
        Ok(())
    }
}
