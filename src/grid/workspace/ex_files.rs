//! Session-aware file inspection, search and buffer or argument navigation.

mod checktime;
mod directory;

use super::{
    Workspace,
    argument_list::{argument_paths, argument_words},
    ex_commands::resolve_buffer,
    projects::SplitDocument,
};
use gpui::{Context, Window};
use nanobug::{
    i18n,
    vim::{
        Action, FileArgument, FileBuffer, FileCommand, FileDestination, FileOperation, find_file,
    },
};
use std::path::{Path, PathBuf};

impl Workspace {
    /// Execute file, buffer and argument actions in the current Session.
    pub(super) fn file_ex_command(
        &mut self,
        command: FileCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.projects.active_project().is_none() || self.modal_open() {
            return;
        }
        self.finish_editor_input(window, cx);
        match command.operation {
            FileOperation::CheckTime => self.ex_checktime(command, window, cx),
            FileOperation::Edit => self.ex_edit_file(command, false, window, cx),
            FileOperation::Find(destination) => self.ex_find_file(command, destination, window, cx),
            FileOperation::Name => self.ex_file_name(command, window, cx),
            FileOperation::WorkingDirectory => self.show_ex_directory(),
            FileOperation::GlobalDirectory
            | FileOperation::TabDirectory
            | FileOperation::WindowDirectory => self.change_ex_directory(command),
            FileOperation::Drop => self.ex_drop_files(command, window, cx),
            FileOperation::View { split: false } => self.ex_edit_file(command, true, window, cx),
            FileOperation::View { split: true } => self.ex_split_view(command, window, cx),
            FileOperation::Buffer { target, split } => {
                self.ex_buffer_file(command, target, split, window, cx)
            }
            FileOperation::Argument { target, write } => {
                self.ex_argument_file(command, target, write, window, cx)
            }
        }
    }

    /// Show or assign the current buffer name using the Session's filename index.
    fn ex_file_name(&mut self, command: FileCommand, window: &mut Window, cx: &mut Context<Self>) {
        let project = self.projects.active();
        let vim = project.editor.vim();
        let naming = command.count == Some(0) || !command.arguments.is_empty();
        if naming {
            if vim.is_read_only() || project.editor.terminal().is_some() {
                self.set_status(i18n::text("editor.read_only_preview").into());
                return;
            }
            let path = if command.count == Some(0) {
                None
            } else {
                match self.file_operand(&command.arguments) {
                    Some(path) => Some(path),
                    None => {
                        self.invalid_file_argument(&command.arguments);
                        return;
                    }
                }
            };
            let number = vim.doc.buffer_number();
            if path
                .as_deref()
                .and_then(|target| project.number_for_path(target))
                .is_some_and(|existing| existing != number)
            {
                self.set_status(i18n::text("ex.buffer_name_exists").into());
                return;
            }
            let old = vim
                .doc
                .path
                .clone()
                .filter(|old| Some(old) != path.as_ref());
            self.run_vim(
                |vim, _| {
                    vim.doc.set_ex_filename(path);
                    Action::None
                },
                window,
                cx,
            );
            let project = self.projects.active_mut();
            if let Some(old) = old {
                project.editor.remember_alternate_path(old);
            }
            project.refresh_shared_documents();
            project.sync_file_root();
        }
        let project = self.projects.active();
        let doc = &project.editor.vim().doc;
        let name = doc
            .path
            .as_ref()
            .map(|path| {
                path.strip_prefix(self.ex_current_dir())
                    .unwrap_or(path)
                    .display()
                    .to_string()
            })
            .unwrap_or_else(|| "[No Name]".into());
        let mut state = String::new();
        if doc.modified() {
            state.push_str(" [Modified]");
        }
        if doc.path.as_deref().is_some_and(|path| !path.exists()) {
            state.push_str(" [New]");
        }
        if project.editor.vim().is_read_only()
            || project.editor.vim().file_read_only()
            || doc
                .path
                .as_deref()
                .and_then(|path| path.metadata().ok())
                .is_some_and(|meta| meta.permissions().readonly())
        {
            state.push_str(" [Readonly]");
        }
        let position = doc.location(doc.cursor).0 + 1;
        let message = format!("\"{name}\"{state} line {position} of {}", doc.starts.len());
        self.ex_output_message(&message, false);
    }

