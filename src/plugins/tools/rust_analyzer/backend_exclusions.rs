//! Translate workspace exclusions into settings understood by backend language servers.

use super::Server;
use anyhow::Result;
use serde_json::{Map, Value, json};
use std::{path::Path, process::Command};

/// Configure native scan exclusions before launching `server` for the project `root`.
/// `exclusions` contains validated project-relative paths. `options` and `configuration`
/// are carried by the initialize and workspace configuration messages respectively.
pub(super) fn apply(
    server: Server,
    root: &Path,
    exclusions: &[String],
    command: &mut Command,
    options: &mut Value,
    configuration: &mut Map<String, Value>,
) -> Result<()> {
    if exclusions.is_empty() {
        return Ok(());
    }
    // These servers treat wildcard characters as patterns. Leave those literal paths to the host.
    let plain_paths = || {
        exclusions
            .iter()
            .map(String::as_str)
            .filter(|path| !path.contains(['*', '?']))
    };
    match server {
        Server::Go if plain_paths().next().is_some() => {
            let filters = ["-**/node_modules".to_owned()]
                .into_iter()
                .chain(plain_paths().map(|path| format!("-{path}")))
                .collect::<Vec<_>>();
            let settings = json!({"directoryFilters": filters});
            *options = settings.clone();
            configuration.insert("gopls".into(), settings);
        }
        Server::Python if plain_paths().next().is_some() => {
            // Older Pyright releases replace their implicit defaults when this list is set.
            let paths = ["**/node_modules", "**/__pycache__", "**/.*"]
                .into_iter()
                .chain(plain_paths())
                .collect::<Vec<_>>();
            configuration.insert("python".into(), json!({"analysis": {"exclude": paths}}));
        }
        Server::Java => {
            // JDT LS applies import exclusions before recognizing Maven, Gradle or Eclipse projects.
            let paths = [
                "**/node_modules/**",
                "**/.metadata/**",
                "**/archetype-resources/**",
                "**/META-INF/maven/**",
            ]
            .map(str::to_owned)
            .into_iter()
            .chain(exclusions.iter().flat_map(|path| {
                let directory = escape_glob(&java_path(&root.join(path)));
                [format!("{directory}/**"), directory]
            }))
            .collect::<Vec<_>>();
            let settings = json!({"import": {"exclusions": paths}});
            *options = json!({"settings": {"java": settings}});
            configuration.insert("java".into(), settings);
        }
        // Phpactor's CLI would replace its existing exclusion array. Preserve it and use the host filter.
        Server::Php if !command.get_args().any(|arg| arg == "language-server") => {
            // The Node-based Intelephense fallback uses these default file globs.
            let patterns = [
                "**/.git/**",
                "**/.svn/**",
                "**/.hg/**",
                "**/CVS/**",
                "**/.DS_Store/**",
                "**/node_modules/**",
                "**/bower_components/**",
                "**/vendor/**/{Tests,tests}/**",
                "**/.history/**",
                "**/vendor/**/vendor/**",
            ]
            .map(str::to_owned)
            .into_iter()
            .chain(
                exclusions
                    .iter()
                    .map(|path| format!("{}/**", escape_glob(path))),
            )
            .collect::<Vec<_>>();
            configuration.insert(
                "intelephense".into(),
                json!({"files": {"exclude": patterns}}),
            );
        }
        _ => {}
    }
    Ok(())
}

/// Convert the native `path` to the form used by Java's glob matcher.
fn java_path(path: &Path) -> String {
    let path = path.to_string_lossy();
    if cfg!(windows) {
        let path = path.replace('\\', "/");
        if let Some(share) = path.strip_prefix("//?/UNC/") {
            format!("//{share}")
        } else {
            path.strip_prefix("//?/").unwrap_or(&path).to_owned()
        }
    } else {
        path.into_owned()
    }
}

/// Escape glob metacharacters so a literal project directory `path` stays literal.
fn escape_glob(path: &str) -> String {
    let mut pattern = String::new();
    for ch in path.chars() {
        if matches!(
            ch,
            '\\' | '?' | '*' | '[' | ']' | '{' | '}' | '(' | ')' | '!' | '+' | '@'
        ) {
            pattern.push('\\');
        }
        pattern.push(ch);
    }
    pattern
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both contributions reach the settings endpoint each backend reads.
    #[test]
    fn backend_scan_exclusions_configure_both_paths() -> Result<()> {
        let root = if cfg!(windows) {
            Path::new(r"\\?\C:\project")
        } else {
            Path::new("/project")
        };
        let excludes = ["auto-artifacts".to_owned(), "manual-artifacts".to_owned()];
        let mut command = Command::new("fixture");
        let mut options = Value::Null;
        let mut settings = Map::new();

        apply(
            Server::Go,
            root,
            &excludes,
            &mut command,
            &mut options,
            &mut settings,
        )?;
        assert_eq!(
            options["directoryFilters"],
            json!(["-**/node_modules", "-auto-artifacts", "-manual-artifacts"])
        );
        assert_eq!(settings["gopls"], options);

        apply(
            Server::Python,
            root,
            &excludes,
            &mut command,
            &mut options,
            &mut settings,
        )?;
        assert_eq!(
            settings["python"]["analysis"]["exclude"][3],
            "auto-artifacts"
        );
        assert_eq!(
            settings["python"]["analysis"]["exclude"][4],
            "manual-artifacts"
        );

        apply(
            Server::Java,
            root,
            &excludes,
            &mut command,
            &mut options,
            &mut settings,
        )?;
        let java = &options["settings"]["java"]["import"]["exclusions"];
        let prefix = if cfg!(windows) {
            "C:/project"
        } else {
            "/project"
        };
        assert_eq!(java[4], format!("{prefix}/auto-artifacts/**"));
        assert_eq!(java[7], format!("{prefix}/manual-artifacts"));

        apply(
            Server::Php,
            root,
            &excludes,
            &mut command,
            &mut options,
            &mut settings,
        )?;
        let php = &settings["intelephense"]["files"]["exclude"];
        assert_eq!(php[10], "auto-artifacts/**");
        assert_eq!(php[11], "manual-artifacts/**");

        let mut command = Command::new("phpactor");
        command.arg("language-server");
        settings.clear();
        apply(
            Server::Php,
            root,
            &excludes,
            &mut command,
            &mut options,
            &mut settings,
        )?;
        assert_eq!(command.get_args().count(), 1);
        assert!(settings.is_empty());
        Ok(())
    }
}
