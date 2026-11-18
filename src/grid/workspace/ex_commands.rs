//! Coordinate native Ex commands that require the workspace's files and windows.

use super::{
    Workspace,
    editor_close::CloseTarget,
    editor_splits::SplitAxis,
    projects::{Project, SplitDocument},
};
use gpui::{Context, Window};
use nanobug::{
    document::Document,
    i18n,
    vim::{Action, ScriptEvent, WindowModifiers, without_ex_keep_jumps},
};
use std::{
    collections::HashSet,
    fmt::Write as _,
    ops::Range,
    path::{Path, PathBuf},
};

/// A parsed source or register body retained while a host action runs.
struct ExProgramFrame {
    source: String,
    program: nanobug::vim::ScriptProgram,
    source_file: bool,
    keymap: Option<String>,
    mode: Option<Action>,
    failure: Option<String>,
    /// Restore the draft-suppression scope after an asynchronous command resumes this frame.
    no_swapfile: bool,
    /// Resume the verbosity requested by an enclosing :verbose source command.
    verbose: Option<u32>,
}

/// Work resumed by an asynchronous Ex command in its originating window.
pub(super) type ExContinuation =
    Box<dyn FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>, Option<String>)>;
impl Workspace {
    /// Execute an expanded user command using the source context of its definition.
    pub(super) fn execute_user_ex(
        &mut self,
        text: &str,
        source: Option<&str>,
        source_line: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.projects.active_project() else {
            return;
        };
        let lines = project.editor.vim().script_lines(text, source_line);
        let display = source.unwrap_or(":command");
        match nanobug::vim::ScriptProgram::user_command(lines, source.map(str::to_owned)) {
            Ok(program) => self.execute_ex_program(
                ExProgramFrame {
                    source: display.into(),
                    program,
                    source_file: false,
                    keymap: None,
                    mode: None,
                    failure: None,
                    no_swapfile: self.active_ex_window_modifiers.no_swapfile,
                    verbose: self.active_ex_window_modifiers.verbose,
                },
                window,
                cx,
            ),
            Err((line, error)) => self.set_status(format!("{display}:{line}: {error}")),
        }
    }

    /// Write the active buffer to `name`. `rename` selects :saveas semantics.
    pub(super) fn write_ex_buffer(
        &mut self,
        name: Option<&str>,
        rename: bool,
        force: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let path = if let Some(name) = name {
            let Some(path) = self.file_operand(name) else {
                self.set_status(i18n::format(
                    "editor.invalid_ex_argument",
                    &[("argument", &name)],
                ));
                return false;
            };
            Some(path)
        } else {
            None
        };
        let destination = path
            .as_ref()
            .map(|path| path.canonicalize().unwrap_or_else(|_| path.clone()));
        if !self.script_write_event(ScriptEvent::BufWritePre, destination.as_deref(), window, cx) {
            if self
                .projects
                .active_project()
                .is_some_and(|project| project.status.is_empty())
            {
                self.set_status(i18n::text("ex.write_buffer_changed").into());
            }
            return false;
        }
        let tab = self.projects.selected;
        let project = self.projects.active();
        let pane = project.active_editor_id();
        let editor = &project.editor;
        let index = editor.active_index();
        let doc = &editor.vim().doc;
        let different = destination
            .as_ref()
            .is_some_and(|target| doc.path.as_ref() != Some(target));
        let overwrite = force || self.input_mappings.options().writeany;
        let result = if let Some(path) = path.filter(|_| different) {
            if !overwrite && std::fs::symlink_metadata(&path).is_ok() {
                Err(anyhow::anyhow!(i18n::text("ex.file_exists")))
            } else if !rename && doc.path.is_some() {
                let result = doc.write_copy(&path, overwrite);
                if result.is_ok() {
                    let path = path.canonicalize().unwrap_or(path);
                    for manager in self
                        .projects
                        .iter_mut()
                        .filter_map(|project| project.manager.as_mut())
                    {
                        manager.invalidate_history(&path);
                    }
                }
                result
            } else if overwrite {
                self.projects
                    .save_editor_buffer_forced(tab, pane, index, Some(path))
            } else {
                self.projects.save_new_editor_buffer(tab, pane, index, path)
            }
        } else if force {
            self.projects
                .save_editor_buffer_forced(tab, pane, index, destination.clone())
        } else {
            self.projects
                .save_editor_buffer(tab, pane, index, destination.clone())
        };
        if !self.report_ex_result(result) {
            return false;
        }
        self.prune_resolved_drafts();
        self.script_write_event(
            ScriptEvent::BufWritePost,
            destination.as_deref(),
            window,
            cx,
        )
    }

