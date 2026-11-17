//! Resolve Ex working directories without changing the process directory or project root.

use super::{FileCommand, FileOperation, Workspace, single_operand};
use nanobug::{i18n, vim::find_directory};
use std::path::{Path, PathBuf};

impl Workspace {
    /// Resolve the Ex directory from window, Tabpage, app or initial Session scope.
    pub(in crate::grid::workspace) fn ex_current_dir(&self) -> &Path {
        let global = self.ex_directory.path();
        self.projects.active_project().map_or_else(
            || global.unwrap_or_else(|| Path::new(".")),
            |project| {
                project
                    .editor
                    .directory()
                    .path()
                    .or_else(|| project.tabpage_directory().path())
                    .or(global)
                    .unwrap_or(&project.root)
            },
        )
    }

    /// Apply a validated current-directory change and maintain the selected scope's history.
    pub(super) fn change_ex_directory(&mut self, command: FileCommand) {
        use FileOperation::{GlobalDirectory, TabDirectory, WindowDirectory};
        let operation = command.operation;
        let argument = &command.arguments;
        if command.force && argument.is_empty() && operation != GlobalDirectory {
            let project = self.projects.active_mut();
            if operation == TabDirectory {
                project.tabpage_directory_mut().clear();
            } else {
                project.editor.directory_mut().clear();
            }
            self.show_ex_directory();
            return;
        }

        let options = self.input_mappings.options();
        let current = self.ex_current_dir();
        let project = self.projects.active();
        let target = if argument.is_empty() {
            if !options.cdhome {
                self.show_ex_directory();
                return;
            }
            dirs::home_dir()
        } else if argument == "-" {
            let slot = match operation {
                GlobalDirectory => &self.ex_directory,
                TabDirectory => project.tabpage_directory(),
                WindowDirectory => project.editor.directory(),
                _ => unreachable!("directory operation"),
            };
            let Some(previous) = slot.previous() else {
                self.set_status(i18n::text("ex.no_previous_directory").into());
                return;
            };
            Some(previous.to_owned())
        } else {
            let Some(name) = single_operand(argument) else {
                self.invalid_file_argument(argument);
                return;
            };
            match name.as_str() {
                "%" | "#" | "%:h" | "#:h" => self.file_operand(argument),
                _ => find_directory(current, &options.cdpath, &name),
            }
        };
        let Some(path) = target
            .and_then(|path| path.canonicalize().ok())
            .filter(|path| path.is_dir())
        else {
            self.set_status(i18n::format("ex.directory_missing", &[("path", &argument)]));
            return;
        };

        let previous = current.to_owned();
        let project = self.projects.active_mut();
        match operation {
            GlobalDirectory => {
                project.tabpage_directory_mut().clear();
                project.editor.directory_mut().clear();
                self.ex_directory.replace(path, previous);
            }
            TabDirectory => {
                project.editor.directory_mut().clear();
                project.tabpage_directory_mut().replace(path, previous);
            }
            WindowDirectory => project.editor.directory_mut().replace(path, previous),
            _ => unreachable!("directory operation"),
        }
        self.show_ex_directory();
    }

    /// Display a successful directory result as a Vim message to keep sourced scripts running.
    pub(super) fn show_ex_directory(&mut self) {
        let directory = self.ex_current_dir().display().to_string();
        self.set_status(String::new());
        self.projects.active_mut().editor.edit(|vim, _| {
            vim.message = directory;
            nanobug::vim::Action::None
        });
    }

    /// Resolve a single escaped Ex filename against the effective directory.
    pub(in crate::grid::workspace) fn file_operand(&self, text: &str) -> Option<PathBuf> {
        let word = single_operand(text)?;
        let project = self.projects.active_project()?;
        let path = match word.as_str() {
            "%" => project.editor.vim().doc.path.clone()?,
            "#" => project.editor.alternate_path()?.to_owned(),
            "%:h" => project
                .editor
                .vim()
                .doc
                .path
                .as_deref()?
                .parent()?
                .to_owned(),
            "#:h" => project.editor.alternate_path()?.parent()?.to_owned(),
            "~" => dirs::home_dir()?,
            _ if word.starts_with("~/") || word.starts_with("~\\") => {
                dirs::home_dir()?.join(&word[2..])
            }
            _ => self.ex_current_dir().join(word),
        };
        Some(path.canonicalize().unwrap_or(path))
    }
}
