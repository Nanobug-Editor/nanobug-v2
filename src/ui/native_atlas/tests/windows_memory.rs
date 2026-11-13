//! Sample the probe's own memory and Windows' per-process GPU accounting.

use anyhow::{Context as _, Result, ensure};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    os::windows::process::CommandExt,
    path::PathBuf,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};
use windows_sys::Win32::System::{
    ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX},
    Threading::GetCurrentProcess,
};

pub(super) struct MemorySampler {
    query: Child,
    requests: ChildStdin,
    replies: BufReader<ChildStdout>,
}

impl MemorySampler {
    pub(super) fn new() -> Result<Self> {
        let executable = PathBuf::from(std::env::var_os("SystemRoot").context("Windows folder")?)
            .join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let script = r#"
while ($null -ne [Console]::ReadLine()) {
    try {
        $rows = @(Get-CimInstance Win32_PerfRawData_GPUPerformanceCounters_GPUProcessMemory -Filter "Name LIKE 'pid_${env:NANOBUG_ATLAS_PID}_%'" -ErrorAction Stop |
            ForEach-Object {
                [pscustomobject]@{adapter = $_.Name; dedicated_bytes = $_.DedicatedUsage; shared_bytes = $_.SharedUsage; committed_bytes = $_.TotalCommitted}
            })
        [Console]::WriteLine((ConvertTo-Json -InputObject $rows -Compress))
    } catch {
        [Console]::WriteLine('null')
    }
}
"#;
        let mut query = Command::new(executable)
            .creation_flags(0x0800_0000)
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                script,
            ])
            .env("NANOBUG_ATLAS_PID", std::process::id().to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let requests = query.stdin.take().context("GPU sampler input")?;
        let replies = BufReader::new(query.stdout.take().context("GPU sampler output")?);
        Ok(Self {
            query,
            requests,
            replies,
        })
    }

    pub(super) fn sample(&mut self) -> Result<Value> {
        let mut process = PROCESS_MEMORY_COUNTERS_EX::default();
        let length = std::mem::size_of_val(&process) as u32;
        process.cb = length;
        // SAFETY: The pseudo-handle belongs to this process and the pointer has the declared size.
        let ok = unsafe {
            K32GetProcessMemoryInfo(GetCurrentProcess(), (&raw mut process).cast(), length)
        };
        ensure!(ok != 0, "could not sample process memory");
        writeln!(self.requests, "sample")?;
        self.requests.flush()?;
        let mut line = String::new();
        ensure!(
            self.replies.read_line(&mut line)? != 0,
            "GPU sampler exited"
        );
        let gpu: Value = serde_json::from_str(&line).context("GPU counter response")?;
        Ok(json!({
            "private_commit_bytes": process.PrivateUsage,
            "working_set_bytes": process.WorkingSetSize,
            "gpu_process_memory": gpu,
        }))
    }
}

impl Drop for MemorySampler {
    fn drop(&mut self) {
        let _ = self.query.kill();
        let _ = self.query.wait();
    }
}
