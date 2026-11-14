//! Prepare and own native tool processes without logging their arguments.

mod archive;
pub(super) mod background;
mod receipt;
pub(super) mod rust_analyzer;
mod rustfmt;

use super::package::{InstallStore, Installed, ToolDecl};
use anyhow::{Context, Result, ensure};
use nanobug_plugin_sdk::{DocumentSnapshot, document_wire::MAX_DOCUMENT_BYTES};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdout, Command, ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

pub(super) use receipt::review;
pub use receipt::{ToolOperation, ToolOperationState};

const OUTPUT_LIMIT: usize = 64 * 1024;

/// Keep the managed archive reserved for the entire lifetime of an executable use.
#[derive(Debug)]
pub(super) struct PreparedExecutable {
    path: PathBuf,
    _lease: Option<archive::Lease>,
}

impl PreparedExecutable {
    pub(super) fn path(&self) -> &Path {
        &self.path
    }
}

/// Package and configuration identity captured when the user authorizes a command.
#[derive(Clone)]
pub(super) struct Source {
    pub store: InstallStore,
    pub record: Arc<Installed>,
    pub declaration: ToolDecl,
    pub configured_path: Option<String>,
    pub project: PathBuf,
    pub closed: Arc<AtomicBool>,
}

impl Source {
    /// Resolve user-selected tools first, then a bundled or verified downloadable asset.
    pub(super) fn executable(&self) -> Result<PreparedExecutable> {
        ensure!(
            !self.closed.load(Ordering::Acquire),
            "Project closed before tool preparation"
        );
        if let Some(path) = &self.configured_path {
            let path = existing(&self.project, Path::new(path))?;
            let lease = archive::lease_existing(self.store.state_root(), &path, &self.closed)?;
            return Ok(PreparedExecutable {
                path,
                _lease: lease,
            });
        }
        if let Some(path) = &self.declaration.executable {
            let path = self.store.verified_asset(&self.record.release, path)?;
            return Ok(PreparedExecutable {
                path: existing(&self.project, &path)?,
                _lease: None,
            });
        }
        let download = self
            .declaration
            .downloads
            .get(&self.store.compatibility().platform)
            .context(
                "No native tool archive for this platform. Configure an existing tool path.",
            )?;
        ensure!(
            self.record
                .grants
                .contains(&nanobug_plugin_sdk::Permission::Network),
            "Native tool downloads require network permission"
        );
        archive::prepare(self.store.state_root(), download, &self.closed)
    }
}

/// Authority and input checked by the host before launching a native command once.
pub(super) struct Run {
    pub profile: PathBuf,
    pub namespace: String,
    pub tool: String,
    pub project: PathBuf,
    pub executable: PathBuf,
    pub args: Vec<String>,
    pub closed: Arc<AtomicBool>,
    pub timeout: Duration,
}

pub(super) fn check_args(args: &[String]) -> Result<()> {
    ensure!(
        args.len() <= 64
            && args.iter().map(String::len).sum::<usize>() <= 8192
            && args.iter().all(|arg| !arg.contains('\0')),
        "Native tool arguments exceed limits or contain NUL"
    );
    Ok(())
}

pub(super) fn execute(run: Run) -> Result<Value> {
    check_run(&run)?;
    let operation = receipt::Operation::start(&run.profile, &run.namespace, &run.tool)?;
    let mut command = command(&run.executable, &run.project);
    command.args(&run.args).stdin(Stdio::null());
    let child = if run.closed.load(Ordering::Acquire) {
        operation.not_started()?;
        anyhow::bail!("Project closed before tool start");
    } else {
        match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                operation.not_started()?;
                return Err(error).context("Could not start declared native tool");
            }
        }
    };
    let pending = || format!("Tool operation {}; result pending", operation.id());
    let (status, output) = collect(
        Process::own(child).with_context(pending)?,
        &run,
        |stdout| Ok(capture(stdout)?),
        |status| {
            // Publish the exit before output collection or delivery to the requesting runtime.
            operation.exited(status.code())
        },
    )
    .with_context(pending)?;
    Ok(json!({
        "operation": operation.id(), "state": "exited", "code": status.code(),
        "stdout": output.as_ref().map(|data| String::from_utf8_lossy(&data.bytes)).unwrap_or_default(),
        "truncated": output.as_ref().is_none_or(|data| data.truncated),
    }))
}

fn check_run(run: &Run) -> Result<()> {
    ensure!(
        !run.closed.load(Ordering::Acquire),
        "Project closed before tool start"
    );
    check_args(&run.args)?;
    ensure!(
        run.executable.is_absolute() && run.executable.is_file(),
        "Native tool executable is unavailable"
    );
    Ok(())
}

