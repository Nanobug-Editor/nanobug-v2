//! Map project scan exclusions onto the auto-import settings provided by frontend servers.

use super::Server;
use anyhow::Result;
use serde_json::{Map, Value, json};
use std::{
    path::{Component, Path},
    process::Command,
};

/// Send supported preferences to `server` through `options` and `configuration`.
/// Each item in `exclusions` is a validated project-relative directory.
pub(super) fn apply(
    server: Server,
    _root: &Path,
    exclusions: &[String],
    _command: &mut Command,
    options: &mut Value,
    configuration: &mut Map<String, Value>,
) -> Result<()> {
    if !matches!(server, Server::Typescript | Server::Astro | Server::Svelte) {
        return Ok(());
    }

    let patterns: Vec<_> = exclusions
        .iter()
        .filter_map(|directory| auto_import_pattern(Path::new(directory)))
        .collect();
    if patterns.is_empty() {
        return Ok(());
    }

    if server == Server::Typescript {
        // The TypeScript language server takes tsserver preferences in its LSP handshake.
        options["preferences"]["autoImportFileExcludePatterns"] = json!(patterns);
    } else {
        // Astro's Volar service requests these sections and Svelte accepts them at startup.
        for language in ["javascript", "typescript"] {
            let settings = configuration.entry(language).or_insert(Value::Null);
            settings["preferences"]["autoImportFileExcludePatterns"] = json!(patterns);
            if server == Server::Svelte {
                options["configuration"][language]["preferences"]["autoImportFileExcludePatterns"] =
                    json!(patterns);
            }
        }
    }
    Ok(())
}

/// Translate a literal project directory to a workspace-rooted tsserver pattern.
fn auto_import_pattern(directory: &Path) -> Option<String> {
    let components = directory
        .components()
        .map(|component| {
            let Component::Normal(name) = component else {
                return None;
            };
            let name = name.to_str()?;
            // TypeScript's patterns treat these characters specially. The host still uses the
            // literal path for transport filtering.
            (!name.contains(['*', '?', '\\'])).then_some(name)
        })
        .collect::<Option<Vec<_>>>()?;
    (!components.is_empty()).then(|| format!("./{}/**", components.join("/")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Keep existing server preferences and apply both merged directory exclusions.
    #[test]
    fn frontend_preferences_exclude_only_workspace_relative_auto_imports() -> Result<()> {
        let exclusions = vec!["automatic/cache".into(), "manual [cache]".into()];
        let expected = json!(["./automatic/cache/**", "./manual [cache]/**"]);
        let root = Path::new(".");
        let mut command = Command::new("server");
        let mut options = json!({"preferences": {"quotePreference": "single"}, "plugins": ["vue"]});
        let mut configuration = Map::new();

        apply(
            Server::Typescript,
            root,
            &exclusions,
            &mut command,
            &mut options,
            &mut configuration,
        )?;
        assert_eq!(
            options["preferences"]["autoImportFileExcludePatterns"],
            expected
        );
        assert_eq!(options["preferences"]["quotePreference"], "single");
        assert_eq!(options["plugins"], json!(["vue"]));
        assert!(configuration.is_empty());

        apply(
            Server::Astro,
            root,
            &exclusions,
            &mut command,
            &mut options,
            &mut configuration,
        )?;
        assert_eq!(
            configuration["typescript"]["preferences"]["autoImportFileExcludePatterns"],
            expected
        );
        assert!(options.get("configuration").is_none());

        apply(
            Server::Svelte,
            root,
            &exclusions,
            &mut command,
            &mut options,
            &mut configuration,
        )?;
        assert_eq!(
            options["configuration"]["javascript"]["preferences"]["autoImportFileExcludePatterns"],
            expected
        );
        assert_eq!(auto_import_pattern(Path::new("literal?")), None);
        Ok(())
    }
}
