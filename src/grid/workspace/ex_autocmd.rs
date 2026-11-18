//! Dispatch genuine editor events to user-defined Vim autocommands.

use super::{Workspace, editor_splits::PaneId};
use gpui::{Context, Window};
use nanobug::vim::ScriptEvent;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

/// Identity and name of the buffer that owns an editor event.
#[derive(Clone)]
struct Target {
    project: u64,
    buffer: u64,
    pane: PaneId,
    path: Option<PathBuf>,
}

#[cfg(all(test, windows))]
mod tests {
    use crate::grid::workspace::native_test;
    use anyhow::{Result, ensure};
    use nanobug::{plugins::session::Options, vim::Action};
    use std::fs;

    /// Exercise real buffer transitions and pre/post write callbacks through native Ex dispatch.
    #[test]
    #[ignore = "requires Windows DirectWrite and DirectX"]
    fn native_ex_autocmd_observes_reads_and_writes() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("project");
        fs::create_dir(&root)?;
        fs::write(root.join("README.md"), "base\n")?;
        fs::write(root.join("target.txt"), "before\n")?;
        let file = root.join("target.txt");
        let options = Options::take(&mut vec!["--no-plugins".into()], temp.path())?;
        native_test::run_async(
            root,
            options,
            |_| {},
            async move |handle, app| {
                let _temp = temp;
                app.update(|app| {
                    handle.update(app, |workspace, window, cx| -> Result<()> {
                        for line in [
                            "augroup TestEvents",
                            "autocmd!",
                            "autocmd BufReadPost target.txt ++once let g:read_event = 1",
                            "autocmd BufEnter target.txt let g:entered = 1",
                            "autocmd BufWritePre target.txt let g:before = 1",
                            "autocmd BufWritePost target.txt let g:after = 1",
                            "augroup END",
                            "edit target.txt",
                            "write",
                        ] {
                            workspace.activate_input_mappings();
                            workspace.run_vim(
                                |vim, register| vim.execute_ex_line(line, register),
                                window,
                                cx,
                            );
                            workspace.sync(window, cx);
                        }
                        workspace.run_vim(
                            |vim, register| {
                                vim.execute_ex_line(
                                    "echo g:read_event + g:entered + g:before + g:after",
                                    register,
                                );
                                Action::None
                            },
                            window,
                            cx,
                        );
                        ensure!(
                            workspace.projects.active().editor.vim().message == "4",
                            "{}",
                            workspace.projects.active().editor.vim().message
                        );
                        ensure!(fs::read_to_string(&file)? == "before\n");
                        workspace.run_vim(
                            |vim, register| vim.execute_ex_line("let g:entered = 0", register),
                            window,
                            cx,
                        );
                        workspace.run_vim(
                            |vim, register| {
                                vim.execute_ex_line("noautocmd doautocmd BufEnter", register)
                            },
                            window,
                            cx,
                        );
                        workspace.run_vim(
                            |vim, register| vim.execute_ex_line("echo g:entered", register),
                            window,
                            cx,
                        );
                        ensure!(workspace.projects.active().editor.vim().message == "0");
                        Ok(())
                    })
                })?
            },
        )
    }
}

/// The currently visible buffer and restrictions inherited from nested autocommands.
#[derive(Default)]
pub(super) struct State {
    active: Option<Target>,
    read: HashSet<(u64, u64)>,
    suppressed: usize,
    nesting: Vec<bool>,
}

