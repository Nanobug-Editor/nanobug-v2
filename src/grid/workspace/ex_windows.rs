//! Adapt modal Ex window commands to retained editors and native surfaces.

use super::{
    Workspace, editor_close::CloseTarget, editor_splits::SplitAxis, ex_commands::resolve_buffer,
    layout::Focus, projects::SplitDocument,
};
use crate::{
    grid::windows::editor::{Buffer, EditorWindow},
    ui::theme::ThemeChoice,
};
use gpui::{Context, Window};
use nanobug::{
    document::Document,
    i18n,
    vim::{Action, ExModeChange, ViewInformation, ViewSize, WindowCommand, WindowOrientation},
};

impl Workspace {
    /// Apply a window `command` from the active Vim editor.
    pub(super) fn window_extras(
        &mut self,
        command: WindowCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match command {
            WindowCommand::ColorScheme(name) => {
                if name.is_empty() {
                    let name = match self.appearance.theme {
                        ThemeChoice::Dark => "dark",
                        ThemeChoice::Light => "light",
                        ThemeChoice::System => "system",
                    };
                    self.show_ex_setting(name.into());
                } else {
                    let name = if name == "default" {
                        "dark".into()
                    } else {
                        name
                    };
                    self.action(Action::Appearance("theme".into(), name), window, cx);
                }
            }
            WindowCommand::Language(arguments) => self.ex_language(&arguments, window, cx),
            WindowCommand::PreviewBuffer { selector, command } => {
                self.preview_ex_buffer(&selector, command, window, cx)
            }
            WindowCommand::Redraw => self.sync(window, cx),
            WindowCommand::SyncBind => self.sync_scrollbound_editors(window, cx),
            WindowCommand::Resize {
                window: number,
                columns,
                size,
            } => {
                let columns = self
                    .active_ex_window_modifiers
                    .orientation
                    .map_or(columns, |orientation| {
                        orientation == WindowOrientation::Vertical
                    });
                self.resize_ex_window(number, columns, size, window, cx);
            }
            WindowCommand::Sleep {
                duration,
                hide_caret,
            } => self.sleep_editor_for(duration, hide_caret, window, cx),
            WindowCommand::ChangeMode { kind, line_end } => {
                self.change_ex_mode(kind, line_end, window, cx)
            }
            WindowCommand::Information(info) => self.show_ex_information(info, window, cx),
            WindowCommand::HelpClose => self.close_ex_help(window, cx),
            WindowCommand::ApplyOption { pane, command } => {
                self.apply_option_from_sheet(pane, &command, window, cx)
            }
        }
    }

    /// Show a successful Ex setting query without interrupting a sourced command sequence.
    pub(super) fn show_ex_setting(&mut self, result: String) {
        self.set_status(String::new());
        if let Some(project) = self.projects.active_project_mut() {
            project.editor.edit(|vim, _| {
                vim.message = result;
                Action::None
            });
        }
    }

