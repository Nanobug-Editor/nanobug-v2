//! Execute a user-entered editor filter against a snapshot of selected lines.

use anyhow::{Context, Result, ensure};
use std::{
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    process::{Command, Stdio},
};

/// Maximum text returned by a filter, matching the document's file size limit.
const LIMIT: u64 = 16 * 1024 * 1024;

/// Run `command` in `directory` with `input` on stdin and return UTF-8 stdout.
pub fn run_filter(directory: &Path, command: &str, input: &str) -> Result<String> {
    let mut stdin = tempfile::tempfile()?;
    stdin.write_all(input.as_bytes())?;
    if !input.ends_with('\n') {
        stdin.write_all(b"\n")?;
    }
    stdin.rewind()?;
    let output = run_shell(directory, command, Stdio::from(stdin), false)?;
    ensure!(
        !output.contains('\0'),
        crate::i18n::text("document.binary_file")
    );
    Ok(output)
}

/// Run an explicit Ex shell command without sending the buffer to stdin.
pub fn run_ex_shell(directory: &Path, command: &str) -> Result<String> {
    run_shell(directory, command, Stdio::null(), true)
}

/// Run a shell command at `directory`, retaining stderr when `display` is requested.
fn run_shell(directory: &Path, command: &str, stdin: Stdio, display: bool) -> Result<String> {
    let mut stderr = tempfile::tempfile()?;
    #[cfg(windows)]
    let mut process = {
        use std::os::windows::process::CommandExt;
        let mut process = Command::new("cmd.exe");
        process
            .args(["/D", "/C"])
            .raw_arg(command)
            .creation_flags(0x0800_0000);
        process
    };
    #[cfg(not(windows))]
    let mut process = {
        let mut process = Command::new("sh");
        process.args(["-c", command]);
        process
    };
    let mut child = process
        .current_dir(directory)
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::from(stderr.try_clone()?))
        .spawn()
        .context("start editor filter")?;
    let mut output = Vec::new();
    let read = child
        .stdout
        .take()
        .context("filter stdout")?
        .take(LIMIT + 1)
        .read_to_end(&mut output);
    if read.is_err() || output.len() as u64 > LIMIT {
        let _ = child.kill();
    }
    let status = child.wait()?;
    read?;
    ensure!(
        output.len() as u64 <= LIMIT,
        crate::i18n::text("document.file_too_large")
    );
    if !status.success() || display {
        stderr.seek(SeekFrom::Start(0))?;
        let mut detail = Vec::new();
        stderr
            .take(if display { LIMIT } else { 4096 })
            .read_to_end(&mut detail)?;
        if !status.success() {
            anyhow::bail!(
                "{status}\n{}{}",
                String::from_utf8_lossy(&output),
                String::from_utf8_lossy(&detail)
            );
        }
        if !detail.is_empty() {
            if !output.is_empty() && !output.ends_with(b"\n") {
                output.push(b'\n');
            }
            output.extend(detail);
        }
    }
    let text = if display {
        String::from_utf8_lossy(&output).replace('\0', "^@")
    } else {
        String::from_utf8(output).context(crate::i18n::text("document.requires_utf8"))?
    };
    Ok(text.replace("\r\n", "\n"))
}
