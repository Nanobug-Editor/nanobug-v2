//! Opt-in diagnostic timestamps written off the native input thread.

use std::{
    fs::File,
    io::{BufWriter, Write},
    path::PathBuf,
    sync::mpsc,
    time::{SystemTime, UNIX_EPOCH},
};

/// Bounded performance trace. It stores event names, counters and timestamps only.
#[derive(Clone)]
pub struct Trace(mpsc::SyncSender<serde_json::Value>);

impl Trace {
    pub(super) fn new(path: PathBuf) -> Self {
        let (send, receive) = mpsc::sync_channel(128);
        std::thread::spawn(move || {
            let Ok(file) = File::create(path) else {
                return;
            };
            let mut output = BufWriter::new(file);
            while let Ok(row) = receive.recv() {
                if serde_json::to_writer(&mut output, &row).is_err()
                    || output
                        .write_all(b"\n")
                        .and_then(|()| output.flush())
                        .is_err()
                {
                    break;
                }
            }
        });
        Self(send)
    }

    /// Record a milestone without waiting for a file write.
    pub fn mark(&self, event: &str, data: serde_json::Value) {
        let at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as u64;
        let _ = self
            .0
            .try_send(serde_json::json!({ "event": event, "at_us": at, "data": data }));
    }
}
