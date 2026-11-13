//! Console entry point for the two P0 worker roles.

mod rules;
mod wasm;

use nanobug_probe_wire::{Event, send};
use std::{
    io::{self, BufReader},
    sync::{Arc, Mutex},
};

/// Serialize each bounded event before allowing another lane to write to stdout.
#[derive(Clone, Default)]
struct Output(Arc<Mutex<()>>);

impl Output {
    /// Send `event` to the host without interleaving its bytes with other replies.
    fn send(&self, event: Event) -> io::Result<()> {
        let _guard = self.0.lock().expect("probe output lock");
        send(&mut io::stdout().lock(), &event)
    }
}

/// Start only the named role. Stdin closing ends the worker's request stream.
fn main() -> anyhow::Result<()> {
    let input = BufReader::new(io::stdin().lock());
    let output = Output::default();
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("rules") => rules::serve(input, output),
        Some("wasm") => wasm::serve(
            input,
            output,
            args.next().as_deref() == Some("--inject-retired-frame"),
        ),
        _ => anyhow::bail!("expected worker role: rules or wasm"),
    }
}