/// A known executable with no shell, no visible console, and an owned process group.
fn command(executable: &Path, directory: &Path) -> Command {
    let mut command = Command::new(executable);
    command
        .current_dir(directory)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command
}

/// Format a snapshot over pipes. Only the caller can apply the returned text to the buffer.
pub(super) fn format_rust(
    root: &Path,
    path: &Path,
    document: &DocumentSnapshot,
    deadline: Instant,
    closed: &AtomicBool,
) -> Result<String> {
    let check = || {
        ensure!(
            !closed.load(Ordering::Acquire),
            "Project execution was stopped"
        );
        ensure!(Instant::now() < deadline, "rustfmt timed out");
        Ok(())
    };
    check()?;
    let mut launch = rustfmt::command(root, path)?;
    launch.stdin(Stdio::piped()).stderr(Stdio::piped());
    check()?;
    let mut process = Process::own(launch.spawn().context("Could not start rustfmt")?)?;
    let mut stdin = process
        .child
        .stdin
        .take()
        .context("rustfmt stdin is unavailable")?;
    let stdout = process
        .child
        .stdout
        .take()
        .context("rustfmt stdout is unavailable")?;
    let stderr = process
        .child
        .stderr
        .take()
        .context("rustfmt stderr is unavailable")?;
    let source = document.text.clone();
    let (writer, written) = mpsc::sync_channel(1);
    // Drain both streams while supplying stdin so large snapshots cannot block the child.
    std::thread::spawn(move || {
        let result = stdin.write_all(source.as_bytes());
        drop(stdin);
        let _ = writer.send(result);
    });
    let output = read_rustfmt_pipe(stdout, MAX_DOCUMENT_BYTES);
    let errors = read_rustfmt_pipe(stderr, OUTPUT_LIMIT);
    let status = loop {
        check()?;
        if let Some(status) = process.child.try_wait()? {
            process.group.take();
            break status;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let remaining = || deadline.saturating_duration_since(Instant::now());
    if !status.success() {
        let stderr = errors.recv_timeout(remaining()).ok().and_then(Result::ok);
        let details = stderr.as_deref().map(String::from_utf8_lossy);
        let summary: String = details
            .as_deref()
            .and_then(|text| text.lines().find(|line| !line.trim().is_empty()))
            .unwrap_or("Check the Rust syntax and installed rustfmt component")
            .chars()
            .filter(|c| !c.is_control())
            .take(240)
            .collect();
        anyhow::bail!("rustfmt failed: {summary}");
    }
    written
        .recv_timeout(remaining())
        .context("rustfmt did not finish reading its input")??;
    let bytes = output
        .recv_timeout(remaining())
        .context("rustfmt did not finish writing its output")??;
    check()?;
    ensure!(
        bytes.len() <= MAX_DOCUMENT_BYTES,
        "rustfmt output exceeds the plugin document size limit"
    );
    ensure!(
        !bytes.is_empty() || document.text.trim().is_empty(),
        "rustfmt returned no text"
    );
    let text = String::from_utf8(bytes).context("rustfmt output is not UTF-8")?;
    Ok(if text.contains("\r\n") {
        text.replace("\r\n", "\n")
    } else {
        text
    })
}

/// Keep a bounded prefix while draining the pipe to EOF so the process can exit normally.
fn read_rustfmt_pipe(
    mut pipe: impl Read + Send + 'static,
    limit: usize,
) -> mpsc::Receiver<std::io::Result<Vec<u8>>> {
    let (send, receive) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = (|| {
            pipe.by_ref()
                .take(limit as u64 + 1)
                .read_to_end(&mut bytes)?;
            std::io::copy(&mut pipe, &mut std::io::sink())?;
            Ok(bytes)
        })();
        let _ = send.send(result);
    });
    receive
}

struct Captured {
    bytes: Vec<u8>,
    truncated: bool,
}

fn capture(mut source: impl Read) -> std::io::Result<Captured> {
    let mut bytes = Vec::new();
    let mut truncated = false;
    let mut block = [0; 8192];
    loop {
        let count = source.read(&mut block)?;
        if count == 0 {
            return Ok(Captured { bytes, truncated });
        }
        let kept = count.min(OUTPUT_LIMIT - bytes.len());
        bytes.extend_from_slice(&block[..kept]);
        truncated |= kept != count;
    }
}

