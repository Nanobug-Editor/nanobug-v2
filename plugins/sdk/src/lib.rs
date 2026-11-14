//! Types shared by the editor, isolated worker and WASM SDK.

pub mod canvas;
#[doc(hidden)]
pub mod document_wire;
/// Data supplied by document language providers.
pub mod language;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    io::{self, BufRead, Write},
    path::PathBuf,
};

/// Public API label. The wire protocol and experimental contracts version independently.
pub const API: &str = "1.0";
/// Private host-to-worker handshake revision.
pub const WIRE: u32 = 3;
/// Maximum payload on one pipe message or WASM call.
pub const MAX_MESSAGE: usize = 1024 * 1024;

/// Implement the Nanobug 1.0 WASM ABI for a function taking Input and returning Output.
/// Export once from a cdylib built for wasm32-unknown-unknown.
#[macro_export]
macro_rules! export_plugin {
    ($handler:path) => {
        #[cfg(target_arch = "wasm32")]
        #[unsafe(no_mangle)]
        pub extern "C" fn nb_alloc(length: u32) -> u32 {
            $crate::guest::allocate(length)
        }

        #[cfg(target_arch = "wasm32")]
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn nb_run(pointer: u32, length: u32) -> u64 {
            // The runtime passes a live input allocation from nb_alloc.
            unsafe { $crate::guest::run(pointer, length, $handler) }
        }

        #[cfg(target_arch = "wasm32")]
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn nb_free(pointer: u32, length: u32) {
            // The runtime frees each input or returned output allocation exactly once.
            unsafe { $crate::guest::release(pointer, length) }
        }
    };
}

/// Implementation details of the exported ABI.
#[cfg(target_arch = "wasm32")]
#[doc(hidden)]
pub mod guest {
    use super::{Input, MAX_MESSAGE, Output};

    pub fn allocate(length: u32) -> u32 {
        if length as usize > MAX_MESSAGE {
            return 0;
        }
        Box::into_raw(vec![0u8; length as usize].into_boxed_slice()) as *mut u8 as u32
    }

    /// # Safety
    /// The pointer and length must identify a live allocation from allocate.
    pub unsafe fn run(pointer: u32, length: u32, handler: fn(Input) -> Output) -> u64 {
        if pointer == 0 || length as usize > MAX_MESSAGE {
            return 0;
        }
        let bytes = unsafe { std::slice::from_raw_parts(pointer as *const u8, length as usize) };
        let result = serde_json::from_slice(bytes)
            .map(handler)
            .and_then(|output| serde_json::to_vec(&output));
        let bytes = match result {
            Ok(bytes) if bytes.len() <= MAX_MESSAGE / 2 => bytes,
            _ => br#"{"message":"Plugin could not encode its response"}"#.to_vec(),
        };
        let length = bytes.len() as u64;
        let pointer = Box::into_raw(bytes.into_boxed_slice()) as *mut u8 as u64;
        (pointer << 32) | length
    }

    /// # Safety
    /// The allocation must still be live and come from allocate or run.
    pub unsafe fn release(pointer: u32, length: u32) {
        if pointer != 0 {
            let slice = std::ptr::slice_from_raw_parts_mut(pointer as *mut u8, length as usize);
            drop(unsafe { Box::from_raw(slice) });
        }
    }
}

/// Access granted by the installation record, scoped to the installed plugin.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    ReadProject,
    ReadDocument,
    EditDocument,
    ChangeFiles,
    RunTool,
    Network,
    Credentials,
    Collaborate,
    ReadRulePaths,
}

impl Permission {
    /// User-facing operation covered by this grant.
    pub fn description(self) -> &'static str {
        match self {
            Self::ReadProject => "Read files in opened projects",
            Self::ReadRulePaths => "Let activation rules read the external files listed below",
            Self::ReadDocument => "Read open documents, including unsaved edits",
            Self::EditDocument => "Propose changes to documents",
            Self::ChangeFiles => "Propose file creation, deletion, or moves",
            Self::RunTool => "Run native tools with the editor's OS permissions",
            Self::Network => "Make network requests",
            Self::Credentials => "Access the plugin's saved login information",
            Self::Collaborate => "Call public services offered by other plugins",
        }
    }
}

