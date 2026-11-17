//! Coordinate Ex buffer lifetime with Session-owned documents and pane layouts.

use super::{
    Workspace,
    argument_list::{argument_paths, argument_words},
    ex_commands::resolve_buffer,
    ex_files::edit_arguments,
    projects::{Project, SplitDocument},
};
use gpui::{Context, Window};
use nanobug::{
    i18n,
    vim::{BufferCommand, BufferSet},
};

impl Workspace {
    /// Apply a parsed buffer lifetime or argument-scope command.
    pub(super) fn buffer_lifecycle(
        &mut self,
        command: BufferCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.projects.active_project().is_none() || self.modal_open() {
            return;
        }
        self.finish_editor_input(window, cx);
        match command {
            BufferCommand::ArgumentScope {
                local,
                arguments,
                force,
            } => self.switch_argument_scope(local, &arguments, force, window, cx),
            BufferCommand::Add {
                alternate,
                arguments,
            } => self.list_new_file(alternate, &arguments),
            BufferCommand::Remove {
                wipe,
                force,
                address,
                selectors,
            } => self.remove_buffer_contents(wipe, force, &address, &selectors),
            BufferCommand::Hide {
                window: position,
                command,
            } => self.hide_buffer_window(position, &command, window, cx),
            BufferCommand::Show {
                set,
                maximum,
                force,
            } => self.show_buffer_windows(set, maximum, force, window, cx),
        }
        self.sync(window, cx);
    }

    /// Switch argument-list scope, optionally replacing the selected list and visiting its first file.
    fn switch_argument_scope(
        &mut self,
        local: bool,
        args: &str,
        force: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let directory = self.ex_current_dir().to_owned();
        let now = self.scroll_epoch.elapsed().as_secs_f64();
        let Some((after, args)) = edit_arguments(args) else {
            self.invalid_buffer_operand(args);
            return;
        };
        let project = self.projects.active_mut();
        let pane = project.active_editor_id();
        let current = project.editor.vim().doc.path.clone();
        let outgoing = project.editor.buffer().id;
        if args.is_empty() {
            project.arguments.set_scope(pane, local, current.as_deref());
            project.status.clear();
            return;
        }
        let files = argument_paths(&argument_words(args), &directory, current.as_deref());
        let Some(first) = files.first() else {
            self.set_status(i18n::text("vim_bracket.no_arguments").into());
            return;
        };
        if let Err(error) = project.jump_file(first, false, now, |doc| Some(doc.cursor)) {
            project.status = format!("{error:#}");
            return;
        }
        if force
            && let Some(index) = project
                .editor
                .buffer_index(outgoing)
                .filter(|&index| index != project.editor.active_index())
        {
            project.editor.discard(index);
        }
        project.arguments.set_scope(pane, local, Some(first));
        for path in &files {
            project.list_unloaded_file(path.clone(), None);
        }
        project.arguments.replace_for_pane(pane, files);
        project.status.clear();
        self.reset_tab_view(window, cx);
        self.after_file_load(after, window, cx);
    }

    /// List a file without opening its contents in the active editor.
    fn list_new_file(&mut self, alternate: bool, args: &str) {
        let Some((after, file)) = edit_arguments(args) else {
            self.invalid_buffer_operand(args);
            return;
        };
        let names = argument_words(file);
        let [file] = names.as_slice() else {
            self.invalid_buffer_operand(args);
            return;
        };
        let line = if let Some(after) = after {
            match after.parse::<usize>() {
                Ok(line) if line > 0 => Some(line),
                _ => {
                    self.invalid_buffer_operand(args);
                    return;
                }
            }
        } else {
            None
        };
        let current = self.projects.active().editor.vim().doc.path.as_deref();
        let files = argument_paths(std::slice::from_ref(file), self.ex_current_dir(), current);
        let [path] = files.as_slice() else {
            self.invalid_buffer_operand(args);
            return;
        };
        if path.is_dir() {
            self.invalid_buffer_operand(args);
            return;
        }
        let project = self.projects.active_mut();
        project.list_unloaded_file(path.clone(), line);
        if alternate {
            project.editor.remember_alternate_path(path.clone());
        }
        project.status.clear();
    }