    /// Handle an edit or write-protected view, including reload without a filename.
    fn ex_edit_file(
        &mut self,
        command: FileCommand,
        protect: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((after, arguments)) = edit_arguments(&command.arguments) else {
            self.invalid_file_argument(&command.arguments);
            return;
        };
        if arguments.is_empty() {
            if self.projects.active().editor.vim().doc.modified() && !command.force {
                self.set_status(i18n::text("ex.unsaved_buffers").into());
                return;
            }
            if self.projects.active().editor.vim().doc.path.is_none() {
                if command.force {
                    self.discard_active_file();
                }
            } else if !self.reload_active_file(window, cx) {
                return;
            }
        } else {
            let Some(path) = self.file_operand(arguments) else {
                self.invalid_file_argument(arguments);
                return;
            };
            if path.is_dir() {
                self.open_project(&path, window, cx);
                return;
            }
            if !self.open_ex_file(&path, command.force, window, cx) {
                return;
            }
        }
        if protect {
            self.protect_ex_view(window, cx);
        }
        self.after_file_load(after, window, cx);
    }

    /// Open a shared split view with native buffer-local write protection.
    fn ex_split_view(&mut self, command: FileCommand, window: &mut Window, cx: &mut Context<Self>) {
        let Some((after, arguments)) = edit_arguments(&command.arguments) else {
            self.invalid_file_argument(&command.arguments);
            return;
        };
        let source = if arguments.is_empty() {
            SplitDocument::Current
        } else if let Some(path) = self.file_operand(arguments) {
            SplitDocument::File(path, None)
        } else {
            self.invalid_file_argument(arguments);
            return;
        };
        if self.open_ex_file_split(source, command.count, window, cx) {
            self.protect_ex_view(window, cx);
            self.after_file_load(after, window, cx);
        }
    }

