//! Exercise built release ZIPs through package installation and the native workers.

use nanobug::plugins::package::{InstallStore, Provider};
use nanobug_plugin_sdk::{
    DocumentSnapshot, Input, Instance, Output, PluginEvent, Query, Reply, Request, Service,
    ServiceResult, WIRE, receive, send,
};
use serde_json::{Value, json};
use std::{
    io::BufReader,
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};

/// A fixture exercises multiline outline and fold captures with UTF-8 range boundaries.
#[derive(Clone, Copy)]
struct Fixture {
    /// Manifest and ZIP identifier.
    id: &'static str,
    /// Filename admitted by the manifest's language provider.
    path: &'static str,
    /// Text passed to the guest and native syntax worker.
    text: &'static str,
}

/// Sample documents cover unique frontend and backend grammars.
const SOURCE_FIXTURES: &[Fixture] = &[
    Fixture {
        id: "angular",
        path: "item.component.html",
        text: "<ng-template>\n  <p>中文</p>\n</ng-template>",
    },
    Fixture {
        id: "astro",
        path: "Item.astro",
        text: "<Item>\n  <p>中文</p>\n</Item>",
    },
    Fixture {
        id: "csharp",
        path: "Item.cs",
        text: "public class Item {\n    public string Name() { return \"中文\"; }\n}",
    },
    Fixture {
        id: "css",
        path: "main.css",
        text: "main {\n  font-family: '中文';\n}",
    },
    Fixture {
        id: "go",
        path: "item.go",
        text: "package main\nfunc title() string {\n    return \"中文\"\n}\n",
    },
    Fixture {
        id: "html",
        path: "index.html",
        text: "<main>\n  <p>中文</p>\n</main>",
    },
    Fixture {
        id: "java",
        path: "Item.java",
        text: "public class Item {\n    public String name() { return \"中文\"; }\n}",
    },
    Fixture {
        id: "javascript",
        path: "item.mjs",
        text: "export function title() {\n  return '中文';\n}",
    },
    Fixture {
        id: "jsx-tsx",
        path: "Item.tsx",
        text: "export function Title() {\n  return <p>中文</p>;\n}",
    },
    Fixture {
        id: "kotlin",
        path: "Item.kt",
        text: "class Item {\n    fun title(): String { return \"中文\" }\n}",
    },
    Fixture {
        id: "php",
        path: "Item.php",
        text: "<?php\nclass Item {\n    public function title(): string { return '中文'; }\n}",
    },
    Fixture {
        id: "python",
        path: "item.py",
        text: "def title():\n    name = '中文'\n    return name\n",
    },
    Fixture {
        id: "ruby",
        path: "item.rb",
        text: "def title\n  '中文'\nend\n",
    },
    Fixture {
        id: "svelte",
        path: "Item.svelte",
        text: "<Item>\n  <p>中文</p>\n</Item>",
    },
    Fixture {
        id: "typescript",
        path: "item.mts",
        text: "export function title(): string {\n  return '中文';\n}",
    },
    Fixture {
        id: "vue",
        path: "Item.vue",
        text: "<template>\n  <p>中文</p>\n</template>",
    },
];

/// Each SQL provider must complete its own dialect keyword in addition to shared captures.
const SQL_COMPLETIONS: &[(&str, &str, &str)] = &[
    ("access-sql", "TRA", "TRANSFORM"),
    ("clickhouse", "PREWH", "PREWHERE"),
    ("databricks-sql", "OPTIM", "OPTIMIZE"),
    ("db2", "OPTI", "OPTIMIZE FOR"),
    ("duckdb", "QUAL", "QUALIFY"),
    ("googlesql", "UNN", "UNNEST"),
    ("h2", "SCRIP", "SCRIPT TO"),
    ("mariadb", "REPLA", "REPLACE INTO"),
    ("mysql", "SHOW", "SHOW TABLES"),
    ("oracle-sql", "ROWNU", "ROWNUM"),
    ("postgresql", "RETUR", "RETURNING"),
    ("snowflake", "QUAL", "QUALIFY"),
    ("sqlite", "PRA", "PRAGMA"),
    ("tsql", "G", "GO"),
];

/// Keep one process per worker role throughout the release bundle smoke test.
struct Worker {
    /// Process owned by the test.
    child: Child,
    /// Sender for the worker protocol.
    input: ChildStdin,
    /// Replies decoded by a pipe reader with a bounded wait in the test thread.
    output: std::sync::mpsc::Receiver<Reply>,
}