    /// Validate all requested buffers before unloading text or removing their references.
    fn remove_buffer_contents(&mut self, wipe: bool, force: bool, address: &str, selectors: &str) {
        let project = self.projects.active_mut();
        project.refresh_shared_documents();
        let Some(numbers) = selected_buffers(project, address, selectors) else {
            self.set_status(i18n::text("vim_bracket.no_buffer").into());
            return;
        };
        if !force
            && numbers.iter().any(|number| {
                project.session_buffer(*number).is_some_and(|buffer| {
                    buffer.vim.doc.modified()
                        || buffer.terminal().is_some_and(|terminal| !terminal.exited())
                })
            })
        {
            self.set_status(i18n::text("ex.unsaved_buffers").into());
            return;
        }
        let now = self.scroll_epoch.elapsed().as_secs_f64();
        for number in numbers {
            if wipe {
                project.wipe_session_buffer(number, now);
            } else {
                project.unload_session_buffer(number, now);
            }
        }
        project.status.clear();
        self.last_cursor_space = None;
    }

    /// Close the addressed pane or page without losing any of its unsaved text.
    fn hide_buffer_window(
        &mut self,
        position: Option<usize>,
        command: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let project = self.projects.active_mut();
        let origin = project.active_editor_id();
        if !command.is_empty() {
            let scope = project.plugin_scope;
            let before = project.editor.buffer().id;
            project.status.clear();
            self.run_vim(
                |vim, register| vim.execute_ex_line(command, register),
                window,
                cx,
            );
            if let Some(project) = self.projects.active_project_mut().filter(|project| {
                project.plugin_scope == scope
                    && project.active_editor_id() == origin
                    && project.status.is_empty()
                    && project.editor.buffer().id != before
            }) {
                project.detach_active_label(before, 0.);
            }
            return;
        }
        let now = self.scroll_epoch.elapsed().as_secs_f64();
        let ids = project.splits().ids();
        if ids.len() == 1 && project.page_count() == 1 {
            self.set_status(i18n::text("ex.last_window").into());
            return;
        }
        let target = position.map_or(origin, |position| {
            ids[position.saturating_sub(1).min(ids.len() - 1)]
        });
        project.retain_hidden_editor(target);
        if ids.len() == 1 {
            project.close_page(now);
        } else {
            project.focus_editor_by_id(target, now);
            project.close_split();
            if origin != target {
                project.focus_editor_by_id(origin, now);
            }
        }
        project.status.clear();
        self.reset_tab_view(window, cx);
    }