/// Location of a live module, assigned by the host and never accepted from guest output.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Instance {
    /// Unique opening of a project within this editor process.
    pub project: u64,
    /// Manifest module ID within one plugin.
    pub module: String,
}

/// An authoritative document snapshot. Offsets are UTF-8 byte boundaries.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DocumentSnapshot {
    pub path: Option<PathBuf>,
    pub opening: u64,
    pub revision: u64,
    pub text: String,
}

/// A change proposed against the supplied document version.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Edit {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

/// An edit group for one open workspace document, bound to its exact opening and revision.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DocumentEdits {
    pub opening: u64,
    pub revision: u64,
    pub edits: Vec<Edit>,
}

/// An unopened UTF-8 project file observed by `ReadProjectText`.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ProjectText {
    pub path: String,
    pub version: String,
    /// LF newlines and UTF-8 byte positions, matching open document snapshots.
    pub text: String,
}

/// Proposed changes to a version of an unopened project file. Open buffers use `DocumentEdits`.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ProjectFileEdits {
    pub path: String,
    pub version: String,
    pub edits: Vec<Edit>,
}

/// A proposed project file change. The host previews the whole plan before executing.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum FileOperation {
    Create { path: String, text: String },
    Remove { path: String },
    Move { from: String, to: String },
}

/// Changes to a saved secret slot. The event itself never carries the secret.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CredentialChange {
    pub slot: String,
    pub revision: u64,
}

/// One event from the editor to a module.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum PluginEvent {
    Open,
    DocumentChanged,
    DraftChanged,
    Command {
        id: String,
        argument: Value,
    },
    Format {
        selection: Option<[usize; 2]>,
        /// Destination when formatting during a save to a different path.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<PathBuf>,
    },
    Complete {
        offset: usize,
    },
    /// Find definitions of the symbol at the active document's UTF-8 byte offset.
    Definition {
        offset: usize,
    },
    Diagnose,
    Ui {
        action: String,
        argument: Value,
    },
    SettingsChanged,
    CredentialChanged {
        change: CredentialChange,
    },
    Layout,
    /// The host needs a complete Canvas with a new baseline before accepting more deltas.
    CanvasResync,
    ServiceResult {
        result: ServiceResult,
    },
    ServiceCall {
        name: String,
        argument: Value,
    },
}

/// Guest input. The document is included only when the corresponding grant allows it.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Input {
    pub event: PluginEvent,
    pub document: Option<DocumentSnapshot>,
    pub settings: BTreeMap<String, Value>,
    pub draft: String,
    #[serde(default)]
    pub viewport: Option<canvas::Viewport>,
    /// Returned only after this module opts in by submitting a sequenced complete Canvas.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canvas_sync: Option<canvas::Sync>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_documents: Vec<DocumentSnapshot>,
}

/// Bounded response decoded from the guest. The host checks grants and versions before use.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Output {
    pub message: String,
    /// Explain why this module cannot use the settings in Open or SettingsChanged.
    /// The host retains the saved values and lets the user retry or correct them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settings_error: Option<String>,
    /// Report a formatting failure so the host can discard the edits and continue a pending save.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format_error: Option<String>,
    /// Set only after a ReadCredential service reply to report rejected authentication.
    /// The host displays a fixed message and keeps the currently saved credential.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub credential_rejected: bool,
    pub edits: Vec<Edit>,
    pub document_edits: Vec<DocumentEdits>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub project_file_edits: Vec<ProjectFileEdits>,
    pub file_operations: Vec<FileOperation>,
    pub completions: Vec<String>,
    /// Labeled replacements for completions that consume an existing prefix.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub completion_items: Vec<language::Completion>,
    /// Host-populated definition locations, with LSP line and UTF-16 column coordinates.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub definitions: Vec<language::Definition>,
    pub diagnostics: Vec<Diagnostic>,
    /// Presentation data for the document revision supplied with this call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_features: Option<language::DocumentFeatures>,
    pub surface: Option<GridSurface>,
    /// An incremental replacement for the current Canvas. Send either this or `surface`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canvas_delta: Option<canvas::Delta>,
    pub services: Vec<ServiceRequest>,
    /// Finish the current invocation with the reply to its single awaited service.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub await_service: bool,
    pub service_value: Option<Value>,
}