impl Workspace {
    /// Observe buffer transitions through every native route that refreshes the workspace.
    pub(super) fn observe_script_buffer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.script_event_target();
        let changed = match (&self.script_events.active, &current) {
            (Some(previous), Some(current)) => {
                (previous.project, previous.buffer) != (current.project, current.buffer)
            }
            (None, None) => false,
            _ => true,
        };
        if !changed {
            return;
        }
        self.script_events.active = current.clone();
        let Some(target) = current else { return };
        if let Some(path) = target.path.as_ref()
            && self
                .script_events
                .read
                .insert((target.project, target.buffer))
        {
            let event = if path.is_file() {
                ScriptEvent::BufReadPost
            } else {
                ScriptEvent::BufNewFile
            };
            self.dispatch_script_event(event, &target, None, None, window, cx);
        }
        if self.script_event_target().is_some_and(|current| {
            current.project == target.project && current.buffer == target.buffer
        }) {
            self.dispatch_script_event(ScriptEvent::BufEnter, &target, None, None, window, cx);
        }
    }

    /// Execute one command while suppressing both automatic and explicit event dispatch.
    pub(super) fn execute_no_autocmd(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.script_events.suppressed += 1;
        self.execute_user_ex(text, None, 1, window, cx);
        self.observe_script_buffer(window, cx);
        self.script_events.suppressed -= 1;
    }

    /// Trigger an event for the current buffer or each loaded buffer in the active Session.
    pub(super) fn execute_explicit_autocmd(
        &mut self,
        event: &str,
        pattern: Option<&str>,
        group: Option<&str>,
        all: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(event) = ScriptEvent::parse(event) else {
            return;
        };
        let Some(initial) = self.script_event_target() else {
            return;
        };
        if !all {
            self.dispatch_script_event(event, &initial, pattern, group, window, cx);
            return;
        }
        let numbers = self
            .projects
            .active()
            .editors()
            .flat_map(|(_, editor)| editor.buffers())
            .map(|buffer| buffer.vim.doc.buffer_number())
            .collect::<std::collections::BTreeSet<_>>();
        let now = self.scroll_epoch.elapsed().as_secs_f64();
        let mut original_error = None;
        for number in numbers {
            let Some(project) = self
                .projects
                .active_project_mut()
                .filter(|project| project.plugin_scope == initial.project)
            else {
                break;
            };
            let owner = project.editors().find_map(|(pane, editor)| {
                editor
                    .buffers()
                    .iter()
                    .find(|buffer| buffer.vim.doc.buffer_number() == number)
                    .map(|buffer| (pane, buffer.id))
            });
            if let Some((pane, id)) = owner {
                project.focus_editor_by_id(pane, now);
                project.editor.select_buffer(id, now);
            } else {
                continue;
            }
            let Some(target) = self.script_event_target() else {
                break;
            };
            // This temporary selection belongs to :doautoall, not a separate user-visible BufEnter.
            self.script_events.active = Some(target.clone());
            if !self.dispatch_script_event(event, &target, pattern, group, window, cx) {
                original_error = self
                    .projects
                    .active_project()
                    .map(|project| project.status.clone());
                break;
            }
        }
        if let Some(project) = self
            .projects
            .active_project_mut()
            .filter(|project| project.plugin_scope == initial.project)
        {
            project.focus_editor_by_id(initial.pane, now);
            project.select_session_buffer(initial.buffer, now);
            self.script_events.active = self.script_event_target();
        }
        if let Some(error) = original_error {
            self.set_status(error);
        }
        self.sync(window, cx);
    }

    /// Execute the corresponding save event, returning false if a handler failed or changed target.
    pub(super) fn script_write_event(
        &mut self,
        event: ScriptEvent,
        path: Option<&Path>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(mut target) = self.script_event_target() else {
            return false;
        };
        if let Some(path) = path {
            target.path = Some(path.to_owned());
        }
        self.dispatch_script_event(event, &target, None, None, window, cx)
            && self.script_event_target().is_some_and(|current| {
                (current.project, current.buffer) == (target.project, target.buffer)
            })
    }

    /// Snapshot the active buffer so callbacks cannot accidentally use a replacement's identity.
    fn script_event_target(&self) -> Option<Target> {
        let project = self.projects.active_project()?;
        let buffer = &project.editor.vim().doc;
        Some(Target {
            project: project.plugin_scope,
            pane: project.active_editor_id(),
            buffer: buffer.buffer_number(),
            path: buffer.path.clone(),
        })
    }

    /// Run matching callbacks in registration order with Vim's default recursion suppression.
    fn dispatch_script_event(
        &mut self,
        event: ScriptEvent,
        target: &Target,
        pattern: Option<&str>,
        group: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.script_events.suppressed != 0
            || self.script_events.nesting.last() == Some(&false)
            || self.script_events.nesting.len() >= 10
        {
            return true;
        }
        let callbacks = self.input_mappings.script_autocmds(
            event,
            group,
            target.buffer,
            target.path.as_deref(),
            pattern,
        );
        if !callbacks.is_empty() && self.input_mappings.ex_verbose() >= 8 {
            self.input_mappings
                .trace_ex(8, &format!("Executing {} autocommands", event.name()));
        }
        for callback in callbacks {
            if self.projects.active_project().is_none() || self.modal_open() {
                return false;
            }
            self.set_status(String::new());
            self.script_events.nesting.push(callback.nested);
            if self.input_mappings.ex_verbose() >= 9 {
                self.input_mappings
                    .trace_ex(9, &format!("autocommand {}", callback.text));
            }
            self.execute_user_ex(
                &callback.text,
                callback.source.as_deref(),
                callback.source_line,
                window,
                cx,
            );
            self.script_events.nesting.pop();
            if self
                .projects
                .active_project()
                .is_none_or(|project| !project.status.is_empty())
            {
                return false;
            }
        }
        true
    }
}
