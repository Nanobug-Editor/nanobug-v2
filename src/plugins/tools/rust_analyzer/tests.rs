//! Optional process-level smoke tests against installed npm language servers.

use super::*;
use std::{env, fs};

/// One fixture edits a known-good on-disk file through two unsaved document snapshots.
struct Fixture {
    server: Server,
    file_name: &'static str,
    clean: &'static str,
    broken: &'static str,
    request: Request,
    at: &'static str,
    /// Message fragment expected for a revision-bound diagnostic after the edit.
    expected_diagnostic: Option<&'static str>,
}

/// Use stable sources that expose a declaration or a list of meaningful completions.
const FIXTURES: &[Fixture] = &[
    Fixture {
        server: Server::Typescript,
        file_name: "main.ts",
        clean: "export const answer: number = 42;\nexport const use = answer;\n",
        broken: "export const answer: number = 'wrong';\nexport const use = answer;\n",
        request: Request::Definition,
        at: "answer;",
        expected_diagnostic: Some("is not assignable to type 'number'"),
    },
    Fixture {
        server: Server::Python,
        file_name: "main.py",
        clean: "answer: int = 42\nuse = answer\n",
        broken: "answer: int = 'wrong'\nuse = answer\n",
        request: Request::Definition,
        at: "answer\n",
        expected_diagnostic: Some("not assignable to declared type"),
    },
    Fixture {
        server: Server::Astro,
        file_name: "main.astro",
        clean: "---\nconst answer: number = 42;\n---\n<p>{answer}</p>\n",
        broken: "---\nconst answer: number = 'wrong';\n---\n<p>{answer}</p>\n",
        request: Request::Definition,
        at: "answer}",
        expected_diagnostic: Some("is not assignable to type 'number'"),
    },
    Fixture {
        server: Server::Svelte,
        file_name: "main.svelte",
        clean: "<script lang=\"ts\">\nlet answer: number = 42;\n</script>\n<p>{answer}</p>\n",
        broken: "<script lang=\"ts\">\nlet answer: number = 'wrong';\n</script>\n<p>{answer}</p>\n",
        request: Request::Definition,
        at: "answer}",
        expected_diagnostic: Some("is not assignable to type 'number'"),
    },
    Fixture {
        server: Server::Vue,
        file_name: "main.vue",
        clean: "<script setup lang=\"ts\">\nimport { answer } from './answer';\n</script>\n<template><p></p><input v-m>{{ answer }}</template>\n",
        broken: "<script setup lang=\"ts\">\nimport { answer } from './answer';\nconst invalid: number = 'wrong';\n</script>\n<template><p></p><input v-m>{{ answer }}</template>\n",
        request: Request::Definition,
        at: "./answer",
        expected_diagnostic: Some("is not assignable to type 'number'"),
    },
    Fixture {
        server: Server::Angular,
        file_name: "app.component.html",
        clean: "<p>{{ answer }}</p>\n",
        broken: "<p>{{ missing }}</p>\n",
        request: Request::Definition,
        at: "answer ",
        // ngserver publishes unversioned diagnostics and has no request to bind to a snapshot.
        expected_diagnostic: None,
    },
    Fixture {
        server: Server::Html,
        file_name: "main.html",
        clean: "<\ntitle\n",
        broken: "<script>const x = ;</script>\n",
        request: Request::Completion,
        at: "<",
        expected_diagnostic: Some("Expression expected."),
    },
    Fixture {
        server: Server::Css,
        file_name: "main.css",
        clean: "p { color: red; }\n",
        broken: "p { coor: red; }\n",
        request: Request::Completion,
        at: "color",
        expected_diagnostic: Some("Unknown property: 'coor'"),
    },
];

/// Exercise real stdio through the same snapshot and request APIs used by editor sessions.
///
/// Set `NANOBUG_TEST_NPM_LSP_ROOT` to an existing absolute directory containing `node_modules`.
/// With Node on PATH, install these packages in that directory before running the ignored test:
/// `typescript@6 typescript-language-server pyright @astrojs/language-server svelte-language-server
/// @angular/language-server @angular/core @angular/compiler @angular/compiler-cli astro svelte
/// @vue/language-server@3 vue
/// vscode-langservers-extracted`.
/// Run `cargo test --lib real_npm_language_servers -- --ignored --nocapture`.
#[test]
#[ignore = "requires a local npm language-server fixture and Node on PATH"]
fn real_npm_language_servers() -> Result<()> {
    let root = env::var_os("NANOBUG_TEST_NPM_LSP_ROOT")
        .map(PathBuf::from)
        .context("Set NANOBUG_TEST_NPM_LSP_ROOT to a directory with the fixture dependencies")?;
    ensure!(root.is_absolute(), "The npm test root must be absolute");
    ensure!(
        root.join("node_modules").is_dir(),
        "Install the npm fixture dependencies first"
    );

    for fixture in FIXTURES {
        run_fixture(&root, fixture)
            .with_context(|| format!("{} transport", fixture.server.name()))?;
        println!("Verified {}", fixture.server.name());
    }
    Ok(())
}