    /// Keep the file editable while preventing accidental writes to its own path.
    fn protect_ex_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.run_vim(
            |vim, _| {
                vim.set_file_read_only(true);
                Action::None
            },
            window,
            cx,
        );
    }

    /// Reuse a window already displaying the first file and install its full argument list.
    fn ex_drop_files(&mut self, command: FileCommand, window: &mut Window, cx: &mut Context<Self>) {
        let Some((after, arguments)) = edit_arguments(&command.arguments) else {
            self.invalid_file_argument(&command.arguments);
            return;
        };
        let project = self.projects.active();
        let editor = &project.editor;
        let origin = project.active_editor_id();
        let current = editor.vim().doc.path.as_deref();
        let mut names = argument_words(arguments);
        for name in &mut names {
            if matches!(name.as_str(), "%" | "#" | "%:h" | "#:h") {
                let Some(path) = self.file_operand(name) else {
                    self.invalid_file_argument(name);
                    return;
                };
                *name = path.to_string_lossy().into_owned();
            }
        }
        let files = argument_paths(&names, self.ex_current_dir(), current);
        let Some(path) = files.first() else {
            self.invalid_file_argument(arguments);
            return;
        };
        if let Some(tab) = self.ex_tab_insertion() {
            self.drop_files_in_tabpages(files, tab, after, window, cx);
            return;
        }
        let displayed = if current == Some(path.as_path()) {
            Some(origin)
        } else {
            project.editors().find_map(|(pane, editor)| {
                (editor.vim().doc.path.as_ref() == Some(path)).then_some(pane)
            })
        };
        let opened = if let Some(pane) = displayed {
            let now = self.scroll_epoch.elapsed().as_secs_f64();
            if self.projects.active_mut().focus_editor_by_id(pane, now) {
                self.reset_tab_view(window, cx);
            }
            self.refresh_dropped_file(window, cx);
            true
        } else {
            // The pane retains its previous buffer, including any unsaved changes.
            self.open_ex_file(path, command.force, window, cx)
        };
        if opened {
            let project = self.projects.active_mut();
            let first = files[0].clone();
            for path in &files {
                project.list_unloaded_file(path.clone(), None);
            }
            project.arguments.replace_for_pane(origin, files);
            let pane = project.active_editor_id();
            if let Some(index) = project
                .arguments
                .files(pane)
                .iter()
                .position(|path| *path == first)
            {
                project.arguments.select(pane, index);
            }
            self.after_file_load(after, window, cx);
        }
    }

    /// Place each argument on its own Tabpage when :drop runs under the :tab modifier.
    fn drop_files_in_tabpages(
        &mut self,
        files: Vec<PathBuf>,
        tab: usize,
        after: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.projects.manager_available() {
            self.set_status(i18n::text("editor.tabpage_requires_project").into());
            return;
        }
        let now = self.scroll_epoch.elapsed().as_secs_f64();
        let project = self.projects.active_mut();
        let origin = project.active_editor_id();
        let doc = &project.editor.vim().doc;
        let reuse = tab == project.page_count()
            && project.active_page() + 1 == tab
            && project.split_count() == 1
            && project.editor.buffers().len() == 1
            && doc.path.is_none()
            && !doc.modified()
            && project.editor.terminal().is_none()
            && !project.editor.vim().is_read_only();
        let first = if reuse { tab - 1 } else { tab };
        let mut opened = Vec::new();
        for (index, path) in files.iter().enumerate() {
            let result = if index == 0 && reuse {
                project
                    .jump_file(path, false, now, |doc| Some(doc.cursor))
                    .map(|_| ())
            } else {
                project.open_page_source(
                    SplitDocument::File(path.clone(), None),
                    first + index,
                    now,
                )
            };
            if let Err(error) = result {
                project.status = format!("{error:#}");
                break;
            }
            opened.push(project.active_editor_id());
        }
        if opened.is_empty() {
            return;
        }
        let complete = opened.len() == files.len();
        for path in &files {
            project.list_unloaded_file(path.clone(), None);
        }
        project.arguments.replace_for_pane(origin, files);
        for (index, pane) in opened.into_iter().enumerate() {
            project.arguments.select(pane, index);
        }
        if complete {
            project.status.clear();
        }
        project.select_page(first, now);
        self.reset_tab_view(window, cx);
        if complete {
            self.after_file_load(after, window, cx);
        }
    }

    /// When :drop returns to a clean buffer, reread only if its disk contents have changed.
    fn refresh_dropped_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut result = Ok(());
        self.run_vim(
            |vim, _| {
                if !vim.doc.modified() {
                    result = vim.doc.check_external_change().and_then(|change| {
                        if change == Some(false) {
                            vim.doc.reload_file()?;
                        }
                        Ok(())
                    });
                }
                Action::None
            },
            window,
            cx,
        );
        if let Err(error) = result {
            self.set_status(format!("{error:#}"));
        }
    }

    /// Locate the requested occurrence in 'path' before changing any workspace topology.
    fn ex_find_file(
        &mut self,
        command: FileCommand,
        destination: FileDestination,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((after, arguments)) = edit_arguments(&command.arguments) else {
            self.invalid_file_argument(&command.arguments);
            return;
        };
        let Some(name) = single_operand(arguments) else {
            self.invalid_file_argument(arguments);
            return;
        };
        let project = self.projects.active();
        let vim = project.editor.vim();
        let from = vim.doc.path.as_deref();
        let name = match name.as_str() {
            "%" => from
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default(),
            "#" => project
                .editor
                .alternate_path()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default(),
            _ => name,
        };
        let occurrence = if destination == FileDestination::Current {
            command.count.unwrap_or(1)
        } else {
            1
        };
        if self.input_mappings.ex_verbose() >= 11 {
            self.input_mappings.trace_ex(
                11,
                &format!("Searching for \"{name}\" in {}", vim.file_search_path()),
            );
        }
        let Some(path) = find_file(
            self.ex_current_dir(),
            from,
            &vim.file_search_path(),
            &name,
            occurrence,
        ) else {
            self.set_status(i18n::format("vim_g.missing_file", &[("path", &name)]));
            return;
        };
        let opened = match destination {
            FileDestination::Current => self.open_ex_file(&path, command.force, window, cx),
            FileDestination::Split => {
                self.open_ex_file_split(SplitDocument::File(path, None), command.count, window, cx)
            }
            FileDestination::Tabpage => {
                if !self.projects.manager_available() {
                    self.set_status(i18n::text("editor.tabpage_requires_project").into());
                    return;
                }
                let after = self
                    .ex_tab_insertion()
                    .or(command.count)
                    .unwrap_or_else(|| self.projects.active().active_page() + 1);
                if after > self.projects.active().page_count() {
                    self.invalid_file_argument(&after.to_string());
                    return;
                }
                let now = self.scroll_epoch.elapsed().as_secs_f64();
                let project = self.projects.active_mut();
                match project.open_page_source(SplitDocument::File(path, None), after, now) {
                    Ok(()) => {
                        self.reset_tab_view(window, cx);
                        true
                    }
                    Err(error) => {
                        self.set_status(format!("{error:#}"));
                        false
                    }
                }
            }
        };
        if opened {
            self.after_file_load(after, window, cx);
        }
    }

    /// Resolve the target from Session buffer numbers before splitting or abandoning a buffer.
    fn ex_buffer_file(
        &mut self,
        command: FileCommand,
        target: FileBuffer,
        split: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((after, arguments)) = edit_arguments(&command.arguments) else {
            self.invalid_file_argument(&command.arguments);
            return;
        };
        let project = self.projects.active();
        let current = project.editor.vim().doc.buffer_number();
        let number = if target == FileBuffer::Exact {
            if arguments.is_empty() {
                command.count.map_or(Some(current), |number| {
                    resolve_buffer(project, &number.to_string())
                })
            } else if command.count.is_none() {
                resolve_buffer(project, arguments)
            } else {
                None
            }
        } else {
            let count = match parse_count(arguments, command.count) {
                Some(count) => count.unwrap_or(1),
                None => {
                    self.invalid_file_argument(arguments);
                    return;
                }
            };
            let buffers: Vec<_> = project
                .session_buffer_entries()
                .into_iter()
                .filter(|(_, buffer)| buffer.supports_shared_view() || buffer.terminal().is_some())
                .map(|(number, buffer)| (number, buffer.vim.doc.modified()))
                .collect();
            choose_buffer(&buffers, current, target, count)
        };
        let Some(number) = number else {
            self.set_status(i18n::text("vim_bracket.no_buffer").into());
            return;
        };
        let opened = if split {
            self.open_ex_file_split(SplitDocument::SessionBuffer(number), None, window, cx)
        } else {
            if command.force && number != current {
                self.discard_active_file();
            }
            let now = self.scroll_epoch.elapsed().as_secs_f64();
            self.projects
                .active_mut()
                .select_session_buffer(number, now)
        };
        if opened {
            self.after_file_load(after, window, cx);
        }
    }

    /// Navigate the same argument list used by :argument, opening splits only for valid entries.
    fn ex_argument_file(
        &mut self,
        command: FileCommand,
        target: FileArgument,
        write: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((after, arguments)) = edit_arguments(&command.arguments) else {
            self.invalid_file_argument(&command.arguments);
            return;
        };
        let project = self.projects.active();
        let pane = project.active_editor_id();
        let current = project.arguments.index(pane);
        if write {
            let count = command.count.unwrap_or(1);
            let name = (!arguments.is_empty()).then_some(arguments);
            if !self.write_ex_buffer(name, false, command.force, window, cx) {
                return;
            }
            let cmd = nanobug::vim::ArgumentCommand::Step {
                forward: target == FileArgument::Next,
            };
            self.argument_list_command(cmd, Some(count), String::new(), window, cx);
            return;
        }
        let new_list = (!arguments.is_empty() && target == FileArgument::Next).then(|| {
            argument_paths(
                &argument_words(arguments),
                self.ex_current_dir(),
                project.editor.vim().doc.path.as_deref(),
            )
        });
        let count = if new_list.is_some() {
            None
        } else {
            match parse_count(arguments, command.count) {
                Some(count) => count,
                None => {
                    self.invalid_file_argument(arguments);
                    return;
                }
            }
        };
        let index = if new_list.is_some() {
            Some(0)
        } else {
            argument_index(target, current, project.arguments.len(pane), count)
        };
        let Some(index) = index else {
            self.set_status(i18n::text("vim_bracket.argument_bounds").into());
            return;
        };
        let file = if let Some(files) = &new_list {
            files.get(index).cloned()
        } else {
            project.arguments.get(pane, index).map(Path::to_owned)
        };
        let Some(file) = file else {
            self.set_status(
                i18n::text(if project.arguments.len(pane) == 0 {
                    "vim_bracket.no_arguments"
                } else {
                    "vim_bracket.argument_bounds"
                })
                .into(),
            );
            return;
        };
        if self.open_ex_file_split(SplitDocument::File(file, None), None, window, cx) {
            let project = self.projects.active_mut();
            let pane = project.active_editor_id();
            if let Some(files) = new_list {
                project.arguments.replace_for_pane(pane, files);
            } else {
                project.arguments.select(pane, index);
            }
            self.after_file_load(after, window, cx);
        }
    }

    /// Detect a newly opened split without assuming it succeeds on a small window.
    fn open_ex_file_split(
        &mut self,
        source: SplitDocument,
        size: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let pane = self.projects.active().active_editor_id();
        self.open_editor_split_at(true, true, source, size, window, cx);
        self.projects.active().active_editor_id() != pane
    }

    /// Load a destination first and then honor an explicit discard of the previous buffer.
    fn open_ex_file(
        &mut self,
        path: &Path,
        force: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let project = self.projects.active();
        let pane = project.active_editor_id();
        let previous = project.editor.buffer().id;
        let same = project.editor.vim().doc.path.as_deref() == Some(path);
        if same {
            if !force && project.editor.vim().doc.modified() {
                self.set_status(i18n::text("ex.unsaved_buffers").into());
                return false;
            }
            return self.reload_active_file(window, cx);
        }
        match self.jump_to_file(path, false, |doc| Some(doc.cursor)) {
            Ok(true) => {
                if force {
                    let editor = self
                        .projects
                        .active_mut()
                        .editor_by_id_mut(pane)
                        .expect("source pane retained");
                    if let Some(index) = editor.buffer_index(previous) {
                        editor.edit_buffer(index, |vim, _| {
                            vim.doc.discard_changes();
                            Action::None
                        });
                    }
                }
                true
            }
            Ok(false) => false,
            Err(error) => {
                self.set_status(format!("{error:#}"));
                false
            }
        }
    }

    /// Apply native `+N` and `+{cmd}` after loading through the ordinary Ex dispatcher.
    pub(super) fn after_file_load(
        &mut self,
        command: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(command) = command else {
            return;
        };
        let line = if command.is_empty() {
            Some(usize::MAX)
        } else {
            command.parse::<usize>().ok()
        };
        if let Some(line) = line {
            self.run_vim(
                |vim, _| {
                    let line = line.clamp(1, vim.line_count());
                    vim.place_jump(vim.doc.starts[line - 1]);
                    Action::None
                },
                window,
                cx,
            );
        } else if let Some(pattern) = command.strip_prefix('/') {
            self.run_vim(
                |vim, register| {
                    vim.key("/", register);
                    vim.text(pattern, register);
                    vim.key("Enter", register)
                },
                window,
                cx,
            );
        } else {
            let command = command.replace("\\ ", " ");
            self.run_vim(
                |vim, register| vim.execute_ex_line(&command, register),
                window,
                cx,
            );
        }
    }

    /// Drop edits explicitly discarded by a bang command while preserving the buffer association.
    fn discard_active_file(&mut self) {
        self.projects.active_mut().editor.edit(|vim, _| {
            vim.doc.discard_changes();
            Action::None
        });
    }

    /// Reload the active file with its shared buffer identity intact.
    fn reload_active_file(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let mut result = Ok(());
        self.run_vim(
            |vim, _| {
                result = vim.doc.reload_file();
                Action::None
            },
            window,
            cx,
        );
        if let Err(error) = result {
            self.set_status(format!("{error:#}"));
            false
        } else {
            self.set_status(String::new());
            true
        }
    }

    /// Report a malformed file command through the same error channel as other Ex actions.
    fn invalid_file_argument(&mut self, argument: &str) {
        self.set_status(i18n::format(
            "editor.invalid_ex_argument",
            &[("argument", &argument)],
        ));
    }
}