impl Worker {
    /// Start the runtime in `role`, with the installation cache when supplied.
    fn start(role: &str, cache: Option<&Path>) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_nanobug-plugin-runtime"));
        command
            .arg(role)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        command.args(cache);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        let mut child = command.spawn().unwrap();
        let input = child.stdin.take().unwrap();
        let output = Self::read_replies(child.stdout.take().unwrap());
        let worker = Self {
            child,
            input,
            output,
        };
        assert!(matches!(worker.next(), Reply::Hello { wire: WIRE }));
        worker
    }

    /// Decode complete JSON lines received on the worker's stdout.
    fn read_replies(output: ChildStdout) -> std::sync::mpsc::Receiver<Reply> {
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut output = BufReader::new(output);
            while let Some(reply) = receive::<Reply>(&mut output).unwrap() {
                if sender.send(reply).is_err() {
                    break;
                }
            }
        });
        receiver
    }

    /// Read the next protocol response or stop when the runtime stops responding.
    fn next(&self) -> Reply {
        self.output
            .recv_timeout(std::time::Duration::from_secs(15))
            .unwrap()
    }

    /// Send one `request` and receive its reply.
    fn request(&mut self, request: Request) -> Reply {
        send(&mut self.input, &request).unwrap();
        self.next()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Project markers sufficient to warm each non-SQL server in the released rule.
fn startup_markers(id: &str) -> &'static [&'static str] {
    match id {
        "angular" => &["angular.json"],
        "astro" => &["astro.config.mjs"],
        "csharp" => &["global.json"],
        "css" => &["src/index.css"],
        "go" => &["go.mod"],
        "html" => &["index.html"],
        "java" => &["pom.xml", "src/main/java"],
        "javascript" => &["jsconfig.json"],
        "jsx-tsx" => &["src/main.tsx"],
        "kotlin" => &["settings.gradle.kts", "src/main/kotlin"],
        "php" => &["composer.json"],
        "python" => &["pyproject.toml"],
        "ruby" => &["Gemfile"],
        "svelte" => &["svelte.config.js"],
        "typescript" => &["tsconfig.json"],
        "vue" => &["vue.config.js"],
        _ => panic!("{id}: add a project fixture for the server"),
    }
}

/// Run one exported module with the current path, project markers and SQL choice.
/// Return its activation, server startup request and project-relative scan exclusions.
fn evaluate_rule(
    worker: &mut Worker,
    source: &str,
    module: &str,
    path: &str,
    markers: &[&str],
    dialect: Option<&str>,
) -> (bool, bool, Vec<String>) {
    evaluate_rule_with_context(
        worker,
        source,
        module,
        path,
        markers,
        json!({"projectSqlDialect": dialect}),
    )
}

/// Evaluate one rule under an explicit host `context`, retaining its physical `path`.
fn evaluate_rule_with_context(
    worker: &mut Worker,
    source: &str,
    module: &str,
    path: &str,
    markers: &[&str],
    context: Value,
) -> (bool, bool, Vec<String>) {
    let extension = Path::new(path).extension().and_then(|part| part.to_str());
    let mut reply = worker.request(Request::Evaluate {
        id: 1,
        source: source.into(),
        module: Some(module.into()),
        context,
        remaining_ms: 5000,
    });
    loop {
        match reply {
            Reply::Rule {
                matches,
                start_language_server,
                exclude_dirs,
                ..
            } => {
                return (matches, start_language_server, exclude_dirs);
            }
            Reply::Query {
                id,
                query,
                kind,
                args,
            } => {
                let value = match kind {
                    Query::DocumentPath => json!(path),
                    Query::DocumentExtension => json!(extension),
                    Query::Exists => {
                        json!(args["path"].as_str().is_some_and(|p| markers.contains(&p)))
                    }
                    Query::ReadFile => Value::Null,
                    Query::List => json!({"entries": [], "complete": true}),
                    _ => panic!("{module}: unexpected query {kind:?}"),
                };
                reply = worker.request(Request::Data {
                    id,
                    query,
                    result: Ok(value),
                });
            }
            _ => panic!("{module}: expected activation result, got {reply:?}"),
        }
    }
}

