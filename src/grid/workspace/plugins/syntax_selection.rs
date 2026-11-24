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
    use crate::{
        grid::{render::Rect, windows::WindowFrame, workspace::native_test},
        ui::theme::syntax_color,
    };
    use anyhow::{Result, ensure};
    use gpui::{KeyDownEvent, Keystroke};
    use nanobug::plugins::session::Session;
    use nanobug_plugin_sdk::language::SyntaxKind;
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
        let config = project.join("config.json");
        fs::write(&config, r#"{"enabled":true,"level":12}"#)?;
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
                    workspace.open(config.clone(), native, cx);
                    let doc = &workspace.projects.active().editor.vim().doc;
                    ensure!(
                        doc.path.as_deref().and_then(|p| p.file_name()) == config.file_name(),
                        "unexpected active document {:?}",
                        doc.path
                    );
                    Ok(())
                })?;
                until(
                    app,
                    window,
                    "native configuration syntax without a plugin",
                    |workspace| {
                        let project = workspace.projects.active();
                        let doc = &project.editor.vim().doc;
                        let Some(result) = workspace
                            .plugins
                            .view
                            .builtin_syntax
                            .get(&(project.plugin_scope, doc.opening))
                        else {
                            return Ok(false);
                        };
                        if Some(&result.path) != doc.path.as_ref()
                            || result.version != (doc.opening, doc.revision)
                        {
                            return Ok(false);
                        }
                        for (token, role) in [
                            ("enabled", SyntaxKind::ConfigKey),
                            ("true", SyntaxKind::Boolean),
                            ("12", SyntaxKind::Number),
                        ] {
                            let offset = doc.text.find(token).unwrap();
                            ensure!(
                                result.highlights.iter().any(|h| h.kind == role
                                    && h.range.start <= offset
                                    && offset < h.range.end),
                                "{token} missing {role:?} in {:?}",
                                result.highlights
                            );
                        }
                        Ok(true)
                    },
                )
                .await
            },
        )
    }

    /// Parse both visible Rust panes before revisiting the first, without a language plugin or LSP.
    #[test]
    #[ignore = "opens a native Windows workspace"]
    fn native_builtin_syntax_colors_each_rust_split_without_plugin() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let profile = temp.path().join("profile");
        let project = temp.path().join("project");
        fs::create_dir(&project)?;
        for name in ["first", "second"] {
            fs::write(
                project.join(format!("{name}.rs")),
                format!("// {name} comment\nfn {name}() {{}}\n"),
            )?;
        }
        let first = project.join("first.rs").canonicalize()?;
        let second = project.join("second.rs").canonicalize()?;
        let session = Session::start(options(&project, &profile)?).0;
        native_test::run_async_visible(
            project.clone(),
            options(&project, &profile)?,
            move |workspace| use_session(workspace, session),
            async move |window, app| {
                // The native pane needs its first layout before a split can fit.
                app.background_executor()
                    .timer(std::time::Duration::from_millis(60))
                    .await;
                let active = step(app, window, |workspace, native, cx| {
                    // Both opens happen before polling for results. The first pane stays inactive.
                    workspace.sync(native, cx);
                    workspace.open(first.clone(), native, cx);
                    workspace.open_editor_split(false, Some("second.rs".into()), native, cx);
                    workspace.sync(native, cx);
                    let project = workspace.projects.active();
                    ensure!(project.page_editors().count() == 2, "{}", project.status);
                    ensure!(project.editor.vim().doc.path.as_ref() == Some(&second));
                    Ok(project.active_editor_id())
                })?;
                until(
                    app,
                    window,
                    "lexical Rust colors in both panes",
                    |workspace| {
                        ensure!(workspace.plugins.view.installed.is_empty());
                        ensure!(workspace.plugins.view.language_servers.is_empty());
                        workspace.refresh_document_presentation();
                        let project = workspace.projects.active();
                        ensure!(project.active_editor_id() == active);
                        ensure!(project.editor.vim().doc.path.as_ref() == Some(&second));
                        let context = WindowFrame {
                            area: Rect {
                                w: 48,
                                h: 8,
                                ..Default::default()
                            },
                            selected: false,
                            modal_open: false,
                            controls_hovered: false,
                            spacing: &workspace.spacing,
                        };
                        let mut ready = 0;
                        for (_, editor) in project.page_editors() {
                            let doc = &editor.vim().doc;
                            ensure!(
                                doc.path.as_ref() == Some(&first)
                                    || doc.path.as_ref() == Some(&second)
                            );
                            let Some(parsed) = workspace
                                .plugins
                                .view
                                .builtin_syntax
                                .get(&(project.plugin_scope, doc.opening))
                                .filter(|parsed| parsed.version == (doc.opening, doc.revision))
                            else {
                                return Ok(false);
                            };
                            for (token, kind) in
                                [("comment", SyntaxKind::Comment), ("fn", SyntaxKind::Keyword)]
                            {
                                let offset = doc.text.find(token).expect("fixture token");
                                ensure!(
                                    parsed.highlights.iter().any(|span| {
                                        span.kind == kind
                                            && span.range.start <= offset
                                            && offset < span.range.end
                                    }),
                                    "{:?} lacks {token} {kind:?}: {:?}",
                                    doc.path,
                                    parsed.highlights,
                                );
                            }
                            let text = editor.content_frames(context, |_| None).1;
                            let comment_column = doc.text.find("comment").expect("fixture comment");
                            ensure!(
                                text.cell_paint_at(0, comment_column).foreground
                                    == syntax_color(SyntaxKind::Comment)
                                    && text.cell_paint_at(1, 0).foreground
                                        == syntax_color(SyntaxKind::Keyword),
                                "Unexpected colors for {:?}, comment {:#x}/{:#x}, keyword {:#x}/{:#x}",
                                doc.path,
                                text.cell_paint_at(0, comment_column).foreground,
                                syntax_color(SyntaxKind::Comment),
                                text.cell_paint_at(1, 0).foreground,
                                syntax_color(SyntaxKind::Keyword),
                            );
                            ready += 1;
                        }
                        Ok(ready == 2)
                    },
                )
                .await
            },
        )
    }
}