/// Split a leading file modifier and return the remaining raw filename or argument list.
pub(super) fn edit_arguments(text: &str) -> Option<(Option<&str>, &str)> {
    let mut rest = text.trim();
    let mut after = None;
    while let Some(modifier) = rest.strip_prefix('+') {
        let mut escaped = false;
        let end = modifier
            .char_indices()
            .find_map(|(index, ch)| {
                let delimiter = ch.is_whitespace() && !escaped;
                escaped = ch == '\\' && !escaped;
                delimiter.then_some(index)
            })
            .unwrap_or(modifier.len());
        let value = &modifier[..end];
        if let Some(option) = value.strip_prefix('+') {
            if !matches!(option, "enc=utf-8" | "encoding=utf-8") {
                return None;
            }
        } else if after.replace(value).is_some() {
            return None;
        }
        rest = modifier[end..].trim_start();
    }
    Some((after, rest))
}

/// Return one escaped word, rejecting an empty argument or a second operand.
fn single_operand(text: &str) -> Option<String> {
    let mut words = argument_words(text).into_iter();
    let first = words.next()?;
    words.next().is_none().then_some(first)
}

/// Use an explicit numeric argument before a leading command address.
fn parse_count(text: &str, address: Option<usize>) -> Option<Option<usize>> {
    if text.is_empty() {
        Some(address)
    } else {
        text.parse::<usize>()
            .ok()
            .filter(|&count| count > 0)
            .map(Some)
    }
}