/// Install one package and exercise its real WASM guest against a projected syntax tree.
fn exercise(
    store: &InstallStore,
    archives: &Path,
    wasm: &mut Worker,
    syntax: &mut Worker,
    rules: &mut Worker,
    opening: u64,
    fixture: &Fixture,
) {
    let id = fixture.id;
    let archive = archives.join(format!("nanobug-{id}-plugin.zip"));
    let prepared = store
        .prepare(&archive)
        .unwrap_or_else(|error| panic!("{}: {error:#}", archive.display()));
    assert_eq!(prepared.release.manifest.id, id);
    let grants = prepared.release.manifest.permissions.clone();
    let installed = store.install(prepared, &grants).unwrap();
    let modules = &installed.release.manifest.modules;
    let [module, complete, format] = modules.as_slice() else {
        panic!("{id}: expected three feature modules");
    };
    assert_eq!(
        (&*module.id, &*complete.id, &*format.id),
        ("document", "complete", "format")
    );
    assert_eq!(
        module.provides.iter().copied().collect::<Vec<_>>(),
        [Provider::Document]
    );
    assert_eq!(
        format.provides.iter().copied().collect::<Vec<_>>(),
        [Provider::Format]
    );
    assert!(
        complete.provides.contains(&Provider::Complete)
            && complete.provides.contains(&Provider::Diagnose),
        "{id}"
    );
    assert!(
        module.language_server.is_none() && format.language_server.is_none(),
        "{id}"
    );
    for other in [complete, format] {
        assert_eq!(other.rule, module.rule, "{id}");
        assert_eq!(other.wasm, module.wasm, "{id}");
        assert_eq!(
            other.document_extensions, module.document_extensions,
            "{id}"
        );
    }
    let rule_path = store
        .verified_asset(&installed.release, module.rule.as_deref().unwrap())
        .unwrap();
    let source = std::fs::read_to_string(rule_path).unwrap();
    let has_server = complete.language_server.is_some();
    assert_eq!(has_server, id != "access-sql", "{id}");
    let (markers, dialect) = if module.document_extensions.contains("sql") {
        (&["dbt_project.yml", "target/manifest.json"][..], Some(id))
    } else {
        (startup_markers(id), None)
    };
    for candidate in modules {
        let name = candidate.id.as_str();
        let (matches, starts, _) = evaluate_rule(rules, &source, name, fixture.path, &[], None);
        assert_eq!((matches, starts), (true, false), "{id}/{name} open source");
        assert_eq!(
            evaluate_rule(rules, &source, name, "README.md", &[], None),
            (false, false, Vec::new()),
            "{id}/{name} unrelated file"
        );
        let expected_start = has_server && name == "complete";
        let (matches, starts, exclusions) =
            evaluate_rule(rules, &source, name, "README.md", markers, dialect);
        assert_eq!(
            (matches, starts),
            (expected_start, expected_start),
            "{id}/{name} project startup"
        );
        assert_eq!(
            exclusions.is_empty(),
            !expected_start,
            "{id}/{name} scan exclusions"
        );
        if expected_start && dialect.is_some() {
            assert_eq!(exclusions, ["target/compiled", "target/run"], "{id}");
        }
    }
    let wasm_path = store
        .verified_asset(&installed.release, module.wasm.as_deref().unwrap())
        .unwrap();
    let instance = Instance {
        project: opening,
        module: module.id.clone(),
    };
    let document = DocumentSnapshot {
        path: Some(fixture.path.into()),
        opening,
        revision: 1,
        text: fixture.text.into(),
    };
    assert!(module.accepts_document(document.path.as_deref()), "{id}");
    let mut input = Input {
        document: Some(document.clone()),
        document_filetype: None,
        event: PluginEvent::Open,
        settings: Default::default(),
        draft: Default::default(),
        viewport: None,
        canvas_sync: None,
        open_documents: Vec::new(),
    };

    let opened = result(
        wasm.request(Request::Open {
            id: 1,
            instance: instance.clone(),
            wasm: wasm_path.clone(),
            input: Box::new(input.clone()),
            interactive: false,
        }),
        id,
    );
    let [request] = opened.services.as_slice() else {
        panic!("{id}: expected one syntax request");
    };
    assert!(opened.await_service, "{id}");
    let Service::QuerySyntax { language, query } = &request.operation else {
        panic!("{id}: expected QuerySyntax");
    };
    let Reply::Parsed { value, .. } = syntax.request(Request::QuerySyntax {
        id: 2,
        language: language.clone(),
        query: query.clone(),
        document: Box::new(document),
    }) else {
        panic!("{id}: expected parsed syntax for {language}");
    };
    assert_eq!(value["complete"], true, "{id}");
    assert_eq!(
        value["diagnostics"].as_array().unwrap().len(),
        0,
        "{id}: {value}"
    );

    input.event = PluginEvent::ServiceResult {
        result: ServiceResult {
            token: request.token,
            value: Ok(value),
        },
    };
    let output = result(
        wasm.request(Request::Call {
            id: 3,
            instance: instance.clone(),
            input: Box::new(input.clone()),
            interactive: false,
        }),
        id,
    );
    let features = output
        .document_features
        .unwrap_or_else(|| panic!("{id}: no document features"));
    assert!(!features.highlights.is_empty(), "{id}: no highlights");
    use nanobug_plugin_sdk::language::SyntaxKind::*;
    let (token, role) = match id {
        "html" | "angular" | "vue" | "jsx-tsx" => ("<p>", Tag),
        "svelte" | "astro" => ("<Item>", Component),
        "css" => ("font-family", CssProperty),
        "csharp" | "java" | "php" | "kotlin" => ("Item", Class),
        "go" | "javascript" | "typescript" | "python" => ("return", Return),
        "ruby" => ("title", Method),
        _ => ("title", SqlColumn),
    };
    let offset = fixture.text.find(token).unwrap() + usize::from(token.starts_with('<'));
    let actual = features
        .highlights
        .iter()
        .find(|s| s.range.start <= offset && offset < s.range.end)
        .map(|s| s.kind);
    assert_eq!(actual, Some(role), "{id}: wrong role for {token}");
    assert!(!features.symbols.is_empty(), "{id}: no outline");
    assert!(!features.folds.is_empty(), "{id}: no folds");
    assert!(
        features
            .highlights
            .iter()
            .all(|span| { fixture.text.get(span.range.start..span.range.end).is_some() }),
        "{id}: invalid UTF-8 highlight"
    );

    if let Some((_, prefix, expected)) = SQL_COMPLETIONS.iter().find(|(dialect, ..)| *dialect == id)
    {
        let completion = Instance {
            project: opening,
            module: complete.id.clone(),
        };
        input.event = PluginEvent::Open;
        result(
            wasm.request(Request::Open {
                id: 4,
                instance: completion.clone(),
                wasm: wasm_path,
                input: Box::new(input.clone()),
                interactive: false,
            }),
            id,
        );
        let text = input.document.as_mut().unwrap();
        text.text = (*prefix).into();
        text.revision += 1;
        input.event = PluginEvent::Complete {
            offset: prefix.len(),
        };
        let completed = result(
            wasm.request(Request::Call {
                id: 5,
                instance: completion,
                input: Box::new(input),
                interactive: false,
            }),
            id,
        );
        assert!(
            completed
                .completion_items
                .iter()
                .any(|item| item.label == *expected),
            "{id}: expected {expected} for {prefix}, got {:?}",
            completed.completion_items
        );
    }
    assert!(matches!(
        wasm.request(Request::Close {
            id: 6,
            project: opening
        }),
        Reply::Closed { .. }
    ));
}

