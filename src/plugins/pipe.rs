//! Bounded off-thread pipe I/O. Each reply is bound to its owning Child.

use anyhow::{Context, Result, ensure};
use nanobug_plugin_sdk::{Reply, Request, WIRE, receive_counted, send};
use std::{
    io::{BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread::Thread,
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

/// The runtime installed beside the editor binary, including Cargo integration test binaries.
pub(super) fn runtime_path() -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let mut dir = exe.parent().context("editor has no executable directory")?;
    if dir.file_name().is_some_and(|name| name == "deps") {
        dir = dir.parent().context("invalid Cargo executable directory")?;
    }
    Ok(dir.join(format!(
        "nanobug-plugin-runtime{}",
        std::env::consts::EXE_SUFFIX
    )))
}

/// A single role. Its queues are independent of the editor's native event loop.
pub(super) struct Worker {
    child: Child,
    requests: Option<mpsc::SyncSender<Outgoing>>,
    replies: mpsc::Receiver<Incoming>,
    queues: Arc<Queues>,
}

/// These counters cover outstanding allocations, including the pipe writer's current buffer.
#[derive(Default)]
struct Queues {
    requests: AtomicUsize,
    replies: AtomicUsize,
    last_queue_us: AtomicUsize,
    peak_requests: AtomicUsize,
    peak_replies: AtomicUsize,
}

/// The receive lane has already decoded a reply, so its wire size is an equivalent measure.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct QueueStatus {
    pub request_bytes: usize,
    pub reply_wire_bytes: usize,
    pub last_write_wait_us: usize,
    pub peak_request_bytes: usize,
    pub peak_reply_wire_bytes: usize,
}

/// The native metric used to measure memory charged to one owned process.
pub const PROCESS_MEMORY_LABEL: &str = if cfg!(windows) {
    "private committed memory"
} else if cfg!(target_os = "linux") {
    "private resident memory"
} else if cfg!(target_os = "macos") {
    "physical footprint"
} else {
    "process memory"
};

struct Outgoing {
    bytes: Zeroizing<Vec<u8>>,
    queued: Instant,
    queues: Arc<Queues>,
}

impl Drop for Outgoing {
    fn drop(&mut self) {
        self.queues
            .requests
            .fetch_sub(self.bytes.capacity(), Ordering::Relaxed);
    }
}

struct Incoming {
    reply: Option<Reply>,
    wire_bytes: usize,
    queues: Arc<Queues>,
}

impl Incoming {
    fn take(mut self) -> Reply {
        self.reply.take().expect("queued worker reply")
    }
}

impl Drop for Incoming {
    fn drop(&mut self) {
        self.queues
            .replies
            .fetch_sub(self.wire_bytes, Ordering::Relaxed);
    }
}

impl Worker {
    /// Start and handshake with the bundled worker on a background thread.
    pub(super) fn start(role: &str, cache: Option<&Path>) -> Result<Self> {
        Self::start_with_wake(role, cache, None)
    }

