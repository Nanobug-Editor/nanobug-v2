//! Optional process-level tests against installed language servers.

use super::*;
use std::{env, fs};

/// A binary target needs a main function even when the renamed declaration still parses.
#[test]
#[ignore = "requires rust-analyzer and Cargo on PATH"]
fn real_rust_missing_main_diagnostic() -> Result<()> {
    let project = tempfile::tempdir()?;
    fs::write(
        project.path().join("Cargo.toml"),
        "[package]\nname='entry_fixture'\nversion='0.1.0'\nedition='2021'\n[[bin]]\nname='tool'\npath='src/entry.rs'\n",
    )?;
    fs::create_dir(project.path().join("src"))?;
    let path = project.path().join("src/entry.rs");
    let valid = "mod child;\nfn main() {}\n";
    let typo = "mod child;\nfn mai() {}\n";
    fs::write(&path, valid)?;
    let others = ["lib.rs", "child.rs"]
        .into_iter()
        .enumerate()
        .map(|(index, name)| -> Result<_> {
            let path = project.path().join("src").join(name);
            fs::write(&path, "pub fn main() {}\n")?;
            Ok(Arc::new(DocumentSnapshot {
                opening: index as u64 + 2,
                revision: 1,
                path: Some(path),
                text: "pub fn mai() {}\n".into(),
            }))
        })
        .collect::<Result<Vec<_>>>()?;
    let (send, events) = mpsc::channel();
    let analyzer = Analyzer::start(
        Server::RustAnalyzer,
        project.path().to_owned(),
        None,
        Arc::new(AtomicBool::new(false)),
        move |event| {
            let _ = send.send(event);
        },
    )?;
    let check = |revision,
                 text: &str,
                 saved: u64,
                 expected: &str,
                 matches: &dyn Fn(&[Value]) -> bool|
     -> Result<Vec<Value>> {
        let mut documents = others.clone();
        documents.push(Arc::new(DocumentSnapshot {
            opening: 1,
            revision,
            path: Some(path.clone()),
            text: text.into(),
        }));
        analyzer.sync_with_languages(documents, BTreeMap::new(), BTreeMap::from([(1, saved)]));
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut last = Vec::new();
        while let Ok(event) =
            events.recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            match event {
                Event::Diagnostics { snapshot, items } if snapshot.opening != 1 => {
                    assert!(
                        !items.iter().any(|item| item["code"] == "E0601"),
                        "library or child module {:?}",
                        snapshot.path
                    );
                }
                Event::Diagnostics { snapshot, items } if snapshot.revision == revision => {
                    if matches(&items) {
                        return Ok(items);
                    }
                    last = items;
                }
                Event::Failed(error) => anyhow::bail!("{error}"),
                _ => {}
            }
        }
        anyhow::bail!("revision {revision}, expected {expected}, received {last:?}");
    };
    let no_entry_error = |items: &[Value]| !items.iter().any(|item| item["code"] == "E0601");
    check(0, valid, 0, "valid program", &no_entry_error)?;
    let items = check(1, typo, 0, "unsaved entry rename", &|items| {
        items
            .iter()
            .any(|i| i["code"] == "E0601" && i["source"] == "nanobug")
    })?;
    let entry = items.iter().find(|i| i["code"] == "E0601").unwrap();
    assert_eq!(entry["severity"], 1);
    let start = offset_at(typo, &entry["range"]["start"]).unwrap();
    let end = offset_at(typo, &entry["range"]["end"]).unwrap();
    assert_eq!(&typo[start..end], "mai");
    assert_eq!(fs::read_to_string(&path)?, valid);
    fs::write(&path, typo)?;
    check(1, typo, 1, "rustc E0601 after saving", &|items| {
        items
            .iter()
            .any(|i| i["code"] == "E0601" && i["source"] == "rustc")
    })?;
    let repaired = "fn main() { let unused_variable = 1; }\n";
    check(2, repaired, 1, "unsaved repair", &no_entry_error)?;
    assert_eq!(fs::read_to_string(&path)?, typo);
    fs::write(&path, repaired)?;
    check(
        2,
        repaired,
        2,
        "fresh compiler warning after the next save",
        &|items| {
            no_entry_error(items)
                && items
                    .iter()
                    .any(|i| i["code"] == "unused_variables" && i["source"] == "rustc")
        },
    )?;
    Ok(())
}