/// Select one target from the current ordered arguments around `current`.
fn argument_index(
    target: FileArgument,
    current: usize,
    length: usize,
    count: Option<usize>,
) -> Option<usize> {
    match target {
        FileArgument::Current => count.map_or(Some(current), |value| value.checked_sub(1)),
        FileArgument::First => Some(0),
        FileArgument::Last => length.checked_sub(1),
        FileArgument::Next => current.checked_add(count.unwrap_or(1)),
        FileArgument::Previous => current.checked_sub(count.unwrap_or(1)),
    }
    .filter(|index| *index < length)
}

/// Select a numbered buffer by endpoint, relative count, or modified status.
fn choose_buffer(
    buffers: &[(u64, bool)],
    current: u64,
    target: FileBuffer,
    count: usize,
) -> Option<u64> {
    let length = buffers.len();
    if length == 0 {
        return None;
    }
    let start = buffers.partition_point(|(number, _)| *number <= current);
    let selected = match target {
        FileBuffer::First => buffers.first(),
        FileBuffer::Last => buffers.last(),
        FileBuffer::Next => buffers.get((start % length + (count - 1) % length) % length),
        FileBuffer::Previous => {
            let earlier = buffers.partition_point(|(number, _)| *number < current);
            buffers.get((earlier % length + length - count % length) % length)
        }
        FileBuffer::Modified => {
            let modified: Vec<_> = buffers[start..]
                .iter()
                .chain(buffers[..start].iter())
                .filter(|(_, dirty)| *dirty)
                .collect();
            if modified.is_empty() {
                return None;
            }
            modified.get((count - 1) % modified.len()).copied()
        }
        FileBuffer::Exact => None,
    };
    selected.map(|(number, _)| *number)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Select the requested modified buffer in shared Session order and wrap on overflow.
    #[test]
    fn ex_files_modified_buffer_navigation_uses_session_order() {
        let buffers = [(3, true), (5, false), (8, true), (11, false)];
        assert_eq!(choose_buffer(&buffers, 3, FileBuffer::Modified, 1), Some(8));
        assert_eq!(choose_buffer(&buffers, 3, FileBuffer::Modified, 2), Some(3));
        assert_eq!(
            choose_buffer(&buffers, 8, FileBuffer::Previous, 3),
            Some(11)
        );
        assert_eq!(choose_buffer(&buffers, 8, FileBuffer::Next, 2), Some(3));
        assert_eq!(argument_index(FileArgument::Next, 2, 3, None), None);
    }
}

