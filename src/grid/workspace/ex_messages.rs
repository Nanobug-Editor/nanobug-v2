//! Apply Ex message modifiers around native command execution.

use super::Workspace;
use gpui::{Context, Window};
use nanobug::vim::{Action, MessageModifier};

impl Workspace {
    /// Run an Ex command while preserving the containing output policy.
    pub(super) fn execute_message_modified(
        &mut self,
        modifier: MessageModifier,
        command: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.projects.active_project() else {
            return;
        };
        let original = (
            project.plugin_scope,
            project.active_editor_id(),
            project.editor.vim().doc.opening,
        );
        let output = project.editor.vim().message.clone();
        let previous_status = project.status.clone();
        self.input_mappings
            .begin_ex_message_modifier(modifier.clone());
        self.set_status(String::new());
        self.activate_input_mappings();
        let edit = self
            .projects
            .active_mut()
            .editor
            .edit(|vim, register| vim.execute_ex_line(command, register));
        let vim = self.projects.active().editor.vim();
        let mut error = vim.ex_failed().then(|| vim.message.clone());
        let local = matches!(edit.action, Action::None);
        if error.is_none() {
            self.apply_edit(edit, window, cx);
            if !local
                && let Some(project) = self.projects.active_project()
                && project.editor.vim().ex_failed()
            {
                error = Some(project.editor.vim().message.clone());
            }
            if !local
                && let Some(project) = self.projects.active_project()
                && !project.status.is_empty()
                && !ordinary_status(command)
                && project.status != nanobug::i18n::text("editor.filter_running")
            {
                error = Some(project.status.clone());
            }
        }
        let mode = self.input_mappings.ex_silence();
        let filter = matches!(modifier, MessageModifier::Filter { .. });
        let status = self
            .projects
            .active_project()
            .map(|project| project.status.clone());
        if let Some(project) = self.projects.active_project_mut() {
            let current = (
                project.plugin_scope,
                project.active_editor_id(),
                project.editor.vim().doc.opening,
            );
            if let Some(error) = error.as_deref() {
                self.input_mappings.remember_ex_error(error);
            }
            if current == original {
                project.editor.edit(|vim, _| {
                    if let Some(error) = error.as_deref() {
                        let ignored = mode == Some(true);
                        vim.publish_ex_message(
                            ignored.then_some(output.as_str()).unwrap_or(error),
                            !ignored,
                        );
                    } else if mode.is_some() && !vim.message.is_empty() && local {
                        vim.publish_ex_message(&output, false);
                    } else if filter && !vim.message.is_empty() {
                        let text = self.input_mappings.filter_ex_output(&vim.message);
                        vim.publish_ex_message(&text, false);
                    }
                    Action::None
                });
            }
            if mode.is_some() && (error.is_none() || mode == Some(true)) {
                project.status = previous_status;
            } else if filter
                && error.is_none()
                && let Some(status) = status
            {
                project.status = self.input_mappings.filter_ex_output(&status);
            }
        }
        self.input_mappings.end_ex_message_modifier();
        // A normal silent command still presents and records errors.
        if mode == Some(false)
            && let Some(error) = error
        {
            self.input_mappings.remember_ex_message(&error);
        }
    }

    /// Present retained command output through a listing without changing the focused pane when quiet.
    pub(super) fn ex_output_listing(
        &mut self,
        listing: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.input_mappings.ex_silence().is_some() {
            return;
        }
        let listing = self.input_mappings.filter_ex_output(listing);
        if listing.is_empty() {
            return;
        }
        self.open_editor_split_at(
            true,
            false,
            super::projects::SplitDocument::ReadOnly(nanobug::document::Document::new(&listing)),
            None,
            window,
            cx,
        );
    }
}

/// Commands whose successful native status resembles an error in the shared footer channel.
fn ordinary_status(command: &str) -> bool {
    let name = command.split_whitespace().next().unwrap_or_default();
    name.len() >= 2 && "pwd".starts_with(name)
}

#[cfg(all(test, windows))]
mod tests {
    use crate::grid::workspace::native_test;
    use anyhow::{Result, ensure};
    use nanobug::plugins::session::Options;
    use std::fs;

    /// Run typed modifiers and verify both display and the script's failure continuation.
    #[test]
    #[ignore = "requires Windows DirectWrite and DirectX"]
    fn native_ex_message_modifiers_scope_output_and_errors() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().join("project");
        fs::create_dir(&root)?;
        fs::write(root.join("README.md"), "base\n")?;
        fs::write(
            root.join("silent.vim"),
            "silent! echoerr 'muted error'\nlet g:last = v:errmsg\nlet g:continued = 1\n",
        )?;
        let options = Options::take(&mut vec!["--no-plugins".into()], temporary.path())?;
        native_test::run_async(
            root,
            options,
            |_| {},
            async move |handle, app| {
                let _temporary = temporary;
                app.update(|app| {
                    handle.update(app, |workspace, window, cx| -> Result<()> {
                        workspace.activate_input_mappings();
                        let mut run = |command: &str| {
                            workspace.run_vim(
                                |vim, register| vim.execute_ex_line(command, register),
                                window,
                                cx,
                            );
                            workspace.projects.active().editor.vim().message.clone()
                        };
                        ensure!(run("silent echo 'hidden'").is_empty());
                        ensure!(run("silent unsilent echo 'visible'") == "visible");
                        ensure!(run("filter /mist/ echo 'miss'").is_empty());
                        ensure!(run("filter /miss/ echo 'miss'") == "miss");
                        run("source silent.vim");
                        ensure!(run("echo g:last").contains("muted error"));
                        ensure!(run("echo g:continued") == "1");
                        Ok(())
                    })
                })?
            },
        )
    }
}
