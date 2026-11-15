//! Optional process-level checks against installed backend language servers.

use super::*;
use std::{env, fs};

/// Source and project configuration that make one declaration discoverable.
struct Fixture {
    /// Language server whose native tool is installed.
    server: Server,
    /// Path within the test workspace.
    file: &'static str,
    /// Clean contents written to disk.
    source: &'static str,
    /// Identifier before the fixture's definition request.
    usage: &'static str,
    /// Files needed for the language server to recognize the workspace.
    project_files: &'static [(&'static str, &'static str)],
}

/// Fixture projects require no third-party dependencies to resolve their declarations.
const FIXTURES: &[Fixture] = &[
    Fixture {
        server: Server::Go,
        file: "main.go",
        source: "package main\nfunc marker() int { return 42 }\nfunc main() { println(marker()) }\n",
        usage: "marker())",
        project_files: &[("go.mod", "module example.invalid/nanobug\n\ngo 1.23.0\n")],
    },
    Fixture {
        server: Server::Csharp,
        file: "Fixture.cs",
        source: "public class Fixture { int Marker() => 42; int Main() => Marker(); }\n",
        usage: "Marker();",
        project_files: &[(
            "Fixture.csproj",
            r#"<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><TargetFramework>net10.0</TargetFramework></PropertyGroup></Project>"#,
        )],
    },
    Fixture {
        server: Server::Java,
        file: "Fixture.java",
        source: "class Fixture { int marker() { return 42; } int main() { return marker(); } }\n",
        usage: "marker();",
        project_files: &[],
    },
    Fixture {
        server: Server::Kotlin,
        file: "Fixture.kt",
        source: "fun marker(): Int = 42\nfun main(): Int = marker()\n",
        usage: "marker()\n",
        project_files: &[],
    },
    Fixture {
        server: Server::Php,
        file: "fixture.php",
        source: "<?php\nfunction marker(): int { return 42; }\nfunction useMarker(): int { return marker(); }\n",
        usage: "marker();",
        project_files: &[("composer.json", "{}")],
    },
    Fixture {
        server: Server::Ruby,
        file: "fixture.rb",
        source: "class Fixture\n  def marker; 42; end\n  def main; marker(); end\nend\n",
        usage: "marker();",
        project_files: &[],
    },
];

/// Run `cargo test --lib real_backend_language_servers -- --ignored --nocapture`.
/// With `NANOBUG_TEST_BACKEND_LSP`, select installed server names in a comma-delimited list.
#[test]
#[ignore = "requires installed backend language servers on PATH"]
fn real_backend_language_servers() -> Result<()> {
    let selection = env::var("NANOBUG_TEST_BACKEND_LSP").unwrap_or_default();
    let mut count = 0;
    for fixture in FIXTURES {
        if selection.is_empty()
            || selection
                .split(',')
                .any(|name| name == fixture.server.name())
        {
            check_fixture(fixture)
                .with_context(|| format!("{} transport", fixture.server.name()))?;
            println!("Verified {}", fixture.server.name());
            count += 1;
        }
    }
    ensure!(count != 0, "Select at least one backend server");
    Ok(())
}

/// Send an unsaved revision and observe a real navigation response without writing it.
fn check_fixture(fixture: &Fixture) -> Result<()> {
    let test_output = Path::new(env!("CARGO_MANIFEST_DIR")).join("test-output");
    fs::create_dir_all(&test_output)?;
    let project = tempfile::Builder::new()
        .prefix("backend-lsp-")
        .tempdir_in(&test_output)?;
    let root = project.path().canonicalize()?;
    let path = root.join(fixture.file);
    fs::write(&path, fixture.source)?;
    for (file, content) in fixture.project_files {
        fs::write(root.join(file), content)?;
    }

    let (send, events) = mpsc::channel();
    let analyzer = Analyzer::start(
        fixture.server,
        root,
        None,
        Arc::new(AtomicBool::new(false)),
        move |event| {
            let _ = send.send(event);
        },
    )?;
    let source = format!("\n{}", fixture.source);
    let position = source.find(fixture.usage).context("Fixture position")? + 1;
    analyzer.sync(vec![Arc::new(DocumentSnapshot {
        opening: 42,
        revision: 1,
        path: Some(path.clone()),
        text: source,
    })]);

    let deadline = Instant::now() + Duration::from_secs(24);
    // Some servers can respond before loading a project. Repeat the read-only lookup
    // after a short pause until the declaration is ready or the request budget runs out.
    for id in 1_u64.. {
        analyzer
            .request(
                id,
                Request::Definition,
                path.clone(),
                position,
                (42, 1),
                deadline,
            )
            .map_err(anyhow::Error::msg)?;
        loop {
            match events.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(Event::Response {
                    id: received,
                    result,
                }) if received == id => {
                    let result = result.map_err(anyhow::Error::msg)?;
                    let definition = result
                        .as_array()
                        .and_then(|entries| entries.first())
                        .unwrap_or(&result);
                    if let Some(uri) = definition["uri"]
                        .as_str()
                        .or_else(|| definition["targetUri"].as_str())
                    {
                        ensure!(
                            uri_path(uri).as_deref().and_then(file_uri) == file_uri(&path),
                            "Definition resolved to an unexpected document: {definition}"
                        );
                        let range = definition
                            .get("targetSelectionRange")
                            .or_else(|| definition.get("range"))
                            .context("Missing definition range")?;
                        ensure!(
                            range["start"]["line"].as_u64().is_some(),
                            "Invalid definition location: {definition}"
                        );
                        check_completion(&analyzer, &events, &path, position)?;
                        ensure!(
                            fs::read_to_string(&path)? == fixture.source,
                            "The server changed the saved file"
                        );
                        return Ok(());
                    }
                    ensure!(Instant::now() < deadline, "Missing definition in {result}");
                    thread::sleep(Duration::from_millis(200));
                    break;
                }
                Ok(Event::Failed(message)) => anyhow::bail!("{message}"),
                Ok(_) => {}
                Err(error) => return Err(error).context("Waiting for the backend server"),
            }
        }
    }
    unreachable!()
}

/// Ask the same live server for completion over the existing unsaved identifier.
fn check_completion(
    analyzer: &Analyzer,
    events: &Receiver<Event>,
    path: &Path,
    position: usize,
) -> Result<()> {
    let request = 0x4e_41_4e_4f;
    let deadline = Instant::now() + Duration::from_secs(8);
    analyzer
        .request(
            request,
            Request::Completion,
            path.to_owned(),
            position,
            (42, 1),
            deadline,
        )
        .map_err(anyhow::Error::msg)?;
    loop {
        match events.recv_timeout(deadline.saturating_duration_since(Instant::now()))? {
            Event::Response { id, result } if id == request => {
                let result = result.map_err(anyhow::Error::msg)?;
                let entries = result.as_array().or_else(|| result["items"].as_array());
                ensure!(
                    entries.is_some_and(|items| items.iter().any(|item| {
                        item["label"]
                            .as_str()
                            .is_some_and(|value| value.to_ascii_lowercase().starts_with("marker"))
                    })),
                    "Missing semantic completion in {result}"
                );
                return Ok(());
            }
            Event::Failed(message) => anyhow::bail!("{message}"),
            _ => {}
        }
    }
}