    /// Open an editor for each selected argument or buffer, up to the requested layout capacity.
    fn show_buffer_windows(
        &mut self,
        set: BufferSet,
        maximum: Option<usize>,
        _force: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tab = self.ex_tab_insertion();
        let project = self.projects.active_mut();
        let now = self.scroll_epoch.elapsed().as_secs_f64();
        let numbers = if set == BufferSet::Arguments {
            let files = project.arguments.files(project.active_editor_id()).to_vec();
            files
                .into_iter()
                .take(maximum.unwrap_or(usize::MAX))
                .map(|path| project.list_unloaded_file(path, None))
                .collect::<Vec<_>>()
        } else {
            project
                .session_buffer_entries()
                .into_iter()
                .filter(|(number, view)| {
                    (view.supports_shared_view() || view.terminal().is_some())
                        && (set != BufferSet::Loaded || project.session_buffer_loaded(*number))
                })
                .map(|(number, _)| number)
                .take(maximum.unwrap_or(usize::MAX))
                .collect()
        };
        let Some(first) = numbers.first().copied() else {
            self.set_status(
                i18n::text(if set == BufferSet::Arguments {
                    "vim_bracket.no_arguments"
                } else {
                    "vim_bracket.no_buffer"
                })
                .into(),
            );
            return;
        };
        if let Some(tab) = tab {
            if project.manager.is_none() {
                project.status = i18n::text("editor.tabpage_requires_project").into();
                return;
            }
            for (index, number) in numbers.into_iter().enumerate() {
                match project.open_page_source(
                    SplitDocument::SessionBuffer(number),
                    tab + index,
                    now,
                ) {
                    Ok(()) if set == BufferSet::Arguments => {
                        let pane = project.active_editor_id();
                        project.arguments.select(pane, index);
                    }
                    Ok(()) => {}
                    Err(error) => {
                        project.status = format!("{error:#}");
                        break;
                    }
                }
            }
            self.reset_tab_view(window, cx);
            return;
        }
        if let Err(error) = project.load_detached_buffer(first) {
            project.status = format!("{error:#}");
            return;
        }
        let original = project.active_editor_id();
        for pane in project
            .splits()
            .ids()
            .into_iter()
            .filter(|pane| *pane != original)
        {
            project.retain_hidden_editor(pane);
        }
        project.only_split();
        project.select_session_buffer(first, now);
        if set == BufferSet::Arguments {
            project.arguments.select(original, 0);
        }
        project.status.clear();
        self.reset_tab_view(window, cx);
        for (argument, number) in numbers.into_iter().enumerate().skip(1) {
            let before = self.projects.active().split_count();
            self.open_editor_split_at(
                true,
                false,
                SplitDocument::SessionBuffer(number),
                None,
                window,
                cx,
            );
            if self.projects.active().split_count() == before {
                break;
            }
            if set == BufferSet::Arguments {
                let project = self.projects.active_mut();
                let pane = project.active_editor_id();
                project.arguments.select(pane, argument);
            }
            self.equalize_buffer_windows();
        }
        self.projects.active_mut().focus_editor_by_id(original, now);
        self.reset_tab_view(window, cx);
    }

    /// Rebalance the current page after opening the next member of a buffer group.
    fn equalize_buffer_windows(&mut self) {
        let project = self.projects.active_mut();
        let panes = project.splits().ids();
        project.splits_mut().equalize(&panes);
    }

    fn invalid_buffer_operand(&mut self, argument: &str) {
        self.set_status(i18n::format(
            "editor.invalid_ex_argument",
            &[("argument", &argument)],
        ));
    }
}

