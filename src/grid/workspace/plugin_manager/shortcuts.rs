//! Resolve current global mappings without stealing the editor's normal keys.

use super::Workspace;
use crate::grid::workspace::{layout::Focus, tabs::ProjectShortcut};
use gpui::{Context, KeyDownEvent, Keystroke, OperatingSystem, Window};
use nanobug::{
    plugins::{
        package::{CommandContext, CommandDecl},
        session::{ConfigScope, Key, State, View},
    },
    vim::Mode,
};
use nanobug_plugin_sdk::Instance;

pub(super) struct Binding {
    pub label: String,
    pub shortcut: String,
    pub command: String,
    pub key: Key,
    pub issue: Option<&'static str>,
    stroke: Option<Keystroke>,
    custom: bool,
    order: u64,
}

pub(super) fn parse(value: &str) -> Option<Keystroke> {
    let os = if cfg!(windows) {
        OperatingSystem::Windows
    } else if cfg!(target_os = "macos") {
        OperatingSystem::Mac
    } else {
        OperatingSystem::Linux
    };
    let stroke = Keystroke::parse_with_os(&value.trim().to_ascii_lowercase(), os).ok()?;
    let mods = stroke.modifiers;
    let function = stroke
        .key
        .strip_prefix('f')
        .and_then(|number| number.parse::<u8>().ok())
        .is_some_and(|number| (1..=24).contains(&number));
    let modifier_chord = mods.control
        && mods.alt
        && stroke.key.len() == 1
        && stroke.key.as_bytes()[0].is_ascii_alphanumeric();
    let reserved = matches!(stroke.key.as_str(), "f5" | "f6" | "f7")
        || cfg!(feature = "plugin-probe") && matches!(stroke.key.as_str(), "f8" | "f9")
        || ProjectShortcut::resolve(&stroke).is_some();
    (stroke.key_char.is_none() && (function || modifier_chord) && !reserved).then_some(stroke)
}

pub(super) fn declaration<'a>(
    view: &'a View,
    namespace: &str,
    id: &str,
) -> Option<&'a CommandDecl> {
    view.installed
        .iter()
        .map(|record| &record.release.manifest)
        .find(|manifest| manifest.identity().namespace() == namespace)
        .or_else(|| {
            view.retained
                .get(namespace)
                .map(|release| &release.manifest)
        })?
        .commands
        .iter()
        .find(|command| command.id == id)
}

/// The help view and the keyboard use the same resolved mapping and availability.
pub(super) fn bindings(
    view: &View,
    project: Option<u64>,
    context: Option<CommandContext>,
) -> Vec<Binding> {
    let configured = view
        .configurations
        .get(&ConfigScope::AllProjects)
        .map(|config| &config.applied.shortcuts);
    let user_keys = configured
        .into_iter()
        .flatten()
        .flat_map(|(owner, mappings)| {
            mappings.iter().filter_map(|(id, mapped)| {
                context
                    .is_some_and(|context| {
                        declaration(view, owner, id).is_none_or(|cmd| cmd.applies(context))
                    })
                    .then(|| parse(mapped))
                    .flatten()
            })
        })
        .collect::<Vec<_>>();
    let mut candidates = Vec::new();
    for record in view.installed.iter() {
        let manifest = &record.release.manifest;
        let namespace = manifest.identity().namespace();
        for command in &manifest.commands {
            let override_key = configured.and_then(|keys| keys.get(&namespace)?.get(&command.id));
            let shortcut = override_key
                .or(command.key.as_ref())
                .cloned()
                .unwrap_or_default();
            let stroke = parse(&shortcut);
            let key = Key {
                namespace: namespace.clone(),
                instance: Instance {
                    project: project.unwrap_or_default(),
                    module: command.module.clone(),
                },
            };
            let issue = if shortcut.is_empty() {
                Some("Unassigned")
            } else if stroke.is_none() {
                Some("Reserved or invalid key")
            } else if !record.enabled {
                Some("Plugin disabled")
            } else if context.is_none_or(|context| !command.applies(context)) {
                Some("Unavailable in this pane or mode")
            } else if project.is_none() || view.modules.get(&key) != Some(&State::Ready) {
                Some("Module unavailable")
            } else {
                None
            };
            candidates.push(Binding {
                label: format!("{} · {}", manifest.name, command.title),
                shortcut,
                command: command.id.clone(),
                key,
                issue,
                stroke,
                custom: override_key.is_some(),
                order: record.shortcut_order.get(&command.id).copied().unwrap_or(0),
            });
        }
    }
    let mut occupied = Vec::new();
    candidates.sort_by_key(|binding| (!binding.custom, binding.order));
    for binding in &mut candidates {
        if binding.issue.is_none()
            && let Some(stroke) = &binding.stroke
        {
            if occupied.contains(&stroke) || !binding.custom && user_keys.contains(stroke) {
                binding.issue = Some("Key conflicts with another command");
            } else {
                occupied.push(stroke);
            }
        }
    }
    candidates.sort_by_key(|binding| binding.issue.is_some());
    candidates
}

