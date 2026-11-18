//! Resolve Ex sign commands against the current Session's shared buffers.

use super::Workspace;
use crate::grid::windows::editor::Buffer;
use anyhow::{Context as _, ensure};
use gpui::{Context, Window};
use nanobug::{
    document::{Document, SignGroup, SignPlacement},
    i18n,
    vim::{Action, SignCommand, SignDefinition, SignId, SignTarget},
};
use std::{fmt::Write as _, path::Path};

impl Workspace {
    /// Dispatch a parsed sign command, using the ordinary Ex output stream for listings.
    pub(super) fn execute_sign(
        &mut self,
        command: SignCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.projects.active_project().is_none() {
            return;
        }
        match self.apply_sign_command(command) {
            Ok(output) => {
                self.set_status(String::new());
                if let Some(text) = output.filter(|text| !text.is_empty()) {
                    self.ex_output_listing(&text, window, cx);
                }
            }
            Err(error) => self.set_status(error.to_string()),
        }
        self.sync(window, cx);
    }

    /// Apply a sign definition, placement, removal or jump, or produce the requested listing.
    fn apply_sign_command(&mut self, command: SignCommand) -> anyhow::Result<Option<String>> {
        match command {
            SignCommand::Define { name, definition } => self
                .input_mappings
                .define_sign(&name, definition)
                .map_err(anyhow::Error::msg)?,
            SignCommand::Undefine { name } => {
                ensure!(
                    self.input_mappings.undefine_sign(Some(&name)),
                    "{}",
                    unknown_sign(&name)
                );
            }
            SignCommand::List { name } => {
                let definitions = if let Some(name) = name {
                    vec![
                        self.input_mappings
                            .sign_definition(&name)
                            .with_context(|| unknown_sign(&name))?,
                    ]
                } else {
                    self.input_mappings.sign_definitions()
                };
                let mut output = String::new();
                for definition in definitions {
                    describe_sign(&mut output, &definition);
                }
                return Ok(Some(output));
            }
            SignCommand::Place {
                id,
                name,
                line,
                group,
                priority,
                target,
            } => {
                let number = self.sign_buffer_number(target.as_ref())?;
                let project = self.projects.active();
                ensure!(
                    project.session_buffer_loaded(number),
                    "{}",
                    i18n::text("sign.unloaded_buffer")
                );
                let buffer = project
                    .session_buffer(number)
                    .context(i18n::text("vim_bracket.no_buffer"))?;
                ensure!(
                    buffer.terminal().is_none(),
                    "{}",
                    i18n::text("sign.text_buffer_required")
                );
                let doc = &buffer.vim.doc;
                let definition = self
                    .input_mappings
                    .sign_definition(&name)
                    .with_context(|| unknown_sign(&name))?;
                let group = group.unwrap_or_default();
                let row = match line {
                    Some(line) => line.checked_sub(1),
                    None => doc
                        .signs()
                        .into_iter()
                        .find(|sign| sign.id == id && sign.group == group)
                        .map(|sign| sign.row),
                }
                .with_context(|| invalid_sign_id(id))?;
                let row = row.min(buffer.vim.line_count().saturating_sub(1));
                let priority = priority
                    .filter(|&value| value != -1)
                    .unwrap_or(definition.priority);
                ensure!(
                    doc.place_sign(SignPlacement {
                        id,
                        group,
                        name: definition.name,
                        row,
                        priority
                    }),
                    "{}",
                    invalid_sign_id(id)
                );
            }
            SignCommand::ListPlaced { group, target } => {
                return Ok(Some(self.list_placed_signs(&group, target.as_ref())?));
            }
            SignCommand::Unplace { id, group, target } => {
                self.unplace_signs(id, &group, target.as_ref())?;
            }
            SignCommand::Jump { id, group, target } => {
                self.jump_to_sign(id, group.as_deref().unwrap_or_default(), target.as_ref())?;
            }
        }
        Ok(None)
    }

    /// Resolve a stable buffer number without loading a new file. `file=` takes the literal tail.
    fn sign_buffer_number(&self, target: Option<&SignTarget>) -> anyhow::Result<u64> {
        let project = self.projects.active();
        match target {
            None => Ok(project.editor.vim().doc.buffer_number()),
            Some(SignTarget::Buffer(number)) => project
                .session_buffer(*number)
                .map(|_| *number)
                .context(i18n::text("vim_bracket.no_buffer")),
            Some(SignTarget::File(name)) => {
                let path = self.ex_current_dir().join(name);
                let path = path.canonicalize().unwrap_or(path);
                project
                    .number_for_path(&path)
                    .with_context(|| i18n::format("sign.invalid_buffer_name", &[("name", name)]))
            }
        }
    }

