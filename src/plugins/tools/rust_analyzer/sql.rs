//! Start SQLFluff's LSP adapter and bind its unversioned diagnostics to immutable snapshots.

use super::{DocumentSnapshot, Value};
use anyhow::{Context, Result};
use std::{path::Path, process::Command};

/// Launch the upstream SQLFluff language server with a fixed, supported dialect.
pub(super) fn command(root: &Path, dialect: &str) -> Result<Command> {
    use super::super::language::executable;

    let server = executable("sqlfluff-lsp").context("sqlfluff-lsp is unavailable on PATH")?;
    let cli = executable("sqlfluff").context("sqlfluff is unavailable on PATH")?;
    let mut sqlfluff_path = std::ffi::OsString::from("--sqlfluff-path=");
    sqlfluff_path.push(cli);
    let mut command = super::super::command(&server, root);
    command
        .arg("serve")
        .arg(format!("--dialect={dialect}"))
        .arg("--templater=raw")
        .arg(sqlfluff_path)
        // The LSP server writes UTF-8 to Python's stdin, including on Windows.
        .env("PYTHONIOENCODING", "utf-8");
    Ok(command)
}

/// Use the project-specific sqls configuration for an explicitly provisioned H2 database.
pub(super) fn h2_command(root: &Path) -> Result<Command> {
    use super::super::language::executable;

    let config = root.join(".nb/sqls-h2.yml");
    anyhow::ensure!(
        config.is_file(),
        "Set up .nb/sqls-h2.yml to connect sqls to H2"
    );
    let server = executable("sqls").context("sqls is unavailable on PATH")?;
    let mut command = super::super::command(&server, root);
    command.arg("--config").arg(config);
    Ok(command)
}

/// Give SQLFluff the workspace-relative lint path and tag its reply with an immutable snapshot.
pub(super) fn snapshot_uri(
    root: &Path,
    path: &Path,
    snapshot: &DocumentSnapshot,
) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    let name = relative.to_str()?.replace('\\', "/");
    // The upstream server passes the raw URI path to SQLFluff's CLI. Use the nearest
    // unencoded directory for config discovery, with a placeholder file for escaped names.
    let mut segments = name.split('/').peekable();
    let mut lint_path = String::from("./");
    while let Some(segment) = segments.next() {
        if !segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'~'))
        {
            lint_path.push_str("nanobug.sql");
            break;
        }
        lint_path.push_str(segment);
        if segments.peek().is_some() {
            lint_path.push('/');
        }
    }
    // An opaque URI avoids /C:/... being passed as a native Windows path.
    Some(format!(
        "nanobug:{lint_path}?snapshot={}-{}",
        snapshot.opening, snapshot.revision,
    ))
}

/// Turn SQLFluff's Unicode scalar columns into the client's UTF-16 LSP coordinates.
pub(super) fn diagnostic_positions(snapshot: &DocumentSnapshot, items: &mut Vec<Value>) {
    let lines: Vec<_> = snapshot.text.split('\n').collect();
    items.retain_mut(|item| {
        position(&lines, &mut item["range"]["start"]) && position(&lines, &mut item["range"]["end"])
    });
}

