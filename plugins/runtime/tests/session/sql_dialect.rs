//! Exercise SQL selection using independently packaged providers in the real session.

use super::*;
use nanobug::vim::{Action as VimAction, Register, Vim};
use std::collections::BTreeMap;

/// Build a SQL plugin fixture with a distinctive completion and formatting result.
fn sql_bundle(root: &Path, dialect: &str, extension: &str, keyword: &str) -> PathBuf {
    let archive = root.join(format!("{dialect}.zip"));
    let manifest = format!(
        r#"name = "{dialect}"
id = "{dialect}"
version = "1.0.0"
permissions = ["read_document", "edit_document"]
[engines]
nanobug = "^0.1.0"
[[modules]]
id = "language"
wasm = "main.wasm"
rule = "detect.js"
provides = ["document", "complete", "format"]
document_extensions = ["sql", "{extension}"]
"#
    );
    let output = serde_json::json!({
        "completion_items": [
            { "label": keyword, "edit": { "start": 0, "end": 0, "text": keyword } }
        ],
        "edits": [
            { "start": 0, "end": 8, "text": format!("SELECT 1; -- {dialect}") }
        ]
    });
    bundle_guest(
        &archive,
        &manifest,
        "return true;",
        &output.to_string(),
        false,
    );
    archive
}

/// Parse a real Ex command as entered in the editor.
fn ex(command: &str) -> VimAction {
    let mut vim = Vim::new(nanobug::document::Document::new(""));
    let mut register = Register::default();
    for character in command.chars() {
        vim.key(&character.to_string(), &mut register);
    }
    vim.key("Enter", &mut register)
}

/// Resolve the name and scope the workspace receives from a dialect Ex command.
fn dialect_command(command: &str) -> (bool, String) {
    let action = ex(command);
    let VimAction::SqlDialect { buffer_only, name } = action else {
        panic!("Unexpected Ex action {action:?}");
    };
    (buffer_only, name)
}

/// Resolve the directory containing the independently built language packages.
fn release_archives() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    std::env::var_os("NANOBUG_LANGUAGE_ARCHIVES")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("dist/language-plugins"))
}

/// Use the same installed dialect and project for a different feature module.
fn feature_module(key: &Key, module: &str) -> Key {
    let mut feature = key.clone();
    feature.instance.module = module.into();
    feature
}

