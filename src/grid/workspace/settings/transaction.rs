//! Coordinate page-sized writes and their inverses across UI and plugin-session owners.

use super::Workspace;
use crate::grid::{
    windows::settings::{Action, Changes, Page, Settings},
    workspace::editor_close::CloseTarget,
};
use gpui::{Context, Window};
use nanobug::i18n;
use std::collections::VecDeque;

/// A button action owns the pending pages until the native or background owner acknowledges them.
struct Transaction {
    action: Action,
    locale: i18n::Locale,
    pending: VecDeque<(Page, Changes)>,
    undo: Vec<(Page, Changes)>,
    failures: Vec<String>,
}

impl Transaction {
    /// Commit the applied baseline only after success. On the first failure, replace the
    /// remaining work with the inverse of each acknowledged write, in reverse order.
    fn record(
        &mut self,
        form: &mut Settings,
        page: Page,
        changes: Changes,
        result: anyhow::Result<()>,
    ) {
        match result {
            Ok(()) => {
                if self.failures.is_empty() {
                    self.undo
                        .push((page.clone(), form.applied_changes(&page.id, &changes)));
                }
                form.mark_written(&page.id, &changes);
            }
            Err(error) => {
                if self.failures.is_empty() {
                    self.pending = self.undo.drain(..).rev().collect();
                }
                self.failures
                    .push(format!("{} · {error:#}", page.path.join(" / ")));
            }
        }
    }
}

impl Workspace {
    /// Start a single action on this opening of the settings dialog.
    pub(in crate::grid::workspace) fn settings_action(
        &mut self,
        action: Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(form) = &mut self.settings else {
            return;
        };
        if self.settings_writing {
            return;
        }
        self.settings_writing = true;
        let transaction = Transaction {
            action,
            locale: i18n::locale(),
            pending: form.plan(action).into(),
            undo: Vec::new(),
            failures: Vec::new(),
        };
        self.continue_settings_write(transaction, window, cx);
    }

    /// Execute synchronous pages immediately and resume on the UI thread after a plugin write.
    fn continue_settings_write(
        &mut self,
        mut tx: Transaction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        while let Some((page, changes)) = tx.pending.pop_front() {
            let result = if page.id.starts_with("plugin.") {
                use super::plugins::WriteIntent;
                let intent = if !tx.failures.is_empty() {
                    WriteIntent::Compensate
                } else if tx.action == Action::Cancel {
                    WriteIntent::Cancel
                } else {
                    WriteIntent::Edit
                };
                match self.begin_plugin_write(&page, &changes, intent) {
                    Ok(write) => {
                        self.settings
                            .as_mut()
                            .unwrap()
                            .set_error(i18n::text("settings_io.saving").into());
                        self.repaint_settings(window, cx);
                        cx.spawn_in(window, async move |view, cx| {
                            let result = write.wait().await;
                            let _ = cx.update(|window, cx| {
                                view.update(cx, |workspace, cx| {
                                    let result = result
                                        .map(|finished| workspace.finish_plugin_write(finished));
                                    if let Some(form) = &mut workspace.settings {
                                        tx.record(form, page, changes, result);
                                        workspace.continue_settings_write(tx, window, cx);
                                    }
                                })
                            });
                        })
                        .detach();
                        return;
                    }
                    Err(error) => Err(error),
                }
            } else if page.id.starts_with("core.cursor.") {
                self.write_cursor_settings(&page, &changes, window, cx)
            } else {
                self.write_core_settings(&page, &changes, window, cx)
            };
            tx.record(self.settings.as_mut().unwrap(), page, changes, result);
        }
        self.settings_writing = false;
        let quit = std::mem::take(&mut self.close_after_settings);
        if tx.failures.is_empty() && tx.action != Action::Apply {
            self.settings = None;
            self.plugins.settings_sources.clear();
            if quit {
                self.request_close(CloseTarget::Quit, window, cx);
            }
        } else {
            if tx
                .undo
                .iter()
                .any(|(p, _)| p.id.starts_with("core.highlight."))
            {
                let pages = self.highlight_settings_pages();
                self.settings
                    .as_mut()
                    .unwrap()
                    .refresh_highlight_pages(pages);
            }
            if tx.locale != i18n::locale() {
                self.relabel_settings();
            }
            self.settings
                .as_mut()
                .unwrap()
                .set_error(tx.failures.join(" · "));
        }
        self.sync(window, cx);
    }

