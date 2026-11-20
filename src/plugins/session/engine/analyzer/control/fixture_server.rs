//! Minimal subprocess used by the language-server lifecycle tests.

use std::{
    fs::{self, OpenOptions},
    io::{self, BufRead, Read, Write},
};

fn main() -> io::Result<()> {
    let lease = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open("server.lock")?;
    if lease.try_lock().is_err() {
        fs::write(
            "overlapping-server",
            "the old language server is still alive",
        )?;
        return Ok(());
    }
    writeln!(
        OpenOptions::new()
            .append(true)
            .create(true)
            .open("launches.txt")?,
        "{}",
        std::process::id()
    )?;
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    loop {
        let mut length = None;
        loop {
            let mut header = String::new();
            if input.read_line(&mut header)? == 0 {
                return Ok(());
            }
            let header = header.trim();
            if header.is_empty() {
                break;
            }
            if let Some(number) = header.strip_prefix("Content-Length:") {
                length = number.trim().parse::<usize>().ok();
            }
        }
        let Some(length) = length else {
            return Err(io::ErrorKind::InvalidData.into());
        };
        let mut body = vec![0; length];
        input.read_exact(&mut body)?;
        let body = String::from_utf8_lossy(&body);
        if body.contains(r#""method":"initialize""#) {
            fs::write("last-initialize.json", body.as_bytes())?;
            let reply = r#"{"jsonrpc":"2.0","id":0,"result":{"capabilities":{"textDocumentSync":1,"completionProvider":{}}}}"#;
            write!(output, "Content-Length: {}\r\n\r\n{reply}", reply.len())?;
            output.flush()?;
        } else if body.contains(r#""method":"textDocument/didOpen""#) {
            fs::write("last-open.json", body.as_bytes())?;
        }
    }
}