    /// Display another view of `selector` in the current Tabpage's preview pane.
    fn preview_ex_buffer(
        &mut self,
        selector: &str,
        command: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.projects.active_project() else {
            return;
        };
        let original = project.active_editor_id();
        let previous_preview = project.preview_pane();
        let number = if selector.is_empty() {
            Some(
                previous_preview
                    .and_then(|id| project.editor_by_id(id))
                    .unwrap_or(&project.editor)
                    .vim()
                    .doc
                    .buffer_number(),
            )
        } else {
            resolve_buffer(project, &selector.replace("\\ ", " "))
        };
        let Some(number) = number else {
            self.set_status(i18n::text("vim_bracket.no_buffer").into());
            return;
        };
        if self.ex_tab_insertion().is_some() {
            let origin = project.active_editor_id();
            let project = self.projects.active_mut();
            if let Err(error) = project.load_detached_buffer(number) {
                self.set_status(format!("{error:#}"));
                return;
            }
            let Some(view) = project
                .session_buffer(number)
                .map(|buffer| buffer.preview_view())
            else {
                return;
            };
            self.open_editor_split_at(
                true,
                false,
                SplitDocument::SharedView(Box::new(view)),
                None,
                window,
                cx,
            );
            if self.projects.active().active_editor_id() != origin
                && let Some(command) = command
            {
                self.after_file_load(Some(&command), window, cx);
            }
            return;
        }
        let now = self.scroll_epoch.elapsed().as_secs_f64();
        let already_open = previous_preview
            .and_then(|pane| project.editor_by_id(pane))
            .and_then(|editor| {
                editor
                    .buffers()
                    .iter()
                    .find(|buffer| buffer.vim.doc.buffer_number() == number)
            })
            .map(|buffer| buffer.id);
        let pane = if let Some(id) = already_open {
            let pane = previous_preview.expect("existing preview");
            self.projects
                .active_mut()
                .editor_by_id_mut(pane)
                .expect("preview pane")
                .select_buffer(id, now);
            pane
        } else {
            let project = self.projects.active_mut();
            if let Err(error) = project.load_detached_buffer(number) {
                self.set_status(format!("{error:#}"));
                return;
            }
            let Some(view) = project
                .session_buffer(number)
                .map(|buffer| buffer.preview_view())
            else {
                self.set_status(i18n::text("vim_bracket.no_buffer").into());
                return;
            };
            if let Some(pane) = previous_preview {
                let id = view.id;
                let preview = self
                    .projects
                    .active_mut()
                    .editor_by_id_mut(pane)
                    .expect("preview pane");
                preview.retain_buffer_view(view);
                preview.select_buffer(id, now);
                pane
            } else {
                let area = self.visible_editor_region();
                let position = self.ex_split_position(SplitAxis::Rows, false);
                self.finish_editor_input(window, cx);
                let project = self.projects.active_mut();
                match project.open_split_positioned(
                    position,
                    SplitDocument::SharedView(Box::new(view)),
                    area,
                    now,
                ) {
                    Ok(true) => {
                        let preview = project.active_editor_id();
                        project.mark_preview_pane(preview);
                        preview
                    }
                    Ok(false) => {
                        self.set_status(i18n::text("editor.split_no_space").into());
                        return;
                    }
                    Err(error) => {
                        self.set_status(format!("{error:#}"));
                        return;
                    }
                }
            }
        };
        let scope = self.projects.active().plugin_scope;
        if let Some(command) = command {
            self.projects.active_mut().focus_editor_by_id(pane, now);
            self.run_vim(
                |vim, register| {
                    if let Ok(row) = command.parse::<usize>() {
                        let row = row.saturating_sub(1).min(vim.doc.starts.len() - 1);
                        vim.doc.cursor = vim.doc.starts[row];
                        Action::None
                    } else if command == "$" {
                        vim.doc.cursor = *vim.doc.starts.last().expect("document line");
                        Action::None
                    } else if command.starts_with(['/', '?']) {
                        vim.text(&command, register);
                        vim.key("Enter", register)
                    } else {
                        vim.execute_ex_line(&command, register)
                    }
                },
                window,
                cx,
            );
        }
        if let Some(project) = self
            .projects
            .active_project_mut()
            .filter(|project| project.plugin_scope == scope)
        {
            project.focus_editor_by_id(original, now);
            self.reset_tab_view(window, cx);
        }
        self.sync(window, cx);
    }

    /// Update the addressed split by `size`, preserving its sibling layout and focus.
    fn resize_ex_window(
        &mut self,
        number: Option<usize>,
        columns: bool,
        size: ViewSize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let area = self.visible_editor_region();
        let Some(project) = self.projects.active_project_mut() else {
            return;
        };
        let pane = if let Some(number) = number {
            let Some(pane) = number
                .checked_sub(1)
                .and_then(|index| project.splits().ids().get(index).copied())
            else {
                self.set_status(i18n::text("vim_bracket.no_buffer").into());
                return;
            };
            pane
        } else {
            project.active_editor_id()
        };
        let Some(rect) = project.splits().rect(pane, area) else {
            return;
        };
        let body = EditorWindow::body(rect);
        let axis = if columns {
            SplitAxis::Columns
        } else {
            SplitAxis::Rows
        };
        let physical = if columns { body.w } else { body.h };
        let maximum = if columns {
            area.w
        } else {
            EditorWindow::body(area).h
        };
        let current = project.editor_by_id(pane).map_or(physical, |editor| {
            if columns {
                physical
            } else {
                editor.vim().view_height(physical)
            }
        });
        let desired = match size {
            ViewSize::Maximum => maximum,
            ViewSize::Absolute(value) => value,
            ViewSize::Relative(increment) => current.saturating_add_signed(increment),
        }
        .max(1)
        .min(maximum.max(1));
        let delta = desired as isize - physical as isize;
        let window_resized = project.splits_mut().resize_pane(pane, area, axis, delta);
        if !columns {
            let limit = (!window_resized && !matches!(size, ViewSize::Maximum)).then_some(desired);
            project
                .editor_by_id_mut(pane)
                .expect("retained resize target")
                .edit(|vim, _| {
                    vim.set_ex_view_height(limit);
                    Action::None
                });
        }
        self.sync(window, cx);
    }