/// Ask the host for a permission-checked asynchronous operation.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ServiceRequest {
    pub token: u32,
    pub operation: Service,
}

/// The host binds a credential to the caller's installation and profile.
#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "service", rename_all = "snake_case", deny_unknown_fields)]
pub enum Service {
    ReadCredential {
        key: String,
    },
    SaveCredential {
        key: String,
        secret: String,
    },
    DeleteCredential {
        key: String,
    },
    ParseDocument {
        language: String,
    },
    /// Run a Tree-sitter query against the supplied version of the active document.
    QuerySyntax {
        language: String,
        query: String,
    },
    ReadProjectText {
        path: String,
    },
    /// Read these basenames from the active file's directory up to the opened project root.
    ReadProjectAncestors {
        names: Vec<String>,
    },
    /// Format the active Rust snapshot with the project's native rustfmt, returning a UTF-8 string.
    /// Available during an awaited Format invocation with native execution granted for this project.
    Rustfmt,
    MeasureText {
        lines: Vec<canvas::MeasureText>,
    },
    StageSettings {
        scope: SettingsScope,
        values: BTreeMap<String, Value>,
    },
    RunTool {
        tool: String,
        args: Vec<String>,
    },
    /// Start a declared rebuildable tool after an explicit user action.
    StartBackgroundTool {
        tool: String,
    },
    /// Read the current phase and acknowledged result of this module's background tool.
    BackgroundToolStatus {
        tool: String,
    },
    CallPlugin {
        name: String,
        id: String,
        method: String,
        argument: Value,
    },
}

impl std::fmt::Debug for Service {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::ReadCredential { .. } => "ReadCredential",
            Self::SaveCredential { .. } => "SaveCredential",
            Self::DeleteCredential { .. } => "DeleteCredential",
            Self::ParseDocument { .. } => "ParseDocument",
            Self::QuerySyntax { .. } => "QuerySyntax",
            Self::ReadProjectText { .. } => "ReadProjectText",
            Self::ReadProjectAncestors { .. } => "ReadProjectAncestors",
            Self::Rustfmt => "Rustfmt",
            Self::MeasureText { .. } => "MeasureText",
            Self::StageSettings { .. } => "StageSettings",
            Self::RunTool { .. } => "RunTool",
            Self::StartBackgroundTool { .. } => "StartBackgroundTool",
            Self::BackgroundToolStatus { .. } => "BackgroundToolStatus",
            Self::CallPlugin { .. } => "CallPlugin",
        })
    }
}

/// A complete versioned input for a rebuildable native service over JSON Lines.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolSnapshot {
    pub protocol: u8,
    pub revision: u64,
    pub documents: Vec<DocumentSnapshot>,
    /// Number of editor snapshots omitted by the transport limits.
    pub omitted_documents: usize,
    /// Release rebuildable caches before acknowledging this snapshot.
    #[serde(default)]
    pub discard_rebuildable_cache: bool,
}

/// A native service acknowledges the exact snapshot it processed.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolSnapshotReply {
    pub revision: u64,
    /// Optional rebuildable data for the owning plugin.
    #[serde(default)]
    pub result: Value,
}

/// Which settings draft a custom settings surface is editing.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingsScope {
    AllProjects,
    CurrentProject,
    CurrentProjectLocal,
}

/// Result of a service request. Its data is redacted from Debug output.
#[derive(Clone, Deserialize, Serialize)]
pub struct ServiceResult {
    pub token: u32,
    pub value: Result<Value, String>,
}

impl std::fmt::Debug for ServiceResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceResult")
            .field("token", &self.token)
            .finish_non_exhaustive()
    }
}

/// A diagnostic in the supplied document snapshot's UTF-8 coordinate space.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Diagnostic {
    pub start: usize,
    pub end: usize,
    pub message: String,
}

/// High-level character grid view. Input text remains host-owned between frames.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct GridSurface {
    pub title: String,
    pub lines: Vec<String>,
    pub input: Option<String>,
    pub canvas: Option<canvas::Canvas>,
}