impl Workspace {
    pub(super) fn plugin_command_context(&self) -> Option<CommandContext> {
        let project = self.projects.active_project()?;
        match (project.focus, project.editor.vim().mode) {
            (Focus::Manager, _) => Some(CommandContext::Explorer),
            (Focus::Editor, Mode::Normal) => Some(CommandContext::EditorNormal),
            (Focus::Editor, Mode::Visual | Mode::VisualLine) => Some(CommandContext::EditorVisual),
            _ => None,
        }
    }

    /// Host mappings are active in command mode only and preserve native text input.
    pub(in crate::grid::workspace) fn plugin_shortcut(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.modal_open() || self.accepts_native_text() || event.is_held || self.prefix.is_some()
        {
            return false;
        }
        let pressed = &event.keystroke;
        if !pressed.modifiers.alt && !pressed.key.starts_with('f') {
            return false;
        }
        let Some(context) = self.plugin_command_context() else {
            return false;
        };
        let Some(binding) = bindings(&self.plugins.view, self.plugins.active, Some(context))
            .into_iter()
            .find(|binding| {
                binding.issue.is_none()
                    && binding.stroke.as_ref().is_some_and(|stroke| {
                        pressed.key == stroke.key && pressed.modifiers == stroke.modifiers
                    })
            })
        else {
            return false;
        };
        self.plugin_command(binding.key, binding.command, window, cx);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nanobug::plugins::{package::Installed, session::ConfigView};
    use serde_json::json;
    use std::{collections::BTreeMap, sync::Arc};

    fn plugin(name: &str, context: &str, order: u64) -> Installed {
        serde_json::from_value(json!({
            "release": {
                "manifest": {
                    "name": name, "id": "keys", "version": "1.0.0",
                    "engines": {"nanobug": "*"},
                    "commands": [{
                        "id": "choose", "title": "Choose", "module": "main",
                        "key": "F10", "contexts": [context]
                    }]
                },
                "digest": "fixture", "source": "fixture.zip", "files": {}
            },
            "grants": [], "enabled": true, "shortcut_order": {"choose": order}
        }))
        .unwrap()
    }

    #[test]
    fn editor_and_project_shortcuts_remain_reserved() {
        for key in ["F5", "F6", "F7", "ctrl-alt-h", "ctrl-alt-l"] {
            assert!(parse(key).is_none(), "{key}");
        }
        for key in ["F1", "F10", "F24", "ctrl-alt-a"] {
            assert!(parse(key).is_some(), "{key}");
        }
    }

    #[test]
    fn contexts_and_availability_share_the_keyboard_and_help_mapping() {
        let older = plugin("Earlier", "editor_normal", 1);
        let newer = plugin("Later", "editor_normal", 3);
        let visual = plugin("Visual", "editor_visual", 2);
        let namespace = |record: &Installed| record.release.manifest.identity().namespace();
        let (old, new, visual_id) = (namespace(&older), namespace(&newer), namespace(&visual));
        let mut view = View {
            installed: Arc::new(vec![newer, visual, older]),
            ..View::default()
        };
        for project in [1, 2] {
            for owner in [&old, &new, &visual_id] {
                view.modules.insert(
                    Key {
                        namespace: owner.clone(),
                        instance: Instance {
                            project,
                            module: "main".into(),
                        },
                    },
                    State::Ready,
                );
            }
        }
        let winner = |view: &View, project, mode| {
            bindings(view, Some(project), mode)
                .into_iter()
                .find(|entry| entry.issue.is_none() && entry.shortcut == "F10")
                .map(|entry| entry.key.namespace)
        };
        assert_eq!(
            winner(&view, 1, Some(CommandContext::EditorNormal)),
            Some(old.clone())
        );
        assert_eq!(
            winner(&view, 1, Some(CommandContext::EditorVisual)),
            Some(visual_id.clone())
        );
        assert_eq!(winner(&view, 1, Some(CommandContext::Explorer)), None);
        assert_eq!(winner(&view, 1, None), None);
        assert!(
            bindings(&view, Some(1), Some(CommandContext::EditorNormal))
                .iter()
                .any(|binding| binding.key.namespace == new
                    && binding.issue == Some("Key conflicts with another command"))
        );

        let normal = declaration(&view, &new, "choose").unwrap();
        let visual_command = declaration(&view, &visual_id, "choose").unwrap();
        assert!(!normal.shares_context(visual_command));
        let mappings = BTreeMap::from([("choose".into(), "F10".into())]);
        let mut config = ConfigView::default();
        config
            .applied
            .shortcuts
            .insert(new.clone(), mappings.clone());
        config.applied.shortcuts.insert(visual_id.clone(), mappings);
        view.configurations = Arc::new(BTreeMap::from([(ConfigScope::AllProjects, config)]));
        for project in [1, 2] {
            assert_eq!(
                winner(&view, project, Some(CommandContext::EditorNormal)),
                Some(new.clone())
            );
            assert_eq!(
                winner(&view, project, Some(CommandContext::EditorVisual)),
                Some(visual_id.clone())
            );
        }
        view.modules
            .retain(|key, _| key.namespace != new || key.instance.project != 2);
        assert_eq!(winner(&view, 2, Some(CommandContext::EditorNormal)), None);
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires the Windows DirectWrite and DirectX renderer"]
    fn native_shortcut_matches_help_and_yields_to_text_input() -> anyhow::Result<()> {
        use crate::{
            grid::{
                render::{Frame, Rect},
                workspace::{native_test, plugin_manager::rows},
            },
            ui::theme::TEXT,
        };
        use anyhow::{Context as _, ensure};
        use nanobug::plugins::session::Options;
        use std::fs;

        let fixture = tempfile::tempdir()?;
        let first = fixture.path().join("first");
        let second = fixture.path().join("second");
        fs::create_dir(&first)?;
        fs::create_dir(&second)?;
        let options = Options::take(
            &mut vec![
                "--plugin-profile".into(),
                fixture.path().join("profile").into_os_string(),
            ],
            fixture.path(),
        )?;
        native_test::run_async(
            first,
            options,
            |workspace| workspace.plugins.options = None,
            async move |handle, cx| {
                cx.update(|app| {
                    handle.update(app, |workspace, window, cx| -> anyhow::Result<()> {
                        let (second, _) = workspace.projects.add(second, None);
                        let record = plugin("Keys Fixture", "editor_normal", 1);
                        let namespace = record.release.manifest.identity().namespace();
                        let mut config = ConfigView::default();
                        config.applied.shortcuts.insert(
                            namespace.clone(),
                            BTreeMap::from([("choose".into(), "F11".into())]),
                        );
                        let view = View {
                            installed: Arc::new(vec![record]),
                            configurations: Arc::new(BTreeMap::from([(
                                ConfigScope::AllProjects,
                                config,
                            )])),
                            modules: workspace
                                .projects
                                .ordered()
                                .map(|project| {
                                    (
                                        Key {
                                            namespace: namespace.clone(),
                                            instance: Instance {
                                                project: project.plugin_scope,
                                                module: "main".into(),
                                            },
                                        },
                                        State::Ready,
                                    )
                                })
                                .collect(),
                            ..View::default()
                        };
                        workspace.plugins.view = Arc::new(view);
                        let event = |key| -> anyhow::Result<KeyDownEvent> {
                            Ok(KeyDownEvent {
                                keystroke: Keystroke::parse(key)?,
                                is_held: false,
                                prefer_character_input: false,
                            })
                        };
                        let overridden = event("f11")?;
                        let default = event("f10")?;
                        for tab in [0, second] {
                            workspace.projects.select(tab);
                            let project = workspace
                                .projects
                                .active_project_mut()
                                .context("active project")?;
                            project.focus = Focus::Editor;
                            workspace.plugins.active = Some(project.plugin_scope);
                            ensure!(workspace.plugin_shortcut(&overridden, window, cx));
                            ensure!(!workspace.plugin_shortcut(&default, window, cx));

                            let area = Rect {
                                w: 60,
                                h: 18,
                                ..Rect::default()
                            };
                            let mut expected = Frame::new(area.w, area.h);
                            expected.border("Plugin shortcuts", true, false, &workspace.spacing);
                            for (index, line) in rows::HELP.iter().enumerate() {
                                expected.label(index + 1, 2..58, line, TEXT, &workspace.spacing);
                            }
                            expected.label(
                                rows::HELP.len() + 1,
                                2..58,
                                &format!("{:<15}{}", "F11", "Keys Fixture · Choose"),
                                TEXT,
                                &workspace.spacing,
                            );
                            workspace.plugins.manager.help = true;
                            ensure!(workspace.manager_frame(area) == expected);
                            workspace.plugins.manager.help = false;

                            let editor = &mut workspace.projects.active_mut().editor;
                            editor.buffers[editor.active].vim.mode = Mode::Insert;
                            ensure!(workspace.accepts_native_text());
                            ensure!(!workspace.plugin_shortcut(&overridden, window, cx));
                            let editor = &mut workspace.projects.active_mut().editor;
                            editor.buffers[editor.active].vim.mode = Mode::Visual;
                            ensure!(!workspace.accepts_native_text());
                            ensure!(!workspace.plugin_shortcut(&overridden, window, cx));
                        }
                        Ok(())
                    })
                })?
            },
        )
    }
}