    /// Refresh translated paths, choices and captions without taking a second opening snapshot.
    fn relabel_settings(&mut self) {
        let mut pages = self
            .settings
            .as_ref()
            .unwrap()
            .pages()
            .filter(|page| page.id.starts_with("plugin."))
            .cloned()
            .collect::<Vec<_>>();
        self.localized_plugin_settings_pages(&mut pages);
        if let Ok(core) = self.core_settings_pages() {
            pages.extend(core);
        }
        pages.extend(self.cursor_settings_pages());
        self.settings.as_mut().unwrap().relabel(pages);
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::grid::workspace::native_test;
    use anyhow::{Context as _, Result, ensure};
    use nanobug::plugins::session::Options;
    use serde_json::json;
    use std::fs;

    /// If a later owner's file becomes unwritable, undo earlier pages and leave the dialog usable.
    #[test]
    #[ignore = "opens a native Windows workspace"]
    fn native_settings_confirm_compensates_an_earlier_page() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let project = temp.path().join("project");
        fs::create_dir(&project)?;
        let vim_path = temp.path().join("vim-settings.toml");
        fs::write(&vim_path, "[unrelated]\nkeep = true\n")?;
        let path_for_workspace = vim_path.clone();
        let options = Options::take(&mut vec!["--no-plugins".into()], temp.path())?;
        native_test::run_async(
            project,
            options,
            move |workspace| {
                workspace.vim_settings_path = Some(path_for_workspace);
            },
            async move |handle, app| {
                handle.update(app, |workspace, window, cx| -> Result<()> {
                    workspace.open_settings(window, cx);
                    let form = workspace
                        .settings
                        .as_ref()
                        .context("settings did not open")?;
                    let page = |id: &str| {
                        form.pages()
                            .find(|p| p.id == id)
                            .cloned()
                            .context("missing page")
                    };
                    let indentation = page("core.vim.indent")?;
                    let fonts = page("core.appearance.font")?;
                    // A directory at the file location fails on every supported Windows filesystem.
                    workspace.appearance_settings_path = Some(temp.path().to_owned());
                    let transaction = Transaction {
                        action: Action::Confirm,
                        locale: i18n::locale(),
                        pending: [
                            (indentation, vec![("shiftwidth".into(), json!(7))]),
                            (fonts, vec![("font_size".into(), json!(20))]),
                        ]
                        .into(),
                        undo: Vec::new(),
                        failures: Vec::new(),
                    };
                    workspace.settings_writing = true;
                    workspace.continue_settings_write(transaction, window, cx);
                    ensure!(!workspace.settings_writing && workspace.settings.is_some());
                    let disk: toml::Value = toml::from_str(&fs::read_to_string(&vim_path)?)?;
                    ensure!(
                        disk.get("vim").is_none()
                            && disk["unrelated"]["keep"].as_bool() == Some(true)
                    );
                    let live = workspace
                        .core_settings_pages()?
                        .into_iter()
                        .find(|p| p.id == "core.vim.indent")
                        .unwrap();
                    ensure!(
                        live.fields
                            .iter()
                            .any(|f| f.id == "shiftwidth" && f.value == json!(4))
                    );
                    workspace.settings_action(Action::Cancel, window, cx);
                    ensure!(workspace.settings.is_none());
                    Ok(())
                })??;
                Ok(())
            },
        )
    }
}