    /// Change the active Vim or native PTY input mode.
    fn change_ex_mode(
        &mut self,
        kind: ExModeChange,
        line_end: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.projects.active_project_mut() else {
            return;
        };
        if let Some(terminal) = project.editor.terminal_mut() {
            if matches!(kind, ExModeChange::Insert | ExModeChange::StopInsert) {
                terminal.set_input(kind == ExModeChange::Insert);
            }
        } else {
            self.run_vim(
                |vim, register| vim.apply_ex_mode(kind, line_end, register),
                window,
                cx,
            );
        }
        self.sync(window, cx);
    }

    /// Open reference information for a native Ex command.
    fn show_ex_information(
        &mut self,
        info: ViewInformation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match info {
            ViewInformation::ViUsage => {
                if self.projects.active_project().is_some() { self.open_shortcut_help(Focus::Editor, window, cx); }
            }
            ViewInformation::ExUsage(text) | ViewInformation::OptionHelp(text) => self.open_editor_split_at(true, true, SplitDocument::Help(Document::new(&text)), None, window, cx),
            ViewInformation::Intro => self.action(Action::ShowListing(format!("Nanobug {}\n\n:help opens editor help.\n:viusage shows the keyboard guide.\n:exusage lists supported Ex commands.\n:quit closes the active editor window.\n", env!("CARGO_PKG_VERSION"))), window, cx),
            ViewInformation::Version => self.action(Action::ShowListing(format!("Nanobug {}\n{} {}\nRust editor and local Ex runtime\n", env!("CARGO_PKG_VERSION"), std::env::consts::OS, std::env::consts::ARCH)), window, cx),
            ViewInformation::Options(text) => {
                let source = self.projects.active_project().map(|project| project.active_editor_id());
                if let Some(source) = source {
                    let view = Buffer::options_sheet(Document::new(&text), source);
                    self.open_editor_split_at(true, true, SplitDocument::SharedView(Box::new(view)), None, window, cx);
                }
            }
        }
    }

    /// Execute a settings line from an options view without changing its source pane's focus.
    fn apply_option_from_sheet(
        &mut self,
        pane: usize,
        command: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self
            .projects
            .active_project_mut()
            .and_then(|project| project.editor_by_id_mut(pane))
        else {
            self.set_status(i18n::text("vim_bracket.no_buffer").into());
            return;
        };
        let edit = editor.edit(|vim, register| vim.execute_ex_line(command, register));
        let error = editor
            .vim()
            .ex_failed()
            .then(|| editor.vim().message.clone());
        if let Some(error) = error {
            self.set_status(error);
        } else if matches!(edit.action, Action::None) {
            self.set_status(String::new());
        }
        self.sync(window, cx);
    }