    /// Write every dirty buffer in the active Session or every open tab when `all` is true.
    pub(super) fn write_ex_buffers(
        &mut self,
        all: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        for project in self.projects.iter_mut() {
            project.refresh_shared_documents();
        }
        let mut seen = HashSet::new();
        let mut pending = Vec::new();
        for (_, project) in self
            .projects
            .indexed()
            .filter(|(tab, _)| all || *tab == self.projects.selected)
        {
            for (_, editor) in project.editors() {
                for buffer in editor.buffers() {
                    let doc = &buffer.vim.doc;
                    if doc.modified() && seen.insert(doc.buffer_number()) {
                        pending.push((project.plugin_scope, doc.buffer_number(), doc.path.clone()));
                    }
                }
            }
            for doc in project.detached_documents().filter(|doc| doc.modified()) {
                if seen.insert(doc.buffer_number()) {
                    pending.push((project.plugin_scope, doc.buffer_number(), doc.path.clone()));
                }
            }
        }
        let mut failures = Vec::new();
        for (scope, number, path) in pending {
            let Some((tab, project)) = self
                .projects
                .indexed()
                .find(|(_, project)| project.plugin_scope == scope)
            else {
                failures.push(i18n::text("ex.write_buffer_changed").to_owned());
                continue;
            };
            let target = project.editors().find_map(|(pane, editor)| {
                editor
                    .buffers()
                    .iter()
                    .position(|buffer| buffer.vim.doc.buffer_number() == number)
                    .map(|index| (pane, index))
            });
            let visible = tab == self.projects.selected
                && self
                    .projects
                    .active_project()
                    .is_some_and(|project| project.editor.vim().doc.buffer_number() == number);
            if visible {
                if !self.write_ex_buffer(None, false, false, window, cx) {
                    let failure = self
                        .projects
                        .active_project()
                        .map_or_else(String::new, |project| project.status.clone());
                    failures.push(if failure.is_empty() {
                        i18n::text("ex.write_buffer_changed").into()
                    } else {
                        failure
                    });
                }
                continue;
            }
            let result = if let Some((pane, index)) = target {
                self.projects.save_editor_buffer(tab, pane, index, None)
            } else {
                self.projects.save_detached_buffer(tab, number, None)
            };
            if let Err(error) = result {
                let name = path
                    .as_deref()
                    .and_then(Path::to_str)
                    .unwrap_or("[No Name]");
                failures.push(format!("{name}: {error:#}"));
            }
        }
        let ok = failures.is_empty();
        self.prune_resolved_drafts();
        self.set_status(failures.join(" | "));
        ok
    }

    /// Insert the contents of `path` below one-based line `after`, or prepend on zero.
    pub(super) fn read_ex_file(
        &mut self,
        after: usize,
        path: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = self.file_operand(path) else {
            self.set_status(i18n::format(
                "editor.invalid_ex_argument",
                &[("argument", &path)],
            ));
            return;
        };
        let source = match Document::open(&path) {
            Ok(source) if path.is_file() => source.text.clone(),
            Ok(_) => {
                self.set_status(i18n::format(
                    "vim_g.missing_file",
                    &[("path", &path.display())],
                ));
                return;
            }
            Err(error) => {
                self.set_status(format!("{error:#}"));
                return;
            }
        };
        if source.is_empty() {
            return;
        }
        self.run_vim(
            |vim, _| {
                let doc = &mut vim.doc;
                let offset = doc.starts.get(after).copied().unwrap_or(doc.text.len());
                let mut insertion = String::new();
                if offset > 0 && doc.text.as_bytes()[offset - 1] != b'\n' {
                    insertion.push('\n');
                }
                let first = offset + insertion.len();
                insertion.push_str(&source);
                if offset < doc.text.len() && !insertion.ends_with('\n') {
                    insertion.push('\n');
                }
                doc.replace(offset..offset, &insertion, true);
                doc.cursor = first;
                Action::None
            },
            window,
            cx,
        );
    }

    /// Close the selected editor window, escalating through pages and Session for `:quit`.
    pub(super) fn close_ex_window(
        &mut self,
        force: bool,
        quit: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let project = self.projects.active();
        let target = if project.split_count() > 1 {
            CloseTarget::Split(project.active_editor_id())
        } else if !quit {
            self.set_status(i18n::text("ex.last_window").into());
            return;
        } else if project.page_count() > 1 {
            CloseTarget::Page
        } else {
            CloseTarget::Tab
        };
        if force {
            self.force_ex_close(target, window, cx);
        } else {
            self.request_close(target, window, cx);
        }
    }

    /// Apply :qall and :qall!, guarding modified buffers before requesting app shutdown.
    pub(super) fn quit_ex(&mut self, force: bool, window: &mut Window, cx: &mut Context<Self>) {
        for project in self.projects.iter_mut() {
            project.refresh_shared_documents();
        }
        if force {
            self.force_ex_close(CloseTarget::Quit, window, cx);
        } else if self.projects.ordered().any(|project| {
            project.detached_documents().any(Document::modified)
                || project.editors().any(|(_, editor)| editor.modified())
        }) {
            self.set_status(i18n::text("ex.unsaved_buffers").into());
        } else {
            self.request_close(CloseTarget::Quit, window, cx);
        }
    }