/// Resolve the selected Session numbers before any buffer or label is removed.
fn selected_buffers(project: &Project, address: &str, selectors: &str) -> Option<Vec<u64>> {
    if address.is_empty() {
        if selectors.is_empty() {
            return Some(vec![project.editor.vim().doc.buffer_number()]);
        }
        let mut numbers = argument_words(selectors)
            .iter()
            .map(|word| resolve_buffer(project, word))
            .collect::<Option<Vec<_>>>()?;
        numbers.sort_unstable();
        numbers.dedup();
        return Some(numbers);
    }
    if !selectors.is_empty() {
        return None;
    }
    let listed = project.session_buffer_numbers();
    let current = project.editor.vim().doc.buffer_number();
    if address == "%" {
        return Some(listed);
    }
    let endpoint = |text: &str| match text.trim() {
        "." => Some(current),
        "$" => listed.last().copied(),
        word => word.parse().ok(),
    };
    if let Some((start, end)) = address.split_once([',', ';']) {
        let (start, end) = (endpoint(start)?, endpoint(end)?);
        (start <= end).then(|| {
            listed
                .into_iter()
                .filter(|number| (start..=end).contains(number))
                .collect()
        })
    } else {
        let number = endpoint(address)?;
        project
            .session_buffer(number)
            .is_some()
            .then(|| vec![number])
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::grid::workspace::native_test;
    use anyhow::{Result, ensure};
    use gpui::AppContext;
    use nanobug::{i18n, plugins::session::Options, vim::Action};
    use std::{fs, time::Duration};

    /// Drive the host actions through the normal Ex parser and workspace dispatcher.
    fn ex(workspace: &mut Workspace, text: &str, window: &mut Window, cx: &mut Context<Workspace>) {
        workspace.run_vim(
            |vim, register| vim.execute_ex_line(text, register),
            window,
            cx,
        );
        workspace.sync(window, cx);
    }

    /// Verify detached drafts and local argument lists through their native Ex entry points.
    #[test]
    #[ignore = "requires Windows DirectWrite and DirectX"]
    fn native_ex_retains_hidden_drafts_and_arranges_scoped_arguments() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().canonicalize()?;
        let readme = root.join("README.md");
        let other = root.join("other.txt");
        let third = root.join("third.txt");
        fs::write(&readme, "first\n")?;
        fs::write(&other, "second\nbody\n")?;
        fs::write(&third, "third\n")?;
        let options = Options::take(&mut vec!["--no-plugins".into()], temporary.path())?;
        native_test::run_async(
            root,
            options,
            |_| {},
            async move |handle, app| {
                let _temporary = temporary;
                app.update_window(handle.into(), |_, window, _| {
                    window.resize(gpui::size(gpui::px(1200.), gpui::px(880.)));
                })?;
                app.background_executor()
                    .timer(Duration::from_millis(60))
                    .await;
                app.update(|app| {
                    handle.update(app, |workspace, window, cx| -> Result<()> {
                        workspace.sync(window, cx);
                        ex(workspace, "badd +2 other.txt", window, cx);
                        let project = workspace.projects.active();
                        let number = project.number_for_path(&other).expect("listed buffer");
                        ensure!(!project.session_buffer_loaded(number));
                        ensure!(project.editor.vim().doc.path.as_ref() == Some(&readme));
                        ex(workspace, "balt other.txt", window, cx);
                        ensure!(
                            workspace.projects.active().editor.alternate_path()
                                == Some(other.as_path())
                        );
                        ex(workspace, &format!("buffer {number}"), window, cx);
                        ensure!(
                            workspace.projects.active().editor.vim().doc.cursor == "second\n".len()
                        );
                        workspace.run_vim(
                            |vim, _| {
                                vim.doc.replace(0..6, "edited", true);
                                Action::None
                            },
                            window,
                            cx,
                        );
                        ex(workspace, "hide edit README.md", window, cx);
                        let project = workspace.projects.active();
                        ensure!(
                            project.editor.buffers().iter().all(|buffer| buffer
                                .vim
                                .doc
                                .buffer_number()
                                != number)
                        );
                        ensure!(
                            project
                                .session_buffer(number)
                                .is_some_and(|buffer| buffer.vim.doc.modified())
                        );
                        ex(workspace, "qa", window, cx);
                        ensure!(
                            workspace.projects.active().status == i18n::text("ex.unsaved_buffers")
                        );
                        ex(workspace, "wall", window, cx);
                        ensure!(fs::read_to_string(&other)? == "edited\nbody\n");
                        ex(workspace, &format!("bunload {number}"), window, cx);
                        ensure!(!workspace.projects.active().session_buffer_loaded(number));
                        ex(workspace, "argglobal README.md other.txt", window, cx);
                        ex(workspace, "arglocal third.txt", window, cx);
                        let project = workspace.projects.active();
                        ensure!(project.arguments.files(project.active_editor_id()) == [third]);
                        ex(workspace, "argglobal", window, cx);
                        ex(workspace, "2all", window, cx);
                        let project = workspace.projects.active();
                        ensure!(project.split_count() == 2, "{}", project.status);
                        ensure!(project.page_editors().any(|(pane, editor)| {
                            editor.vim().doc.path.as_ref() == Some(&other)
                                && project.arguments.index(pane) == 1
                        }));
                        ex(workspace, &format!("{number}bwipeout"), window, cx);
                        let project = workspace.projects.active();
                        ensure!(project.split_count() == 2);
                        ensure!(project.session_buffer(number).is_none());
                        Ok(())
                    })
                })?
            },
        )
    }
}