    /// Close the active page's Vim Help editor, including the keyboard guide overlay.
    fn close_ex_help(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.help.is_open() {
            self.help.close();
        }
        let Some(project) = self.projects.active_project() else {
            return;
        };
        let help = project
            .page_editors()
            .find_map(|(pane, editor)| editor.vim().is_help().then_some(pane));
        if let Some(pane) = help {
            if project.split_count() == 1 {
                self.action(Action::Close(false), window, cx);
            } else {
                self.request_close(CloseTarget::Split(pane), window, cx);
            }
        }
        self.sync(window, cx);
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::grid::workspace::native_test;
    use anyhow::{Context as _, Result, ensure};
    use gpui::{AppContext, px, size};
    use nanobug::plugins::session::Options;
    use std::{fs, time::Duration};

    /// Theme and language commands change the active native appearance and save that preference.
    #[test]
    #[ignore = "requires Windows DirectWrite and DirectX"]
    fn native_ex_window_theme_and_language() -> Result<()> {
        struct RestoreLocale(i18n::Locale);

        impl Drop for RestoreLocale {
            fn drop(&mut self) {
                i18n::set_locale(self.0);
            }
        }

        let _locale = RestoreLocale(i18n::locale());
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().join("project");
        fs::create_dir(&root)?;
        let options = Options::take(&mut vec!["--no-plugins".into()], temporary.path())?;
        native_test::run_async(
            root,
            options,
            |_| {},
            async move |handle, app| {
                app.update(|app| {
                    handle.update(app, |workspace, window, cx| -> Result<()> {
                        let mut run = |workspace: &mut Workspace, command: &str| {
                            workspace.run_vim(
                                |vim, register| vim.execute_ex_line(command, register),
                                window,
                                cx,
                            );
                        };
                        run(workspace, "colorscheme light");
                        ensure!(workspace.appearance.theme == ThemeChoice::Light);
                        run(workspace, "colorscheme");
                        ensure!(workspace.projects.active().editor.vim().message == "light");
                        run(workspace, "colorscheme unknown");
                        ensure!(workspace.appearance.theme == ThemeChoice::Light);
                        ensure!(!workspace.projects.active().status.is_empty());
                        run(workspace, "language messages zh_CN.UTF-8");
                        ensure!(i18n::locale() == i18n::Locale::SimplifiedChinese);
                        run(workspace, "language messages");
                        ensure!(
                            workspace
                                .projects
                                .active()
                                .editor
                                .vim()
                                .message
                                .contains("zh-CN")
                        );
                        let path = workspace
                            .appearance_settings_path
                            .as_ref()
                            .context("settings path")?;
                        let settings = fs::read_to_string(path)?;
                        ensure!(settings.contains("theme = \"light\""));
                        ensure!(settings.contains("language = \"zh-CN\""));
                        Ok(())
                    })
                })?
            },
        )
    }

    /// Preview the existing buffer and retarget an options change to the source window.
    #[test]
    #[ignore = "requires Windows DirectWrite and DirectX"]
    fn native_ex_window_previews_and_applies_options_in_the_source() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().join("project");
        fs::create_dir(&root)?;
        let first = root.join("first.txt");
        let second = root.join("second.txt");
        fs::write(&first, "first\n")?;
        fs::write(&second, "second\n")?;
        let options = Options::take(&mut vec!["--no-plugins".into()], temporary.path())?;
        native_test::run_async(
            root,
            options,
            |_| {},
            async move |handle, app| {
                app.update_window(handle.into(), |_, window, _| {
                    window.resize(size(px(1200.), px(880.)));
                })?;
                app.background_executor()
                    .timer(Duration::from_millis(60))
                    .await;
                app.update(|app| {
                    handle.update(app, |workspace, window, cx| -> Result<()> {
                        workspace.open(first.clone(), window, cx);
                        let shared = workspace.projects.active().editor.vim().doc.buffer_number();
                        workspace.open(second.clone(), window, cx);
                        workspace.sync(window, cx);
                        let source = workspace.projects.active().active_editor_id();
                        let run = |workspace: &mut Workspace,
                                   command: &str,
                                   window: &mut Window,
                                   cx: &mut Context<Workspace>| {
                            workspace.run_vim(
                                |vim, register| vim.execute_ex_line(command, register),
                                window,
                                cx,
                            );
                            workspace.sync(window, cx);
                        };
                        run(workspace, &format!("pbuffer {shared}"), window, cx);
                        let project = workspace.projects.active();
                        ensure!(project.active_editor_id() == source);
                        let preview = project
                            .preview_pane()
                            .and_then(|pane| project.editor_by_id(pane))
                            .with_context(|| {
                                let area = workspace.visible_editor_region();
                                format!(
                                    "preview pane was not created: {} ({} by {})",
                                    project.status, area.w, area.h
                                )
                            })?;
                        ensure!(preview.vim().doc.buffer_number() == shared);
                        ensure!(preview.vim().doc.text == "first\n");
                        run(workspace, "pclose", window, cx);
                        run(workspace, "options", window, cx);
                        let sheet = workspace.projects.active().active_editor_id();
                        ensure!(sheet != source);
                        ensure!(
                            workspace
                                .projects
                                .active()
                                .editor
                                .vim()
                                .doc
                                .text
                                .contains("  set tabstop=")
                        );
                        workspace.run_vim(
                            |vim, _| {
                                let offset =
                                    vim.doc.text.find("  set tabstop=").expect("tabstop row");
                                let row = vim.doc.location(offset).0;
                                let range = vim.doc.line_range(row);
                                let start = range.start;
                                vim.doc.replace(range, "  set tabstop=8", true);
                                vim.set_cursor(start);
                                Action::None
                            },
                            window,
                            cx,
                        );
                        workspace.run_vim(|vim, register| vim.key("Enter", register), window, cx);
                        let project = workspace.projects.active();
                        ensure!(project.active_editor_id() == sheet);
                        ensure!(
                            project
                                .editor_by_id(source)
                                .expect("source editor")
                                .vim()
                                .doc
                                .tabstop()
                                == 8
                        );
                        ensure!(!project.editor.vim().doc.modified());
                        Ok(())
                    })
                })?
            },
        )
    }
}
