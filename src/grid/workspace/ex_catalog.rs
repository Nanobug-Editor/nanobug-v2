//! Index local help files and display diagnostics from the native workspace.

use super::{Workspace, argument_list::argument_words};
use anyhow::{Context as _, Result, bail, ensure};
use gpui::{Context, Window};
use nanobug::{document::Document, i18n, vim::Action};
use std::{
    collections::{BTreeMap, HashSet},
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

impl Workspace {
    /// Restart after applying the requested `close` policy and retain the startup `command`.
    pub(super) fn restart_from_ex(
        &mut self,
        close: &str,
        command: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use super::editor_close::CloseTarget;
        if matches!(close, "wqa" | "wqall") && !self.write_ex_buffers(true, window, cx) {
            return;
        }
        let force = close.ends_with('!');
        for project in self.projects.iter_mut() {
            project.refresh_shared_documents();
        }
        if !force
            && !self.active_ex_window_modifiers.confirm
            && self.projects.ordered().any(|project| {
                project.detached_documents().any(Document::modified)
                    || project.editors().any(|(_, editor)| editor.modified())
            })
        {
            self.set_status(i18n::text("ex.unsaved_buffers").into());
            return;
        }
        self.restart_ex_command = Some(command);
        if force {
            self.force_ex_close(CloseTarget::Restart, window, cx);
        } else {
            self.request_close(CloseTarget::Restart, window, cx);
        }
    }

    /// Search the effective runtime path for `patterns` and run the matched scripts as Ex.
    pub(super) fn source_ex_runtime(
        &mut self,
        patterns: &str,
        all: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let patterns = argument_words(patterns);
        if patterns.is_empty() {
            self.set_status(i18n::text("ex.runtime_path").into());
            return;
        }
        let options = self.input_mappings.options();
        let root = self.ex_current_dir();
        let configured = if options.runtimepath.is_empty() {
            let portable = self
                .appearance_settings_path
                .as_deref()
                .and_then(Path::parent)
                .map_or_else(String::new, |path| {
                    path.join("runtime").to_string_lossy().replace(',', "\\,")
                });
            format!(",runtime,{portable}")
        } else {
            options.runtimepath
        };
        let mut seen = HashSet::new();
        let mut statements = Vec::new();
        for pattern in &patterns {
            for occurrence in 1.. {
                let Some(path) =
                    nanobug::vim::find_file(root, None, &configured, pattern, occurrence)
                else {
                    break;
                };
                if seen.insert(path.clone()) {
                    let operand = path.to_string_lossy();
                    if !operand.contains(['\n', '\r']) {
                        let operand = operand.replace(' ', "\\ ").replace('|', "\\|");
                        statements.push(format!("source {operand}"));
                    }
                }
                if !all {
                    break;
                }
            }
        }
        if statements.is_empty() {
            self.set_status(i18n::format(
                "ex.runtime_not_found",
                &[("pattern", &patterns.join(" "))],
            ));
        } else {
            self.execute_ex_input(&statements.join("\n"), ":runtime", 1, false, window, cx);
        }
    }

    /// Update `directory` (or all configured help directories) and remember it for :help.
    pub(super) fn index_ex_help(&mut self, directory: &str, list_self: bool) {
        let directories = if directory == "ALL" {
            self.local_help_directories()
                .into_iter()
                .filter(|dir| dir.is_dir())
                .collect()
        } else if let Some(path) = self.file_operand(directory) {
            vec![path]
        } else {
            self.set_status(i18n::text("ex.help_tags_argument").into());
            return;
        };
        for directory in directories {
            if let Err(error) = write_help_tags(&directory, list_self) {
                self.set_status(format!("{error:#}"));
                return;
            }
            if !self.help_tag_directories.contains(&directory) {
                self.help_tag_directories.push(directory);
            }
        }
        self.set_status(String::new());
    }

    /// Resolve a local `topic`, with `@ll` selecting the explicitly translated index.
    pub(super) fn lookup_help_tag(&self, topic: &str) -> Option<Document> {
        let (topic, index) = topic
            .rsplit_once('@')
            .filter(|(_, language)| {
                language.len() == 2 && language.bytes().all(|byte| byte.is_ascii_alphabetic())
            })
            .map_or_else(
                || (topic, "tags".to_owned()),
                |(topic, language)| (topic, format!("tags-{language}")),
            );
        self.local_help_directories()
            .into_iter()
            .find_map(|directory| {
                let table = fs::read_to_string(directory.join(&index)).ok()?;
                let file = table.lines().find_map(|line| {
                    let (name, fields) = line.split_once('\t')?;
                    let (file, _) = fields.split_once('\t')?;
                    (name == topic).then_some(file)
                })?;
                let mut document = Document::open(&directory.join(file)).ok()?;
                document.cursor = document.text.find(&format!("*{topic}*")).unwrap_or(0);
                Some(document)
            })
    }

    /// The help directories discoverable through the current project and portable installation.
    fn local_help_directories(&self) -> Vec<PathBuf> {
        let mut directories = self.help_tag_directories.clone();
        if let Some(project) = self.projects.active_project() {
            directories.push(project.root.join("doc"));
            directories.push(self.ex_current_dir().join("doc"));
        }
        if let Some(data) = self
            .appearance_settings_path
            .as_deref()
            .and_then(Path::parent)
        {
            directories.push(data.join("help"));
        }
        directories.sort_unstable();
        directories.dedup();
        directories
    }

    /// Report the local `components` selected by name or a trailing-star prefix.
    pub(super) fn show_ex_health(
        &mut self,
        components: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let wanted = components.split_whitespace().collect::<Vec<_>>();
        let mut listing = String::new();
        for component in ["nanobug", "plugins", "lsp"] {
            if !wanted.is_empty()
                && !wanted.iter().any(|name| {
                    *name == component
                        || name
                            .strip_suffix('*')
                            .is_some_and(|prefix| component.starts_with(prefix))
                })
            {
                continue;
            }
            let _ = writeln!(listing, "{component}");
            match component {
                "nanobug" => {
                    let _ = writeln!(
                        listing,
                        "  Nanobug {} · {}/{}",
                        env!("CARGO_PKG_VERSION"),
                        std::env::consts::OS,
                        std::env::consts::ARCH
                    );
                    let directory = self.ex_current_dir();
                    let _ = writeln!(
                        listing,
                        "  {} {}",
                        if directory.is_dir() { "OK" } else { "ERROR" },
                        directory.display()
                    );
                }
                "plugins" => {
                    let executable = std::env::current_exe().ok().map(|path| {
                        path.with_file_name(format!(
                            "nanobug-plugin-runtime{}",
                            std::env::consts::EXE_SUFFIX
                        ))
                    });
                    if let Some(path) = executable {
                        let _ = writeln!(
                            listing,
                            "  {} {}",
                            if path.is_file() { "OK" } else { "WARN" },
                            path.display()
                        );
                    }
                }
                "lsp" => {
                    if let Some((server, phase)) = self.active_language_server_status() {
                        let _ = writeln!(listing, "  {server} · {phase:?}");
                    } else {
                        let _ = writeln!(listing, "  {}", i18n::text("ex.health_no_lsp"));
                    }
                }
                _ => unreachable!(),
            }
            listing.push('\n');
        }
        if listing.is_empty() {
            self.set_status(i18n::format(
                "ex.health_no_component",
                &[("components", &components)],
            ));
        } else {
            self.action(Action::ShowListing(listing), window, cx);
        }
    }
}

/// Collect definition tokens from `directory` before replacing any generated index.
fn write_help_tags(directory: &Path, list_self: bool) -> Result<()> {
    ensure!(
        directory.is_dir(),
        "{}",
        i18n::format("ex.help_tags_directory", &[("path", &directory.display())])
    );
    let mut files = vec![directory.to_path_buf()];
    let mut tags = BTreeMap::<String, BTreeMap<String, String>>::new();
    while let Some(folder) = files.pop() {
        for entry in fs::read_dir(&folder).with_context(|| format!("Read {}", folder.display()))? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                files.push(entry.path());
                continue;
            }
            let path = entry.path();
            let index = match path.extension().and_then(|extension| extension.to_str()) {
                Some("txt") => "tags".to_owned(),
                Some(ext)
                    if ext.len() == 3
                        && ext.ends_with('x')
                        && ext[..2].bytes().all(|byte| byte.is_ascii_alphabetic()) =>
                {
                    format!("tags-{}", &ext[..2])
                }
                _ => continue,
            };
            let names = tags.entry(index).or_default();
            let file = path
                .strip_prefix(directory)?
                .to_string_lossy()
                .replace('\\', "/");
            let document = Document::open(&path)?;
            let mut example = false;
            for line in document.text.lines() {
                if line.starts_with('<') {
                    example = false;
                }
                if example {
                    continue;
                }
                if line.trim_end().ends_with('>') || line.trim_end().ends_with(">vim") {
                    example = true;
                }
                for token in line.split_whitespace() {
                    let Some(tag) = token
                        .strip_prefix('*')
                        .and_then(|name| name.strip_suffix('*'))
                        .filter(|name| !name.is_empty() && !name.contains('*'))
                    else {
                        continue;
                    };
                    if names.insert(tag.to_owned(), file.clone()).is_some() {
                        bail!(
                            "{}",
                            i18n::format("ex.help_tags_duplicate", &[("tag", &tag)])
                        );
                    }
                }
            }
        }
    }
    for (index, mut tags) in tags {
        if list_self {
            tags.insert("help-tags".to_owned(), index.clone());
        }
        let mut output = String::from("!_TAG_FILE_ENCODING\tutf-8\t//\n");
        for (tag, file) in tags {
            let search = tag.replace('\\', "\\\\").replace('/', "\\/");
            let _ = writeln!(output, "{tag}\t{file}\t/*{search}*");
        }
        Document::new(&output).write_copy(&directory.join(index), true)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Keep sorted English and translated tags, and preserve existing indexes on a duplicate.
    #[test]
    fn help_tags_index_nested_and_translated_documents() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let directory = temp.path();
        let nested = directory.join("nested");
        fs::create_dir(&nested)?;
        fs::write(nested.join("manual.txt"), "*zebra* text\n*alpha* text\n")?;
        fs::write(directory.join("manual.cnx"), "*zebra* 中文\n")?;
        write_help_tags(directory, true)?;
        let english = fs::read_to_string(directory.join("tags"))?;
        assert!(english.contains("alpha\tnested/manual.txt\t/*alpha*\nhelp-tags\ttags"));
        assert!(fs::read_to_string(directory.join("tags-cn"))?.contains("zebra\tmanual.cnx"));
        fs::write(directory.join("duplicate.txt"), "*alpha* duplicate\n")?;
        assert!(write_help_tags(directory, false).is_err());
        assert_eq!(fs::read_to_string(directory.join("tags"))?, english);
        Ok(())
    }
}
