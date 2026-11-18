//! Buffer filetype detection and Ex requests to the host's language servers.

use super::{Action, Register, Vim};
use std::{path::Path, sync::Arc};

/// Ex names offered by command-line completion.
pub(super) const NAMES: &[&str] = &["filetype", "lsp", "setfiletype"];

/// An action affecting this workspace's automatic filetype detection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FiletypeCommand {
    /// Show whether detection is enabled.
    Status,
    /// Recompute the active buffer's filetype from its real path and contents.
    Detect,
    /// Enable inference when a buffer has no explicit filetype.
    On,
    /// Turn off automatic inference.
    Off,
}

/// A language server lifecycle operation requested by the user.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LspCommand {
    Enable,
    Disable,
    Stop,
    Restart,
}

impl LspCommand {
    /// Command words understood by the parser and shown in completion.
    pub(super) const NAMES: &[&str] = &["disable", "enable", "restart", "stop"];

    /// Resolve the operation's exact command word.
    pub(super) fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "enable" => Self::Enable,
            "disable" => Self::Disable,
            "stop" => Self::Stop,
            "restart" => Self::Restart,
            _ => return None,
        })
    }
}

/// An explicit buffer type or a guess that another detection can replace.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct BufferFiletype {
    /// Canonical filetype name when assigned by an option or detection command.
    pub(super) name: Option<Arc<str>>,
    /// Whether :setfiletype may replace the current choice without an explicit reset.
    pub(super) fallback: bool,
}

/// Normalize one user type name without interpreting filenames or script fragments.
pub(super) fn normalize(name: &str) -> Option<String> {
    let bytes = name.as_bytes();
    (bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(b)))
    .then(|| name.to_ascii_lowercase())
}

impl Vim {
    /// Parse filetype commands and delegate lifecycle operations to the host.
    pub(super) fn ex_language_command(&mut self, text: &str) -> Option<Action> {
        let mut words = text.split_whitespace();
        let command = words.next()?;
        let invalid = |vim: &mut Vim, key| {
            vim.publish_ex_message(&crate::i18n::format(key, &[("argument", &text)]), true);
            Action::None
        };
        if command.starts_with("filet") && "filetype".starts_with(command) {
            let operation = match words.next() {
                None => FiletypeCommand::Status,
                Some("detect") => FiletypeCommand::Detect,
                Some("on") => FiletypeCommand::On,
                Some("off") => FiletypeCommand::Off,
                _ => return Some(invalid(self, "ex.filetype_usage")),
            };
            return Some(if words.next().is_none() {
                Action::Filetype(operation)
            } else {
                invalid(self, "ex.filetype_usage")
            });
        }
        if command.starts_with("setf") && "setfiletype".starts_with(command) {
            let first = words.next();
            let fallback = first == Some("FALLBACK");
            let name = if fallback { words.next() } else { first };
            let Some(name) = name.and_then(normalize).filter(|_| words.next().is_none()) else {
                return Some(invalid(self, "ex.setfiletype_usage"));
            };
            self.assign_detected_filetype(name, fallback);
            return Some(Action::None);
        }
        if command == "lsp" {
            let Some(command) = words.next().and_then(LspCommand::parse) else {
                return Some(invalid(self, "ex.lsp_usage"));
            };
            let target = words.next().map(str::to_owned);
            return Some(if words.next().is_none() {
                Action::LanguageServer { command, target }
            } else {
                invalid(self, "ex.lsp_usage")
            });
        }
        None
    }