fn collect<T: Send + 'static>(
    mut process: Process,
    run: &Run,
    read_output: impl FnOnce(ChildStdout) -> Result<T> + Send + 'static,
    on_exit: impl FnOnce(ExitStatus) -> Result<()>,
) -> Result<(ExitStatus, Option<T>)> {
    let stdout = process
        .child
        .stdout
        .take()
        .context("Native tool output is unavailable")?;
    let (send, receive) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = send.send(read_output(stdout));
    });
    let deadline = Instant::now() + run.timeout;
    loop {
        if let Some(status) = process.child.try_wait()? {
            process.group.take();
            on_exit(status)?;
            return Ok((
                status,
                receive
                    .recv_timeout(Duration::from_secs(1))
                    .ok()
                    .and_then(Result::ok),
            ));
        }
        if Instant::now() >= deadline || run.closed.load(Ordering::Acquire) {
            anyhow::bail!(
                "Native tool did not finish before the project closed or deadline expired"
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Development builds are repeatable. They do not need a one-time tool operation journal.
pub(super) fn build<T: Send + 'static>(
    run: Run,
    read_output: impl FnOnce(ChildStdout) -> Result<T> + Send + 'static,
) -> Result<(Option<i32>, Option<T>)> {
    check_run(&run)?;
    let mut launch = command(&run.executable, &run.project);
    let process = Process::own(launch.args(&run.args).stdin(Stdio::null()).spawn()?)?;
    let (status, output) = collect(process, &run, read_output, |_| Ok(()))?;
    Ok((status.code(), output))
}

/// A process that is stopped and reaped even on an early return.
struct Process {
    child: Child,
    group: Option<ProcessGroup>,
}

impl Process {
    fn own(mut child: Child) -> Result<Self> {
        match ProcessGroup::attach(&child) {
            Ok(group) => Ok(Self {
                child,
                group: Some(group),
            }),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                Err(error)
            }
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.group.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(windows)]
struct ProcessGroup(windows_sys::Win32::Foundation::HANDLE);

// SAFETY: A Windows job handle is process-wide. This unique owner can move across threads.
#[cfg(windows)]
unsafe impl Send for ProcessGroup {}

#[cfg(windows)]
impl ProcessGroup {
    fn attach(child: &Child) -> Result<Self> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::JobObjects::*;
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        ensure!(!job.is_null(), "Could not create tool process group");
        let group = Self(job);
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let configured = unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        };
        ensure!(configured != 0, "Could not configure tool process group");
        ensure!(
            unsafe { AssignProcessToJobObject(job, child.as_raw_handle()) } != 0,
            "Could not own tool process"
        );
        Ok(group)
    }
}

#[cfg(windows)]
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
    }
}

#[cfg(unix)]
struct ProcessGroup(rustix::process::Pid);

#[cfg(unix)]
impl ProcessGroup {
    fn attach(child: &Child) -> Result<Self> {
        Ok(Self(
            rustix::process::Pid::from_raw(child.id() as i32).context("Invalid tool process ID")?,
        ))
    }
}

#[cfg(unix)]
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        let _ = rustix::process::kill_process_group(self.0, rustix::process::Signal::KILL);
    }
}

/// Resolve a user-selected existing executable, including paths relative to a project.
pub(super) fn existing(root: &Path, path: &Path) -> Result<PathBuf> {
    let path = root.join(path).canonicalize()?;
    ensure!(path.is_file(), "Native tool path is not a file");
    #[cfg(windows)]
    ensure!(
        path.extension()
            .and_then(|value| value.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("exe")),
        "Windows native tools must be executable files"
    );
    Ok(path)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn existing_tool_runs_in_the_project_and_records_one_outcome() -> Result<()> {
        let folder = tempfile::tempdir()?;
        fs::write(
            folder.path().join("sample.txt"),
            "synthetic tool fixture\r\n",
        )?;
        let executable = PathBuf::from(std::env::var_os("SystemRoot").context("Windows path")?)
            .join("System32/findstr.exe");
        let profile = folder.path().join("profile");
        let result = execute(Run {
            profile: profile.clone(),
            namespace: "1".repeat(64),
            tool: "find".into(),
            project: folder.path().to_path_buf(),
            executable,
            args: vec!["synthetic".into(), "sample.txt".into()],
            closed: Arc::new(AtomicBool::new(false)),
            timeout: Duration::from_secs(3),
        })?;
        assert_eq!(result["state"], "exited");
        assert_eq!(result["code"], 0);
        assert!(
            result["stdout"]
                .as_str()
                .is_some_and(|text| text.contains("synthetic tool fixture"))
        );
        let receipts = review(&profile, &"1".repeat(64))?;
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].id, result["operation"]);
        assert_eq!(receipts[0].state, ToolOperationState::Exited);
        assert_eq!(receipts[0].code, Some(0));
        Ok(())
    }
}
