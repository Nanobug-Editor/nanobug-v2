//! Bridge explicit Vim syntax selection to the bundled Tree-sitter worker.

use super::super::layout::Focus;
use super::{Context, Workspace};
use crate::grid::windows::editor::EditorWindow;
use gpui::{ClipboardItem, Window};
use nanobug::animation::scroll::ScrollInput;
use nanobug_plugin_sdk::DocumentSnapshot;
use std::{ops::Range, path::Path, sync::Arc};

/// Match the syntax worker's existing text budget before cloning a source document.
const MAX_SYNTAX_BYTES: usize = 120 * 1024;

impl Workspace {
    /// Ask for the nodes of the active editor or focused `preview` through `window` and `cx`.
    pub(in crate::grid::workspace) fn request_syntax_selection(
        &mut self,
        preview: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.syntax_editor(preview) else {
            return;
        };
        let vim = editor.vim();
        let Some(request) = vim.syntax_selection_request() else {
            return;
        };
        let doc = &vim.doc;
        let version = (doc.opening, doc.revision);
        let path = doc.path.clone();
        let receiver = self.plugins.session.as_ref().and_then(|session| {
            let language = syntax_grammar(path.as_deref()?)?;
            if doc.text.len() > MAX_SYNTAX_BYTES {
                return None;
            }
            session
                .syntax_nodes(
                    language,
                    DocumentSnapshot {
                        path: None,
                        opening: version.0,
                        revision: version.1,
                        text: doc.text.clone(),
                    },
                )
                .ok()
        });
        let Some(receiver) = receiver else {
            self.finish_syntax_selection(request, version, preview, Arc::from([]), window, cx);
            return;
        };
        cx.spawn_in(window, async move |view, cx| {
            let nodes = receiver
                .recv()
                .await
                .ok()
                .and_then(Result::ok)
                .unwrap_or_default();
            let _ = cx.update(|window, cx| {
                let _ = view.update(cx, |workspace, cx| {
                    if workspace
                        .syntax_editor(preview)
                        .is_some_and(|editor| editor.vim().doc.path == path)
                    {
                        workspace.finish_syntax_selection(
                            request,
                            version,
                            preview,
                            nodes.into(),
                            window,
                            cx,
                        );
                    }
                });
            });
        })
        .detach();
    }

    /// Resolve the current source editor for an explicit syntax selection request.
    fn syntax_editor(&self, preview: bool) -> Option<&EditorWindow> {
        if preview {
            self.file_picker.as_ref()?.focused_editor()
        } else if !self.modal_open() {
            self.projects
                .active_project()
                .filter(|project| project.focus == Focus::Editor)
                .map(|project| &project.editor)
        } else {
            None
        }
    }

    /// Resume `request` on the same document `version` using the returned `nodes`.
    /// `preview` routes the operation back to the picker instead of the project editor.
    fn finish_syntax_selection(
        &mut self,
        request: u64,
        version: (u64, u64),
        preview: bool,
        nodes: Arc<[Range<usize>]>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.syntax_editor(preview).is_some_and(|editor| {
            editor.vim().syntax_selection_request() == Some(request)
                && (editor.vim().doc.opening, editor.vim().doc.revision) == version
        }) {
            return;
        }
        if preview {
            let area = self.file_picker_layout().preview_editor_area();
            let editor = self
                .file_picker
                .as_mut()
                .and_then(|session| session.focused_editor_mut())
                .expect("validated preview editor");
            let before = editor.register().yank_revision();
            let edit = editor.apply_syntax_selection(version, nodes, area, &self.spacing);
            if editor.register().yank_revision() != before {
                cx.write_to_clipboard(ClipboardItem::new_string(editor.register().text.clone()));
            }
            self.finish_focused_file_action(edit.action, "@", [ScrollInput::Reveal; 2], window, cx);
        } else {
            let area = self.active_editor_area();
            let editor = &mut self.projects.active_mut().editor;
            let before = editor.register().yank_revision();
            let edit = editor.apply_syntax_selection(version, nodes, area, &self.spacing);
            let register = editor.register();
            if register.yank_revision() != before {
                cx.write_to_clipboard(ClipboardItem::new_string(register.text.clone()));
            }
            self.apply_edit(edit, window, cx);
            self.sync(window, cx);
        }
    }
}