    /// Apply a workspace detection action using this view's shared input options.
    pub fn apply_filetype_command(&mut self, command: FiletypeCommand, register: &mut Register) {
        self.set_input_options(&register.input_maps);
        let mut options = register.input_maps.options();
        match command {
            FiletypeCommand::Status => {
                let message = if options.filetype_detection {
                    "ex.filetype_on"
                } else {
                    "ex.filetype_off"
                };
                self.publish_ex_message(crate::i18n::text(message), false);
            }
            FiletypeCommand::Detect => {
                options.filetype_detection = true;
                register.input_maps.set_options(options);
                self.redetect_filetype();
                self.publish_ex_message(&format!("filetype={}", self.filetype()), false);
            }
            FiletypeCommand::On | FiletypeCommand::Off => {
                options.filetype_detection = command == FiletypeCommand::On;
                register.input_maps.set_options(options);
            }
        }
    }
}

/// Infer a common Vim filetype from the real buffer name, then a supported shebang.
pub(super) fn infer(path: Option<&Path>, text: &str) -> Option<&'static str> {
    let name = path
        .and_then(Path::file_name)
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    let ext = path
        .and_then(Path::extension)
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    Some(match (name, ext.as_str()) {
        ("Makefile" | "makefile" | "GNUmakefile", _) | (_, "mk") => "make",
        ("Dockerfile" | "Containerfile", _) => "dockerfile",
        ("Gemfile" | "Rakefile", _) | (_, "rb" | "rake" | "gemspec" | "ru") => "ruby",
        (_, "py" | "pyi" | "pyw") => "python",
        (_, "rs") => "rust",
        (_, "c" | "h") => "c",
        (_, "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx") => "cpp",
        (_, "cs" | "csx") => "cs",
        (_, "go") => "go",
        (_, "java") => "java",
        (_, "kt" | "kts") => "kotlin",
        (_, "php" | "phtml") => "php",
        (_, "lua") => "lua",
        (_, "js" | "mjs" | "cjs") => "javascript",
        (_, "jsx") => "javascriptreact",
        (_, "ts" | "mts" | "cts") => "typescript",
        (_, "tsx") => "typescriptreact",
        (_, "md" | "markdown" | "mdown" | "mkd" | "mkdn" | "mdwn") => "markdown",
        (_, "html" | "htm") => "html",
        (_, "css") => "css",
        (_, "vue") => "vue",
        (_, "svelte") => "svelte",
        (_, "astro") => "astro",
        (_, "sh" | "bash") => "sh",
        (_, "zsh") => "zsh",
        (_, "json" | "jsonl") => "json",
        (_, "toml") => "toml",
        (_, "yaml" | "yml") => "yaml",
        (_, "xml") => "xml",
        (_, "sql") => "sql",
        (_, "vim") => "vim",
        (_, "txt") => "text",
        _ => return shebang(text),
    })
}