/// Exercise installed guests, dialect-specific diagnostics, and the shared offline formatter.
#[test]
#[ignore = "requires the language release ZIPs plus sqlfluff-lsp and sqlfluff on PATH"]
fn sql_dialect_release_packages_route_presented_features() {
    let archives = release_archives();
    let temp = tempfile::tempdir().unwrap();
    let editor = session(&temp.path().join("profile"), false);
    let mut project = project(temp.path(), 1);
    let install_dialect = |id| {
        let archive = archives.join(format!("nanobug-{id}-plugin.zip"));
        assert!(archive.is_file(), "missing {}", archive.display());
        let namespace = install(&editor, archive, "0.2.2");
        Key {
            namespace,
            instance: Instance {
                project: project.id,
                module: "complete".into(),
            },
        }
    };
    let postgres = install_dialect("postgresql");
    let tsql = install_dialect("tsql");
    let path = project.root.join("fixture.sql");
    let on_disk = "SELECT 42;\n";
    fs::write(&path, on_disk).unwrap();
    let completion = |text: &str| {
        assert_eq!(ex(":complete"), VimAction::PluginComplete);
        PluginEvent::Complete { offset: text.len() }
    };
    for (revision, provider, other, expected, excluded) in [
        (0, &postgres, &tsql, "ILIKE", "TOP"),
        (1, &tsql, &postgres, "TOP", "ILIKE"),
    ] {
        project.document = Some(Arc::new(DocumentSnapshot {
            opening: 1,
            revision,
            path: Some(path.clone()),
            text: String::new(),
        }));
        if revision == 0 {
            editor.projects(Some(1), vec![project.clone()]);
        } else {
            let (buffer_only, name) = dialect_command(":sqldialect! tsql");
            assert!(buffer_only);
            editor.projects_with_sql_dialects(
                Some(1),
                vec![project.clone()],
                BTreeMap::from([(1, name)]),
            );
        }
        wait(&editor, |view| {
            ["document", "complete", "format"]
                .into_iter()
                .all(|module| {
                    view.modules.get(&feature_module(provider, module)) == Some(&State::Ready)
                        && view.modules.get(&feature_module(other, module))
                            == Some(&State::Unmatched)
                })
        });
        let output = request(&editor, provider, completion("")).unwrap().output;
        let contains = |label| {
            output
                .completion_items
                .iter()
                .any(|item| item.label == label)
        };
        assert!(contains(expected));
        assert!(!contains(excluded));
        assert!(request(&editor, other, completion("")).is_err());
    }

    let original = "SELECT  id,title FROM account WHERE id = 1;";
    project.document = Some(Arc::new(DocumentSnapshot {
        opening: 1,
        revision: 2,
        path: Some(path.clone()),
        text: original.into(),
    }));
    editor.projects_with_sql_dialects(
        Some(1),
        vec![project.clone()],
        BTreeMap::from([(1, "tsql".into())]),
    );
    let VimAction::Format(selection) = ex(":format") else {
        panic!("Unexpected formatter command");
    };
    let event = PluginEvent::Format {
        selection: selection.map(|range| [range.start, range.end]),
        path: None,
    };
    assert!(request(&editor, &feature_module(&postgres, "format"), event.clone()).is_err());
    let output = request(&editor, &feature_module(&tsql, "format"), event)
        .unwrap()
        .output;
    assert!(output.format_error.is_none(), "{:?}", output.format_error);
    assert_eq!(output.edits.len(), 1);
    let edit = &output.edits[0];
    assert_eq!((edit.start, edit.end), (0, original.len()));
    assert!(edit.text.contains('\n') && edit.text.contains("account"));

    editor.send(Action::SetProjectExecution(1, true)).unwrap();
    wait(&editor, |view| {
        view.project_execution.get(&1) == Some(&true)
    });
    let sql = "SELECT [x] FROM [dbo].[test];\n";
    for (revision, active, other, override_id, expects_parse_error) in [
        (3, &postgres, &tsql, None, true),
        (4, &tsql, &postgres, Some("tsql"), false),
    ] {
        project.document = Some(Arc::new(DocumentSnapshot {
            opening: 1,
            revision,
            path: Some(path.clone()),
            text: sql.into(),
        }));
        editor.projects_with_sql_dialects(
            Some(1),
            vec![project.clone()],
            override_id
                .into_iter()
                .map(|id| (1, id.to_owned()))
                .collect(),
        );
        let view = wait(&editor, |view| {
            view.document_diagnostics
                .get(&(active.clone(), 1))
                .is_some_and(|diagnostics| diagnostics.version == (1, revision))
        });
        assert!(!view.document_diagnostics.contains_key(&(other.clone(), 1)));
        assert_eq!(
            view.document_diagnostics[&(active.clone(), 1)]
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.starts_with("PRS:")),
            expects_parse_error
        );
    }
    assert_eq!(fs::read_to_string(path).unwrap(), on_disk);
}

