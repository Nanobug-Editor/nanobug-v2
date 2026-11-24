//! Apply editor actions to retained buffers, files and the native window.

use crate::grid::windows::editor::EditResult;
use crate::ui::theme::{AppearanceChange, GridFont};

use super::{
    Workspace,
    editor_close::CloseTarget,
    editor_splits::PaneId,
    ex_commands::ExContinuation,
    layout::{Focus, ShellWindow},
    projects::SplitDocument,
    sleep::Source,
};
use anyhow::Context as _;
use gpui::{ClipboardItem, Context, Window};
use nanobug::{
    i18n,
    vim::{Action, Register, Vim},
};
use std::path::{Path, PathBuf};

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
        if let Err(error) =
            project.open_editor_file(path, self.scroll_epoch.elapsed().as_secs_f64())
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

    /// Follow a validated file destination inside the Vim window that initiated the jump.
    pub(super) fn jump_to_file(
        &mut self,
        path: &Path,
        require_file: bool,
        resolve: impl FnOnce(&nanobug::document::Document) -> Option<usize>,
    ) -> anyhow::Result<bool> {
        let now = self.scroll_epoch.elapsed().as_secs_f64();
        let visited = self
            .projects
            .active_mut()
            .jump_file(path, require_file, now, resolve)?;
        if visited {
            self.finish_file_jump();
        }
        Ok(visited)
    }

    /// Reveal and focus the pane containing the new cursor position.
    pub(super) fn finish_file_jump(&mut self) {
        self.last_cursor_space = None;
        let project = self.projects.active_mut();
        project.focus_pane(Focus::Editor);
        if project.hidden_pane == Some(Focus::Editor) {
            project.hidden_pane = None;
        }
        project.status.clear();
    }

    /// Execute modal `action` in `window` through `cx`, preserving buffers on I/O errors.
    pub(super) fn action(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        let scope = self.begin_noswapfile_dispatch();
        self.apply_noswapfile_to_loaded();
        let removes_drafts = matches!(action, Action::Files(_) | Action::BufferLifecycle(_));
        self.dispatch_action(action, window, cx);
        self.apply_noswapfile_to_loaded();
        if scope {
            self.noswapfile_buffers = None;
        }
        if removes_drafts {
            self.prune_resolved_drafts();
        }
    }

    /// Route an editor action inside the active Ex modifier context.
    fn dispatch_action(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            Action::None => {}
            Action::Sign(command) => self.execute_sign(command, window, cx),
            Action::Recovery(command) => self.ex_recovery(command, window, cx),
            Action::Filetype(command) => self.run_filetype_command(command),
            Action::LanguageServer { command, target } => {
                self.control_ex_language_server(command, target, None, window, cx)
            }
            Action::CommandWindow(prompt) => self.open_command_window(prompt, window, cx),
            Action::VimMenu { path } => self.popup_vim_menu(path, window, cx),
            Action::Help(topic) => self.open_vim_help(&topic, window, cx),
            Action::HelpTags {
                directory,
                list_self,
            } => self.index_ex_help(&directory, list_self),
            Action::CheckHealth(components) => self.show_ex_health(&components, window, cx),
            Action::QuitWithCode(code) => {
                self.force_ex_close(CloseTarget::QuitWithCode(code), window, cx)
            }
            Action::Runtime {
                patterns,
                all_matches,
            } => self.source_ex_runtime(&patterns, all_matches, window, cx),
            Action::Restart { close, command } => self.restart_from_ex(&close, command, window, cx),
            Action::NumberedMarks { write } => self.update_numbered_marks(write),
            Action::VimInfo { write, force, path } => {
                self.transfer_vim_info(write, force, path.as_deref())
            }
            Action::TerminalOpen(command) => self.open_terminal(command, window, cx),
            Action::Sleep(seconds) => self.sleep_editor(seconds, window, cx),
            Action::AlternateBuffer(id) => self.switch_alternate_buffer(id, window, cx),
            Action::Open(name) => {
                let path = self
                    .file_operand(&name)
                    .unwrap_or_else(|| self.ex_current_dir().join(name.replace("\\ ", " ")));
                if path.is_dir() {
                    self.open_project(&path, window, cx);
                } else if let Err(error) = self.jump_to_file(&path, false, |doc| Some(doc.cursor)) {
                    self.set_status(format!("{error:#}"));
                }
            }
            Action::OpenFile(name, position) => {
                let path = self.vim_file_path(&name);
                let result = self.jump_to_file(&path, true, |doc| {
                    Some(position.map_or(doc.cursor, |(row, column)| {
                        let range = doc.line_range(row.min(doc.starts.len() - 1));
                        range.start + column.min(range.len())
                    }))
                });
                if let Err(error) = result {
                    self.set_status(format!("{error:#}"));
                }
            }
            Action::OpenFileMark {
                name,
                path,
                opening,
                row,
                column,
                linewise,
            } => {
                let now = self.scroll_epoch.elapsed().as_secs_f64();
                match self.projects.active_mut().jump_file_mark(
                    name,
                    (opening, path.as_deref()),
                    (row, column),
                    linewise,
                    now,
                ) {
                    Ok(()) => self.finish_file_jump(),
                    Err(error) => self.set_status(format!("{error:#}")),
                }
            }
            Action::JumpHistory { forward, count } => {
                let now = self.scroll_epoch.elapsed().as_secs_f64();
                if self
                    .projects
                    .active_mut()
                    .visit_editor_jump(forward, count, now)
                {
                    self.finish_file_jump();
                }
            }
            Action::ShowListing(list) => self.ex_output_listing(&list, window, cx),
            Action::ShowStyledListing(output) => self.ex_styled_output_listing(output, window, cx),
            Action::RedirectFile { path, mode } => self.ex_redirect_file(&path, mode),
            Action::RedirectClipboard { append } => {
                let existing = if append {
                    cx.read_from_clipboard()
                        .and_then(|item| item.text())
                        .unwrap_or_default()
                } else {
                    String::new()
                };
                self.input_mappings.begin_redirect_clipboard(existing);
            }
            Action::EditableListing(list) => self.open_editor_split_at(
                true,
                false,
                SplitDocument::Editable(nanobug::document::Document::new(&list)),
                None,
                window,
                cx,
            ),
            Action::IncludeSplit {
                path,
                row,
                column,
                preview,
            } => {
                if preview {
                    self.open_include_preview(path, row, column, window, cx);
                } else {
                    self.open_editor_split_at(
                        true,
                        true,
                        SplitDocument::Include(path, row, column),
                        None,
                        window,
                        cx,
                    );
                }
            }
            Action::Files(command) => self.file_ex_command(command, window, cx),
            Action::BufferLifecycle(command) => self.buffer_lifecycle(command, window, cx),
            Action::SessionExport(command) => self.session_export_ex(command, window, cx),
            Action::WindowModified { modifiers, command } => {
                self.execute_window_modified(modifiers, &command, window, cx)
            }
            Action::ExState { modifiers, action } => {
                let merged = self.active_ex_window_modifiers.overlay(modifiers);
                self.with_ex_window_modifiers(merged, |workspace| {
                    workspace.action(*action, window, cx)
                });
            }
            Action::MessageModified { modifier, command } => {
                self.execute_message_modified(modifier, &command, window, cx)
            }
            Action::UndoFile { path, write, force } => {
                if let Some(path) = self.file_operand(&path) {
                    let result = self
                        .projects
                        .active_mut()
                        .editor
                        .ex_undo_file(&path, write, force);
                    self.report_ex_result(result);
                } else {
                    self.set_status(i18n::text("ex.undo_file_argument").into());
                }
            }
            Action::WindowExtras(command) => self.window_extras(command, window, cx),
            Action::Shell(command) => self.run_shell_command(command, None, window, cx),
            Action::RegisterEx(text) => self.execute_ex_input(&text, "@", 1, false, window, cx),
            Action::ExPipeline(text) => self.execute_ex_input(&text, ":", 1, false, window, cx),
            Action::UserEx {
                text,
                source,
                source_line,
            } => self.execute_user_ex(&text, source.as_deref(), source_line, window, cx),
            Action::Autocmd {
                event,
                pattern,
                all,
                group,
            } => self.execute_explicit_autocmd(
                &event,
                pattern.as_deref(),
                group.as_deref(),
                all,
                window,
                cx,
            ),
            Action::NoAutocmd(command) => self.execute_no_autocmd(&command, window, cx),
            Action::ClipboardEx => {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    self.execute_ex_input(&text, "@+", 1, false, window, cx);
                } else {
                    self.set_status(i18n::text("ex.register_empty").into());
                }
            }
            Action::DeleteNumberedMarks(slots) => self.delete_numbered_marks(&slots),
            Action::FinishSource => self.set_status(i18n::text("ex.finish_outside_source").into()),
            Action::ListScripts => self.list_sourced_scripts(window, cx),
            Action::EditSourcedScript { address, path } => {
                self.open_sourced_script(address, path, window, cx)
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
            Action::TabEndpoint(last) => {
                let index = if last {
                    self.projects.active().page_count() - 1
                } else {
                    0
                };
                self.select_editor_page(index, window, cx);
            }
            Action::ListTabs => self.list_ex_tabs(window, cx),
            Action::TabMove(address) => self.move_ex_tabpage(&address, window, cx),
            Action::TabNew(file) => self.open_editor_page(file, window, cx),
            Action::TabClose => {
                if self.projects.active().page_count() > 1 {
                    self.finish_editor_input(window, cx);
                    self.request_close(CloseTarget::Page, window, cx);
                } else {
                    self.set_status(i18n::text("editor.last_tabpage").into());
                }
            }
            Action::TabOnly => self.request_close(CloseTarget::OtherPages, window, cx),
            Action::Only => {
                self.only_editor_split(window, cx);
            }
            Action::EmptyBuffer => self.open_ex_empty_buffer(),
            Action::EmptySplit(stacked) => {
                self.open_editor_split_at(stacked, false, SplitDocument::Empty, None, window, cx)
            }
            Action::Split { stacked, file } => self.open_editor_split(stacked, file, window, cx),
            Action::Arguments {
                command,
                count,
                arguments,
            } => {
                self.argument_list_command(command, count, arguments, window, cx);
            }
            Action::PreviewOpen(path) => self.open_vim_preview(path, window, cx),
            Action::PreviewClose => self.close_vim_preview(window, cx),
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
                severity,
            } => self.jump_diagnostic(forward, edge, count, severity, window, cx),
            Action::Save(name) => {
                let path = name.map(|name| self.ex_current_dir().join(name));
                self.save_with_plugins(path, false, window, cx);
            }
            Action::WriteFile { name, force, close } => {
                if self.write_ex_buffer(name.as_deref(), false, force, window, cx) && close {
                    self.close_ex_window(force, true, window, cx);
                }
            }
            Action::SaveAs { path, force } => {
                self.write_ex_buffer(Some(&path), true, force, window, cx);
            }
            Action::ReadFile { after, path } => self.read_ex_file(after, &path, window, cx),
            Action::WriteAll => {
                self.write_ex_buffers(false, window, cx);
            }
            Action::WriteQuitAll => {
                if self.write_ex_buffers(true, window, cx) {
                    self.request_close(CloseTarget::Quit, window, cx);
                }
            }
            Action::SaveClose => {
                if self.write_ex_buffer(None, false, false, window, cx) {
                    self.close_ex_window(false, true, window, cx);
                }
            }
            Action::SaveVimConfig => {
                self.save_vim_config();
            }
            Action::ListBuffers => self.list_ex_buffers(window, cx),
            Action::SelectBuffer(selector) => self.select_ex_buffer(&selector),
            Action::NavigateBuffer {
                direction,
                count,
                edge,
            } => self.navigate_ex_buffer(direction, count, edge),
            Action::DeleteBuffer { selector, force } => {
                self.delete_ex_buffer(&selector, force, window, cx)
            }
            Action::WindowCommand { keys, count } => {
                self.ex_window_command(&keys, count, window, cx)
            }
            Action::Source { file, lines } => self.source_ex(file.as_deref(), lines, window, cx),
            Action::RunBatch {
                scope,
                range,
                commands,
            } => self.run_ex_batch(scope, &range, &commands, window, cx),
            Action::UndoBatch => self.undo_ex_batch(window, cx),
            Action::Format(selection) => self.manual_format(selection, window, cx),
            Action::SqlDialect { buffer_only, name } => {
                self.set_sql_dialect(buffer_only, name, window, cx);
            }
            Action::CancelFormat => self.cancel_format(window, cx),
            Action::UndoPlugin => self.undo_plugin_edits(window, cx),
            Action::PluginComplete => {
                self.plugin_language(nanobug::plugins::package::Provider::Complete, window, cx)
            }
            Action::SyntaxSelection => self.request_syntax_selection(false, window, cx),
            Action::InsertCandidates {
                start,
                values,
                reverse,
            } => {
                self.show_insert_candidates(start, values, reverse, cx);
            }
            Action::PluginDiagnose => {
                self.plugin_language(nanobug::plugins::package::Provider::Diagnose, window, cx)
            }
            Action::Quickfix { location, command } => {
                self.quickfix_command(location, command, window, cx)
            }
            Action::FilterList { location, filter } => {
                self.apply_list_filter(location, filter, window, cx)
            }
            Action::Diagnostic(command) => self.diagnostic_command(command, window, cx),
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
            Action::Close(force) => self.close_ex_window(force, true, window, cx),
            Action::CloseWindow(force) => self.close_ex_window(force, false, window, cx),
            Action::QuitAll(force) => self.quit_ex(force, window, cx),
            Action::Filter { range, command } => {
                self.filter_lines(range, command, None, window, cx)
            }
        }
    }

    /// Resolve a name from under the caret relative to the buffer and then the Ex directory.
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
        self.ex_current_dir().join(name)
    }

    /// Switch editor tabpages, or top-level tabs from a standalone editor with only one page.
    /// `direction` and the optional one-based `count` follow the Vim tab commands.
    pub(super) fn navigate_vim_tab(
        &mut self,
        direction: i32,
        count: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(project) = self.projects.active_project()
            && (project.manager.is_some() || project.page_count() > 1)
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

    /// Open a page with an unnamed buffer or `file` relative to the Ex directory.
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
        let path = file.as_deref().map(|file| {
            self.file_operand(file)
                .unwrap_or_else(|| self.ex_current_dir().join(file.replace("\\ ", " ")))
        });
        let project = self.projects.active_mut();
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

    /// Run a line filter, applying the result to the same revision before resuming Ex input.
    pub(super) fn filter_lines(
        &mut self,
        range: std::ops::Range<usize>,
        command: String,
        continuation: Option<ExContinuation>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.input_mappings.ex_verbose() >= 4 {
            self.input_mappings
                .trace_ex(4, &format!("Calling shell to execute: {command}"));
        }
        let silence = self.input_mappings.ex_silence();
        let marks = nanobug::document::current_mark_modifiers();
        let root = self.ex_current_dir().to_owned();
        let project = self.projects.active_mut();
        let scope = project.plugin_scope;
        let pane = project.active_editor_id();
        let buffer = project.editor.buffer();
        let id = buffer.id;
        let opening = buffer.vim.doc.opening;
        let revision = buffer.vim.doc.revision;
        let text = buffer.vim.doc.text[range.clone()].to_owned();
        if silence.is_none() {
            project.status = i18n::text("editor.filter_running").into();
        }
        let worker = cx
            .background_executor()
            .spawn(async move { nanobug::vim::run_filter(&root, &command, &text) });
        cx.spawn_in(window, async move |view, cx| {
            let result = worker.await;
            let _ = cx.update(|window, cx| {
                view.update(cx, |workspace, cx| {
                    let mut changed_path = None;
                    let mut completed = false;
                    let mut failure = None;
                    for project in workspace.projects.iter_mut() {
                        let Some((pane, index)) = project.editors().find_map(|(pane, editor)| {
                            editor.buffer_index(id).map(|index| (pane, index))
                        }) else {
                            continue;
                        };
                        let editor = project.editor_by_id_mut(pane).expect("retained editor");
                        let status = match result {
                            Ok(output) if editor.buffers()[index].vim.doc.revision == revision => {
                                changed_path = editor
                                    .edit_buffer(index, |vim, _| {
                                        nanobug::document::with_mark_modifiers(marks, || {
                                            vim.apply_filter(range, &output);
                                        });
                                        nanobug::vim::Action::None
                                    })
                                    .changed_path;
                                completed = true;
                                i18n::text("editor.filter_complete").into()
                            }
                            Ok(_) => i18n::text("editor.filter_changed").into(),
                            Err(error) => i18n::format(
                                "editor.filter_failed",
                                &[("error", &format!("{error:#}"))],
                            ),
                        };
                        if silence.is_none() || !completed && silence != Some(true) {
                            project.status = status.clone();
                        }
                        if !completed {
                            workspace.input_mappings.remember_ex_error(&status);
                            if continuation.is_some() && silence != Some(true) {
                                failure = Some(status);
                            }
                        }
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
                    if let Some(resume) = continuation
                        && workspace.ex_shell_view_is_active(scope, pane, opening)
                    {
                        resume(workspace, window, cx, failure);
                    }
                    workspace.sync(window, cx);
                })
            });
        })
        .detach();
    }

    /// Execute Shell in the initiating Session and resume its Ex input without opening a split.
    pub(super) fn run_shell_command(
        &mut self,
        command: String,
        continuation: Option<ExContinuation>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.input_mappings.ex_verbose() >= 4 {
            self.input_mappings
                .trace_ex(4, &format!("Calling shell to execute: {command}"));
        }
        let silence = self.input_mappings.ex_silence();
        let root = self.ex_current_dir().to_owned();
        let project = self.projects.active_mut();
        let scope = project.plugin_scope;
        let pane = project.active_editor_id();
        let opening = project.editor.vim().doc.opening;
        if silence.is_none() {
            project.status = i18n::text("editor.filter_running").into();
        }
        let worker = cx
            .background_executor()
            .spawn(async move { nanobug::vim::run_ex_shell(&root, &command) });
        cx.spawn_in(window, async move |view, cx| {
            let result = worker.await;
            let _ = cx.update(|window, cx| {
                view.update(cx, |workspace, cx| {
                    let (output, completed) = match result {
                        Ok(output) => (output, true),
                        Err(error) => (format!("{error:#}"), false),
                    };
                    let failure = (!completed && continuation.is_some() && silence != Some(true))
                        .then(|| output.trim().to_owned());
                    if !completed {
                        workspace.input_mappings.remember_ex_error(output.trim());
                    }
                    let visible = silence.is_none() || !completed && silence != Some(true);
                    if visible
                        && continuation.is_none()
                        && workspace
                            .projects
                            .active_project()
                            .is_some_and(|project| project.plugin_scope == scope)
                        && output.lines().count() > 1
                        && !workspace.modal_open()
                    {
                        workspace.set_status(String::new());
                        workspace.action(Action::ShowListing(output), window, cx);
                    } else {
                        let output = output.trim_end_matches(['\r', '\n']);
                        if !output.is_empty() {
                            workspace.input_mappings.record_ex_output(output, true);
                        }
                        if visible
                            && let Some(project) = workspace
                                .projects
                                .iter_mut()
                                .find(|project| project.plugin_scope == scope)
                        {
                            project.status = output.trim().into();
                        }
                    }
                    if let Some(resume) = continuation
                        && workspace.ex_shell_view_is_active(scope, pane, opening)
                    {
                        resume(workspace, window, cx, failure);
                    }
                    workspace.sync(window, cx);
                })
            });
        })
        .detach();
    }

    /// Confirm that asynchronous Ex input still targets the initiating editor view.
    fn ex_shell_view_is_active(&self, scope: u64, pane: PaneId, opening: u64) -> bool {
        !self.modal_open()
            && matches!(self.active_vim_input(), Some((Source::Editor, editor)) if editor.vim().doc.opening == opening)
            && self.projects.active_project().is_some_and(|project| {
                project.plugin_scope == scope && project.active_editor_id() == pane
            })
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
        if let Some(text) = self.input_mappings.take_redirected_clipboard() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
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
        _ => Some(
            (selected + total - count.unwrap_or(direction.unsigned_abs() as usize) % total) % total,
        ),
    }
}
