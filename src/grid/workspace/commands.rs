//! Apply editor actions to retained buffers, files and the native window.

use crate::grid::windows::editor::EditResult;
use crate::ui::theme::{AppearanceChange, GridFont};

use super::{
    Workspace,
    editor_close::CloseTarget,
    layout::{Focus, ShellWindow},
};
use anyhow::Context as _;
use gpui::{Context, Window};
use nanobug::{
    i18n,
    vim::{Action, Register, Vim},
};
use std::path::PathBuf;

impl Workspace {
    /// Open `path` in the current project or in its own standalone tab.
    pub(super) fn open(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let path = path.canonicalize().unwrap_or(path);
        if !self
            .projects
            .active_project()
            .is_some_and(|project| project.split_count() > 1)
            && (!self.projects.manager_available() || self.projects.file_index(&path).is_some())
        {
            if let Err(error) = self.open_standalone_file(path, window, cx) {
                self.set_status(format!("{error:#}"));
            }
            return;
        }
        let project = self.projects.active_mut();
        if let Err(error) = project
            .editor
            .open(path, self.scroll_epoch.elapsed().as_secs_f64())
        {
            project.status = format!("{error:#}");
            return;
        }
        self.last_cursor_space = None;
        project.focus_pane(Focus::Editor);
        if project.hidden_pane == Some(Focus::Editor) {
            project.hidden_pane = None;
        }
        project.status.clear();
    }

