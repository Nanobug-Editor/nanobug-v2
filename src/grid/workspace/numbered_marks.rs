//! Save caret positions after an approved close and share them with the native editor.

use super::Workspace;
use nanobug::app_data::numbered_marks::{self, Mark};
use std::{path::Path, sync::Arc};

/// Read saved marks from `path`, leaving a damaged or absent history out of active input.
pub(super) fn load(path: Option<&Path>) -> Arc<[Mark]> {
    path.and_then(|path| match numbered_marks::load(path) {
        Ok(marks) => Some(marks.into()),
        Err(error) => {
            eprintln!("Could not load Vim numbered marks: {error:#}");
            None
        }
    })
    .unwrap_or_default()
}

impl Workspace {
    /// Apply :delmarks to the specified portable numbered positions.
    pub(super) fn delete_numbered_marks(&mut self, slots: &[usize]) {
        if let Some(path) = &self.numbered_marks_path {
            match numbered_marks::delete(path, slots) {
                Ok(marks) => self.numbered_marks = marks.into(),
                Err(error) => self.set_status(format!("{error:#}")),
            }
        } else {
            let mut marks = self.numbered_marks.to_vec();
            numbered_marks::clear_slots(&mut marks, slots);
            self.numbered_marks = marks.into();
        }
        if let Some(project) = self.projects.active_project_mut() {
            project
                .editor
                .set_numbered_marks(self.numbered_marks.clone());
        }
    }

    /// Apply the portable numbered-mark portion of rshada or wshada according to `write`.
    pub(super) fn update_numbered_marks(&mut self, write: bool) {
        if write {
            self.save_numbered_mark();
        } else {
            self.numbered_marks = load(self.numbered_marks_path.as_deref());
        }
    }

    /// Transfer the portable numbered positions used by the ShaDa aliases to `path`.
    pub(super) fn transfer_vim_info(&mut self, write: bool, force: bool, path: Option<&str>) {
        let destination = match path {
            Some(path) => self.file_operand(path),
            None => self.numbered_marks_path.clone(),
        };
        let Some(destination) = destination else {
            self.set_status(nanobug::i18n::text("ex.viminfo_path").into());
            return;
        };
        let result = if write {
            let mut marks = self.numbered_marks.to_vec();
            if let Some(mark) = self.current_numbered_mark() {
                marks.retain(|prior| prior != &mark);
                marks.insert(0, mark);
                marks.truncate(10);
            }
            numbered_marks::export(&destination, &marks, force).map(|marks| {
                if path.is_none() {
                    self.numbered_marks = marks.into();
                }
            })
        } else {
            numbered_marks::load(&destination).map(|mut marks| {
                if !force {
                    let incoming = std::mem::replace(&mut marks, self.numbered_marks.to_vec());
                    numbered_marks::merge(&mut marks, incoming);
                }
                self.numbered_marks = marks.into();
            })
        };
        self.report_ex_result(result);
    }

    /// Put the active file's caret at mark 0 on disk after a confirmed close or explicit save.
    pub(super) fn save_numbered_mark(&mut self) {
        let Some(path) = self.numbered_marks_path.as_ref() else {
            return;
        };
        let Some(mark) = self.current_numbered_mark() else {
            return;
        };
        match numbered_marks::remember(path, mark) {
            Ok(marks) => self.numbered_marks = marks.into(),
            Err(error) => eprintln!("Could not save Vim numbered marks: {error:#}"),
        }
    }

    /// Locate the active file caret eligible for portable numbered history.
    fn current_numbered_mark(&self) -> Option<Mark> {
        let vim = self
            .projects
            .active_project()
            .map(|project| project.editor.vim())
            .filter(|vim| !vim.is_read_only())?;
        let file = vim.doc.path.as_ref().filter(|path| path.is_file())?;
        let (row, column) = vim.doc.location(vim.doc.cursor);
        Some(Mark {
            path: file.clone(),
            row,
            column,
        })
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::grid::workspace::native_test;
    use anyhow::{Result, ensure};
    use gpui::{KeyDownEvent, Keystroke};
    use nanobug::plugins::session::Options;
    use std::fs;

    /// Read a previous session's file marks and use exact and linewise native key sequences.
    #[test]
    #[ignore = "opens a native Windows workspace"]
    fn native_numbered_marks_restore_from_portable_history() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("project");
        fs::create_dir(&root)?;
        let selected = root.join("README.md");
        let saved = root.join("中文.txt");
        fs::write(&selected, "first\n")?;
        fs::write(&saved, "skip\n  abc def\n")?;
        let saved = saved.canonicalize()?;
        let path = temp.path().join("vim-numbered-marks.json");
        numbered_marks::remember(
            &path,
            Mark {
                path: saved.clone(),
                row: 1,
                column: 6,
            },
        )?;
        let options = Options::take(&mut vec!["--no-plugins".into()], temp.path())?;
        native_test::run_async(
            root,
            options,
            move |workspace| {
                workspace.numbered_marks_path = Some(path);
                workspace.numbered_marks = load(workspace.numbered_marks_path.as_deref());
            },
            async move |handle, app| {
                app.update(|app| {
                    handle.update(app, |workspace, window, cx| -> Result<()> {
                        workspace.open(selected, window, cx);
                        workspace.sync(window, cx);
                        for key in ["`", "0", "l", "'", "0"] {
                            workspace.key_down(
                                &KeyDownEvent {
                                    keystroke: Keystroke::parse(key)?,
                                    is_held: false,
                                    prefer_character_input: false,
                                },
                                window,
                                cx,
                            );
                            if key == "0" {
                                let vim = workspace.projects.active().editor.vim();
                                ensure!(vim.doc.path.as_ref() == Some(&saved));
                                ensure!(vim.doc.location(vim.doc.cursor).0 == 1);
                            }
                            if key == "l" {
                                let vim = workspace.projects.active().editor.vim();
                                ensure!(vim.doc.location(vim.doc.cursor).1 == 7);
                            }
                        }
                        let vim = workspace.projects.active().editor.vim();
                        ensure!(vim.doc.location(vim.doc.cursor) == (1, 2));
                        workspace.save_numbered_mark();
                        ensure!(workspace.numbered_marks[0].column == 2);
                        ensure!(workspace.numbered_marks[1].column == 6);
                        Ok(())
                    })
                })?
            },
        )
    }
}
