//! Run built-in formatter adapters against an unsaved editor snapshot.

use super::{command, format_snapshot, rustfmt};
use crate::plugins::query::resolve_prefix;
use anyhow::{Context, Result, ensure};
use nanobug_plugin_sdk::{DocumentSnapshot, document_wire::MAX_DOCUMENT_BYTES};
use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

/// Format `document` at `path` in `project` using the authorized `tool`.
pub(in crate::plugins) fn format_language(
    tool: &str,
    project: &Path,
    path: &Path,
    document: &DocumentSnapshot,
    deadline: Instant,
    closed: &AtomicBool,
) -> Result<String> {
    if tool == "sqlformat" {
        return format_sql(&document.text, deadline, closed);
    }
    let process = if tool == "rustfmt" {
        rustfmt::command(project, path)?
    } else {
        formatter(tool, project, path)?
    };
    format_snapshot(process, tool, document, deadline, closed)
}

/// Build a stdin-only command from a known formatter and a project-contained file path.
fn formatter(tool: &str, project: &Path, path: &Path) -> Result<Command> {
    let root = project.canonicalize()?;
    let file = resolve_prefix(&root.join(path))?;
    ensure!(
        file.starts_with(&root),
        "The document is outside the project"
    );
    let file_name = file.file_name().context("The document has no filename")?;
    let directory = file
        .parent()
        .into_iter()
        .flat_map(Path::ancestors)
        .take_while(|path| path.starts_with(&root))
        .find(|path| path.is_dir())
        .context("The document has no existing project directory")?;

    let mut formatter = None;
    if tool == "prettier" {
        // The npm installation provides a JS entry point and command-shell shims on Windows.
        // Execute its known entry directly with Node so filenames are never shell arguments.
        formatter = prettier_script(&root, directory).and_then(|script| {
            let mut node = command(&executable("node")?, directory);
            node.arg(script);
            Some(node)
        });
    }
    let formatter = formatter.or_else(|| executable(tool).map(|path| command(&path, directory)));
    let args: &[&str] = match tool {
        "prettier" => &["--stdin-filepath"],
        "ruff" => &["format", "--stdin-filename"],
        "gofmt" => &[],
        "google-java-format" => &["-"],
        "csharpier" => &["format"],
        "ktlint" => &["--log-level=none", "--format", "--stdin", "--stdin-path"],
        "rubyfmt" => &[],
        "phpcbf" => &["-q", "--no-colors"],
        _ => anyhow::bail!("Unsupported formatter {tool}"),
    };
    let mut formatter = formatter.with_context(|| format!("{tool} is unavailable on PATH"))?;
    formatter.args(args);
    if matches!(tool, "prettier" | "ruff" | "ktlint") {
        formatter.arg(file_name);
    }
    if tool == "phpcbf" {
        // PHP_CodeSniffer only accepts this option in the equals form.
        let mut stdin_path = std::ffi::OsString::from("--stdin-path=");
        stdin_path.push(file_name);
        formatter.arg(stdin_path);
    }
    if matches!(tool, "ruff" | "phpcbf") {
        formatter.arg("-");
    }
    Ok(formatter)
}

/// Resolve an executable from absolute PATH entries without searching the project implicitly.
pub(super) fn executable(name: &str) -> Option<PathBuf> {
    let name = format!("{name}{}", env::consts::EXE_SUFFIX);
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .filter(|entry| entry.is_absolute())
        .map(|entry| entry.join(&name))
        .find(|candidate| candidate.is_file())
}

/// Find project-local Prettier first, then the npm installation adjacent to PATH.
fn prettier_script(root: &Path, directory: &Path) -> Option<PathBuf> {
    let entry = "node_modules/prettier/bin/prettier.cjs";
    directory
        .ancestors()
        .take_while(|dir| dir.starts_with(root))
        .map(|dir| dir.join(entry))
        .find(|script| script.is_file())
        .or_else(|| npm_entry(entry))
}

/// Resolve a known npm entry relative to an absolute PATH directory.
pub(super) fn npm_entry(entry: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .filter(|entry| entry.is_absolute())
        .map(|dir| dir.join(entry))
        .find(|entry| entry.is_file())
}

/// Format shared SQL syntax without discarding literal, identifier or comment text.
fn format_sql(source: &str, deadline: Instant, closed: &AtomicBool) -> Result<String> {
    ensure!(
        !closed.load(Ordering::Acquire) && Instant::now() < deadline,
        "SQL formatting stopped"
    );
    ensure!(
        !source.contains('$')
            && !source.contains("/*")
            && !source.contains('\\')
            && !source.lines().any(|line| {
                let line = line.trim();
                line.eq_ignore_ascii_case("go")
                    || line
                        .get(..9)
                        .is_some_and(|head| head.eq_ignore_ascii_case("delimiter"))
                    || line.starts_with(['.', '#'])
            }),
        "This formatter supports ordinary SQL statements without script directives or procedural blocks"
    );
    let formatted = sqlformat::format(
        source,
        &sqlformat::QueryParams::None,
        &sqlformat::FormatOptions::default(),
    );
    // Verify the formatter only changed safe whitespace. Whitespace inside quoted values and
    // line comments is part of the token, as is adjacency between operator characters.
    ensure!(
        sql_tokens(source) == sql_tokens(&formatted),
        "This SQL construct cannot be formatted without changing its tokens"
    );
    ensure!(
        formatted.len() <= MAX_DOCUMENT_BYTES,
        "Formatted SQL exceeds the document size limit"
    );
    Ok(formatted)
}