/// Create a scoped project below the npm fixture root and send unsaved snapshots to the server.
fn run_fixture(root: &Path, fixture: &Fixture) -> Result<()> {
    let project = tempfile::Builder::new()
        .prefix("nanobug-lsp-")
        .tempdir_in(root)?;
    let path = project.path().join(fixture.file_name);
    fs::write(&path, fixture.clean)?;
    match fixture.server {
        Server::Typescript => fs::write(
            project.path().join("tsconfig.json"),
            r#"{"compilerOptions":{"strict":true}}"#,
        )?,
        Server::Astro => fs::write(
            project.path().join("tsconfig.json"),
            r#"{"extends":"astro/tsconfigs/strict"}"#,
        )?,
        Server::Vue => {
            fs::write(
                project.path().join("tsconfig.json"),
                r#"{"compilerOptions":{"strict":true,"moduleResolution":"bundler","module":"esnext"},"include":["*.ts","*.vue"]}"#,
            )?;
            fs::write(
                project.path().join("answer.ts"),
                "export const answer = 42;\n",
            )?;
        }
        Server::Angular => {
            fs::write(
                project.path().join("angular.json"),
                r#"{"version":1,"projects":{"fixture":{"root":"","sourceRoot":".","projectType":"application"}}}"#,
            )?;
            fs::write(
                project.path().join("tsconfig.json"),
                r#"{"compilerOptions":{"strict":true,"experimentalDecorators":true,"moduleResolution":"bundler","module":"esnext","target":"es2022"},"angularCompilerOptions":{"strictTemplates":true},"include":["*.ts"]}"#,
            )?;
            fs::write(
                project.path().join("app.component.ts"),
                "import { Component } from '@angular/core';\n@Component({selector: 'app-fixture', templateUrl: './app.component.html'})\nexport class AppComponent { answer = 42; }\n",
            )?;
        }
        _ => {}
    }

    let (send, events) = mpsc::channel();
    let analyzer = Analyzer::start(
        fixture.server,
        root.to_owned(),
        None,
        Arc::new(AtomicBool::new(false)),
        move |event| {
            let _ = send.send(event);
        },
    )?;
    let snapshot = |revision, text: &str| {
        Arc::new(DocumentSnapshot {
            opening: 42,
            revision,
            path: Some(path.clone()),
            text: text.into(),
        })
    };

    analyzer.sync(vec![snapshot(0, fixture.clean)]);
    let deadline = Instant::now() + Duration::from_secs(15);
    analyzer
        .request(
            1,
            fixture.request,
            path.clone(),
            fixture
                .clean
                .find(fixture.at)
                .context("Fixture request position")?
                + 1,
            (42, 0),
            deadline,
        )
        .map_err(anyhow::Error::msg)?;

    loop {
        match events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .context("Timed out waiting for a language server reply")?
        {
            Event::Response { id: 1, result } => {
                let result = result.map_err(anyhow::Error::msg)?;
                let entries = result.as_array().or_else(|| result["items"].as_array());
                ensure!(
                    entries.is_some_and(|items| !items.is_empty()),
                    "Missing navigation or completion result: {result}"
                );
                if fixture.server == Server::Vue {
                    let entry = &entries.context("Vue definition result")?[0];
                    let target = entry["targetUri"]
                        .as_str()
                        .or_else(|| entry["uri"].as_str());
                    ensure!(
                        target.is_some_and(|uri| uri.ends_with("/answer.ts")),
                        "Vue definition must resolve the imported TypeScript symbol: {result}"
                    );
                }
                break;
            }
            Event::Failed(message) => anyhow::bail!("{message}"),
            _ => {}
        }
    }

    if fixture.server == Server::Vue {
        let deadline = Instant::now() + Duration::from_secs(10);
        analyzer
            .request(
                2,
                Request::Completion,
                path.clone(),
                fixture
                    .clean
                    .find("v-m")
                    .context("Vue directive completion position")?
                    + "v-m".len(),
                (42, 0),
                deadline,
            )
            .map_err(anyhow::Error::msg)?;
        loop {
            match events
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .context("Timed out waiting for Vue template completion")?
            {
                Event::Response { id: 2, result } => {
                    let result = result.map_err(anyhow::Error::msg)?;
                    let entries = result.as_array().or_else(|| result["items"].as_array());
                    ensure!(
                        entries.is_some_and(|items| items
                            .iter()
                            .any(|item| item["label"] == "v-model")),
                        "Vue template definition did not complete v-model: {result}"
                    );
                    break;
                }
                Event::Failed(message) => anyhow::bail!("{message}"),
                _ => {}
            }
        }
    }

    if let Some(expected) = fixture.expected_diagnostic {
        analyzer.sync(vec![snapshot(1, fixture.broken)]);
        let deadline = Instant::now() + Duration::from_secs(12);
        loop {
            match events
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .context("Timed out waiting for the unsaved diagnostic")?
            {
                Event::Diagnostics { snapshot, items } if snapshot.revision == 1 => {
                    ensure!(
                        snapshot.opening == 42 && snapshot.text == fixture.broken,
                        "Diagnostic was bound to the wrong snapshot"
                    );
                    if let Some(item) = items.iter().find(|item| {
                        item["message"]
                            .as_str()
                            .is_some_and(|message| message.contains(expected))
                    }) {
                        if fixture.server == Server::Typescript {
                            let start = offset_at(&snapshot.text, &item["range"]["start"]);
                            let end = offset_at(&snapshot.text, &item["range"]["end"]);
                            ensure!(
                                item["code"] == 2322,
                                "Unexpected TypeScript diagnostic code: {item}"
                            );
                            ensure!(
                                start
                                    .zip(end)
                                    .and_then(|(start, end)| snapshot.text.get(start..end))
                                    == Some("answer"),
                                "Incorrect TypeScript diagnostic range: {item}"
                            );
                        }
                        break;
                    }
                }
                Event::Failed(message) => anyhow::bail!("{message}"),
                _ => {}
            }
        }
    }
    ensure!(
        fs::read_to_string(&path)? == fixture.clean,
        "The server changed the saved file"
    );
    Ok(())
}