    /// Display a numbered buffer list using the standard read-only Ex listing.
    pub(super) fn list_ex_buffers(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let project = self.projects.active();
        let active = project.editor.vim().doc.buffer_number();
        let mut listing = String::new();
        for (id, buffer) in project.session_buffer_entries() {
            let doc = &buffer.vim.doc;
            let current = if id == active { '%' } else { ' ' };
            if let Some(terminal) = buffer.terminal() {
                let label = if terminal.exited() {
                    i18n::text("terminal.exited")
                } else {
                    i18n::text("terminal.title")
                };
                let _ = writeln!(listing, "{id:>3} {current}  [{label}]");
                continue;
            }
            let modified = if doc.modified() { '+' } else { ' ' };
            let name = doc
                .path
                .as_deref()
                .and_then(Path::to_str)
                .unwrap_or("[No Name]");
            let (line, _) = doc.location(doc.cursor);
            let _ = writeln!(
                listing,
                "{id:>3} {current}{modified} \"{name}\" line {}",
                line + 1
            );
        }
        self.action(Action::ShowListing(listing), window, cx);
    }

    /// Select `selector` in the current window using the current Session's shared buffers.
    pub(super) fn select_ex_buffer(&mut self, selector: &str) {
        let number = resolve_buffer(self.projects.active(), selector);
        let now = self.scroll_epoch.elapsed().as_secs_f64();
        let project = self.projects.active_mut();
        project.status.clear();
        if !number.is_some_and(|id| project.select_session_buffer(id, now))
            && project.status.is_empty()
        {
            project.status = i18n::text("vim_bracket.no_buffer").into();
        }
    }

    /// Step `count` entries in `direction`, or select the corresponding endpoint when `edge`.
    pub(super) fn navigate_ex_buffer(&mut self, direction: i32, count: usize, edge: bool) {
        let project = self.projects.active();
        let numbers = project.session_buffer_numbers();
        let current = project.editor.vim().doc.buffer_number();
        let Some(index) = numbers.iter().position(|&id| id == current) else {
            return;
        };
        let len = numbers.len();
        let index = if edge {
            if direction < 0 { 0 } else { len - 1 }
        } else if direction < 0 {
            (index + len - count % len) % len
        } else {
            (index + count % len) % len
        };
        let now = self.scroll_epoch.elapsed().as_secs_f64();
        self.projects
            .active_mut()
            .select_session_buffer(numbers[index], now);
    }

    /// Remove the buffer identified by `selector`, keeping window geometry and disk files.
    pub(super) fn delete_ex_buffer(
        &mut self,
        selector: &str,
        force: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let project = self.projects.active();
        let Some(number) = resolve_buffer(project, selector) else {
            self.set_status(i18n::text("vim_bracket.no_buffer").into());
            return;
        };
        if !force
            && project
                .terminal(number)
                .is_some_and(|terminal| !terminal.exited())
        {
            self.set_status(i18n::format(
                "terminal.delete_running",
                &[("buffer", &number.to_string())],
            ));
            return;
        }
        let target = CloseTarget::SessionBuffer(number);
        if force {
            self.finish_close(target, window, cx);
        } else {
            self.request_close(target, window, cx);
        }
    }

    /// Retain an untitled document in the current editor's local file labels.
    pub(super) fn open_ex_empty_buffer(&mut self) {
        let now = self.scroll_epoch.elapsed().as_secs_f64();
        let editor = &mut self.projects.active_mut().editor;
        let index = editor.retain_document(Document::new(""));
        editor.select_buffer(editor.buffers()[index].id, now);
    }

