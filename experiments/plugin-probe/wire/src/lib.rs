//! Bounded messages between the P0 host and its local test workers.

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::io::{self, BufRead, Write};

/// Maximum serialized line, excluding its delimiter.
const MAX_LINE: usize = 16 * 1024;
/// Maximum UTF-8 bytes accepted by the fixture's single-line input surface.
pub const MAX_DRAFT: usize = 1024;
/// Maximum UTF-8 bytes returned by the fixture's one file query.
pub const MAX_FLAG: usize = 256;
/// Shared admission limit for independently executing or waiting rule evaluations.
pub const RULE_LANES: usize = 2;

/// Host-assigned scope and runtime lifetime, both required to accept a response.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Owner {
    /// A particular opening of a project.
    pub scope: u64,
    /// The runtime that serves that opening.
    pub generation: u64,
}

/// Host-confirmed input and its monotonic revision within a scope.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Draft {
    /// Increases before the host acknowledges any change.
    pub revision: u64,
    /// The complete bounded UTF-8 input value.
    pub text: String,
}

/// Rule fixtures exercised through the same worker boundary.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum Rule {
    /// Ask the host for the project flag and test its contents.
    Flag,
    /// Exercise the JavaScript interrupt while another rule can run.
    Busy,
}

/// One operation accepted by the relevant worker role.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "snake_case")]
pub enum Request {
    /// Begin one rule evaluation.
    Evaluate { id: u64, rule: Rule },
    /// Complete the one pending host file query for this evaluation.
    FileResult {
        id: u64,
        result: Result<String, String>,
    },
    /// Create a project instance with the current host draft.
    Open { owner: Owner, draft: Draft },
    /// Send the newest full draft to an existing instance.
    Input { owner: Owner, draft: Draft },
    /// Drop the instance and release its private store.
    Close { owner: Owner },
    /// Run the sample business job, optionally using its bounded busy-loop fixture.
    Work { owner: Owner, spin: bool },
}

/// Result categories used by the probe and its verification harness.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Failure {
    /// The worker cannot accept another evaluation or job now.
    Busy,
    /// JavaScript execution used its cumulative time budget.
    JsBudget,
    /// The rule's overall deadline expired, including time spent waiting for data.
    RuleDeadline,
    /// The sample WASM job consumed its fuel budget.
    WasmBudget,
    /// The caller or worker returned a concrete error.
    Error(String),
}

/// One observed worker event, carrying identities back to the host.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// The rule is awaiting a host-side file read. The host chooses the path.
    ReadFlag { id: u64 },
    /// Completion of a rule, with execution and total wall times in microseconds.
    RuleDone {
        id: u64,
        result: Result<bool, Failure>,
        js_us: u64,
        total_us: u64,
    },
    /// One complete sample frame copied out of a guest instance.
    Frame {
        owner: Owner,
        draft: Draft,
        title: String,
        color: u32,
        instances: usize,
    },
    /// The private store for the indicated scope was dropped.
    Closed { owner: Owner, instances: usize },
    /// The spin fixture has entered the business lane.
    WorkStarted { owner: Owner },
    /// Result and duration of a job in the WASM business lane.
    WorkDone {
        owner: Owner,
        spin: bool,
        result: Result<u32, Failure>,
        elapsed_us: u64,
    },
    /// Runtime could not handle the requested scope operation.
    ScopeError { owner: Owner, message: String },
}

/// Write one delimited message, rejecting an oversized payload before sending bytes.
pub fn send(writer: &mut impl Write, message: &impl Serialize) -> io::Result<()> {
    let bytes = serde_json::to_vec(message)?;
    if bytes.len() > MAX_LINE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "probe message too large",
        ));
    }
    writer.write_all(&bytes)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

/// Read a complete bounded line. EOF between messages means the peer has closed.
pub fn receive<T: DeserializeOwned>(reader: &mut impl BufRead) -> io::Result<Option<T>> {
    let mut line = Vec::new();
    loop {
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Err(io::ErrorKind::UnexpectedEof.into())
            };
        }
        let end = chunk.iter().position(|&byte| byte == b'\n');
        let count = end.unwrap_or(chunk.len());
        if line.len() + count > MAX_LINE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "probe message too large",
            ));
        }
        line.extend_from_slice(&chunk[..count]);
        reader.consume(count + usize::from(end.is_some()));
        if end.is_some() {
            return Ok(Some(serde_json::from_slice(&line)?));
        }
    }
}