/// Recognize interpreters in direct and `/usr/bin/env` shebangs without running them.
fn shebang(text: &str) -> Option<&'static str> {
    let mut words = text.lines().next()?.strip_prefix("#!")?.split_whitespace();
    let base = |word: &str| word.rsplit('/').next().unwrap_or(word).to_owned();
    let command = base(words.next()?);
    let command = if command == "env" {
        base(words.find(|word| !word.starts_with('-') && !word.contains('='))?)
    } else {
        command
    };
    Some(
        match command.trim_end_matches(|ch: char| ch.is_ascii_digit() || ch == '.') {
            "python" => "python",
            "ruby" => "ruby",
            "node" => "javascript",
            "sh" | "bash" => "sh",
            "zsh" => "zsh",
            "php" => "php",
            "lua" => "lua",
            _ => return None,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;
    use std::path::PathBuf;

    /// Keep filetype shared across views and invalidate results without introducing an undo edit.
    #[test]
    fn filetype_override_changes_the_shared_version_without_editing_text() {
        let mut vim = Vim::new(Document::new("original\n"));
        let mut register = Register::default();
        vim.execute_ex_line("setlocal ft?", &mut register);
        let mut split = Vim::new(vim.doc.shared_view());
        split.share_buffer_options(&vim);
        split.doc.cursor = 4;
        let before = vim.doc.revision;

        vim.execute_ex_line("setlocal filetype=Python", &mut register);
        assert_eq!(vim.filetype_override().as_deref(), Some("python"));
        assert_eq!(split.filetype_override().as_deref(), Some("python"));
        assert!(split.refresh_shared_document());
        assert!(split.doc.metadata_revised_since(before));
        assert_eq!(split.doc.cursor, 4);
        assert!(!split.doc.modified());
        vim.execute_ex_line("undo", &mut register);
        assert_eq!(vim.doc.text, "original\n");
        assert_eq!(vim.filetype_override().as_deref(), Some("python"));

        split.doc.detach_buffer();
        split.detach_buffer_options();
        split.execute_ex_line("setlocal ft=", &mut Register::default());
        assert_eq!(split.filetype_override(), None);
        assert_eq!(vim.filetype_override().as_deref(), Some("python"));
    }

    /// Redetect a named extensionless script, replace a guess once and preserve manual choices.
    #[test]
    fn filetype_detection_respects_fallbacks_and_uses_the_real_file_name() {
        let path = PathBuf::from("build-tool");
        let mut document = Document::new("#!/usr/bin/env python3\nprint('ok')\n");
        document.set_ex_filename(Some(path.clone()));
        let mut vim = Vim::new(document);
        let mut register = Register::default();
        for operation in [FiletypeCommand::Off, FiletypeCommand::Detect] {
            let action = if operation == FiletypeCommand::Off {
                "off"
            } else {
                "detect"
            };
            let Action::Filetype(command) =
                vim.execute_ex_line(&format!("filetype {action}"), &mut register)
            else {
                panic!("expected filetype action");
            };
            vim.apply_filetype_command(command, &mut register);
            if operation == FiletypeCommand::Off {
                assert_eq!(vim.filetype(), "");
            }
        }
        assert_eq!(vim.filetype_override().as_deref(), Some("python"));

        vim.execute_ex_line("setfiletype FALLBACK sh", &mut register);
        assert_eq!(vim.filetype_override().as_deref(), Some("sh"));
        vim.execute_ex_line("setfiletype lua", &mut register);
        vim.execute_ex_line("setfiletype ruby", &mut register);
        assert_eq!(vim.filetype_override().as_deref(), Some("lua"));
        vim.execute_ex_line("setlocal ft?", &mut register);
        assert_eq!(vim.message, "filetype=lua");
        vim.execute_ex_line("setlocal ft=../invalid", &mut register);
        assert!(vim.ex_failed());
        assert_eq!(vim.filetype_override().as_deref(), Some("lua"));
        assert_eq!(vim.doc.path.as_ref(), Some(&path));
    }

    /// Complete lifecycle verbs and only Session-provided targets, then parse the exact name.
    #[test]
    fn language_server_commands_complete_and_parse_names() {
        let mut vim = Vim::new(Document::new(""));
        let mut register = Register::default();
        vim.set_language_completion(
            vec!["python/complete".into(), "rust".into()],
            vec!["python".into()],
        );
        assert_eq!(
            vim.command_candidates("lsp re", &register),
            Some((4, vec!["restart".into()]))
        );
        assert_eq!(
            vim.command_candidates("lsp restart p", &register),
            Some((12, vec!["python/complete".into()]))
        );
        let Action::LanguageServer { command, target } =
            vim.execute_ex_line("lsp restart python/complete", &mut register)
        else {
            panic!("expected a language server operation");
        };
        assert_eq!(command, LspCommand::Restart);
        assert_eq!(target.as_deref(), Some("python/complete"));
        assert_eq!(
            vim.command_candidates("setlocal ft=py", &register),
            Some((12, vec!["python".into()]))
        );
        assert_eq!(
            vim.command_candidates("setfiletype FALLBACK py", &register),
            Some((21, vec!["python".into()]))
        );
        vim.execute_ex_line("lsp stop python rust", &mut register);
        assert!(vim.ex_failed());
    }
}