#[cfg(all(test, windows))]
mod native_tests {
    use super::*;
    use crate::grid::workspace::native_test;
    use anyhow::{Result, ensure};
    use gpui::AppContext;
    use nanobug::plugins::session::Options;
    use std::{fs, time::Duration};

    /// Exercise the editor-to-workspace command route without a foreign Ex engine.
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

    /// Use scoped Ex directories, protect shared writes and focus an already visible file.
    #[test]
    #[ignore = "requires Windows DirectWrite and DirectX"]
    fn native_ex_files_directories_views_and_drop() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().join("project");
        for directory in ["global", "window", "tab"] {
            fs::create_dir_all(root.join(directory))?;
        }
        let note = root.join("tab/note.txt");
        fs::write(&note, "note\n")?;
        fs::write(root.join("tab/other.txt"), "other\n")?;
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
                        let project_root = workspace.projects.active().root.clone();
                        command(workspace, "cd global", window, cx);
                        ensure!(workspace.ex_current_dir().ends_with("global"));
                        command(workspace, "lchdir ../window", window, cx);
                        ensure!(workspace.ex_current_dir().ends_with("window"));
                        command(workspace, "lcd -", window, cx);
                        ensure!(workspace.ex_current_dir().ends_with("global"));
                        command(workspace, "tchdir ../tab", window, cx);
                        ensure!(workspace.ex_current_dir().ends_with("tab"));
                        ensure!(
                            workspace
                                .projects
                                .active()
                                .editor
                                .directory()
                                .path()
                                .is_none()
                        );
                        ensure!(workspace.projects.active().root == project_root);