/// Check project startup, authorization and per-buffer routing with real SQLFluff packages.
#[test]
#[ignore = "requires the language release ZIPs plus sqlfluff-lsp and sqlfluff on PATH"]
fn sql_project_prelaunch_uses_the_authorized_saved_dialect() {
    use nanobug::plugins::session::LanguageServerPhase;

    let temp = tempfile::tempdir().unwrap();
    let profile = temp.path().join("profile");
    let editor = session(&profile, false);
    let archives = release_archives();
    let mut project = project(temp.path(), 1);
    let install_dialect = |id| Key {
        namespace: install(
            &editor,
            archives.join(format!("nanobug-{id}-plugin.zip")),
            "0.2.2",
        ),
        instance: Instance {
            project: project.id,
            module: "complete".into(),
        },
    };
    let postgres = install_dialect("postgresql");
    let tsql = install_dialect("tsql");
    let readme_path = project.root.join("README.md");
    fs::write(&readme_path, "# SQL project\n").unwrap();
    let readme = Arc::new(DocumentSnapshot {
        opening: 1,
        revision: 0,
        path: Some(readme_path),
        text: "# SQL project\n".into(),
    });
    project.document = Some(readme.clone());
    editor.projects(Some(1), vec![project.clone()]);
    editor.send(Action::SetProjectExecution(1, true)).unwrap();
    wait(&editor, |view| {
        view.project_execution.get(&1) == Some(&true)
            && view.modules.get(&postgres) == Some(&State::Unmatched)
            && view.modules.get(&tsql) == Some(&State::Unmatched)
            && !view.language_servers.contains_key(&1)
    });

    editor.send(Action::SetProjectExecution(1, false)).unwrap();
    wait(&editor, |view| {
        view.project_execution.get(&1) == Some(&false)
    });
    let set_dialect = |editor: &Session, dialect| {
        let (reply, response) = async_channel::bounded(1);
        editor
            .send(Action::SetProjectSqlDialect {
                project: 1,
                dialect,
                reply,
            })
            .unwrap();
        response.recv_blocking().unwrap().unwrap();
    };
    set_dialect(&editor, Some("tsql".into()));
    wait(&editor, |view| {
        view.modules.get(&tsql) == Some(&State::Ready)
            && view.modules.get(&postgres) == Some(&State::Unmatched)
            && !view.language_servers.contains_key(&1)
    });

    let prelaunched = |view: &View| {
        view.modules.get(&tsql) == Some(&State::Ready)
            && view.modules.get(&feature_module(&tsql, "document")) == Some(&State::Unmatched)
            && view.modules.get(&feature_module(&tsql, "format")) == Some(&State::Unmatched)
            && view.modules.get(&postgres) == Some(&State::Unmatched)
            && view.language_servers.get(&1).is_some_and(|status| {
                status.document.is_none()
                    && status.name == "sqlfluff-lsp"
                    && status.phase == LanguageServerPhase::Ready
            })
    };
    editor.send(Action::SetProjectExecution(1, true)).unwrap();
    wait(&editor, &prelaunched);
    drop(editor);

    let reopened = session(&profile, false);
    reopened.projects(Some(1), vec![project.clone()]);
    wait(&reopened, &prelaunched);

    let path = project.root.join("fixture.sql");
    fs::write(&path, "SELECT 1;\n").unwrap();
    project.document = Some(Arc::new(DocumentSnapshot {
        opening: 2,
        revision: 0,
        path: Some(path.clone()),
        text: "SELECT [x] FROM [dbo].[test];\n".into(),
    }));
    reopened.projects_with_sql_dialects(
        Some(1),
        vec![project.clone()],
        BTreeMap::from([(1, "postgresql".into())]),
    );
    wait(&reopened, |view| {
        view.modules.get(&tsql) == Some(&State::Unmatched)
            && view.language_servers.get(&1).is_some_and(|status| {
                status
                    .document
                    .as_ref()
                    .is_some_and(|(opening, current)| *opening == 2 && current == &path)
                    && status.phase == LanguageServerPhase::Ready
            })
            && view
                .document_diagnostics
                .get(&(postgres.clone(), 2))
                .is_some_and(|diagnostics| {
                    diagnostics.version == (2, 0)
                        && diagnostics
                            .diagnostics
                            .iter()
                            .any(|diagnostic| diagnostic.message.starts_with("PRS:"))
                })
            && !view.document_diagnostics.contains_key(&(tsql.clone(), 2))
    });

    project.document = Some(readme);
    reopened.projects(Some(1), vec![project]);
    wait(&reopened, &prelaunched);
    set_dialect(&reopened, None);
    wait(&reopened, |view| {
        view.modules.get(&tsql) == Some(&State::Unmatched)
            && view.modules.get(&postgres) == Some(&State::Unmatched)
            && !view.language_servers.contains_key(&1)
    });
}

