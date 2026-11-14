//! A project-owned rust-analyzer process and its bounded LSP transport.

use anyhow::{Context, Result, ensure};
use nanobug_plugin_sdk::{DocumentSnapshot, document_wire::MAX_DOCUMENT_BYTES};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    env,
    io::{self, BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::Stdio,
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

/// Only these read-only LSP operations are available to editor callers.
#[derive(Clone, Copy)]
pub(in crate::plugins) enum Request {
    Completion,
    Definition,
}

impl Request {
    /// Map the editor request to its LSP method.
    fn method(self) -> &'static str {
        match self {
            Self::Completion => "textDocument/completion",
            Self::Definition => "textDocument/definition",
        }
    }
}

/// Transport results forwarded to the owning editor session.
pub(in crate::plugins) enum Event {
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
    version: (u64, u64),
    deadline: Instant,
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
    /// The worker stops when the handle is dropped or the shared project flag `closed` is set.
    /// Deliver events on the worker thread through the nonblocking callback `event`.
    pub(in crate::plugins) fn start(
        root: PathBuf,
        closed: Arc<AtomicBool>,
        event: impl Fn(Event) + Send + 'static,
    ) -> io::Result<Self> {
        let snapshots = Snapshots::default();
        let stop = Arc::new(AtomicBool::new(false));
        let (queries, incoming) = mpsc::sync_channel(24);
        let latest = snapshots.clone();
        let stopped = stop.clone();
        thread::Builder::new()
            .name("nanobug-rust-analyzer".into())
            .spawn(move || {
                let cancelled =
                    || closed.load(Ordering::Acquire) || stopped.load(Ordering::Acquire);
                if !cancelled()
                    && let Err(error) = run(&root, latest, incoming, cancelled, &event)
                    && !cancelled()
                {
                    event(Event::Failed(error.to_string()));
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
        let uri = file_uri(&path).ok_or("Unsupported rust-analyzer document path")?;
        self.queries
            .try_send(Query {
                id,
                kind,
                uri,
                offset,
                version,
                deadline,
            })
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => "rust-analyzer request queue is busy".into(),
                mpsc::TrySendError::Disconnected(_) => "rust-analyzer is stopped".into(),
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
}

/// Local state shared by the initialize handshake, document changes and requests.
struct Client {
    writer: SyncSender<Value>,
    documents: BTreeMap<String, Open>,
    /// Global within a server session so old diagnostics cannot match a reopened file.
    next_version: i32,
    pending: BTreeMap<u64, Query>,
    /// Wait for the initial project load before sending semantic queries.
    loading: bool,
    workspace: Value,
    options: Value,
}

/// Own one native process while interleaving LSP I/O, document changes and deadlines.
fn run(
    root: &Path,
    snapshots: Snapshots,
    queries: Receiver<Query>,
    cancelled: impl Fn() -> bool,
    event: &impl Fn(Event),
) -> Result<()> {
    let root_uri = file_uri(root).context("Unsupported rust-analyzer project path")?;
    let executable = executable()?;
    let mut command = super::command(&executable, root);
    command
        .stdin(Stdio::piped())
        .env("RUSTUP_AUTO_INSTALL", "0");
    if cancelled() {
        return Ok(());
    }
    let mut process =
        super::Process::own(command.spawn().context("Could not start rust-analyzer")?)?;
    let stdin = process
        .child
        .stdin
        .take()
        .context("rust-analyzer input pipe")?;
    let stdout = process
        .child
        .stdout
        .take()
        .context("rust-analyzer output pipe")?;
    // Blocking OS pipe I/O stays off the supervisor so a stopped project can always be reaped.
    let (writer, outgoing) = mpsc::sync_channel::<Value>(96);
    let (send, incoming) = mpsc::sync_channel(24);
    let errors = send.clone();
    thread::Builder::new()
        .name("nanobug-rust-analyzer-input".into())
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
        .name("nanobug-rust-analyzer-output".into())
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
    let workspace = json!([{"uri": root_uri, "name": root.file_name().and_then(|n| n.to_str()).unwrap_or("Rust")}]);
    let options = json!({
        "checkOnSave": false,
        "diagnostics": {"enable": true},
        "completion": {"autoimport": {"enable": false}, "postfix": {"enable": false}},
    });
    let mut client = Client {
        writer,
        documents: BTreeMap::new(),
        next_version: 0,
        pending: BTreeMap::new(),
        loading: true,
        workspace,
        options,
    };
    client.send(json!({
        "jsonrpc": "2.0", "id": 0, "method": "initialize",
        "params": {
            "processId": std::process::id(),
            "clientInfo": {"name": "Nanobug"},
            "rootUri": root_uri,
            "workspaceFolders": client.workspace,
            "initializationOptions": client.options,
            "capabilities": {
                "general": {"positionEncodings": ["utf-16"]},
                "workspace": {"configuration": true, "workspaceFolders": true},
                "window": {"workDoneProgress": true},
                "textDocument": {
                    "publishDiagnostics": {"versionSupport": true},
                    "completion": {"completionItem": {"snippetSupport": false, "insertReplaceSupport": true}},
                    "definition": {"linkSupport": true}
                },
                "experimental": {"serverStatusNotification": true}
            }
        }
    }))?;
    let started = Instant::now();
    let mut initialized = false;
    let mut deferred: Vec<Query> = Vec::new();
    while !cancelled() {
        // Bound batches so a continuous stream of server notifications cannot starve editor input.
        for _ in 0..24 {
            match incoming.try_recv() {
                Ok(Ok(message))
                    if !initialized
                        && message["id"].as_u64() == Some(0)
                        && message.get("method").is_none() =>
                {
                    if message.get("error").is_some() {
                        anyhow::bail!(
                            "rust-analyzer rejected initialization: {}",
                            response_error(&message["error"])
                        );
                    }
                    let encoding = message["result"]["capabilities"]["positionEncoding"]
                        .as_str()
                        .unwrap_or("utf-16");
                    ensure!(
                        encoding == "utf-16",
                        "rust-analyzer negotiated unsupported position encoding"
                    );
                    client.notify("initialized", json!({}))?;
                    initialized = true;
                }
                Ok(Ok(message)) => client.on_message(message, event)?,
                Ok(Err(error)) if !initialized => {
                    return Err(error).context("rust-analyzer did not initialize. Check rust-analyzer --version in the project, or install it with rustup component add rust-analyzer.");
                }
                Ok(Err(error)) => return Err(error),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => anyhow::bail!("rust-analyzer output ended"),
            }
        }
        let now = Instant::now();
        if !initialized && now.duration_since(started) >= INITIALIZE_WAIT {
            anyhow::bail!("rust-analyzer initialization timed out");
        }
        // Opening an overlay before initial indexing can suppress its first semantic diagnostics.
        // Older servers may omit the status notification, so use a bounded wait.
        let ready =
            initialized && (!client.loading || now.duration_since(started) >= INITIALIZE_WAIT);
        if ready {
            let changed = snapshots.lock().unwrap().take();
            if let Some(changed) = changed {
                client.sync(changed)?;
            }
        }
        client.expire(now, event)?;
        for query in queries.try_iter().take(24) {
            if deferred.len() + client.pending.len() >= 24 {
                respond(
                    query.id,
                    Err("rust-analyzer request queue is busy".into()),
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
                    Err("rust-analyzer request timed out".into()),
                    event,
                );
            } else if ready {
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

impl Client {
    /// Queue one outbound message without letting a stalled server block project shutdown.
    fn send(&self, message: Value) -> Result<()> {
        self.writer
            .try_send(message)
            .context("rust-analyzer input is no longer accepting messages")
    }

    /// Send a client notification with no reply ID.
    fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}))
    }

    /// Close missing URIs and send whole-document updates only for new revisions.
    fn sync(&mut self, documents: Vec<Arc<DocumentSnapshot>>) -> Result<()> {
        let documents: BTreeMap<_, _> = documents
            .into_iter()
            .filter(|doc| doc.text.len() <= MAX_DOCUMENT_BYTES)
            .filter_map(|doc| {
                let path = doc.path.as_deref()?;
                if !path.extension()?.eq_ignore_ascii_case("rs") {
                    return None;
                }
                Some((file_uri(path)?, doc))
            })
            .collect();
        for (uri, open) in &self.documents {
            if documents
                .get(uri)
                .is_none_or(|doc| doc.opening != open.snapshot.opening)
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
                .is_some_and(|doc| doc.opening == open.snapshot.opening)
        });
        for (uri, snapshot) in documents {
            let previous = self.documents.get(&uri);
            if previous.is_some_and(|open| open.snapshot.revision == snapshot.revision) {
                continue;
            }
            self.next_version = self
                .next_version
                .checked_add(1)
                .context("rust-analyzer document version overflowed")?;
            let version = self.next_version;
            if previous.is_some() {
                self.notify(
                    "textDocument/didChange",
                    json!({
                        "textDocument": {"uri": uri, "version": version},
                        "contentChanges": [{"text": snapshot.text}]
                    }),
                )?;
            } else {
                self.notify("textDocument/didOpen", json!({
                    "textDocument": {"uri": uri, "version": version, "languageId": "rust", "text": snapshot.text}
                }))?;
            }
            self.documents.insert(uri, Open { snapshot, version });
        }
        Ok(())
    }

    /// Dispatch once the requested text matches the current server snapshot.
    fn request(&mut self, query: Query, event: &impl Fn(Event)) -> Result<()> {
        let Some(open) = self.current(&query) else {
            respond(
                query.id,
                Err("Rust document changed before analysis".into()),
                event,
            );
            return Ok(());
        };
        let Some(position) = position_at(&open.snapshot.text, query.offset) else {
            respond(
                query.id,
                Err("Invalid Rust document position".into()),
                event,
            );
            return Ok(());
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
        self.documents
            .get(&query.uri)
            .filter(|open| (open.snapshot.opening, open.snapshot.revision) == query.version)
    }

    /// Cancel queries when their deadline or original snapshot is no longer current.
    fn expire(&mut self, now: Instant, event: &impl Fn(Event)) -> Result<()> {
        let expired: Vec<_> = self
            .pending
            .values()
            .filter_map(|query| {
                if now >= query.deadline {
                    Some((query.id, "rust-analyzer request timed out"))
                } else if self.current(query).is_none() {
                    Some((query.id, "Rust document changed during analysis"))
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
                self.answer_server(id, method, &message["params"])?;
            } else if method == "experimental/serverStatus" {
                if message["params"]["quiescent"].as_bool() == Some(true) {
                    self.loading = false;
                }
            } else if method == "textDocument/publishDiagnostics" {
                let params = &mut message["params"];
                if let Some(uri) = params["uri"].as_str()
                    && let Some(uri) = uri_path(uri).as_deref().and_then(file_uri)
                    && let Some(open) = self.documents.get(&uri)
                    && params["version"].as_i64() == Some(i64::from(open.version))
                    && let Value::Array(mut items) = params["diagnostics"].take()
                {
                    items.truncate(512);
                    event(Event::Diagnostics {
                        snapshot: open.snapshot.clone(),
                        items,
                    });
                }
            }
            return Ok(());
        }
        if let Some(id) = message["id"].as_u64()
            && let Some(query) = self.pending.remove(&id)
        {
            let result = if self.current(&query).is_none() {
                Err("Rust document changed during analysis".into())
            } else if Instant::now() >= query.deadline {
                Err("rust-analyzer request timed out".into())
            } else if let Some(error) = message.get("error") {
                Err(response_error(error))
            } else {
                Ok(message["result"].take())
            };
            respond(id, result, event);
        }
        Ok(())
    }

    /// Reply only to supported protocol requests. No filesystem mutation is delegated to the server.
    fn answer_server(&self, id: &Value, method: &str, params: &Value) -> Result<()> {
        let result = match method {
            "window/workDoneProgress/create" | "window/showMessageRequest" => Value::Null,
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
                            Value::Null
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
    format!("rust-analyzer: {message}")
}

/// Resolve the rustup proxy or a standalone analyzer from an absolute PATH entry.
fn executable() -> Result<PathBuf> {
    let name = format!("rust-analyzer{}", env::consts::EXE_SUFFIX);
    env::var_os("PATH")
        .and_then(|path| {
            env::split_paths(&path)
                .filter(|dir| dir.is_absolute())
                .map(|dir| dir.join(&name))
                .find(|path| {
                    let Ok(metadata) = path.metadata() else { return false };
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        if metadata.permissions().mode() & 0o111 == 0 {
                            return false;
                        }
                    }
                    metadata.is_file()
                })
        })
        .context("rust-analyzer is unavailable on PATH. Install it with rustup component add rust-analyzer.")
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
        ensure!(read > 0, "rust-analyzer output ended");
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
