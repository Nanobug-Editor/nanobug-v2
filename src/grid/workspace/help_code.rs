//! Choose a Help topic or file and open its document in a read-only editor split.

use super::{Context, Workspace, projects::SplitDocument};
use gpui::Window;
use nanobug::{document::Document, i18n};

/// Built-in topics and executable examples available without a Neovim installation.
const HELP: &str = include_str!("../../../assets/vim/help.txt");

impl Workspace {
    /// Open a built-in `topic` or a user-selected file in a Help split.
    pub(super) fn open_vim_help(
        &mut self,
        topic: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.projects.active_project().is_none() {
            return;
        }
        let path = self.vim_file_path(topic);
        let doc = if !topic.is_empty() && path.is_file() {
            match Document::open(&path) {
                Ok(doc) => doc,
                Err(error) => {
                    self.set_status(format!("{error:#}"));
                    return;
                }
            }
        } else {
            let mut doc = Document::new(HELP);
            if !topic.is_empty() {
                let marker = format!("*{topic}*");
                if let Some(offset) = HELP.find(&marker) {
                    doc.cursor = offset;
                } else if let Some(indexed) = self.lookup_help_tag(topic) {
                    doc = indexed;
                } else {
                    self.set_status(i18n::format(
                        "vim_g.help_unknown_topic",
                        &[("topic", &topic)],
                    ));
                    return;
                }
            }
            doc
        };
        self.open_editor_split_at(true, true, SplitDocument::Help(doc), None, window, cx);
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::grid::workspace::native_test;
    use anyhow::{Result, ensure};
    use gpui::{AppContext, EntityInputHandler, KeyDownEvent, Keystroke, px, size};
    use nanobug::plugins::session::Options;
    use std::{fs, time::Duration};

    /// Invoke an Ex-defined operator, then run the multiline example in a Help split.
    #[test]
    #[ignore = "opens a native Windows workspace"]
    fn native_vim_operator_and_help_code_run_from_keyboard() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("project");
        fs::create_dir(&root)?;
        let source = root.join("README.md");
        fs::write(&source, "original\n")?;
        let options = Options::take(&mut vec!["--no-plugins".into()], temp.path())?;
        native_test::run_async(
            root,
            options,
            |_| {},
            async move |handle, app| {
                app.update_window(handle.into(), |_, window, _| {
                    window.resize(size(px(920.), px(580.)));
                })?;
                app.background_executor()
                    .timer(Duration::from_millis(60))
                    .await;
                app.update(|app| {
                    handle.update(app, |workspace, native, cx| -> Result<()> {
                        let press = |workspace: &mut Workspace,
                                     key: &'static str,
                                     native: &mut Window,
                                     cx: &mut Context<Workspace>|
                         -> Result<()> {
                            workspace.key_down(
                                &KeyDownEvent {
                                    keystroke: Keystroke::parse(key)?,
                                    is_held: false,
                                    prefer_character_input: false,
                                },
                                native,
                                cx,
                            );
                            Ok(())
                        };
                        workspace.open(source, native, cx);
                        workspace.sync(native, cx);
                        for command in [
                            "function! ShowKind(kind) | echo a:kind | endfunction",
                            "set operatorfunc=ShowKind",
                        ] {
                            press(workspace, "shift-;", native, cx)?;
                            workspace.replace_text_in_range(None, command, native, cx);
                            press(workspace, "enter", native, cx)?;
                        }
                        for key in ["g", "shift-2", "shift-2"] {
                            press(workspace, key, native, cx)?;
                        }
                        ensure!(workspace.projects.active().editor.vim().message == "line");
                        press(workspace, "shift-;", native, cx)?;
                        workspace.replace_text_in_range(None, "help g==", native, cx);
                        press(workspace, "enter", native, cx)?;
                        let project = workspace.projects.active_mut();
                        ensure!(
                            project.split_count() == 2,
                            "could not open Help split: {}",
                            project.status
                        );
                        ensure!(
                            project.editor.vim().is_help() && project.editor.vim().is_read_only()
                        );
                        let source = project
                            .editor
                            .vim()
                            .doc
                            .text
                            .find("echo join(values")
                            .expect("multiline Vim example");
                        project.editor.jump_to(source);
                        for key in ["g", "=", "="] {
                            press(workspace, key, native, cx)?;
                        }
                        ensure!(workspace.projects.active().editor.vim().message == "2, 4, 8");
                        Ok(())
                    })
                })?
            },
        )
    }
}