    /// List the current Session's Tabpages, showing the current page marker.
    pub(super) fn list_ex_tabs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let project = self.projects.active();
        let mut listing = String::new();
        for page in 0..project.page_count() {
            let current = if page == project.active_page() {
                '>'
            } else {
                ' '
            };
            let _ = writeln!(listing, "{current} Tab page {}", page + 1);
            for editor in project.tabpage_editors(page) {
                let doc = &editor.vim().doc;
                let modified = if doc.modified() { '+' } else { ' ' };
                let path = doc
                    .path
                    .as_deref()
                    .and_then(Path::to_str)
                    .unwrap_or("[No Name]");
                let _ = writeln!(listing, "   {modified} {path}");
            }
        }
        self.action(Action::ShowListing(listing), window, cx);
    }

    /// Move a Tabpage to the Ex absolute or relative `address` in its Session.
    pub(super) fn move_ex_tabpage(
        &mut self,
        address: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let project = self.projects.active();
        let old = project.active_page();
        let total = project.page_count();
        let destination = match address {
            "" | "$" => Some(total - 1),
            "." => Some(old),
            "+" => old.checked_add(1),
            "-" => old.checked_sub(1),
            relative if relative.starts_with(['+', '-']) => relative
                .parse::<isize>()
                .ok()
                .and_then(|step| old.checked_add_signed(step)),
            absolute => absolute
                .parse::<usize>()
                .ok()
                .filter(|&after| after <= total)
                .map(|after| after - usize::from(after > old)),
        };
        let Some(destination) = destination.filter(|&index| index < total) else {
            self.set_status(i18n::text("ex.batch_range").into());
            return;
        };
        self.projects.active_mut().move_active_page(destination);
        self.reset_tab_view(window, cx);
    }

    /// Evaluate the selected `lines` of the current buffer or Ex statements from `file`.
    pub(super) fn source_ex(
        &mut self,
        file: Option<&str>,
        lines: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (text, source, first) = if let Some(file) = file {
            let Some(path) = self.file_operand(file) else {
                self.set_status(i18n::format(
                    "editor.invalid_ex_argument",
                    &[("argument", &file)],
                ));
                return;
            };
            match std::fs::read(&path).and_then(|bytes| nanobug::vim::decode_ex_source(&bytes)) {
                Ok(text) => {
                    let path = path.canonicalize().unwrap_or(path);
                    if !self.ex_sourced_files.contains(&path) {
                        self.ex_sourced_files.push(path.clone());
                    }
                    (text, path.display().to_string(), 1)
                }
                Err(error) => {
                    self.set_status(format!("{}: {error}", path.display()));
                    return;
                }
            }
        } else {
            let doc = &self.projects.active().editor.vim().doc;
            let lines = lines.unwrap_or(0..doc.starts.len());
            let first = lines.start + 1;
            let text = lines
                .map(|row| doc.line(row))
                .collect::<Vec<_>>()
                .join("\n");
            let name = doc
                .path
                .as_deref()
                .and_then(Path::to_str)
                .unwrap_or("[No Name]");
            (text, name.to_owned(), first)
        };
        if file.is_some() && self.restore_exported_session(&text, &source, window, cx) {
            return;
        }
        if self.input_mappings.ex_verbose() >= 2 {
            self.input_mappings
                .trace_ex(2, &format!("sourcing \"{source}\""));
        }
        self.execute_ex_input(&text, &source, first, true, window, cx);
    }

    /// Execute Ex `text` loaded from `source`, starting at one-based line `first`.
    pub(super) fn execute_ex_input(
        &mut self,
        text: &str,
        source: &str,
        first: usize,
        source_file: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.projects.active_project() else {
            return;
        };
        let keymap_offset = source_file
            .then(|| {
                let mut offset = 0;
                text.split_inclusive('\n').find_map(|line| {
                    offset += line.len();
                    is_loadkeymap(line).then_some(offset)
                })
            })
            .flatten();
        let lines = project
            .editor
            .vim()
            .script_lines(&text[..keymap_offset.unwrap_or(text.len())], first);
        let scope = source_file.then(|| source.to_owned());
        match nanobug::vim::ScriptProgram::compile(lines, scope) {
            Ok(program) => self.execute_ex_program(
                ExProgramFrame {
                    source: source.into(),
                    program,
                    source_file,
                    keymap: keymap_offset.map(|_| text.into()),
                    mode: None,
                    failure: None,
                    no_swapfile: self.active_ex_window_modifiers.no_swapfile,
                    verbose: self.active_ex_window_modifiers.verbose,
                },
                window,
                cx,
            ),
            Err((line, error)) => self.set_status(format!("{source}:{line}: {error}")),
        }
    }

    /// Run a source frame and retain it unchanged while a shell command or timer is pending.
    fn execute_ex_program(
        &mut self,
        frame: ExProgramFrame,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut modifiers = self.active_ex_window_modifiers.clone();
        modifiers.no_swapfile |= frame.no_swapfile;
        modifiers.marks.keep_jumps = false;
        modifiers.verbose = frame.verbose.or(modifiers.verbose);
        without_ex_keep_jumps(|| {
            self.with_ex_window_modifiers(modifiers, |workspace| {
                let scope = workspace.begin_noswapfile_dispatch();
                workspace.execute_ex_frame(frame, window, cx);
                workspace.apply_noswapfile_to_loaded();
                if scope {
                    workspace.noswapfile_buffers = None;
                }
            });
        });
    }

    /// Dispatch the remaining statements of `frame` within its current Ex context.
    fn execute_ex_frame(
        &mut self,
        mut frame: ExProgramFrame,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.main_window_removing {
            return;
        }
        if self.ex_source_depth >= 16 {
            self.set_status(i18n::text("ex.source_depth").into());
            return;
        }
        self.ex_source_depth += 1;
        let target = self
            .projects
            .active_project()
            .map(|project| (project.plugin_scope, project.active_editor_id()));
        while !self.main_window_removing
            && !self.modal_open()
            && self
                .projects
                .active_project()
                .is_some_and(|project| !project.editor.vim().substitute_confirmation_active())
        {
            self.activate_input_mappings();
            let mappings = self.input_mappings.clone();
            let editor = &mut self.projects.active_mut().editor;
            let mut yielded = Ok(None);
            let mut keymap = false;
            let mut modifiers = WindowModifiers::default();
            let mut messages = Vec::new();
            let edit = editor.edit(|vim, register| {
                match frame.program.next(vim, register, frame.failure.take()) {
                    Err(error) => yielded = Err(error),
                    Ok(None) if vim.collecting_ex_lines() => {
                        vim.accept_ex_line(".");
                    }
                    Ok(None) => {}
                    Ok(Some(line)) => {
                        yielded = Ok(Some(line.number));
                        if frame.keymap.is_some()
                            && (line.text.starts_with("let b:keymap")
                                || line.text.starts_with("scriptencoding "))
                        {
                            return Action::None;
                        }
                        if is_loadkeymap(&line.text)
                            && let Some(text) = frame.keymap.take()
                        {
                            keymap = true;
                            vim.load_ex_keymap(&text);
                        } else {
                            if let Some((state, output, command)) =
                                vim.source_ex_modifiers(&line.text, register)
                            {
                                let statement = nanobug::vim::ScriptLine {
                                    number: line.number,
                                    text: command.to_owned(),
                                    literal: line.literal,
                                };
                                modifiers = state;
                                messages = output;
                                return modifiers.with_ex_state(&mappings, || {
                                    vim.execute_script_statement(
                                        &frame.program,
                                        &statement,
                                        register,
                                    )
                                });
                            }
                            return vim.execute_script_statement(&frame.program, &line, register);
                        }
                    }
                }
                Action::None
            });
            let line = match yielded {
                Err((line, error)) => {
                    self.set_status(format!("{}:{line}: {error}", frame.source));
                    break;
                }
                Ok(None) => {
                    self.apply_edit(edit, window, cx);
                    break;
                }
                Ok(Some(line)) => line,
            };
            let _origin = mappings.enter_ex_origin(frame.program.ex_origin(line));
            let editor_error = editor
                .vim()
                .ex_failed()
                .then(|| editor.vim().message.clone());
            self.set_status(String::new());
            if keymap {
                if let Some(error) = editor_error {
                    self.set_status(format!("{}:{line}: {error}", frame.source));
                }
                break;
            }
            if let Action::WindowExtras(nanobug::vim::WindowCommand::Sleep {
                duration,
                hide_caret,
            }) = &edit.action
            {
                self.sleep_with_ex_continuation(
                    *duration,
                    *hide_caret,
                    move |workspace, window, cx| {
                        workspace.execute_ex_program(frame, window, cx);
                    },
                    window,
                    cx,
                );
                self.ex_source_depth -= 1;
                return;
            }
            if let Action::LanguageServer { command, target } = edit.action {
                let resume: ExContinuation = Box::new(move |workspace, window, cx, failure| {
                    frame.failure = failure;
                    workspace.execute_ex_program(frame, window, cx);
                });
                self.ex_source_depth -= 1;
                self.control_ex_language_server(command, target, Some(resume), window, cx);
                return;
            }
            if matches!(edit.action, Action::Shell(_) | Action::Filter { .. }) {
                let resume: ExContinuation = Box::new(move |workspace, window, cx, failure| {
                    frame.failure = failure;
                    workspace.execute_ex_program(frame, window, cx);
                });
                let modifiers = self.active_ex_window_modifiers.overlay(modifiers);
                self.with_ex_window_modifiers(modifiers, |workspace| {
                    for message in &messages {
                        mappings.begin_ex_message_modifier(message.clone());
                    }
                    match edit.action {
                        Action::Shell(command) => {
                            workspace.run_shell_command(command, Some(resume), window, cx)
                        }
                        Action::Filter { range, command } => {
                            workspace.filter_lines(range, command, Some(resume), window, cx)
                        }
                        _ => unreachable!("matched shell and filter"),
                    }
                    for _ in messages {
                        mappings.end_ex_message_modifier();
                    }
                });
                self.ex_source_depth -= 1;
                return;
            }
            if frame.source_file && matches!(edit.action, Action::FinishSource) {
                break;
            }
            if frame.source_file
                && matches!(
                    edit.action,
                    Action::WindowExtras(nanobug::vim::WindowCommand::ChangeMode { .. })
                )
            {
                frame.mode = Some(edit.action);
                continue;
            }
            let exits_app = matches!(
                edit.action,
                Action::QuitAll(_)
                    | Action::QuitWithCode(_)
                    | Action::Restart { .. }
                    | Action::WriteQuitAll
            );
            let nested = matches!(
                edit.action,
                Action::Source { .. }
                    | Action::RegisterEx(_)
                    | Action::ExPipeline(_)
                    | Action::ClipboardEx
                    | Action::UserEx { .. }
                    | Action::Runtime { .. }
                    | Action::MessageModified { .. }
            );
            let message_only = matches!(&edit.action, Action::Files(command)
                if command.operation == nanobug::vim::FileOperation::WorkingDirectory);
            let failure = if editor_error.is_some() {
                editor_error
            } else if matches!(edit.action, Action::SaveVimConfig) && self.save_vim_config() {
                None
            } else {
                if !matches!(edit.action, Action::SaveVimConfig) {
                    let modifiers = self.active_ex_window_modifiers.overlay(modifiers);
                    self.with_ex_window_modifiers(modifiers, |workspace| {
                        workspace.apply_edit(edit, window, cx);
                    });
                }
                self.projects.active_project().and_then(|project| {
                    if nested && project.editor.vim().ex_failed() {
                        Some(project.editor.vim().message.clone())
                    } else if !message_only && !project.status.is_empty() {
                        Some(project.status.clone())
                    } else {
                        None
                    }
                })
            };
            frame.failure = failure;
            if nested && self.ex_sleep_is_pending() {
                self.then_ex_sleep(move |workspace, window, cx| {
                    workspace.execute_ex_program(frame, window, cx);
                });
                self.ex_source_depth -= 1;
                return;
            }
            if exits_app {
                break;
            }
        }
        self.ex_source_depth -= 1;
        if !self.main_window_removing
            && let Some(mode) = frame.mode
            && self
                .projects
                .active_project()
                .map(|project| (project.plugin_scope, project.active_editor_id()))
                == target
            && !self.modal_open()
        {
            self.action(mode, window, cx);
        }
    }
    /// Display sourced filenames in the order in which they were initially loaded.
    pub(super) fn list_sourced_scripts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut listing = String::new();
        for (index, path) in self.ex_sourced_files.iter().enumerate() {
            let _ = writeln!(listing, "{:>3}: {}", index + 1, path.display());
        }
        if !listing.is_empty() {
            self.action(Action::ShowListing(listing), window, cx);
        }
    }

    /// Open the requested script filename or its one-based registered address.
    pub(super) fn open_sourced_script(
        &mut self,
        address: Option<usize>,
        path: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path = if let Some(address) = address {
            address
                .checked_sub(1)
                .and_then(|index| self.ex_sourced_files.get(index))
                .cloned()
        } else {
            path.and_then(|name| self.file_operand(&name))
        };
        if let Some(path) = path {
            self.action(
                Action::Open(path.to_string_lossy().into_owned()),
                window,
                cx,
            );
        } else {
            self.set_status(i18n::text("ex.invalid_script").into());
        }
    }

    /// Display the include match without changing the initiating window's focus.
    pub(super) fn open_include_preview(
        &mut self,
        path: Option<PathBuf>,
        row: usize,
        column: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let now = self.scroll_epoch.elapsed().as_secs_f64();
        let region = self.visible_editor_region();
        let project = self.projects.active_mut();
        let original = project.active_editor_id();
        if let Some(pane) = project.preview_pane() {
            let source_number = project.editor.vim().doc.buffer_number();
            project.focus_editor_by_id(pane, now);
            let result = if let Some(path) = path {
                project.open_editor_file(path, now)
            } else {
                project.select_session_buffer(source_number, now);
                Ok(())
            };
            if result.is_ok() {
                let doc = &project.editor.vim().doc;
                let line = doc.line_range(row.min(doc.starts.len() - 1));
                project.editor.jump_to(line.start + column.min(line.len()));
            }
            project.focus_editor_by_id(original, now);
            self.report_ex_result(result);
        } else {
            match project.open_split(
                SplitAxis::Rows,
                false,
                SplitDocument::Include(path, row, column),
                region,
                now,
            ) {
                Ok(true) => {
                    project.mark_preview_pane(project.active_editor_id());
                    project.focus_editor_by_id(original, now);
                    self.reset_tab_view(window, cx);
                }
                Ok(false) => self.set_status(i18n::text("editor.split_no_space").into()),
                Err(error) => self.set_status(format!("{error:#}")),
            }
        }
        self.sync(window, cx);
    }

    /// Display a host error from an Ex operation and report whether it completed.
    pub(super) fn report_ex_result(&mut self, result: anyhow::Result<()>) -> bool {
        match result {
            Ok(()) => {
                self.set_status(String::new());
                true
            }
            Err(error) => {
                self.set_status(format!("{error:#}"));
                false
            }
        }
    }
}

