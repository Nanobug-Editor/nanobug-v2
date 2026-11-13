//! Console sidecar for independently isolated plugin and rule roles.

mod rules;
mod syntax;
mod wasm;

use nanobug_plugin_sdk::{ErrorKind, Reply, WIRE, send};
use std::{
    io::{self, BufReader},
    sync::{Arc, Mutex},
};

/// Serialize worker replies from independent execution lanes.
#[derive(Clone, Default)]
struct Output(Arc<Mutex<()>>);

impl Output {
    /// Send one reply on this worker's stdout.
    fn send(&self, reply: Reply) -> io::Result<()> {
        let _guard = self.0.lock().unwrap();
        send(&mut io::stdout().lock(), &reply)
    }

    /// Send an error correlated with the original request.
    fn error(&self, id: u64, kind: ErrorKind, message: impl std::fmt::Display) -> io::Result<()> {
        self.send(Reply::Error {
            id,
            kind,
            message: message.to_string().chars().take(2048).collect(),
        })
    }
}

/// A worker runs one role until its owning editor closes the input pipe.
fn main() -> anyhow::Result<()> {
    let mut args = std::env::args_os().skip(1);
    let role = args.next();
    let input = BufReader::new(io::stdin().lock());
    let output = Output::default();
    match role.as_deref().and_then(|value| value.to_str()) {
        Some("wasm") => {
            let engine = wasm::engine(args.next().map(std::path::PathBuf::from).as_deref())?;
            output.send(Reply::Hello { wire: WIRE })?;
            wasm::serve(input, output, engine)
        }
        Some("rules") => {
            output.send(Reply::Hello { wire: WIRE })?;
            rules::serve(input, output)
        }
        Some("syntax") => {
            output.send(Reply::Hello { wire: WIRE })?;
            syntax::serve(input, output)
        }
        _ => anyhow::bail!("expected worker role: wasm or rules"),
    }
}
