//! Write sourceable configuration, view and Session files through Rust filesystem APIs.

use super::Workspace;
use anyhow::{Context as _, Result, ensure};
use gpui::{Context, Window};
use nanobug::{
    app_data, i18n,
    vim::{ExportKind, SessionExport},
};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

/// Comment marker preceding an exact Nanobug layout snapshot in a sourceable script.
const SESSION_PREFIX: &str = "\" nanobug-session-v1 ";

impl Workspace {
    /// Save the selected export or source a numbered View in the current Session.
    pub(super) fn session_export_ex(
        &mut self,
        command: SessionExport,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let result = match command {
            SessionExport::Save {
                kind,
                target,
                force,
            } => self.save_ex_export(kind, &target, force),
            SessionExport::LoadView(slot) => self.view_path(slot, false).map(|path| {
                self.source_ex(Some(&escaped_path(&path)), None, window, cx);
            }),
        };
        if let Err(error) = result {
            self.set_status(format!("{error:#}"));
        }
    }

    /// Intercept an exported layout before reading subsequent portable view commands.
    pub(super) fn restore_exported_session(
        &mut self,
        text: &str,
        source: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(snapshot) = text
            .trim_start_matches('\u{feff}')
            .lines()
            .next()
            .and_then(|line| line.strip_prefix(SESSION_PREFIX))
        else {
            return false;
        };
        self.finish_editor_input(window, cx);
        match self.projects.restore_ex_session(snapshot) {
            Ok(()) => self.reset_tab_view(window, cx),
            Err(error) => self.set_status(format!("{source}: {error:#}")),
        }
        true
    }

    /// Resolve the default or explicit target before atomically publishing an export.
    fn save_ex_export(&mut self, kind: ExportKind, argument: &str, force: bool) -> Result<()> {
        let numbered =
            kind == ExportKind::View && (argument.is_empty() || view_slot(argument).is_some());
        let path = if numbered {
            self.view_path(view_slot(argument), true)?
        } else if argument.is_empty() {
            self.ex_current_dir().join(match kind {
                ExportKind::Session => "Session.vim",
                ExportKind::Exrc => ".exrc",
                ExportKind::Vimrc => ".nvimrc",
                ExportKind::View => unreachable!("numbered view"),
            })
        } else {
            self.file_operand(argument).with_context(|| {
                i18n::format("editor.invalid_ex_argument", &[("argument", &argument)])
            })?
        };
        let mut lines = match kind {
            ExportKind::View => self.ex_view_lines(!numbered),
            ExportKind::Session => {
                let snapshot = self.projects.active().ex_session_snapshot()?;
                let mut lines = vec![format!("{SESSION_PREFIX}{snapshot}")];
                lines.extend(self.ex_view_lines(true));
                lines
            }
            ExportKind::Exrc | ExportKind::Vimrc => {
                let mut lines = self.input_mappings.ex_configuration_lines();
                if kind == ExportKind::Vimrc {
                    lines.insert(0, format!("\" Nanobug {}", env!("CARGO_PKG_VERSION")));
                }
                lines
            }
        };
        lines.push(String::new());
        write_export(&path, &lines.join("\n"), numbered || force)?;
        self.set_status(String::new());
        Ok(())
    }

    /// Build the portable Ex operations which restore the displayed editor window.
    fn ex_view_lines(&self, named: bool) -> Vec<String> {
        let project = self.projects.active();
        let editor = &project.editor;
        let vim = editor.vim();
        let file = vim.doc.path.as_deref();
        let pane = project.active_editor_id();
        let files = project.arguments.files(pane);
        let index = project.arguments.index(pane);
        let mut lines = Vec::new();
        if !files.is_empty() {
            lines.push(format!(
                "args {}",
                files
                    .iter()
                    .map(|path| escaped_path(path))
                    .collect::<Vec<_>>()
                    .join(" ")
            ));
            if index != 0 {
                lines.push(format!("argument {}", index + 1));
            }
        }
        if (files.is_empty() && named)
            || (!files.is_empty() && files.get(index).map(PathBuf::as_path) != file)
        {
            if let Some(file) = file {
                lines.push(format!("badd {}", escaped_path(file)));
                lines.push(format!("buffer {}", escaped_path(file)));
            } else {
                lines.push("enew".into());
            }
        }
        if let Some(path) = editor.directory().path() {
            lines.push(format!("lcd {}", escaped_path(path)));
        } else if let Some(path) = project.tabpage_directory().path() {
            lines.push(format!("tcd {}", escaped_path(path)));
        }
        lines.extend(vim.ex_view_option_lines(editor.register()));
        lines.extend(vim.ex_local_mapping_lines());
        lines.push("normal! zE".into());
        let folds = vim.doc.ex_manual_fold_ranges();
        for (range, _) in &folds {
            lines.push(format!("{},{}fold", range.start + 1, range.end));
        }
        for (range, closed) in folds {
            if !closed {
                lines.push(format!("{}foldopen", range.start + 1));
            }
        }
        let (row, column) = vim.doc.location(vim.doc.cursor);
        lines.push(format!("normal! {}Gzt", editor.ex_view_top_row() + 1));
        lines.push(format!("normal! {}G{}|", row + 1, column + 1));
        lines
    }

    /// Locate a numbered View by the original filename and the configured portable directory.
    fn view_path(&self, slot: Option<u8>, create: bool) -> Result<PathBuf> {
        let name = self
            .projects
            .active()
            .editor
            .vim()
            .doc
            .path
            .as_deref()
            .context(i18n::text("ex.view_needs_file"))?;
        let directory = self.input_mappings.options().viewdir;
        let base = if directory.is_empty() {
            app_data::project_sessions_directory()?.with_file_name("view")
        } else {
            self.file_operand(&directory)
                .unwrap_or_else(|| self.ex_current_dir().join(directory))
        };
        if create {
            fs::create_dir_all(&base)?;
        }
        let key = Sha256::digest(name.to_string_lossy().as_bytes());
        let suffix = slot.map_or_else(String::new, |slot| slot.to_string());
        Ok(base.join(format!("{key:x}={suffix}.vim")))
    }
}