/// Route a provider request through the supervisor and return its version-bound reply.
fn request(
    editor: &Session,
    owner: &Key,
    event: PluginEvent,
) -> Result<nanobug::plugins::session::CallResult, String> {
    let (reply, response) = async_channel::bounded(1);
    editor
        .send(Action::Invoke {
            key: owner.clone(),
            event,
            reply: Some(reply),
            timeout: Duration::from_secs(3),
        })
        .unwrap();
    response.recv_blocking().unwrap()
}

#[test]
fn sql_dialect_selects_one_provider_across_commands_and_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let profile = temp.path().join("profile");
    let editor = session(&profile, false);
    let postgres = install(
        &editor,
        sql_bundle(temp.path(), "postgresql", "pgsql", "ILIKE"),
        "1.0.0",
    );
    let tsql = install(
        &editor,
        sql_bundle(temp.path(), "tsql", "tsql", "TOP"),
        "1.0.0",
    );
    let mut project = project(temp.path(), 1);
    let sql_file = project.root.join("query.SQL");
    project.document = Some(Arc::new(DocumentSnapshot {
        opening: 1,
        revision: 0,
        path: Some(sql_file.clone()),
        text: "select 1".into(),
    }));
    let owner = |namespace: String| Key {
        namespace,
        instance: Instance {
            project: 1,
            module: "language".into(),
        },
    };
    let postgres = owner(postgres);
    let tsql = owner(tsql);
    let verify = |editor: &Session, expected: &Key, other: &Key, keyword: &str, dialect: &str| {
        wait(editor, |view| {
            view.modules.get(expected) == Some(&State::Ready)
                && view.modules.get(other) == Some(&State::Unmatched)
        });
        assert!(
            request(editor, other, PluginEvent::Complete { offset: 0 }).is_err(),
            "inactive dialect completion must be rejected"
        );
        let completion = request(editor, expected, PluginEvent::Complete { offset: 0 }).unwrap();
        assert_eq!(completion.output.completion_items.len(), 1);
        assert_eq!(completion.output.completion_items[0].label, keyword);
        let event = PluginEvent::Format {
            selection: None,
            path: None,
        };
        assert!(request(editor, other, event.clone()).is_err());
        let format = request(editor, expected, event).unwrap();
        assert_eq!(
            format.output.edits[0].text,
            format!("SELECT 1; -- {dialect}")
        );
    };
    editor.projects(Some(1), vec![project.clone()]);
    verify(&editor, &postgres, &tsql, "ILIKE", "postgresql");

    let (buffer_only, dialect) = dialect_command(":sqldialect tsql");
    assert!(!buffer_only);
    let (reply, result) = async_channel::bounded(1);
    editor
        .send(Action::SetProjectSqlDialect {
            project: 1,
            dialect: Some(dialect),
            reply,
        })
        .unwrap();
    assert!(result.recv_blocking().unwrap().is_ok());
    verify(&editor, &tsql, &postgres, "TOP", "tsql");

    let (buffer_only, dialect) = dialect_command(":sqldialect! POSTGRESQL");
    assert!(buffer_only);
    editor.projects_with_sql_dialects(
        Some(1),
        vec![project.clone()],
        BTreeMap::from([(1, dialect)]),
    );
    verify(&editor, &postgres, &tsql, "ILIKE", "postgresql");

    let original = project.document.as_ref().unwrap().as_ref().clone();
    project.document = Some(Arc::new(DocumentSnapshot {
        opening: 2,
        path: Some(project.root.join("query.pgsql")),
        ..original.clone()
    }));
    editor.projects(Some(1), vec![project.clone()]);
    verify(&editor, &postgres, &tsql, "ILIKE", "postgresql");
    drop(editor);

    let reopened = session(&profile, false);
    project.document = Some(Arc::new(DocumentSnapshot {
        opening: 3,
        ..original
    }));
    reopened.projects(Some(1), vec![project]);
    verify(&reopened, &tsql, &postgres, "TOP", "tsql");
}
