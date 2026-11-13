//! Opt-in P0 host. Worker startup and pipe I/O live outside the GPUI thread.

mod session;

pub(crate) use session::{Client, Control, Project, Scene, Surface};

use serde_json::{Value, json};
use std::{
    ffi::OsString,
    fs::{File, OpenOptions},
    io::{self, Write},
    path::PathBuf,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

/// Explicit experiment settings, absent for ordinary launches.
pub(crate) struct Options {
    /// Where the background supervisor writes this run's JSON lines.
    trace: PathBuf,
    /// Measure the same host without any rule or plugin worker.
    baseline: bool,
    /// Hold the first project's host response past the JS deadline during the delay check.
    delay_first_read: bool,
    /// Entry time before native window setup.
    clock: Clock,
    /// Small set of startup timestamps captured before the trace file is opened.
    milestones: Vec<(&'static str, Instant)>,
}

impl Options {
    /// Consume the experiment flag and trace path from `args`, leaving normal paths in order.
    pub(crate) fn take(args: &mut Vec<OsString>) -> anyhow::Result<Option<Self>> {
        let clock = Clock::now();
        let (baseline, delay_first_read) = match args.first().and_then(|arg| arg.to_str()) {
            Some("--plugin-probe") => (false, false),
            Some("--plugin-probe-baseline") => (true, false),
            Some("--plugin-probe-delay-first") => (false, true),
            _ => return Ok(None),
        };
        anyhow::ensure!(
            args.len() >= 2,
            "plugin probe requires a new JSONL trace path"
        );
        let trace = args.remove(1).into();
        args.remove(0);
        Ok(Some(Self {
            trace,
            baseline,
            delay_first_read,
            clock,
            milestones: Vec::new(),
        }))
    }

    /// Record an early `name` without writing files on the startup path.
    pub(crate) fn milestone(&mut self, name: &'static str) {
        self.milestones.push((name, Instant::now()));
    }

    /// Record readiness at `ready` and start the background experiment. `editable` marks an open document.
    pub(crate) fn start(self, ready: Instant, editable: bool) -> Client {
        Client::start(self, ready, editable)
    }
}

/// Monotonic elapsed time with a wall-clock anchor for the Windows launch harness.
#[derive(Clone, Copy)]
struct Clock {
    /// Timing origin in this process.
    entry: Instant,
    /// Microseconds since Unix epoch at that origin.
    unix_us: u64,
}

impl Clock {
    /// Capture the timestamps used to relate host events to process creation.
    fn now() -> Self {
        Self {
            entry: Instant::now(),
            unix_us: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_micros() as u64,
        }
    }
}

/// The supervisor's trace output. The UI only supplies timestamps and bounded details.
struct Trace {
    /// A newly created trace file, private to this run.
    file: File,
    /// Origin shared by all events in the run.
    clock: Clock,
}

impl Trace {
    /// Open the explicitly selected trace destination without overwriting an existing file.
    fn open(options: &Options) -> io::Result<Self> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&options.trace)?;
        Ok(Self {
            file,
            clock: options.clock,
        })
    }

    /// Write `name` at monotonic time `at`, with experiment metadata but no user text.
    fn mark(&mut self, name: &'static str, at: Instant, detail: Value) {
        let us = at.saturating_duration_since(self.clock.entry).as_micros() as u64;
        let line = json!({ "event": name, "us": us, "unix_us": self.clock.unix_us + us, "detail": detail });
        let _ = writeln!(self.file, "{line}");
    }
}
