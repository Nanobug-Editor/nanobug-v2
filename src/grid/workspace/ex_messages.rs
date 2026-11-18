//! Apply Ex message modifiers around native command execution.

use super::Workspace;
use anyhow::Context as _;
use gpui::{Context, Window};
use nanobug::vim::{Action, ExMessageOutput, MessageModifier, RedirectMode};
use std::path::PathBuf;

impl Workspace {
    /// Resolve one redirection filename against the current Ex directory.
    pub(super) fn ex_redirect_file(&mut self, filename: &str, mode: RedirectMode) {
        let name = expand_redirect_path(filename);
        let path = if matches!(name.as_str(), "~") {
            dirs::home_dir().unwrap_or_else(|| PathBuf::from(&name))
        } else if let Some(relative) = name.strip_prefix("~/").or_else(|| name.strip_prefix("~\\"))
        {
            dirs::home_dir()
                .map(|home| home.join(relative))
                .unwrap_or_else(|| self.ex_current_dir().join(&name))
        } else {
            self.ex_current_dir().join(&name)
        };
        let result = self
            .input_mappings
            .begin_redirect_file(&path, mode)
            .with_context(|| format!("Cannot redirect Ex output to {}", path.display()));
        self.report_ex_result(result);
    }
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
        let output = project.editor.vim().displayed_ex_message();
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
                        if ignored {
                            vim.publish_styled_ex_message(output.clone());
                        } else {
                            vim.publish_ex_message(error, true);
                        }
                    } else if mode.is_some() && !vim.message.is_empty() && local {
                        vim.publish_styled_ex_message(output);
                    } else if filter && !vim.message.is_empty() {
                        let output = self
                            .input_mappings
                            .filter_styled_ex_output(vim.displayed_ex_message());
                        vim.publish_styled_ex_message(output);
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
        self.ex_styled_output_listing(
            ExMessageOutput {
                text: listing.to_owned(),
                spans: Vec::new(),
            },
            window,
            cx,
        );
    }

    /// Position retained spans in the read-only result after filtering its text.
    pub(super) fn ex_styled_output_listing(
        &mut self,
        output: ExMessageOutput,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let output = self.input_mappings.filter_styled_ex_output(output);
        if output.text.is_empty() {
            return;
        }
        self.input_mappings.record_ex_output(&output.text, true);
        if self.input_mappings.ex_silence().is_some() {
            return;
        }
        self.open_editor_split_at(
            true,
            false,
            super::projects::SplitDocument::ReadOnly(output),
            None,
            window,
            cx,
        );
    }

    /// Publish a native Ex status message through the same stream as a script message.
    pub(super) fn ex_output_message(&mut self, message: &str, remember: bool) {
        let message = self.input_mappings.filter_ex_output(message);
        if message.is_empty() {
            return;
        }
        self.input_mappings.record_ex_output(&message, true);
        if self.input_mappings.ex_silence().is_some() {
            return;
        }
        if remember {
            self.input_mappings.remember_ex_message(&message);
        }
        self.projects.active_mut().editor.edit(|vim, _| {
            vim.publish_ex_message(&message, false);
            Action::None
        });
    }
}

/// Expand environment variables and preserve spaces in the destination filename.
fn expand_redirect_path(filename: &str) -> String {
    let mut name = String::with_capacity(filename.len());
    let mut remaining = filename;
    while let Some(ch) = remaining.chars().next() {
        remaining = &remaining[ch.len_utf8()..];
        match ch {
            '\\' if remaining.starts_with(' ') => {
                remaining = &remaining[1..];
                name.push(' ');
            }
            '$' => {
                let variable = remaining
                    .strip_prefix('{')
                    .and_then(|tail| tail.split_once('}'))
                    .or_else(|| {
                        let end = remaining
                            .find(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
                            .unwrap_or(remaining.len());
                        (end > 0).then(|| remaining.split_at(end))
                    });
                if let Some((variable, tail)) = variable
                    && let Ok(value) = std::env::var(variable)
                {
                    name.push_str(&value);
                    remaining = tail;
                } else {
                    name.push(ch);
                }
            }
            _ => name.push(ch),
        }
    }
    name
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

    /// Exercise the native Ex pipeline with a trailing :echohl reset.
    #[test]
    #[ignore = "requires Windows DirectWrite and DirectX"]
    fn native_ex_echohl_keeps_emitted_style_after_pipeline_reset() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().join("project");
        fs::create_dir(&root)?;
        fs::write(root.join("README.md"), "base\n")?;
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
                            workspace
                                .projects
                                .active()
                                .editor
                                .vim()
                                .displayed_ex_message()
                        };
                        run("highlight EchoTest guifg=#123456 gui=bold");
                        let warning = run("echohl EchoTest | echo '提示' | echohl None");
                        ensure!(warning.text == "提示");
                        ensure!(warning.spans[0].range == (0..6));
                        ensure!(warning.spans[0].style.foreground == Some(0x123456));
                        ensure!(warning.spans[0].style.attributes.bold);
                        let ordinary = run("echo 'ordinary'");
                        ensure!(ordinary.text == "ordinary");
                        ensure!(ordinary.spans.is_empty());
                        Ok(())
                    })
                })?
            },
        )
    }

    /// Check that workspace-relative files capture quiet output and native listings.
    #[test]
    #[ignore = "requires Windows DirectWrite and DirectX"]
    fn native_ex_redirection_captures_silent_output_in_project_file() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().join("project");
        fs::create_dir(&root)?;
        fs::write(root.join("README.md"), "base\n")?;
        let capture = root.join("capture output.txt");
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
                        let initial_splits = workspace.projects.active().split_count();
                        let mut run = |command: &str| {
                            workspace.run_vim(
                                |vim, register| vim.execute_ex_line(command, register),
                                window,
                                cx,
                            );
                            workspace.projects.active().editor.vim().message.clone()
                        };
                        run("redir > capture output.txt");
                        ensure!(run("silent echo 'hidden'").is_empty());
                        run("silent registers");
                        run("silent! echoerr 'captured error'");
                        run("redir END");
                        ensure!(workspace.projects.active().split_count() == initial_splits);
                        let contents = fs::read_to_string(&capture)?;
                        ensure!(contents.starts_with("\nhidden\nType Name Content"));
                        ensure!(contents.contains("captured error"));
                        Ok(())
                    })
                })?
            },
        )
    }

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