    /// Get each matching Session buffer once, including retained buffers with no open label.
    fn sign_buffers(&self, target: Option<&SignTarget>) -> anyhow::Result<Vec<&Buffer>> {
        let project = self.projects.active();
        if let Some(target) = target {
            let number = self.sign_buffer_number(Some(target))?;
            Ok(project.session_buffer(number).into_iter().collect())
        } else {
            Ok(project.session_buffer_entries().into_values().collect())
        }
    }

    /// Render placements ordered by buffer, line and sign priority.
    fn list_placed_signs(
        &self,
        group: &SignGroup,
        target: Option<&SignTarget>,
    ) -> anyhow::Result<String> {
        let mut output = format!("{}\n", i18n::text("sign.placements"));
        for buffer in self.sign_buffers(target)? {
            let doc = &buffer.vim.doc;
            let mut signs = doc
                .signs()
                .into_iter()
                .filter(|sign| group.matches(&sign.group))
                .peekable();
            if signs.peek().is_none() {
                continue;
            }
            let filename = doc.path.as_deref().map(Path::display);
            let label = filename.map_or_else(
                || format!("[buffer {}]", doc.buffer_number()),
                |path| path.to_string(),
            );
            let _ = writeln!(output, "{} {label}", i18n::text("sign.buffer"));
            for sign in signs {
                let name = if self.input_mappings.sign_definition(&sign.name).is_some() {
                    sign.name.as_str()
                } else {
                    i18n::text("sign.deleted")
                };
                let _ = write!(output, "    line={}  id={}", sign.row + 1, sign.id);
                if !sign.group.is_empty() {
                    let _ = write!(output, "  group={}", sign.group);
                }
                let _ = writeln!(output, "  name={name}  priority={}", sign.priority);
            }
        }
        Ok(output)
    }

    /// Remove by ID, by group, or remove the highest-ranked qualifying sign under the cursor.
    fn unplace_signs(
        &self,
        id: SignId,
        group: &SignGroup,
        target: Option<&SignTarget>,
    ) -> anyhow::Result<()> {
        let id = match id {
            SignId::One(id) => Some(id),
            SignId::All => None,
            SignId::Cursor => {
                let doc = &self.projects.active().editor.vim().doc;
                let row = doc.location(doc.cursor).0;
                let sign = doc
                    .signs()
                    .into_iter()
                    .find(|sign| sign.row == row && group.matches(&sign.group))
                    .context(i18n::text("sign.no_cursor_sign"))?;
                let exact_group = if sign.group.is_empty() {
                    SignGroup::Global
                } else {
                    SignGroup::Named(sign.group)
                };
                doc.remove_signs(&exact_group, Some(sign.id));
                return Ok(());
            }
        };
        for buffer in self.sign_buffers(target)? {
            buffer.vim.doc.remove_signs(group, id);
        }
        Ok(())
    }

    /// Visit a sign through the originating editor's own buffer and jump history.
    fn jump_to_sign(
        &mut self,
        id: u64,
        group: &str,
        target: Option<&SignTarget>,
    ) -> anyhow::Result<()> {
        let number = self.sign_buffer_number(target)?;
        let now = self.scroll_epoch.elapsed().as_secs_f64();
        let project = self.projects.active_mut();
        project.refresh_shared_documents();
        let target = project
            .session_buffer(number)
            .context(i18n::text("vim_bracket.no_buffer"))?;
        let doc = &target.vim.doc;
        let sign = doc
            .signs()
            .into_iter()
            .find(|sign| sign.id == id && sign.group == group)
            .with_context(|| invalid_sign_id(id))?;
        let offset = sign_offset(doc, sign.row);
        let current = &project.editor.vim().doc;
        if current.buffer_number() != number || current.cursor != offset {
            project.editor.edit(|vim, _| {
                vim.record_jump();
                Action::None
            });
        }
        ensure!(
            project.select_session_buffer(number, now),
            "{}",
            i18n::text("vim_bracket.no_buffer")
        );
        project.editor.edit(|vim, _| {
            vim.place_jump(sign_offset(&vim.doc, sign.row));
            Action::None
        });
        self.finish_file_jump();
        Ok(())
    }
}

/// Resolve an anchored row to its first nonblank byte, including whitespace-only lines.
fn sign_offset(doc: &Document, row: usize) -> usize {
    let row = row.min(doc.starts.len() - 1);
    let line = doc.line(row);
    doc.starts[row] + line.len() - line.trim_start_matches([' ', '\t']).len()
}

/// Format one sign definition with the same field names accepted by the Ex command.
fn describe_sign(output: &mut String, definition: &SignDefinition) {
    let _ = write!(output, "sign {}", definition.name);
    if let Some(icon) = &definition.icon {
        let _ = write!(
            output,
            " icon={} ({})",
            icon.display(),
            i18n::text("sign.icon_not_rendered")
        );
    }
    if let Some(text) = &definition.text {
        let _ = write!(output, " text={text}");
    }
    let _ = write!(output, " priority={}", definition.priority);
    for (key, group) in [
        ("linehl", &definition.linehl),
        ("texthl", &definition.texthl),
        ("culhl", &definition.culhl),
        ("numhl", &definition.numhl),
    ] {
        if let Some(group) = group {
            let _ = write!(output, " {key}={group}");
        }
    }
    output.push('\n');
}