    /// Wake the supervisor when a reply arrives or the worker exits.
    pub(super) fn start_with_wake(
        role: &str,
        cache: Option<&Path>,
        supervisor: Option<Thread>,
    ) -> Result<Self> {
        let mut command = Command::new(runtime_path()?);
        command.arg(role);
        if let Some(cache) = cache {
            command.arg(cache);
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        let mut child = command
            .spawn()
            .context("start bundled Nanobug plugin worker")?;
        let mut input = child.stdin.take().expect("worker stdin is piped");
        let mut output = BufReader::new(child.stdout.take().expect("worker stdout is piped"));
        let queues = Arc::new(Queues::default());
        let (requests, queue) = mpsc::sync_channel::<Outgoing>(32);
        let (results, replies) = mpsc::sync_channel(64);
        std::thread::spawn(move || {
            while let Ok(request) = queue.recv() {
                request.queues.last_queue_us.store(
                    usize::try_from(request.queued.elapsed().as_micros()).unwrap_or(usize::MAX),
                    Ordering::Relaxed,
                );
                if input
                    .write_all(&request.bytes)
                    .and_then(|()| input.flush())
                    .is_err()
                {
                    break;
                }
            }
        });
        let response_queues = Arc::clone(&queues);
        std::thread::spawn(move || {
            while let Ok(Some((reply, wire_bytes))) = receive_counted(&mut output) {
                let queued = response_queues
                    .replies
                    .fetch_add(wire_bytes, Ordering::Relaxed)
                    + wire_bytes;
                response_queues
                    .peak_replies
                    .fetch_max(queued, Ordering::Relaxed);
                if results
                    .send(Incoming {
                        reply: Some(reply),
                        wire_bytes,
                        queues: Arc::clone(&response_queues),
                    })
                    .is_err()
                {
                    break;
                }
                if let Some(supervisor) = &supervisor {
                    supervisor.unpark();
                }
            }
            drop(results);
            if let Some(supervisor) = supervisor {
                supervisor.unpark();
            }
        });
        let worker = Self {
            child,
            requests: Some(requests),
            replies,
            queues,
        };
        ensure!(
            matches!(
                worker.wait(Duration::from_secs(3))?,
                Reply::Hello { wire: WIRE }
            ),
            "incompatible bundled worker protocol"
        );
        Ok(worker)
    }

    /// The process identity used for diagnostics.
    pub(super) fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Stop this owned child while leaving the host's recovery path intact.
    #[cfg(test)]
    pub(super) fn kill_for_test(&mut self) -> std::io::Result<()> {
        self.child.kill()?;
        self.child.wait().map(|_| ())
    }

    /// Return private committed bytes for this exact owned child where the OS exposes them.
    pub(super) fn private_bytes(&self) -> Option<u64> {
        process_private_bytes(&self.child)
    }

    pub(super) fn queue_status(&self) -> QueueStatus {
        QueueStatus {
            request_bytes: self.queues.requests.load(Ordering::Relaxed),
            reply_wire_bytes: self.queues.replies.load(Ordering::Relaxed),
            last_write_wait_us: self.queues.last_queue_us.load(Ordering::Relaxed),
            peak_request_bytes: self.queues.peak_requests.load(Ordering::Relaxed),
            peak_reply_wire_bytes: self.queues.peak_replies.load(Ordering::Relaxed),
        }
    }

    /// Enqueue a bounded request, reporting busy or disconnected without losing caller state.
    pub(super) fn post(&self, request: impl std::borrow::Borrow<Request>) -> Result<()> {
        let mut bytes = Zeroizing::new(Vec::new());
        send(&mut *bytes, request.borrow()).context("plugin request exceeds protocol limits")?;
        let requests = self.requests.as_ref().context("plugin worker is closing")?;
        let queued_bytes = self
            .queues
            .requests
            .fetch_add(bytes.capacity(), Ordering::Relaxed)
            + bytes.capacity();
        let queued = Outgoing {
            bytes,
            queued: Instant::now(),
            queues: Arc::clone(&self.queues),
        };
        requests.try_send(queued).map_err(|error| match error {
            mpsc::TrySendError::Full(_) => anyhow::anyhow!("Plugin worker queue is full"),
            mpsc::TrySendError::Disconnected(_) => anyhow::anyhow!("Plugin worker is disconnected"),
        })?;
        self.queues
            .peak_requests
            .fetch_max(queued_bytes, Ordering::Relaxed);
        Ok(())
    }

    /// Read a ready event without waiting.
    pub(super) fn poll(&self) -> Result<Option<Reply>, mpsc::TryRecvError> {
        match self.replies.try_recv() {
            Ok(reply) => Ok(Some(reply.take())),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Wait up to a deadline in an installation or other background job.
    pub(super) fn wait(&self, timeout: Duration) -> Result<Reply> {
        Ok(self.replies.recv_timeout(timeout)?.take())
    }

    /// Stop the runtime after it acknowledged the release of its last project.
    pub(super) fn stop(mut self) {
        self.requests.take();
        for _ in 0..25 {
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(4));
        }
    }
}

/// Sample only an owned process handle, never a process looked up by a reusable PID.
pub(super) fn process_private_bytes(child: &Child) -> Option<u64> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::ProcessStatus::{
            K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX,
        };
        let mut info = PROCESS_MEMORY_COUNTERS_EX::default();
        let length = std::mem::size_of_val(&info) as u32;
        info.cb = length;
        let ok = unsafe {
            K32GetProcessMemoryInfo(child.as_raw_handle(), (&raw mut info).cast(), length)
        };
        (ok != 0).then_some(info.PrivateUsage as u64)
    }
    #[cfg(target_os = "linux")]
    {
        use std::io::Read;

        // A Child owns this PID until it is reaped. smaps_rollup excludes shared RSS.
        let mut text = String::new();
        std::fs::File::open(format!("/proc/{}/smaps_rollup", child.id()))
            .ok()?
            .take(16 * 1024)
            .read_to_string(&mut text)
            .ok()?;
        let mut bytes = 0_u64;
        let mut fields = 0;
        for line in text.lines() {
            let Some((name, data)) = line.split_once(':') else {
                continue;
            };
            if matches!(name, "Private_Clean" | "Private_Dirty" | "Private_Hugetlb") {
                let amount: u64 = data.split_ascii_whitespace().next()?.parse().ok()?;
                bytes = bytes.checked_add(amount.checked_mul(1024)?)?;
                fields += 1;
            }
        }
        (fields >= 2).then_some(bytes)
    }
    #[cfg(target_os = "macos")]
    {
        let mut usage = std::mem::MaybeUninit::<libc::rusage_info_v0>::uninit();
        let pid = i32::try_from(child.id()).ok()?;
        // SAFETY: flavor V0 fills the matching buffer for this child, which has not been reaped.
        let status =
            unsafe { libc::proc_pid_rusage(pid, libc::RUSAGE_INFO_V0, usage.as_mut_ptr().cast()) };
        (status == 0).then(|| unsafe { usage.assume_init().ri_phys_footprint })
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        let _ = child;
        None
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.requests.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Validate rule bodies without evaluating them or exposing project data.
pub(super) fn check_scripts(scripts: Vec<String>) -> Result<()> {
    if scripts.is_empty() {
        return Ok(());
    }
    let worker = Worker::start("rules", None)?;
    for (index, source) in scripts.into_iter().enumerate() {
        let id = index as u64;
        worker.post(Request::CheckRule { id, source })?;
        match worker.wait(Duration::from_secs(5))? {
            Reply::RuleChecked { id: received } if received == id => {}
            Reply::Error {
                id: received,
                message,
                ..
            } if received == id => anyhow::bail!("{message}"),
            _ => anyhow::bail!("Invalid syntax check reply"),
        }
    }
    worker.stop();
    Ok(())
}