/// Host requests accepted by the private worker protocol.
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "request", rename_all = "snake_case")]
pub enum Request {
    Inspect {
        id: u64,
        modules: Vec<PathBuf>,
    },
    Open {
        id: u64,
        instance: Instance,
        wasm: PathBuf,
        input: Box<Input>,
        interactive: bool,
    },
    Call {
        id: u64,
        instance: Instance,
        input: Box<Input>,
        interactive: bool,
    },
    RefreshDocument {
        id: u64,
        instance: Instance,
        input: Box<Input>,
        interactive: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        delta: Option<document_wire::DocumentDelta>,
    },
    Close {
        id: u64,
        project: u64,
    },
    TrimCache,
    Evaluate {
        id: u64,
        source: String,
        context: Value,
        remaining_ms: u64,
    },
    CancelRule {
        id: u64,
    },
    CheckRule {
        id: u64,
        source: String,
    },
    Parse {
        id: u64,
        language: String,
        document: Box<DocumentSnapshot>,
    },
    /// Query a grammar in the same worker and tree cache as Parse.
    QuerySyntax {
        id: u64,
        language: String,
        query: String,
        document: Box<DocumentSnapshot>,
    },
    Data {
        id: u64,
        query: u32,
        result: Result<Value, String>,
    },
}

/// The host associates a response with the child pipe and its outstanding request.
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum Reply {
    Hello {
        wire: u32,
    },
    Inspected {
        id: u64,
    },
    Result {
        id: u64,
        output: Box<Output>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timing: Option<WorkerTiming>,
    },
    DocumentResync {
        id: u64,
    },
    Closed {
        id: u64,
        project: u64,
    },
    Rule {
        id: u64,
        matches: bool,
    },
    RuleChecked {
        id: u64,
    },
    Parsed {
        id: u64,
        value: Value,
    },
    Query {
        id: u64,
        query: u32,
        kind: Query,
        args: Value,
    },
    Error {
        id: u64,
        kind: ErrorKind,
        message: String,
    },
}

/// Sidecar timing separates execution from the host and worker queues.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkerTiming {
    pub queue_us: u64,
    pub compute_us: u64,
    pub compile_us: u64,
    pub initialize_us: u64,
}

/// Rule data queries. These have no network or execution operation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Query {
    Exists,
    ReadFile,
    ReadBytes,
    ScanText,
    Document,
    /// Active document path, tracked independently from its text revision.
    DocumentPath,
    List,
}

/// Distinguishes failures that affect scheduling and recovery.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    Invalid,
    Busy,
    Cancelled,
    Computation,
    Deadline,
    Failed,
}

/// Send a bounded JSON line without interleaving it with other messages.
pub fn send(out: &mut impl Write, value: &impl Serialize) -> io::Result<()> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > MAX_MESSAGE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "plugin message too large",
        ));
    }
    out.write_all(&bytes)?;
    out.write_all(b"\n")?;
    out.flush()
}

/// Read a single bounded JSON line. A partial message at EOF is invalid.
pub fn receive<T: DeserializeOwned>(input: &mut impl BufRead) -> io::Result<Option<T>> {
    receive_counted(input).map(|message| message.map(|(value, _)| value))
}

/// Read the message and its wire size without serializing decoded data again.
pub fn receive_counted<T: DeserializeOwned>(
    input: &mut impl BufRead,
) -> io::Result<Option<(T, usize)>> {
    let mut data = Vec::new();
    loop {
        let buf = input.fill_buf()?;
        if buf.is_empty() {
            return if data.is_empty() {
                Ok(None)
            } else {
                Err(io::ErrorKind::UnexpectedEof.into())
            };
        }
        let delimiter = buf.iter().position(|byte| *byte == b'\n');
        let count = delimiter.unwrap_or(buf.len());
        if data.len() + count > MAX_MESSAGE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "plugin message too large",
            ));
        }
        data.extend_from_slice(&buf[..count]);
        input.consume(count + usize::from(delimiter.is_some()));
        if delimiter.is_some() {
            return Ok(Some((serde_json::from_slice(&data)?, data.len() + 1)));
        }
    }
}