/// Preserve quoted regions, comment lines and contiguous operators when comparing formatter output.
fn sql_tokens(source: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut rest = source;
    while !rest.is_empty() {
        rest = rest.trim_start_matches(|ch: char| ch.is_ascii_whitespace());
        if rest.is_empty() {
            break;
        }
        let mut chars = rest.char_indices().peekable();
        let (_, first) = chars.next().expect("nonempty SQL suffix");
        let end = if rest.starts_with("--") || rest.starts_with('#') {
            rest.find(['\r', '\n']).unwrap_or(rest.len())
        } else if let Some(comment) = rest.strip_prefix("/*") {
            comment.find("*/").map_or(rest.len(), |index| index + 4)
        } else if matches!(first, '\'' | '"' | '`' | '[') {
            let delimiter = if first == '[' { ']' } else { first };
            let mut end = rest.len();
            while let Some((index, ch)) = chars.next() {
                if ch == '\\' {
                    chars.next();
                } else if ch == delimiter {
                    if chars.peek().is_some_and(|(_, next)| *next == delimiter) {
                        chars.next();
                    } else {
                        end = index + ch.len_utf8();
                        break;
                    }
                }
            }
            end
        } else if matches!(first, '(' | ')' | ',' | ';') {
            first.len_utf8()
        } else {
            let word = |ch: char| ch.is_alphanumeric() || matches!(ch, '_' | '$');
            chars
                .find(|(_, ch)| {
                    ch.is_ascii_whitespace()
                        || matches!(ch, '(' | ')' | ',' | ';' | '\'' | '"' | '`' | '[')
                        || word(*ch) != word(first)
                })
                .map_or(rest.len(), |(index, _)| index)
        };
        tokens.push(&rest[..end]);
        rest = &rest[end..];
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, time::Duration};

    /// Format transient snapshots via the production adapter and an absolute project path.
    fn format(tool: &str, project: &Path, path: &Path, source: &str) -> Result<String> {
        let document = DocumentSnapshot {
            path: Some(path.into()),
            opening: 1,
            revision: 1,
            text: source.into(),
        };
        format_language(
            tool,
            project,
            path,
            &document,
            Instant::now() + Duration::from_secs(10),
            &AtomicBool::new(false),
        )
    }

    /// Built-in formatting preserves literal text and rejects unsupported script directives.
    #[test]
    fn sql_formats_shared_syntax_and_preserves_unsupported_scripts() -> Result<()> {
        let source = "select '中  文',value from items where value>=1;";
        let formatted = format("sqlformat", Path::new("."), Path::new("item.sql"), source)?;
        assert_ne!(formatted, source);
        assert_eq!(sql_tokens(source), sql_tokens(&formatted));
        assert!(formatted.contains("'中  文'"));

        for script in [
            "SELECT 1;\nGO\nSELECT 2;",
            "DO $$ BEGIN RAISE NOTICE 'hi'; END $$;",
        ] {
            let error =
                format("sqlformat", Path::new("."), Path::new("item.sql"), script).unwrap_err();
            assert!(error.to_string().contains("script directives"));
        }
        Ok(())
    }

    /// Check the actual adapter's stdin/stdout, file isolation and native tool resolution.
    #[test]
    #[ignore = "requires the supported native formatter commands on PATH"]
    fn native_formatter_adapters_keep_disk_files_unchanged() -> Result<()> {
        let fixtures = [
            (
                "prettier",
                "item.ts",
                "let x={a:\"中文\"}\n",
                "let x = { a: \"中文\" };",
            ),
            (
                "ruff",
                "item.py",
                "x={ 'a':'中文'}\n",
                "x = {\"a\": \"中文\"}",
            ),
            (
                "gofmt",
                "item.go",
                "package main\nfunc item()int{return 1}\n",
                "func item() int { return 1 }",
            ),
            (
                "google-java-format",
                "Item.java",
                "class Item{int x=1;}\n",
                "int x = 1;",
            ),
            (
                "csharpier",
                "Item.cs",
                "public class Item{int Foo()=>42;}\n",
                "int Foo() => 42;",
            ),
            ("ktlint", "Item.kt", "val hello=1\n", "val hello = 1"),
            ("rubyfmt", "item.rb", "def thing(x);x+1;end\n", "x + 1"),
            (
                "phpcbf",
                "verify file.php",
                "<?php function f(){return 1;}\n",
                "return 1;",
            ),
        ];
        let output = Path::new(env!("CARGO_MANIFEST_DIR")).join("test-output");
        fs::create_dir_all(&output)?;
        let project = tempfile::Builder::new()
            .prefix("format-language-")
            .tempdir_in(output)?;
        for (tool, filename, snapshot, expected) in fixtures {
            let path = project.path().join(filename);
            let saved = "Existing disk text";
            fs::write(&path, saved)?;
            let result = format(tool, project.path(), &path, snapshot)
                .with_context(|| format!("{tool} fixture"))?;
            assert!(result.contains(expected), "{tool}: {result:?}");
            assert_eq!(fs::read_to_string(&path)?, saved, "{tool} wrote to disk");
        }
        Ok(())
    }
}