/// Unpack a plugin call's `reply` with package context on errors.
fn result(reply: Reply, package: &str) -> Output {
    match reply {
        Reply::Result { output, .. } => *output,
        other => panic!("{package}: {other:?}"),
    }
}

/// Install the Python pilot and exercise its rule, syntax and completion with a real extensionless path.
#[test]
#[ignore = "requires the built Python plugin ZIP in NANOBUG_LANGUAGE_ARCHIVES or dist/language-plugins"]
fn release_python_explicit_filetype_with_no_extension() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let archives = std::env::var_os("NANOBUG_LANGUAGE_ARCHIVES")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("dist/language-plugins"));
    let profile = tempfile::tempdir().unwrap();
    let store = InstallStore::new(profile.path().join("store"));
    let prepared = store
        .prepare(&archives.join("nanobug-python-plugin.zip"))
        .unwrap();
    let grants = prepared.release.manifest.permissions.clone();
    let installed = store.install(prepared, &grants).unwrap();
    let document = installed
        .release
        .manifest
        .modules
        .iter()
        .find(|module| module.id == "document")
        .unwrap();
    assert!(document.document_filetypes.contains("python"));
    let source = std::fs::read_to_string(
        store
            .verified_asset(&installed.release, document.rule.as_deref().unwrap())
            .unwrap(),
    )
    .unwrap();
    let mut rules = Worker::start("rules", None);
    assert!(
        evaluate_rule_with_context(
            &mut rules,
            &source,
            "document",
            "script",
            &[],
            json!({"documentFiletype": "python"})
        )
        .0
    );
    assert!(evaluate_rule(&mut rules, &source, "document", "script.py", &[], None).0);
    assert!(
        !evaluate_rule_with_context(
            &mut rules,
            &source,
            "document",
            "script.py",
            &[],
            json!({"documentFiletype": "text"})
        )
        .0
    );

    let mut wasm = Worker::start("wasm", Some(&store.root().join("wasmtime-cache.toml")));
    let mut parser = Worker::start("syntax", None);
    let module = store
        .verified_asset(&installed.release, document.wasm.as_deref().unwrap())
        .unwrap();
    let instance = Instance {
        project: 1,
        module: "document".into(),
    };
    let doc = DocumentSnapshot {
        path: Some("script".into()),
        opening: 1,
        revision: 1,
        text: "def title():\n    return '中文'\n".into(),
    };
    let mut input = Input {
        document: Some(doc.clone()),
        document_filetype: Some("python".into()),
        event: PluginEvent::Open,
        settings: Default::default(),
        draft: String::new(),
        viewport: None,
        canvas_sync: None,
        open_documents: Vec::new(),
    };
    let opened = result(
        wasm.request(Request::Open {
            id: 1,
            instance: instance.clone(),
            wasm: module.clone(),
            input: Box::new(input.clone()),
            interactive: false,
        }),
        "python",
    );
    let [request] = opened.services.as_slice() else {
        panic!("expected a syntax query")
    };
    let Service::QuerySyntax { language, query } = &request.operation else {
        panic!("expected syntax query")
    };
    assert_eq!(language, "python");
    let Reply::Parsed { value, .. } = parser.request(Request::QuerySyntax {
        id: 2,
        language: language.clone(),
        query: query.clone(),
        document: Box::new(doc),
    }) else {
        panic!("expected parsed Python")
    };
    input.event = PluginEvent::ServiceResult {
        result: ServiceResult {
            token: request.token,
            value: Ok(value),
        },
    };
    let highlighted = result(
        wasm.request(Request::Call {
            id: 3,
            instance,
            input: Box::new(input.clone()),
            interactive: false,
        }),
        "python",
    );
    assert!(
        highlighted
            .document_features
            .is_some_and(|features| !features.highlights.is_empty())
    );

    let instance = Instance {
        project: 1,
        module: "complete".into(),
    };
    input.event = PluginEvent::Open;
    result(
        wasm.request(Request::Open {
            id: 4,
            instance: instance.clone(),
            wasm: module,
            input: Box::new(input.clone()),
            interactive: false,
        }),
        "python",
    );
    let current = input.document.as_mut().unwrap();
    current.text = "pri".into();
    current.revision += 1;
    input.event = PluginEvent::Complete { offset: 3 };
    let completed = result(
        wasm.request(Request::Call {
            id: 5,
            instance,
            input: Box::new(input),
            interactive: false,
        }),
        "python",
    );
    assert!(
        completed
            .completion_items
            .iter()
            .any(|item| item.label == "print")
    );
}