/// Match the optional one-digit file slot of :mkview or :loadview.
fn view_slot(argument: &str) -> Option<u8> {
    match argument.as_bytes() {
        [number @ b'1'..=b'9'] => Some(number - b'0'),
        _ => None,
    }
}

/// Escape a filename with spaces so source can pass it to the Ex parser.
fn escaped_path(path: &Path) -> String {
    path.to_string_lossy().replace(' ', "\\ ")
}

/// Stage bytes beside the final file and replace it only when the command authorizes this.
fn write_export(path: &Path, content: &str, force: bool) -> Result<()> {
    let parent = path.parent().context("Missing Ex output directory")?;
    ensure!(force || !path.exists(), "{}", i18n::text("ex.file_exists"));
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    staged.write_all(content.as_bytes())?;
    staged.as_file().sync_all()?;
    if force {
        staged.persist(path)?;
    } else {
        staged.persist_noclobber(path)?;
    }
    Ok(())
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::grid::workspace::native_test;
    use gpui::AppContext;
    use nanobug::plugins::session::Options;
    use std::time::Duration;

    /// Invoke a real Ex action and refresh the native view before the next command.
    fn command(
        workspace: &mut Workspace,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        workspace.run_vim(
            |vim, register| vim.execute_ex_line(text, register),
            window,
            cx,
        );
        workspace.sync(window, cx);
    }

    /// Restore a numbered current view, source configuration and rebuild a captured Session.
    #[test]
    #[ignore = "requires Windows DirectWrite and DirectX"]
    fn native_ex_session_exports_round_trip() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().join("project");
        fs::create_dir(&root)?;
        fs::write(root.join("note.txt"), "first\nsecond\nthird line\n")?;
        let options = Options::take(&mut vec!["--no-plugins".into()], temporary.path())?;
        native_test::run_async(
            root,
            options,
            |_| {},
            async move |handle, app| {
                app.update_window(handle.into(), |_, window, _| {
                    window.resize(gpui::size(gpui::px(920.), gpui::px(580.)))
                })?;
                app.background_executor()
                    .timer(Duration::from_millis(60))
                    .await;
                app.update(|app| {
                    handle.update(app, |workspace, window, cx| -> Result<()> {
                        command(workspace, "edit note.txt", window, cx);
                        command(workspace, "set viewdir=views", window, cx);
                        command(workspace, "setlocal number", window, cx);
                        workspace.run_vim(
                            |vim, _| {
                                vim.place_jump(vim.doc.offset(2, 4));
                                nanobug::vim::Action::None
                            },
                            window,
                            cx,
                        );
                        command(workspace, "mkview 1", window, cx);
                        let path = workspace.view_path(Some(1), false)?;
                        ensure!(path.is_file());
                        command(workspace, "normal! gg", window, cx);
                        command(workspace, "setlocal nonumber", window, cx);
                        command(workspace, "loadview 1", window, cx);
                        let editor = &workspace.projects.active().editor;
                        let restored = editor.vim().doc.location(editor.vim().doc.cursor);
                        ensure!(
                            restored == (2, 4),
                            "Restored view at {restored:?}. {} {}. Script: {}",
                            workspace.projects.active().status,
                            editor.vim().message,
                            fs::read_to_string(&path)?
                        );
                        ensure!(editor.window_options().number);

                        command(workspace, "set ignorecase", window, cx);
                        command(workspace, "nnoremap <F8> w", window, cx);
                        command(workspace, "mkvimrc local.vim", window, cx);
                        let path = workspace.ex_current_dir().join("local.vim");
                        let script = fs::read_to_string(path)?;
                        ensure!(
                            script.contains("set ignorecase") && script.contains("nnoremap <F8> w")
                        );
                        command(workspace, "set noignorecase", window, cx);
                        command(workspace, "nunmap <F8>", window, cx);
                        command(workspace, "source local.vim", window, cx);
                        ensure!(workspace.input_mappings.options().ignorecase);
                        let editor = &workspace.projects.active().editor;
                        ensure!(editor.vim().accepts_input_mapping("F8", editor.register()));

                        command(workspace, "split", window, cx);
                        command(workspace, "tabnew", window, cx);
                        command(workspace, "mksession", window, cx);
                        command(workspace, "tabonly", window, cx);
                        ensure!(workspace.projects.active().page_count() == 1);
                        command(workspace, "edit note.txt", window, cx);
                        let note = workspace.projects.active().editor.vim().doc.buffer_number();
                        command(workspace, "terminal pause", window, cx);
                        let terminal = workspace.projects.active().editor.vim().doc.buffer_number();
                        ensure!(workspace.projects.active().terminal(terminal).is_some());
                        command(workspace, &format!("buffer {note}"), window, cx);
                        ensure!(
                            workspace.projects.active().editor.vim().doc.buffer_number() == note
                        );
                        command(workspace, "source Session.vim", window, cx);
                        ensure!(
                            workspace.projects.active().page_count() == 2,
                            "{} {}",
                            workspace.projects.active().status,
                            workspace.projects.active().editor.vim().message
                        );
                        ensure!(workspace.projects.active().terminal(terminal).is_some());
                        ensure!(workspace.projects.active().editor.terminal().is_none());
                        command(workspace, "tabfirst", window, cx);
                        ensure!(workspace.projects.active().split_count() == 2);
                        Ok(())
                    })
                })?
            },
        )
    }
}