/// Exercise both typos on an unsaved buffer while Cargo sees a valid program on disk.
#[test]
#[ignore = "requires rust-analyzer and Cargo on PATH"]
fn real_rust_analyzer_reports_misspelled_module_and_call() -> Result<()> {
    let project = tempfile::tempdir()?;
    let src = project.path().join("src");
    fs::create_dir(&src)?;
    fs::write(
        project.path().join("Cargo.toml"),
        "[package]\nname = 'diagnostics_fixture'\nversion = '0.1.0'\nedition = '2021'\n",
    )?;
    let path = src.join("main.rs");
    let clean = concat!(
        "mod app;\nmod ui;\n",
        "fn main() { let _ = \"🌱 app::rn()\"; app::run(); }\n",
        "#[cfg(any())]\nfn disabled() { app::missing(); }\n",
    );
    fs::write(&path, clean)?;
    fs::write(src.join("ui.rs"), "")?;
    fs::write(src.join("app.rs"), "pub fn run() {}\n")?;

    let (send, events) = mpsc::channel();
    let analyzer = Analyzer::start(
        Server::RustAnalyzer,
        project.path().to_owned(),
        None,
        Arc::new(AtomicBool::new(false)),
        move |event| {
            let _ = send.send(event);
        },
    )?;
    let check = |revision,
                 text: &str,
                 app: Option<Arc<DocumentSnapshot>>,
                 expected: &[&str]|
     -> Result<()> {
        let mut documents = vec![Arc::new(DocumentSnapshot {
            opening: 1,
            revision,
            path: Some(path.clone()),
            text: text.into(),
        })];
        documents.extend(app);
        analyzer.sync(documents);
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut errors = None;
        let mut semantic = false;
        while let Ok(event) =
            events.recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            match event {
                Event::Diagnostics { snapshot, items }
                    if snapshot.opening == 1 && snapshot.revision == revision =>
                {
                    let mut labels = Vec::new();
                    for item in items.iter().filter(|item| item["severity"] == 1) {
                        let range = &item["range"];
                        let start = offset_at(text, &range["start"]).context("invalid start")?;
                        let end = offset_at(text, &range["end"]).context("invalid end")?;
                        labels.push(text.get(start..end).context("invalid diagnostic range")?);
                    }
                    labels.sort_unstable();
                    errors = Some(labels);
                }
                Event::Semantic { snapshot, .. }
                    if snapshot.opening == 1 && snapshot.revision == revision =>
                {
                    semantic = true
                }
                Event::Failed(error) => anyhow::bail!("{error}"),
                _ => {}
            }
            if semantic && errors.as_deref() == Some(expected) {
                return Ok(());
            }
        }
        anyhow::bail!(
            "revision {revision} expected {expected:?}, received {errors:?}, semantic reply {semantic}"
        )
    };
    let misspelled = clean.replace("app::run();", "app::rn();");
    check(0, clean, None, &[])?;
    check(1, &misspelled, None, &["rn"])?;
    check(
        2,
        &misspelled.replace("mod ui;", "mod u;"),
        None,
        &["mod u;", "rn"],
    )?;
    check(3, &clean.replace("mod ui;", "mod u;"), None, &["mod u;"])?;
    check(
        4,
        &misspelled.replace(
            "app::rn();",
            "use app as local; local /* ns */ :: /* item */ rn();",
        ),
        None,
        &["rn"],
    )?;
    check(5, &misspelled, None, &["rn"])?;
    // A definition in a second unsaved document can fix the call without editing main.rs.
    let app = Arc::new(DocumentSnapshot {
        opening: 2,
        revision: 1,
        path: Some(src.join("app.rs")),
        text: "pub fn run() {}\npub fn rn() {}\n".into(),
    });
    check(5, &misspelled, Some(app), &[])?;
    check(5, &misspelled, None, &["rn"])?;
    check(6, clean, None, &[])?;
    assert_eq!(fs::read_to_string(path)?, clean);
    Ok(())
}

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
