//! Known executables and file identifiers for host-managed LSP sessions.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{path::Path, process::Command};

/// Build Rust's handshake with the effective project-relative `exclusions`.
fn rust_analyzer_options(exclusions: &[String]) -> Value {
    let mut options = json!({
        "checkOnSave": false,
        "diagnostics": {"enable": true},
        "completion": {"autoimport": {"enable": false}, "postfix": {"enable": false}},
    });
    if !exclusions.is_empty() {
        // This alias is shared by current and older rust-analyzer versions.
        options["files"] = json!({"excludeDirs": exclusions});
    }
    options
}

/// Language servers that can be launched via standard input and output.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Server {
    RustAnalyzer,
    Typescript,
    Html,
    Css,
    Python,
    Go,
    Csharp,
    Java,
    Php,
    Kotlin,
    Ruby,
    Angular,
    Vue,
    Svelte,
    Astro,
    SqlPostgres,
    SqlSqlite,
    SqlMysql,
    SqlTsql,
    SqlMariadb,
    SqlOracle,
    SqlBigquery,
    SqlDuckdb,
    SqlSnowflake,
    SqlClickhouse,
    SqlDatabricks,
    SqlDb2,
    SqlH2,
}

impl Server {
    /// Return the executable basename advertised to users for this server.
    pub(crate) fn name(self) -> &'static str {
        if self.sqlfluff_dialect().is_some() {
            return "sqlfluff-lsp";
        }
        match self {
            Self::RustAnalyzer => "rust-analyzer",
            Self::Typescript => "typescript-language-server",
            Self::Html => "vscode-html-language-server",
            Self::Css => "vscode-css-language-server",
            Self::Python => "pyright-langserver",
            Self::Go => "gopls",
            Self::Csharp => "csharp-ls",
            Self::Java => "jdtls",
            Self::Php => "phpactor",
            Self::Kotlin => "kotlin-language-server",
            Self::Ruby => "solargraph",
            Self::Angular => "ngserver",
            Self::Vue => "vue-language-server",
            Self::Svelte => "svelteserver",
            Self::Astro => "astro-ls",
            Self::SqlH2 => "sqls",
            _ => unreachable!("SQLFluff language servers use the shared executable"),
        }
    }

    /// Whether only the selected SQL buffer should be synchronized to this server.
    pub(in crate::plugins) fn is_sql(self) -> bool {
        self == Self::SqlH2 || self.sqlfluff_dialect().is_some()
    }

    /// Resolve the CLI dialect label for supported SQLFluff servers.
    pub(in crate::plugins) fn sqlfluff_dialect(self) -> Option<&'static str> {
        Some(match self {
            Self::SqlPostgres => "postgres",
            Self::SqlSqlite => "sqlite",
            Self::SqlMysql => "mysql",
            Self::SqlTsql => "tsql",
            Self::SqlMariadb => "mariadb",
            Self::SqlOracle => "oracle",
            Self::SqlBigquery => "bigquery",
            Self::SqlDuckdb => "duckdb",
            Self::SqlSnowflake => "snowflake",
            Self::SqlClickhouse => "clickhouse",
            Self::SqlDatabricks => "databricks",
            Self::SqlDb2 => "db2",
            _ => return None,
        })
    }

    /// Resolve a supported executable and its fixed arguments for project `root`.
    pub(super) fn command(self, root: &Path) -> Result<Command> {
        use super::super::language::{executable, npm_entry};
        if self == Self::Vue {
            return Self::Typescript.command(root);
        }
        if let Some(dialect) = self.sqlfluff_dialect() {
            return super::sql::command(root, dialect);
        }
        if self == Self::SqlH2 {
            return super::sql::h2_command(root);
        }
        let args: &[&str] = match self {
            Self::RustAnalyzer | Self::Csharp | Self::Java | Self::Kotlin => &[],
            Self::Php => &[],
            Self::Ruby => &["stdio"],
            Self::Go => &["serve"],
            _ => &["--stdio"],
        };
        let name = self.name();
        let mut command = if self == Self::Php {
            super::backend::command(self, root)?
        } else if let Some(path) = executable(name) {
            super::super::command(&path, root)
        } else if matches!(self, Self::Java | Self::Kotlin | Self::Ruby) {
            super::backend::command(self, root)?
        } else {
            // npm's Windows shims run through a shell. Resolve known JS entry points instead.
            let entry = match self {
                Self::Typescript => Some("typescript-language-server/lib/cli.mjs"),
                Self::Html => Some("vscode-langservers-extracted/bin/vscode-html-language-server"),
                Self::Css => Some("vscode-langservers-extracted/bin/vscode-css-language-server"),
                Self::Python => Some("pyright/langserver.index.js"),
                Self::Angular => Some("@angular/language-server/bin/ngserver"),
                Self::Svelte => Some("svelte-language-server/bin/server.js"),
                Self::Astro => Some("@astrojs/language-server/bin/nodeServer.js"),
                _ => None,
            };
            let script = entry.and_then(|entry| {
                let local = root.join("node_modules").join(entry);
                local
                    .is_file()
                    .then_some(local)
                    .or_else(|| npm_entry(&format!("node_modules/{entry}")))
            });
            let (script, node) = script
                .zip(executable("node"))
                .with_context(|| format!("{name} is unavailable on PATH"))?;
            let mut command = super::super::command(&node, root);
            command.arg(script);
            command
        };
        command.args(args);
        if self == Self::Angular {
            // Resolve the Angular compiler and TypeScript versions selected by this workspace.
            command
                .arg("--tsProbeLocations")
                .arg(root)
                .arg("--ngProbeLocations")
                .arg(root);
        }
        if self == Self::RustAnalyzer {
            command.env("RUSTUP_AUTO_INSTALL", "0");
        }
        Ok(command)
    }

    /// Supply initialization options for `root` and Rust's native scan exclusions.
    pub(super) fn options(self, root: &Path, exclusions: &[String]) -> Value {
        match self {
            Self::RustAnalyzer => rust_analyzer_options(exclusions),
            Self::Astro => {
                let local = root.join("node_modules/typescript/lib");
                let tsdk = if local.is_dir() {
                    local
                } else {
                    super::super::language::npm_entry("node_modules/typescript/lib/typescript.js")
                        .and_then(|file| file.parent().map(Path::to_owned))
                        .unwrap_or(local)
                };
                json!({"typescript": {"tsdk": tsdk}})
            }
            _ => Value::Null,
        }
    }

    /// Select the LSP document identifier for the path `file` when supported.
    pub(in crate::plugins) fn language_id(self, file: &Path) -> Option<&'static str> {
        let extension = file.extension()?.to_str()?.to_ascii_lowercase();
        Some(match (self, extension.as_str()) {
            (server, "sql") if server.is_sql() => "sql",
            (Self::SqlPostgres, "pgsql" | "psql") => "sql",
            (Self::SqlTsql, "tsql") => "sql",
            (Self::SqlBigquery, "bqsql") => "sql",
            (Self::SqlDb2, "db2") => "sql",
            (Self::RustAnalyzer, "rs") => "rust",
            (Self::Typescript, "js" | "cjs" | "mjs") => "javascript",
            (Self::Typescript, "ts" | "cts" | "mts") => "typescript",
            (Self::Typescript, "jsx") => "javascriptreact",
            (Self::Typescript, "tsx") => "typescriptreact",
            (Self::Html | Self::Angular, "html" | "htm") => "html",
            (Self::Css, "css") => "css",
            (Self::Python, "py" | "pyi" | "pyw") => "python",
            (Self::Go, "go") => "go",
            (Self::Csharp, "cs" | "csx") => "csharp",
            (Self::Java, "java") => "java",
            (Self::Php, "php" | "phtml" | "inc" | "php3" | "php4" | "php5" | "phps") => "php",
            (Self::Kotlin, "kt" | "kts") => "kotlin",
            (Self::Ruby, "rb" | "rake" | "gemspec" | "ru") => "ruby",
            (Self::Svelte, "svelte") => "svelte",
            (Self::Astro, "astro") => "astro",
            (Self::Vue, "vue") => "vue",
            (Self::Vue, _) => return Self::Typescript.language_id(file),
            _ => return None,
        })
    }

    /// Convert a module's explicit `filetype` to an identifier understood by this server.
    pub(in crate::plugins) fn explicit_language_id(self, filetype: &str) -> Option<&'static str> {
        Some(match (self, filetype) {
            (Self::RustAnalyzer, "rust") => "rust",
            (Self::Typescript | Self::Vue, "javascript") => "javascript",
            (Self::Typescript | Self::Vue, "typescript") => "typescript",
            (Self::Typescript | Self::Vue, "javascriptreact") => "javascriptreact",
            (Self::Typescript | Self::Vue, "typescriptreact") => "typescriptreact",
            (Self::Html | Self::Angular, "html") => "html",
            (Self::Css, "css") => "css",
            (Self::Python, "python") => "python",
            (Self::Go, "go") => "go",
            (Self::Csharp, "cs" | "csharp") => "csharp",
            (Self::Java, "java") => "java",
            (Self::Php, "php") => "php",
            (Self::Kotlin, "kotlin") => "kotlin",
            (Self::Ruby, "ruby") => "ruby",
            (Self::Svelte, "svelte") => "svelte",
            (Self::Astro, "astro") => "astro",
            (Self::Vue, "vue") => "vue",
            (server, "sql") if server.is_sql() => "sql",
            _ => return None,
        })
    }

    /// Vue resolves imports through a paired TypeScript service, including unsaved JS/TS files.
    pub(in crate::plugins) fn accepts_project_overlay(self, path: &Path) -> bool {
        self == Self::Vue && Self::Typescript.language_id(path).is_some()
    }

    /// The Vue session uses a TypeScript server with the Vue TypeScript plugin loaded.
    pub(super) fn uses_typescript_diagnostics(self) -> bool {
        matches!(self, Self::Typescript | Self::Vue)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::tools::rust_analyzer::{ProjectSettings, ScanExclusions};
    use std::{fs, sync::Arc};

    /// Apply JS defaults and project TOML together, with the new path taking precedence over JSON.
    #[test]
    fn rust_scan_exclusions_merge_by_plugin_id() -> Result<()> {
        let project = tempfile::tempdir()?;
        let root = project.path();
        let rust_options = |settings: &ProjectSettings| -> Result<Value> {
            let exclusions = ScanExclusions::load(root, Some(settings))?;
            Ok(Server::RustAnalyzer.options(root, exclusions.directories()))
        };
        let mut settings = ProjectSettings {
            plugin_id: "rust".into(),
            automatic_exclusions: Arc::from([]),
        };
        fs::write(root.join(".gitignore"), "/test-output/\n")?;
        assert!(rust_options(&settings)?.get("files").is_none());

        fs::create_dir(root.join(".nb"))?;
        fs::write(
            root.join(".nb/rust-analyzer.json"),
            r#"{"excludeDirs":["legacy"]}"#,
        )?;
        settings.automatic_exclusions = Arc::from(["fuzz/artifacts".to_owned()]);
        assert_eq!(
            rust_options(&settings)?["files"]["excludeDirs"],
            json!(["fuzz/artifacts", "legacy"])
        );

        let directory = root.join(".nb/plugin-setting");
        fs::create_dir(&directory)?;
        let config = directory.join("rust.toml");
        fs::write(
            &config,
            r#"excludeDirs = ["fuzz/artifacts/", "test-output", "build/报告"]"#,
        )?;
        let options = rust_options(&settings)?;
        assert_eq!(
            options["files"]["excludeDirs"],
            json!(["fuzz/artifacts", "test-output", "build/报告"])
        );
        assert_eq!(options["diagnostics"]["enable"], true);
        assert_eq!(Server::Html.options(root, &[]), Value::Null);

        settings.plugin_id = "custom-rust".into();
        assert_eq!(
            rust_options(&settings)?["files"]["excludeDirs"],
            json!(["fuzz/artifacts"])
        );
        fs::write(
            directory.join("custom-rust.toml"),
            r#"excludeDirs = ["../another-project"]"#,
        )?;
        let error = rust_options(&settings).unwrap_err();
        assert!(error.to_string().contains("relative to the workspace"));
        Ok(())
    }
}