/// Verify the actual distribution packages, including each separate SQL dialect.
#[test]
#[ignore = "requires built plugin ZIPs under NANOBUG_LANGUAGE_ARCHIVES or dist/language-plugins"]
fn release_language_zips_install_and_produce_document_features() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let archives = std::env::var_os("NANOBUG_LANGUAGE_ARCHIVES")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("dist/language-plugins"));
    let test_output = root.join("test-output");
    std::fs::create_dir_all(&test_output).unwrap();
    let profile = tempfile::Builder::new()
        .prefix("language-packages-")
        .tempdir_in(test_output)
        .unwrap();
    let store = InstallStore::new(profile.path().join("store"));
    // Use the installer's compiled-module cache for the subsequent execution worker.
    let first = archives.join("nanobug-angular-plugin.zip");
    drop(
        store
            .prepare(&first)
            .unwrap_or_else(|error| panic!("{}: {error:#}", first.display())),
    );
    let mut wasm = Worker::start("wasm", Some(&store.root().join("wasmtime-cache.toml")));
    let mut syntax = Worker::start("syntax", None);
    let mut rules = Worker::start("rules", None);
    let sql = SQL_COMPLETIONS.iter().map(|(id, ..)| Fixture {
        id,
        path: "item.sql",
        text: "CREATE TABLE item (\n    title VARCHAR(64) DEFAULT '中文'\n);",
    });
    for (index, fixture) in SOURCE_FIXTURES.iter().cloned().chain(sql).enumerate() {
        exercise(
            &store,
            &archives,
            &mut wasm,
            &mut syntax,
            &mut rules,
            index as u64 + 1,
            &fixture,
        );
    }
}