/// Recognize the native keymap data directive, including its valid abbreviations.
fn is_loadkeymap(line: &str) -> bool {
    let line = line.trim().trim_start_matches(':').trim_start();
    line.len() >= 5 && "loadkeymap".starts_with(line)
}

/// Resolve a buffer by number, exact path, or an unambiguous path fragment.
pub(super) fn resolve_buffer(project: &Project, selector: &str) -> Option<u64> {
    if selector.is_empty() {
        return Some(project.editor.vim().doc.buffer_number());
    }
    if let Ok(id) = selector.parse::<u64>() {
        return project.session_buffer(id).is_some().then_some(id);
    }
    let selector = selector.replace("\\ ", " ");
    let target = project.root.join(&selector);
    let target = target.canonicalize().unwrap_or(target);
    let mut partial = Vec::new();
    for (_, buffer) in project.session_buffer_entries() {
        let doc = &buffer.vim.doc;
        let Some(path) = doc.path.as_deref() else {
            continue;
        };
        if path == target {
            return Some(doc.buffer_number());
        }
        if path.to_string_lossy().contains(&selector) {
            partial.push(doc.buffer_number());
        }
    }
    (partial.len() == 1).then(|| partial[0])
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::grid::workspace::native_test;
    use anyhow::{Result, ensure};
    use nanobug::plugins::session::Options;
    use std::fs;

    /// Exercise the same host action dispatch as submission of a typed Ex command.
    fn command(
        workspace: &mut Workspace,
        line: &str,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        workspace.run_vim(
            |vim, register| vim.execute_ex_line(line, register),
            window,
            cx,
        );
        workspace.sync(window, cx);
    }

    /// Write a copy, then explicitly name the destination without changing the original file.
    #[test]
    #[ignore = "requires Windows DirectWrite and DirectX"]
    fn native_ex_preserves_buffer_identity_when_writing_a_copy() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().join("project");
        fs::create_dir(&root)?;
        let original = root.join("original.txt");
        let copy = root.join("copy.txt");
        let renamed = root.join("new.txt");
        fs::write(&original, "old\n")?;
        fs::write(root.join("insert.txt"), "new\n")?;
        fs::write(
            root.join("write-copy.vim"),
            "0read insert.txt | write copy.txt\n",
        )?;
        let options = Options::take(&mut vec!["--no-plugins".into()], temporary.path())?;
        native_test::run_async(
            root,
            options,
            |_| {},
            async move |handle, app| {
                app.update(|app| {
                    handle.update(app, |workspace, window, cx| -> Result<()> {
                        workspace.open(original.clone(), window, cx);
                        command(workspace, "source write-copy.vim", window, cx);
                        ensure!(workspace.projects.active().editor.vim().doc.text == "new\nold\n");
                        ensure!(fs::read_to_string(&copy)? == "new\nold\n");
                        let doc = &workspace.projects.active().editor.vim().doc;
                        ensure!(
                            doc.path
                                .as_ref()
                                .is_some_and(|path| path.ends_with("original.txt"))
                                && doc.modified()
                        );
                        command(workspace, "saveas new.txt", window, cx);
                        let doc = &workspace.projects.active().editor.vim().doc;
                        ensure!(
                            doc.path
                                .as_ref()
                                .is_some_and(|path| path.ends_with("new.txt"))
                                && !doc.modified()
                        );
                        ensure!(
                            fs::read_to_string(renamed)? == "new\nold\n"
                                && fs::read_to_string(original)? == "old\n"
                        );
                        Ok(())
                    })
                })?
            },
        )
    }

    /// Stop a batch on the first failed target and only undo its successful text edits.
    #[test]
    #[ignore = "requires Windows DirectWrite and DirectX"]
    fn native_ex_argument_batch_undoes_its_actual_changes() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().join("project");
        fs::create_dir(&root)?;
        for (name, text) in [("a.txt", "red\n"), ("b.txt", "blue\n"), ("c.txt", "red\n")] {
            fs::write(root.join(name), text)?;
        }
        let options = Options::take(&mut vec!["--no-plugins".into()], temporary.path())?;
        native_test::run_async(
            root,
            options,
            |_| {},
            async move |handle, app| {
                app.update(|app| {
                    handle.update(app, |workspace, window, cx| -> Result<()> {
                        command(workspace, "args a.txt b.txt c.txt", window, cx);
                        command(workspace, "argdo %s/red/green/g", window, cx);
                        ensure!(workspace.projects.active().editor.vim().doc.text == "blue\n");
                        command(workspace, "buffer a.txt", window, cx);
                        ensure!(workspace.projects.active().editor.vim().doc.text == "green\n");
                        command(workspace, "undo-batch", window, cx);
                        ensure!(workspace.projects.active().editor.vim().doc.text == "red\n");
                        ensure!(workspace.projects.active().session_buffers().iter().all(
                            |(pane, index)| {
                                let buffer = &workspace
                                    .projects
                                    .active()
                                    .editor_by_id(*pane)
                                    .unwrap()
                                    .buffers()[*index];
                                buffer
                                    .vim
                                    .doc
                                    .path
                                    .as_ref()
                                    .is_none_or(|path| !path.ends_with("c.txt"))
                            }
                        ));
                        Ok(())
                    })
                })?
            },
        )
    }

    /// A script can close the last project and safely return to the start page.
    #[test]
    #[ignore = "requires Windows DirectWrite and DirectX"]
    fn native_ex_source_can_close_last_session() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().join("project");
        fs::create_dir(&root)?;
        fs::write(root.join("close.vim"), "q\nset number\n")?;
        let options = Options::take(&mut vec!["--no-plugins".into()], temporary.path())?;
        native_test::run_async(
            root,
            options,
            |_| {},
            async move |handle, app| {
                app.update(|app| {
                    handle.update(app, |workspace, window, cx| -> Result<()> {
                        command(workspace, "source close.vim", window, cx);
                        ensure!(workspace.projects.active_project().is_none());
                        Ok(())
                    })
                })?
            },
        )
    }

    /// Resume a real sourced file after :sleep and stop exactly at :finish.
    #[test]
    #[ignore = "requires Windows DirectWrite and DirectX"]
    fn native_ex_source_sleep_keeps_statement_order() -> Result<()> {
        use std::time::Duration;
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().join("project");
        fs::create_dir(&root)?;
        fs::write(root.join("README.md"), "base\n")?;
        fs::write(root.join("after.txt"), "after\n")?;
        fs::write(root.join("never.txt"), "never\n")?;
        fs::write(
            root.join("ordered.vim"),
            "sleep 50m | 0read after.txt\nfinish\n0read never.txt\n",
        )?;
        let options = Options::take(&mut vec!["--no-plugins".into()], temporary.path())?;
        native_test::run_async(
            root,
            options,
            |_| {},
            async move |handle, app| {
                app.update(|app| {
                    handle.update(app, |workspace, window, cx| -> Result<()> {
                        command(workspace, "source ordered.vim", window, cx);
                        ensure!(workspace.projects.active().editor.vim().doc.text == "base\n");
                        Ok(())
                    })
                })??;
                app.background_executor()
                    .timer(Duration::from_millis(150))
                    .await;
                app.update(|app| {
                    handle.update(app, |workspace, _, _| -> Result<()> {
                        ensure!(
                            workspace.projects.active().editor.vim().doc.text == "after\nbase\n"
                        );
                        ensure!(
                            workspace.ex_sourced_files.len() == 1
                                && workspace.ex_sourced_files[0].ends_with("ordered.vim")
                        );
                        Ok(())
                    })
                })?
            },
        )
    }

    /// Apply the original filter result before evaluating the next sourced command.
    #[test]
    #[ignore = "requires Windows DirectWrite and DirectX"]
    fn native_ex_source_waits_for_ranged_shell() -> Result<()> {
        use std::time::Duration;
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().join("project");
        fs::create_dir(&root)?;
        fs::write(root.join("README.md"), "base\n")?;
        fs::write(root.join("after.txt"), "after\n")?;
        fs::write(
            root.join("ordered.vim"),
            "1!echo changed\n0read after.txt\n",
        )?;
        let options = Options::take(&mut vec!["--no-plugins".into()], temporary.path())?;
        native_test::run_async(
            root,
            options,
            |_| {},
            async move |handle, app| {
                app.update(|app| {
                    handle.update(app, |workspace, window, cx| {
                        command(workspace, "source ordered.vim", window, cx);
                    })
                })?;
                for _ in 0..40 {
                    app.background_executor()
                        .timer(Duration::from_millis(25))
                        .await;
                    let done = app.update(|app| {
                        handle.update(app, |workspace, _, _| {
                            workspace.projects.active().editor.vim().doc.text == "after\nchanged\n"
                        })
                    })?;
                    if done {
                        return Ok(());
                    }
                }
                app.update(|app| {
                    handle.update(app, |workspace, _, _| {
                        let project = workspace.projects.active();
                        anyhow::bail!(
                            "source did not finish: {:?}; {}",
                            project.editor.vim().doc.text,
                            project.status
                        )
                    })
                })?
            },
        )
    }

    /// Keep the alternate file and filter mark, and capture Shell output before ending redirection.
    #[test]
    #[ignore = "requires Windows DirectWrite and DirectX"]
    fn native_ex_state_scopes_buffer_switches_and_redirected_shell() -> Result<()> {
        use std::time::Duration;
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().join("project");
        fs::create_dir(&root)?;
        fs::write(root.join("README.md"), "original\ntail\n")?;
        fs::write(root.join("second.txt"), "second\n")?;
        fs::write(root.join("third.txt"), "third\n")?;
        fs::write(
            root.join("state.vim"),
            concat!(
                "keepa edit third.txt\n",
                "keepalt edit #\n",
                "normal! ggma\n",
                "redir > output.txt\n",
                "silent kee 1!echo filtered\n",
                "silent 2verbose !echo shell-output\n",
                "echo 'after-shell'\n",
                "redir END\n",
            ),
        )?;
        let output = root.join("output.txt");
        let options = Options::take(&mut vec!["--no-plugins".into()], temporary.path())?;
        native_test::run_async(
            root,
            options,
            |_| {},
            async move |handle, app| {
                let _temporary = temporary;
                app.update(|app| {
                    handle.update(app, |workspace, window, cx| {
                        command(workspace, "edit second.txt", window, cx);
                        command(workspace, "source state.vim", window, cx);
                    })
                })?;
                for _ in 0..80 {
                    app.background_executor()
                        .timer(Duration::from_millis(25))
                        .await;
                    if fs::read_to_string(&output).is_ok_and(|text| text.contains("after-shell")) {
                        break;
                    }
                }
                app.update(|app| {
                    handle.update(app, |workspace, window, cx| -> Result<()> {
                        let project = workspace.projects.active();
                        ensure!(
                            project.editor.vim().doc.text == "filtered\ntail\n",
                            "{:?}: {}",
                            project.editor.vim().doc.text,
                            project.status
                        );
                        ensure!(
                            project
                                .editor
                                .alternate_path()
                                .is_some_and(|path| path.ends_with("README.md"))
                        );
                        let captured = fs::read_to_string(output)?;
                        ensure!(
                            captured.contains("shell-output\nafter-shell"),
                            "{captured:?}"
                        );
                        command(workspace, "normal! G'a", window, cx);
                        let vim = workspace.projects.active().editor.vim();
                        ensure!(vim.doc.location(vim.doc.cursor).0 == 0, "{}", vim.message);
                        ensure!(workspace.input_mappings.ex_verbose() == 0);
                        command(
                            workspace,
                            "redir => g:out | silent echo 'pipeline' | redir END",
                            window,
                            cx,
                        );
                        command(workspace, "echo g:out", window, cx);
                        ensure!(workspace.projects.active().editor.vim().message == "\npipeline");
                        Ok(())
                    })
                })?
            },
        )
    }
}