    /// Execute modal `action` in `window` through `cx`, preserving buffers on I/O errors.
    pub(super) fn action(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            Action::None => {}
            Action::Open(name) => {
                let path = self.projects.active().root.join(name);
                if path.is_dir() {
                    self.open_project(&path, window, cx);
                } else {
                    self.open(path, window, cx);
                }
            }
            Action::OpenAt(name, line) => {
                let path = self.vim_file_path(&name);
                match path.canonicalize() {
                    Ok(path) if path.is_file() => {
                        self.open(path.clone(), window, cx);
                        if let Some(project) = self.projects.active_project_mut()
                            && project.editor.vim().doc.path.as_ref() == Some(&path)
                        {
                            project.editor.edit(|vim, _| {
                                vim.move_to_row(line.saturating_sub(1).min(vim.line_count() - 1));
                                Action::None
                            });
                        }
                    }
                    _ => self.set_status(i18n::format("vim_g.missing_file", &[("path", &name)])),
                }
            }
            Action::OpenExternal(target) => {
                let result = if url::Url::parse(&target)
                    .is_ok_and(|url| matches!(url.scheme(), "https" | "http" | "mailto"))
                {
                    open::that_detached(&target)
                } else {
                    let path = self.vim_file_path(&target);
                    if !path.is_file() && !path.is_dir() {
                        self.set_status(i18n::format("vim_g.missing_file", &[("path", &target)]));
                        return;
                    }
                    open::that_detached(path)
                };
                if let Err(error) = result {
                    self.set_status(error.to_string());
                }
            }
            Action::TabNavigate(direction, count) => {
                self.navigate_vim_tab(direction, count, window, cx)
            }
            Action::TabNew(file) => self.open_editor_page(file, window, cx),
            Action::TabClose => {
                if self.projects.active().page_count() > 1 {
                    self.finish_editor_input(window, cx);
                    self.request_close(CloseTarget::Page, window, cx);
                } else {
                    self.set_status(i18n::text("editor.last_tabpage").into());
                }
            }
            Action::Split { stacked, file } => self.open_editor_split(stacked, file, window, cx),
            Action::BracketBuffer {
                forward,
                edge,
                count,
            } => {
                let project = self.projects.active_mut();
                if project.editor.navigate_buffers(forward, edge, count) {
                    self.last_cursor_space = None;
                    project.status.clear();
                } else {
                    project.status = i18n::text("vim_bracket.no_buffer").into();
                }
            }
            Action::BracketDiagnostic {
                forward,
                edge,
                count,
            } => {
                let project = self.projects.active_mut();
                if let Some(offset) = project
                    .editor
                    .buffer()
                    .diagnostic_target(forward, edge, count)
                {
                    project.editor.jump_to(offset);
                    project.status.clear();
                } else {
                    project.status = i18n::text("editor.no_diagnostics").into();
                }
            }
            Action::Save(name) => {
                let path = name.map(|name| self.projects.active().root.join(name));
                self.save_with_plugins(path, false, window, cx);
            }
            Action::SaveClose => self.save_with_plugins(None, true, window, cx),
            Action::Format(selection) => self.manual_format(selection, window, cx),
            Action::SqlDialect { buffer_only, name } => {
                self.set_sql_dialect(buffer_only, name, window, cx);
            }
            Action::CancelFormat => self.cancel_format(window, cx),
            Action::UndoPlugin => self.undo_plugin_edits(window, cx),
            Action::PluginComplete => {
                self.plugin_language(nanobug::plugins::package::Provider::Complete, window, cx)
            }
            Action::PluginDiagnose => {
                self.plugin_language(nanobug::plugins::package::Provider::Diagnose, window, cx)
            }
            Action::PluginDefinition => {
                self.plugin_language(nanobug::plugins::package::Provider::Definition, window, cx)
            }
            Action::PluginHover => self.plugin_definition_hover(window, cx),
            Action::PluginOutline => self.plugin_outline(window, cx),
            Action::PluginFold => self.plugin_fold(window, cx),
            Action::PluginCommand(id) => self.plugin_command_by_id(&id, window, cx),
            Action::Appearance(option, value) => {
                if let Some(change) = AppearanceChange::from_command(&option, &value) {
                    self.change_appearance(change, window, cx);
                } else {
                    self.set_status(i18n::text("editor.appearance_usage").into());
                }
            }
            Action::Close(force) => {
                let project = self.projects.active();
                if project.split_count() > 1 && project.focus == Focus::Editor {
                    if force {
                        self.finish_close_editor_split(project.active_editor_id(), window, cx);
                    } else {
                        self.close_editor_split(None, window, cx);
                    }
                } else if force {
                    self.finish_close(CloseTarget::Quit, window, cx);
                } else {
                    self.request_close(CloseTarget::Quit, window, cx);
                }
            }
            Action::QuitAll(force) => {
                if force {
                    self.finish_close(CloseTarget::Quit, window, cx);
                } else {
                    self.request_close(CloseTarget::Quit, window, cx);
                }
            }
            Action::Filter { range, command } => self.filter_lines(range, command, window, cx),
        }
    }

    /// Resolve a file name from under the caret relative to the buffer and then the project.
    pub(super) fn vim_file_path(&self, name: &str) -> PathBuf {
        let project = self.projects.active();
        if let Some(parent) = project
            .editor
            .vim()
            .doc
            .path
            .as_deref()
            .and_then(std::path::Path::parent)
        {
            let path = parent.join(name);
            if path.exists() {
                return path;
            }
        }
        project.root.join(name)
    }

    /// Switch pages inside a directory project, or top-level tabs from a standalone editor.
    /// `direction` and the optional one-based `count` follow the Vim tab commands.
    pub(super) fn navigate_vim_tab(
        &mut self,
        direction: i32,
        count: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(project) = self.projects.active_project()
            && project.manager.is_some()
        {
            if let Some(index) = tab_destination(
                project.active_page(),
                project.page_count(),
                project.previous_page(),
                direction,
                count,
            ) {
                self.select_editor_page(index, window, cx);
            }
            return;
        }
        let total = self.projects.len();
        let selected = self.projects.selected;
        let index = tab_destination(
            selected,
            total,
            self.projects.previous_tab_index(),
            direction,
            count,
        );
        if let Some(index) = index {
            self.activate_project_tab(index, window, cx);
        }
    }

    /// Restore editor page `index` and repaint its saved layout in `window`.
    pub(super) fn select_editor_page(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal_open()
            || self.projects.active_project().is_none_or(|project| {
                index == project.active_page() || index >= project.page_count()
            })
        {
            return;
        }
        self.finish_editor_input(window, cx);
        let now = self.scroll_epoch.elapsed().as_secs_f64();
        if let Some(project) = self.projects.active_project_mut()
            && project.select_page(index, now)
        {
            project.status.clear();
            self.reset_tab_view(window, cx);
            self.sync(window, cx);
        }
    }

    /// Finish transient editor input before changing its page or displayed buffer.
    pub(super) fn finish_editor_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let edit = self.projects.active_mut().editor.finish_buffer_input();
        self.apply_edit(edit, window, cx);
    }

    /// Open a page showing the current buffer or a project-relative `file`.
    fn open_editor_page(
        &mut self,
        file: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.projects.manager_available() {
            self.set_status(i18n::text("editor.tabpage_requires_project").into());
            return;
        }
        let now = self.scroll_epoch.elapsed().as_secs_f64();
        let project = self.projects.active_mut();
        let path = file.map(|file| project.root.join(file));
        match project.open_page(path, now) {
            Ok(()) => {
                project.status.clear();
                self.reset_tab_view(window, cx);
            }
            Err(error) => self.set_status(format!("{error:#}")),
        }
    }

    /// Apply `change` in `window` through `cx`, then write the selected setting for future launches.
    fn change_appearance(
        &mut self,
        change: AppearanceChange,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(change, AppearanceChange::Font(GridFont::MapleMono))
            && let Err(error) = GridFont::ensure_maple(self.glyphs.as_ref())
        {
            self.set_status(i18n::format(
                "editor.appearance_font_error",
                &[("error", &error)],
            ));
            return;
        }
        let mut next = self.appearance;
        change.apply(&mut next);
        self.apply_appearance(next, window, cx);
        let save = self
            .appearance_settings_path
            .as_deref()
            .context("Editor settings path is unavailable")
            .and_then(|path| change.save(path));
        if let Err(error) = save {
            self.set_status(i18n::format(
                "editor.appearance_save_failed",
                &[("error", &format_args!("{error:#}"))],
            ));
        }
    }

    /// Run a line filter off the UI thread and apply it only to the same buffer revision.
    fn filter_lines(
        &mut self,
        range: std::ops::Range<usize>,
        command: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let project = self.projects.active_mut();
        let buffer = project.editor.buffer();
        let id = buffer.id;
        let revision = buffer.vim.doc.revision;
        let text = buffer.vim.doc.text[range.clone()].to_owned();
        let root = project.root.clone();
        project.status = i18n::text("editor.filter_running").into();
        let worker = cx
            .background_executor()
            .spawn(async move { nanobug::vim::run_filter(&root, &command, &text) });
        cx.spawn_in(window, async move |view, cx| {
            let result = worker.await;
            let _ = cx.update(|window, cx| {
                view.update(cx, |workspace, cx| {
                    let mut changed_path = None;
                    for project in workspace.projects.iter_mut() {
                        let Some((pane, index)) = project.editors().find_map(|(pane, editor)| {
                            editor.buffer_index(id).map(|index| (pane, index))
                        }) else {
                            continue;
                        };
                        let editor = project.editor_by_id_mut(pane).expect("retained editor");
                        project.status = match result {
                            Ok(output) if editor.buffers()[index].vim.doc.revision == revision => {
                                changed_path = editor
                                    .edit_buffer(index, |vim, _| {
                                        vim.apply_filter(range, &output);
                                        nanobug::vim::Action::None
                                    })
                                    .changed_path;
                                i18n::text("editor.filter_complete").into()
                            }
                            Ok(_) => i18n::text("editor.filter_changed").into(),
                            Err(error) => i18n::format(
                                "editor.filter_failed",
                                &[("error", &format!("{error:#}"))],
                            ),
                        };
                        break;
                    }
                    workspace.apply_edit(
                        EditResult {
                            action: Action::None,
                            changed_path,
                        },
                        window,
                        cx,
                    );
                    workspace.sync(window, cx);
                })
            });
        })
        .detach();
    }

    /// Apply editor `input`, then coordinate its effects through `window` and `cx`.
    pub(super) fn run_vim(
        &mut self,
        input: impl FnOnce(&mut Vim, &mut Register) -> Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let edit = self.projects.active_mut().editor.edit(input);
        self.apply_edit(edit, window, cx);
    }

    /// Reconcile `edit` with transfer history, then dispatch through `window` and `cx`.
    pub(super) fn apply_edit(
        &mut self,
        edit: EditResult,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(path) = edit.changed_path {
            for manager in self
                .projects
                .iter_mut()
                .filter_map(|project| project.manager.as_mut())
            {
                manager.invalidate_history(&path);
            }
        }
        self.action(edit.action, window, cx);
    }

    /// Close the surface that owns input in `window`, using `cx` for any confirmation and repaint.
    pub(super) fn close_focused_grid(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editor_close.is_some() {
            self.cancel_editor_close(window, cx);
            return;
        }
        if self.language_selection.is_some() {
            self.finish_language_picker(None, window, cx);
            return;
        }
        if self.help.is_open() {
            self.close_grid_window(ShellWindow::ShortcutHelp, window, cx);
        } else if let Some(session) = &self.file_picker {
            if session.state.delete.is_some() {
                self.confirm_picker_delete(false, window, cx);
                return;
            }
            self.close_grid_window(ShellWindow::FilePicker, window, cx);
        } else if let Some(page) = self.projects.start_mut() {
            if page.picker.take().is_some() {
                page.status.clear();
            }
        } else if !self.close_editor_split(None, window, cx) {
            self.close_grid_window(ShellWindow::Pane(self.projects.active().focus), window, cx);
        }
        self.sync(window, cx);
    }

    /// Close `target` in `window` through `cx`, resolving dirty buffers in its closing scope.
    pub(super) fn close_grid_window(
        &mut self,
        target: ShellWindow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.prefix = None;
        if let Some(manager) = self
            .projects
            .active_project_mut()
            .and_then(|project| project.manager.as_mut())
        {
            manager.cancel_pending_undo();
            manager.clear_count();
            manager.clear_click();
        }
        // Require a fresh hover before exposing the close button on an expanded pane.
        self.hovered_controls = None;
        match target {
            ShellWindow::ShortcutHelp => self.help.close(),
            ShellWindow::FilePicker => {
                if !self.picker_operation_pending() {
                    self.finish_file_picker(None, window, cx);
                }
            }
            ShellWindow::Pane(_)
                if !self.projects.manager_available()
                    || self.projects.active().hidden_pane.is_some() =>
            {
                self.request_close(CloseTarget::Tab, window, cx)
            }
            ShellWindow::Pane(Focus::Editor) => self.request_close(CloseTarget::Editor, window, cx),
            ShellWindow::Pane(Focus::Manager) => self.hide_pane(Focus::Manager),
        }
    }

    /// Hide `pane` after any close confirmation and focus the other pane.
    pub(super) fn hide_pane(&mut self, pane: Focus) {
        let project = self.projects.active_mut();
        if pane == Focus::Editor {
            let editor = &mut project.editor;
            editor.cancel_pending();
            editor.composition_mut().clear();
        }
        project.hidden_pane = Some(pane);
        project.focus_pane(pane.other());
        project.status = i18n::text(match pane {
            Focus::Manager => "workspace.manager_closed",
            Focus::Editor => "workspace.editor_closed",
        })
        .into();
    }
}

/// Resolve a one-based absolute tab count or a wrapping relative movement.
fn tab_destination(
    selected: usize,
    total: usize,
    previous: Option<usize>,
    direction: i32,
    count: Option<usize>,
) -> Option<usize> {
    match (direction, count) {
        (0, _) => previous,
        (1, Some(count)) if count > total => None,
        (1, Some(count)) => Some(count.saturating_sub(1)),
        (1, None) => Some((selected + 1) % total),
        _ => Some((selected + total - count.unwrap_or(1) % total) % total),
    }
}