/// Translate one scalar-based position against the reported line.
fn position(lines: &[&str], value: &mut Value) -> bool {
    let Some(line) = value["line"]
        .as_u64()
        .and_then(|line| usize::try_from(line).ok())
        .and_then(|line| lines.get(line))
    else {
        return false;
    };
    let Some(column) = value["character"]
        .as_u64()
        .and_then(|column| usize::try_from(column).ok())
    else {
        return false;
    };
    let (count, units) = line
        .chars()
        .take(column)
        .fold((0, 0), |(count, units), ch| {
            (count + 1, units + ch.len_utf16())
        });
    if count != column {
        return false;
    }
    value["character"] = units.into();
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::tools::rust_analyzer::{
        Analyzer, Event, ProjectSettings, Request, Server, offset_at,
    };
    use std::{
        collections::BTreeSet,
        fs,
        sync::{Arc, atomic::AtomicBool, mpsc},
        time::{Duration, Instant},
    };

    /// Exercise the real SQLFluff server with Windows- and URI-sensitive paths and unsaved text.
    #[test]
    #[ignore = "requires sqlfluff-lsp and sqlfluff on PATH"]
    fn real_sqlfluff_language_server() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let directory = root.join("子目录 with spaces");
        fs::create_dir(&directory)?;
        let path = directory.join("query #1.sql");
        let clean = "SELECT 42;\n";
        fs::write(&path, clean)?;
        let (send, incoming) = mpsc::channel();
        let analyzer = Analyzer::start(
            Server::SqlPostgres,
            root,
            None,
            Arc::new(AtomicBool::new(false)),
            move |event| {
                let _ = send.send(event);
            },
        )?;
        let lint = |revision, text: &str| -> Result<Vec<Value>> {
            analyzer.sync(vec![Arc::new(DocumentSnapshot {
                opening: 1,
                revision,
                path: Some(path.clone()),
                text: text.into(),
            })]);
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                match incoming.recv_timeout(deadline.saturating_duration_since(Instant::now()))? {
                    Event::Diagnostics { snapshot, items } if snapshot.revision == revision => {
                        assert_eq!(snapshot.text, text);
                        assert_eq!(snapshot.path.as_deref(), Some(path.as_path()));
                        return Ok(items);
                    }
                    Event::Failed(error) => anyhow::bail!("{error}"),
                    _ => {}
                }
            }
        };
        let unsaved = "SELECT '😃' as label FROM accounts;\n";
        let diagnostics = lint(1, unsaved)?;
        let capitalisation = diagnostics
            .iter()
            .find(|item| {
                item["message"]
                    .as_str()
                    .is_some_and(|message| message.starts_with("CP01"))
            })
            .context("Expected a capitalization diagnostic on AS")?;
        let start = offset_at(unsaved, &capitalisation["range"]["start"]).unwrap();
        let end = offset_at(unsaved, &capitalisation["range"]["end"]).unwrap();
        assert_eq!(&unsaved[start..end], "as");
        let fixed = lint(2, "SELECT '😃' AS label FROM accounts;\n")?;
        assert!(!fixed.iter().any(|item| {
            item["message"]
                .as_str()
                .is_some_and(|message| message.starts_with("CP01"))
        }));
        assert_eq!(fs::read_to_string(path)?, clean);
        Ok(())
    }

    /// Check that the language-server transport forwards only SQL documents outside both lists.
    #[test]
    #[ignore = "requires sqlfluff-lsp and sqlfluff on PATH"]
    fn real_sqlfluff_language_server_filters_auto_and_manual_exclusions() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let directories = ["target/compiled", "manual-artifacts", "queries"];
        let sql = "SELECT 'x' as label FROM accounts;\n";
        let mut snapshots = Vec::new();
        for (index, directory) in directories.iter().enumerate() {
            fs::create_dir_all(root.join(directory))?;
            let path = root.join(directory).join("query.sql");
            fs::write(&path, sql)?;
            snapshots.push(Arc::new(DocumentSnapshot {
                opening: index as u64 + 1,
                revision: 1,
                path: Some(path),
                text: sql.into(),
            }));
        }
        fs::create_dir_all(root.join(".nb/plugin-setting"))?;
        fs::write(
            root.join(".nb/plugin-setting/postgresql.toml"),
            "excludeDirs = [\"manual-artifacts\"]\n",
        )?;

        for treatment in [false, true] {
            let (send, incoming) = mpsc::channel();
            let settings = treatment.then(|| ProjectSettings {
                plugin_id: "postgresql".into(),
                automatic_exclusions: Arc::from(["target/compiled".into()]),
            });
            let analyzer = Analyzer::start(
                Server::SqlPostgres,
                root.clone(),
                settings,
                Arc::new(AtomicBool::new(false)),
                move |event| {
                    let _ = send.send(event);
                },
            )?;
            analyzer.sync(snapshots.clone());

            let deadline = Instant::now() + Duration::from_secs(10);
            let mut linted = BTreeSet::new();
            let mut quiet_until = None;
            // The linter publishes a violation for each forwarded input. Allow extra time for
            // late diagnostics after the included query has proved the server is processing it.
            loop {
                let timeout = quiet_until.unwrap_or(deadline);
                match incoming.recv_timeout(timeout.saturating_duration_since(Instant::now())) {
                    Ok(Event::Diagnostics { snapshot, items })
                        if items.iter().any(|item| {
                            item["message"]
                                .as_str()
                                .is_some_and(|message| message.starts_with("CP01"))
                        }) =>
                    {
                        linted.insert(snapshot.opening);
                        if treatment && snapshot.opening == 3 && quiet_until.is_none() {
                            quiet_until = Some(Instant::now() + Duration::from_secs(2));
                        }
                        if (!treatment && linted.len() == snapshots.len())
                            || (treatment && snapshot.opening != 3)
                        {
                            break;
                        }
                    }
                    Ok(Event::Failed(error)) => anyhow::bail!("{error}"),
                    Ok(_) => {}
                    Err(mpsc::RecvTimeoutError::Timeout) => break,
                    Err(error) => return Err(error.into()),
                }
            }
            let expected = if treatment {
                BTreeSet::from([3])
            } else {
                BTreeSet::from([1, 2, 3])
            };
            assert_eq!(
                linted, expected,
                "SQL documents linted with exclusions={treatment}"
            );
        }
        Ok(())
    }

    /// Verify H2 keyword completion for ordinary SQL and withholding of both excluded directories.
    #[test]
    #[ignore = "requires sqls on PATH and an H2 connection in the fixture config"]
    fn real_h2_language_server() -> Result<()> {
        let fixture = std::env::var_os("NANOBUG_TEST_SQLS_H2_CONFIG")
            .map(std::path::PathBuf::from)
            .context("Set NANOBUG_TEST_SQLS_H2_CONFIG to the fixture connection config path")?;
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        assert!(
            h2_command(&root).is_err(),
            "A missing project config must fail"
        );
        fs::create_dir_all(root.join(".nb/plugin-setting"))?;
        fs::copy(fixture, root.join(".nb/sqls-h2.yml"))?;
        fs::write(
            root.join(".nb/plugin-setting/h2.toml"),
            "excludeDirs = [\"manual-artifacts\"]\n",
        )?;
        let text = "QUAL";
        let directories = ["target/compiled", "manual-artifacts", "queries"];
        let mut snapshots = Vec::new();
        for (index, directory) in directories.iter().enumerate() {
            fs::create_dir_all(root.join(directory))?;
            let path = root.join(directory).join("fixture.sql");
            fs::write(&path, text)?;
            snapshots.push(Arc::new(DocumentSnapshot {
                opening: index as u64 + 1,
                revision: 1,
                path: Some(path),
                text: text.into(),
            }));
        }

        for treatment in [false, true] {
            let (send, incoming) = mpsc::channel();
            let settings = treatment.then(|| ProjectSettings {
                plugin_id: "h2".into(),
                automatic_exclusions: Arc::from(["target/compiled".into()]),
            });
            let analyzer = Analyzer::start(
                Server::SqlH2,
                root.clone(),
                settings,
                Arc::new(AtomicBool::new(false)),
                move |event| {
                    let _ = send.send(event);
                },
            )?;
            analyzer.sync(snapshots.clone());
            let deadline = Instant::now() + Duration::from_secs(10);
            for document in &snapshots {
                analyzer
                    .request(
                        document.opening,
                        Request::Completion,
                        document.path.clone().context("Missing SQL fixture path")?,
                        text.len(),
                        (document.opening, document.revision),
                        deadline,
                    )
                    .map_err(anyhow::Error::msg)?;
            }
            let mut received = BTreeSet::new();
            while received.len() < snapshots.len() {
                let remaining = deadline.saturating_duration_since(Instant::now());
                match incoming.recv_timeout(remaining)? {
                    Event::Response { id, result } => {
                        let items = result.map_err(anyhow::Error::msg)?;
                        if treatment && id <= 2 {
                            assert!(items.is_null(), "Excluded H2 request {id}: {items}");
                        } else {
                            assert!(
                                items.as_array().is_some_and(|items| {
                                    items.iter().any(|item| item["label"] == "QUALIFY")
                                }),
                                "Expected H2 keyword completion on request {id}, got {items}"
                            );
                        }
                        received.insert(id);
                    }
                    Event::Failed(error) => anyhow::bail!("{error}"),
                    _ => {}
                }
            }
            assert_eq!(received, BTreeSet::from([1, 2, 3]));
        }
        Ok(())
    }
}