                        command(workspace, "view note.txt", window, cx);
                        let editor = &workspace.projects.active().editor;
                        ensure!(editor.vim().doc.path == Some(note.canonicalize()?));
                        ensure!(editor.vim().file_read_only() && !editor.vim().is_read_only());
                        workspace.run_vim(
                            |vim, register| {
                                vim.text("i!", register);
                                vim.key("Escape", register)
                            },
                            window,
                            cx,
                        );
                        command(workspace, "write", window, cx);
                        ensure!(workspace.projects.active().editor.vim().doc.modified());
                        ensure!(fs::read_to_string(&note)? == "note\n");
                        command(workspace, "write!", window, cx);
                        ensure!(fs::read_to_string(&note)? == "!note\n");
                        ensure!(!workspace.projects.active().editor.vim().file_read_only());

                        let number = workspace.projects.active().editor.vim().doc.buffer_number();
                        command(workspace, "sview", window, cx);
                        ensure!(workspace.projects.active().split_count() == 2);
                        ensure!(
                            workspace
                                .projects
                                .active()
                                .page_editors()
                                .all(|(_, editor)| {
                                    editor.vim().doc.buffer_number() == number
                                        && editor.vim().file_read_only()
                                })
                        );
                        command(workspace, "tabnew", window, cx);
                        ensure!(workspace.projects.active().active_page() == 1);
                        ensure!(workspace.ex_current_dir().ends_with("tab"));
                        fs::write(&note, "external\n")?;
                        command(workspace, "drop note.txt other.txt", window, cx);
                        let project = workspace.projects.active();
                        ensure!(project.active_page() == 0 && project.split_count() == 2);
                        ensure!(project.editor.vim().doc.buffer_number() == number);
                        ensure!(project.editor.vim().doc.text == "external\n");
                        ensure!(project.arguments.len(project.active_editor_id()) == 2);