/// Return the common message for an undefined sign name.
fn unknown_sign(name: &str) -> String {
    i18n::format("sign.unknown_sign", &[("name", &name)])
}

/// Return the common message for an invalid placement ID.
fn invalid_sign_id(id: u64) -> String {
    i18n::format("sign.invalid_id", &[("id", &id)])
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::grid::workspace::native_test;
    use gpui::AppContext;
    use nanobug::plugins::session::Options;
    use std::{fs, time::Duration};

    /// Exercise native Ex dispatch, Session buffer targeting, output and view sharing together.
    #[test]
    #[ignore = "requires Windows DirectWrite and DirectX"]
    fn native_ex_signs_share_placements_and_list_literal_file_targets() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("project");
        fs::create_dir(&root)?;
        fs::write(root.join("README.md"), "project\n")?;
        fs::write(root.join("name with space.txt"), "head\n  middle\nend\n")?;
        let options = Options::take(&mut vec!["--no-plugins".into()], temp.path())?;
        native_test::run_async(
            root,
            options,
            |_| {},
            async move |handle, app| {
                let _temp = temp;
                app.update_window(handle.into(), |_, window, _| {
                    window.resize(gpui::size(gpui::px(1200.), gpui::px(880.)));
                })?;
                app.background_executor()
                    .timer(Duration::from_millis(60))
                    .await;
                app.update(|app| {
                    handle.update(app, |workspace, window, cx| -> anyhow::Result<()> {
                        workspace.sync(window, cx);
                        let mut ex =
                            |workspace: &mut Workspace, command: &str| -> anyhow::Result<String> {
                                workspace.activate_input_mappings();
                                workspace.run_vim(
                                    |vim, register| vim.execute_ex_line(command, register),
                                    window,
                                    cx,
                                );
                                let project = workspace.projects.active();
                                ensure!(project.status.is_empty(), "{command}: {}", project.status);
                                let vim = project.editor.vim();
                                ensure!(!vim.ex_failed(), "{command}: {}", vim.message);
                                Ok(vim.message.clone())
                            };
                        ex(workspace, r"edit name\ with\ space.txt")?;
                        let number = workspace.projects.active().editor.vim().doc.buffer_number();
                        ex(
                            workspace,
                            "sign define Break text=!! texthl=WarningMsg priority=20",
                        )?;
                        ex(workspace, "sign define Pin text=+")?;
                        ex(
                            workspace,
                            "sign place 7 line=2 name=Break file=name with space.txt",
                        )?;
                        ex(
                            workspace,
                            &format!("sign place 7 group=review line=2 name=Pin buffer={number}"),
                        )?;
                        ex(workspace, "vsplit")?;
                        let project = workspace.projects.active();
                        ensure!(project.split_count() == 2);
                        for (_, editor) in project.editors() {
                            let doc = &editor.vim().doc;
                            ensure!(doc.buffer_number() == number);
                            let signs = doc.signs();
                            ensure!(
                                signs.len() == 2
                                    && signs[0].name == "Break"
                                    && signs[1].group == "review"
                            );
                        }
                        ex(workspace, "sign jump 7 group=review")?;
                        let doc = &workspace.projects.active().editor.vim().doc;
                        ensure!(doc.location(doc.cursor) == (1, 2));
                        ex(workspace, "redir => g:placements")?;
                        ex(workspace, "silent sign list Break")?;
                        ex(
                            workspace,
                            &format!("silent sign place group=* buffer={number}"),
                        )?;
                        ex(workspace, "redir END")?;
                        let output = ex(workspace, "echo g:placements")?;
                        ensure!(
                            output.contains("sign Break text=!! priority=20 texthl=WarningMsg"),
                            "{output}"
                        );
                        ensure!(
                            output.contains("line=2  id=7  group=review  name=Pin"),
                            "{output}"
                        );
                        ex(
                            workspace,
                            &format!("sign unplace 7 group=review buffer={number}"),
                        )?;
                        ex(workspace, "sign undefine Break")?;
                        let deleted = workspace.list_placed_signs(&SignGroup::All, None)?;
                        ensure!(deleted.contains(i18n::text("sign.deleted")));
                        ex(workspace, "sign unplace * group=*")?;
                        ensure!(
                            workspace
                                .projects
                                .active()
                                .editors()
                                .all(|(_, editor)| editor.vim().doc.signs().is_empty())
                        );
                        Ok(())
                    })
                })?
            },
        )
    }
}