/// Resolve a source `path` to a grammar bundled with the syntax worker.
fn syntax_grammar(path: &Path) -> Option<&'static str> {
    Some(
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "rs" => "rust",
            "c" | "h" => "c",
            "cpp" | "cxx" | "cc" | "hpp" | "hxx" => "cpp",
            "cs" => "csharp",
            "go" => "go",
            "java" => "java",
            "kt" | "kts" => "kotlin",
            "lua" => "lua",
            "md" | "markdown" => "markdown",
            "php" => "php",
            "py" | "pyi" => "python",
            "rb" => "ruby",
            "js" | "mjs" | "cjs" | "jsx" => "javascript",
            "ts" | "mts" | "cts" => "typescript",
            "tsx" => "tsx",
            "html" | "htm" => "html",
            "css" => "css",
            "vue" => "vue",
            "svelte" => "svelte",
            "astro" => "astro",
            "json" => "json",
            "sql" => "sql",
            "toml" => "toml",
            "yaml" | "yml" => "yaml",
            _ => return None,
        },
    )
}

#[cfg(all(test, windows))]
mod tests {
    use super::super::native_editing_tests::{options, step, until, use_session};
    use crate::grid::workspace::native_test;
    use anyhow::{Result, ensure};
    use gpui::{KeyDownEvent, Keystroke};
    use nanobug::plugins::session::Session;
    use std::fs;

    /// Exercise native Vim key routing and the real bundled parser without a language plugin.
    #[test]
    #[ignore = "opens a native Windows workspace"]
    fn native_syntax_selection_uses_bundled_worker() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let profile = temp.path().join("profile");
        let project = temp.path().join("project");
        fs::create_dir(&project)?;
        let path = project.join("main.rs");
        fs::write(&path, "fn needle() { let _ = \"中文\"; }\n")?;
        let session = Session::start(options(temp.path(), &profile)?).0;
        native_test::run_async(
            project,
            options(temp.path(), &profile)?,
            move |workspace| use_session(workspace, session),
            async move |window, app| {
                let press = |workspace: &mut super::Workspace,
                             native: &mut gpui::Window,
                             cx: &mut gpui::Context<super::Workspace>,
                             keys: &[&'static str]|
                 -> Result<()> {
                    for key in keys {
                        workspace.key_down(
                            &KeyDownEvent {
                                keystroke: Keystroke::parse(key)?,
                                is_held: false,
                                prefer_character_input: false,
                            },
                            native,
                            cx,
                        );
                    }
                    Ok(())
                };
                step(app, window, |workspace, native, cx| {
                    workspace.open(path, native, cx);
                    press(workspace, native, cx, &["0", "w", "v", "a", "n"])
                })?;
                until(app, window, "Rust identifier selection", |workspace| {
                    let vim = workspace.projects.active().editor.vim();
                    Ok(vim.selection().and_then(|range| vim.doc.text.get(range)) == Some("needle"))
                })
                .await?;
                step(app, window, |workspace, native, cx| {
                    press(workspace, native, cx, &["a", "n", "i", "n"])?;
                    let vim = workspace.projects.active().editor.vim();
                    ensure!(
                        vim.selection().and_then(|range| vim.doc.text.get(range)) == Some("needle")
                    );
                    press(workspace, native, cx, &["escape", "0", "w", "d", "a", "n"])?;
                    ensure!(
                        workspace
                            .projects
                            .active()
                            .editor
                            .vim()
                            .doc
                            .text
                            .starts_with("fn ()")
                    );
                    Ok(())
                })
            },
        )
    }
}