                        workspace.run_vim(
                            |vim, register| {
                                vim.text("i!", register);
                                vim.key("Escape", register)
                            },
                            window,
                            cx,
                        );
                        command(workspace, "drop other.txt", window, cx);
                        let project = workspace.projects.active();
                        ensure!(
                            project.split_count() == 2
                                && project.editor.vim().doc.text == "other\n"
                        );
                        ensure!(project.session_buffer_entries()[&number].vim.doc.modified());
                        command(workspace, "tcd!", window, cx);
                        ensure!(workspace.ex_current_dir().ends_with("global"));
                        command(workspace, "chdir -", window, cx);
                        ensure!(workspace.ex_current_dir() == project_root);
                        Ok(())
                    })
                })?
            },
        )
    }

    /// Search the second path match, track the previous name and reread clean changed contents.
    #[test]
    #[ignore = "requires Windows DirectWrite and DirectX"]
    fn native_ex_files_search_rename_and_checktime() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().join("project");
        fs::create_dir_all(root.join("one"))?;
        fs::create_dir_all(root.join("two"))?;
        fs::write(root.join("main.txt"), "main\n")?;
        fs::write(root.join("one/shared.txt"), "one\n")?;
        let selected = root.join("two/shared.txt");
        fs::write(&selected, "two\n")?;
        let first = root.join("main.txt");
        let renamed = root.canonicalize()?.join("renamed.txt");
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
                        workspace.open(first, window, cx);
                        command(workspace, "set path=one,two", window, cx);
                        command(workspace, "2find shared.txt", window, cx);
                        let canonical = selected.canonicalize()?;
                        ensure!(
                            workspace.projects.active().editor.vim().doc.path.as_ref()
                                == Some(&canonical)
                        );
                        let number = workspace.projects.active().editor.vim().doc.buffer_number();
                        fs::write(&selected, "external\n")?;
                        command(workspace, "checktime", window, cx);
                        let doc = &workspace.projects.active().editor.vim().doc;
                        ensure!(
                            doc.buffer_number() == number
                                && doc.text == "external\n"
                                && !doc.modified()
                        );
                        workspace.execute_ex_input(
                            "file renamed.txt | let g:renamed = 1",
                            "test",
                            1,
                            false,
                            window,
                            cx,
                        );
                        let expected = workspace.projects.active().root.join("renamed.txt");
                        ensure!(
                            workspace.projects.active().editor.vim().doc.path.as_deref()
                                == Some(expected.as_path())
                        );
                        ensure!(
                            !renamed.exists() && fs::read_to_string(&selected)? == "external\n"
                        );
                        ensure!(
                            workspace.projects.active().editor.alternate_path()
                                == Some(canonical.as_path())
                        );
                        command(workspace, "echo g:renamed", window, cx);
                        ensure!(workspace.projects.active().editor.vim().message == "1");
                        command(workspace, "ex #", window, cx);
                        ensure!(
                            workspace.projects.active().editor.vim().doc.path.as_ref()
                                == Some(&canonical)
                        );
                        Ok(())
                    })
                })?
            },
        )
    }

    /// Split to an argument, write before going back, and reopen the same modified buffer in a split.
    #[test]
    #[ignore = "requires Windows DirectWrite and DirectX"]
    fn native_ex_files_argument_and_buffer_splits_reuse_local_contents() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().join("project");
        fs::create_dir(&root)?;
        let first = root.join("a.txt");
        let second = root.join("b.txt");
        fs::write(&first, "a\n")?;
        fs::write(&second, "b\n")?;
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
                        command(workspace, "args a.txt b.txt", window, cx);
                        let original = workspace.projects.active().editor.vim().doc.buffer_number();
                        workspace.run_vim(
                            |vim, _| {
                                vim.doc.replace(0..1, "changed-a", true);
                                Action::None
                            },
                            window,
                            cx,
                        );
                        command(workspace, "snext", window, cx);
                        ensure!(
                            workspace.projects.active().split_count() == 2,
                            "{}",
                            workspace.projects.active().status
                        );
                        let doc = &workspace.projects.active().editor.vim().doc;
                        ensure!(doc.path.as_deref() == Some(second.canonicalize()?.as_path()));
                        workspace.run_vim(
                            |vim, _| {
                                vim.doc.replace(0..1, "changed-b", true);
                                Action::None
                            },
                            window,
                            cx,
                        );
                        command(workspace, "wNext", window, cx);
                        ensure!(fs::read_to_string(&second)? == "changed-b\n");
                        let doc = &workspace.projects.active().editor.vim().doc;
                        ensure!(doc.buffer_number() == original && doc.text == "changed-a\n");
                        command(workspace, &format!("sbuffer {original}"), window, cx);
                        ensure!(workspace.projects.active().split_count() == 3);
                        ensure!(
                            workspace.projects.active().editor.vim().doc.buffer_number()
                                == original
                        );
                        Ok(())
                    })
                })?
            },
        )
    }
}
