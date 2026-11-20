//! Bind the active cursor preset and project timing overrides to the native settings dialog.

use super::super::Workspace;
use crate::grid::windows::settings::{Choice, Field, Kind, Page};
use anyhow::{Context as _, Result, bail, ensure};
use gpui::{Context, Window};
use nanobug::{
    animation::cursor::{CursorConfig, Effect},
    app_data::settings::{self, OverrideFile},
    i18n,
};
use serde_json::{Value, json};
use std::path::PathBuf;

const PRESETS: [(&str, Effect); 4] = [
    ("fluid", Effect::Fluid),
    ("elastic", Effect::Elastic),
    ("afterglow", Effect::Afterglow),
    ("ripple", Effect::Ripple),
];
const STAGES: [(&str, &str, &[&str]); 7] = [
    (
        "prepare",
        "settings_cursor.prepare",
        &["duration_ms", "insert_ms"],
    ),
    ("travel", "settings_cursor.travel", &["response_ms"]),
    ("approach", "settings_cursor.approach", &["response_ms"]),
    ("settle", "settings_cursor.settle", &["response_ms"]),
    (
        "post_move",
        "settings_cursor.post_move",
        &["duration_ms", "insert_ms"],
    ),
    (
        "scroll_follow",
        "settings_cursor.scroll_follow",
        &["response_ms"],
    ),
    (
        "scroll_settle",
        "settings_cursor.scroll_settle",
        &["response_ms"],
    ),
];

fn preset(name: &str) -> Option<Effect> {
    PRESETS.iter().find(|(n, _)| *n == name).map(|(_, p)| *p)
}
fn name(preset: Effect) -> &'static str {
    match preset {
        Effect::Fluid => "fluid",
        Effect::Elastic => "elastic",
        Effect::Afterglow => "afterglow",
        Effect::Ripple => "ripple",
    }
}

/// The editor has historically opened with Afterglow, independently of the simulation default.
pub(in crate::grid::workspace) fn saved_cursor_effect() -> Effect {
    settings::path()
        .ok()
        .and_then(|path| {
            settings::read_ui(&path, |ui| {
                ui.get("cursor_effect")?.as_str().and_then(preset)
            })
            .ok()
            .flatten()
            .flatten()
        })
        .unwrap_or(Effect::Afterglow)
}

/// A project's configuration file and the runtime snapshot from its most recent load.
struct ProjectTiming {
    root: PathBuf,
    file: OverrideFile,
    opening_config: CursorConfig,
    opening_source: String,
}

/// Preserve the runtime F6 choice independently of the persisted default preset.
#[derive(Default)]
pub(in crate::grid::workspace) struct CursorBaseline {
    preset: Option<Effect>,
    ui: Option<OverrideFile>,
    project: Option<ProjectTiming>,
    project_error: Option<String>,
}

fn stage_path<'a>(scope: &'a str, stage: &'a str, field: &'a str) -> Vec<&'a str> {
    if scope == "shared" {
        vec!["stages", stage, field]
    } else {
        vec!["presets", scope, stage, field]
    }
}

fn bounds(field: &str) -> (i64, i64) {
    (if field == "response_ms" { 12 } else { 0 }, 2000)
}

fn base_path() -> Vec<String> {
    vec![
        i18n::text("settings_core.root").into(),
        i18n::text("settings_cursor.root").into(),
    ]
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::grid::workspace::native_test;
    use nanobug::plugins::session::Options;
    use std::fs;

    #[test]
    #[ignore = "opens a native Windows workspace"]
    fn native_settings_restore_transient_preset_and_inherited_project_timing() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("project");
        fs::create_dir_all(&root)?;
        let timing = root.join("cursor-effects.toml");
        let path = temp.path().join("settings.toml");
        let original = "[ui]\ncursor_effect = 'fluid' # saved independently of F6\n";
        fs::write(&path, original)?;
        let settings_path = path.clone();
        let options = Options::take(&mut vec!["--no-plugins".into()], temp.path())?;
        native_test::run_async(
            root,
            options,
            move |ws| ws.appearance_settings_path = Some(settings_path),
            async move |handle, app| {
                let _temp = temp;
                handle.update(app, |ws, window, cx| -> Result<()> {
                    ws.cursor.update(cx, |cursor, cx| {
                        cursor.select_effect(Effect::Ripple, window, cx)
                    });
                    ws.open_settings(window, cx);
                    let settings = ws.settings.as_ref().context("settings did not open")?;
                    let find = |id: &str| {
                        settings
                            .pages()
                            .find(|page| page.id == id)
                            .cloned()
                            .context("missing cursor page")
                    };
                    let preset = find("core.cursor.preset")?;
                    let stage = find("core.cursor.stage.shared.prepare")?;
                    ensure!(preset.fields[0].value == "ripple");
                    ws.write_cursor_settings(
                        &preset,
                        &[("cursor_effect".into(), json!("elastic"))],
                        window,
                        cx,
                    )?;
                    ensure!(
                        ws.cursor.read(cx).effect() == Effect::Elastic
                            && fs::read_to_string(&path)?.contains("elastic")
                    );
                    ws.write_cursor_settings(
                        &stage,
                        &[
                            ("duration_ms".into(), json!(75)),
                            ("insert_ms".into(), json!(9)),
                        ],
                        window,
                        cx,
                    )?;
                    ensure!(CursorConfig::load(timing.parent().unwrap()).is_ok());
                    ensure!(fs::read_to_string(&timing)?.contains("insert_ms = 9"));
                    ws.write_cursor_settings(
                        &stage,
                        &[
                            ("duration_ms".into(), Value::Null),
                            ("insert_ms".into(), Value::Null),
                        ],
                        window,
                        cx,
                    )?;
                    ensure!(!timing.exists());
                    ws.write_cursor_settings(
                        &preset,
                        &[("cursor_effect".into(), json!("ripple"))],
                        window,
                        cx,
                    )?;
                    ensure!(
                        ws.cursor.read(cx).effect() == Effect::Ripple
                            && fs::read_to_string(&path)? == original
                    );
                    fs::write(&timing, "[bad syntax")?;
                    ws.capture_cursor_settings(cx)?;
                    let pages = ws.cursor_settings_pages();
                    ensure!(pages.iter().any(|p| p.id == "core.cursor.preset"));
                    ensure!(pages.iter().any(|p| p.id == "core.cursor.project_error"));
                    ensure!(!pages.iter().any(|p| p.id.starts_with("core.cursor.stage.")));
                    Ok(())
                })??;
                Ok(())
            },
        )
    }
}

impl Workspace {
    /// Read the owners without creating any project override or portable preference.
    pub(super) fn capture_cursor_settings(&mut self, cx: &mut Context<Self>) -> Result<()> {
        self.cursor_settings_baseline = CursorBaseline::default();
        let ui = self
            .appearance_settings_path
            .as_deref()
            .map(OverrideFile::load)
            .transpose()?;
        let project = self
            .projects
            .active_project()
            .map(|project| -> Result<ProjectTiming> {
                let file = OverrideFile::load(&project.root.join("cursor-effects.toml"))?;
                let opening_source = file.opening_document();
                CursorConfig::parse(&opening_source).with_context(|| {
                    project
                        .root
                        .join("cursor-effects.toml")
                        .display()
                        .to_string()
                })?;
                Ok(ProjectTiming {
                    root: project.root.clone(),
                    file,
                    opening_config: project.cursor_config.clone(),
                    opening_source,
                })
            })
            .transpose();
        let (project, project_error) = match project {
            Ok(project) => (project, None),
            Err(error) => (None, Some(format!("{error:#}"))),
        };
        self.cursor_settings_baseline = CursorBaseline {
            preset: Some(self.cursor.read(cx).effect()),
            ui,
            project,
            project_error,
        };
        Ok(())
    }

    pub(super) fn cursor_settings_pages(&self) -> Vec<Page> {
        let baseline = &self.cursor_settings_baseline;
        let mut path = base_path();
        path.push(i18n::text("settings_cursor.preset").into());
        let mut pages = vec![Page {
            id: "core.cursor.preset".into(),
            path,
            fields: vec![Field {
                id: "cursor_effect".into(),
                label: i18n::text("settings_cursor.preset").into(),
                kind: Kind::Select(
                    PRESETS
                        .iter()
                        .map(|(id, p)| Choice {
                            id: (*id).into(),
                            label: p.label().into(),
                        })
                        .collect(),
                ),
                value: json!(name(baseline.preset.unwrap_or(Effect::Afterglow))),
                default: json!("afterglow"),
            }],
        }];
        if let Some(error) = &baseline.project_error {
            let mut path = base_path();
            path.push(i18n::text("settings_cursor.project").into());
            let notice = json!(format!(
                "{}\n{error}",
                i18n::text("settings_cursor.unavailable")
            ));
            pages.push(Page {
                id: "core.cursor.project_error".into(),
                path,
                fields: vec![Field {
                    id: "error".into(),
                    label: String::new(),
                    kind: Kind::Notice,
                    value: notice.clone(),
                    default: notice,
                }],
            });
        }
        let Some(project) = &baseline.project else {
            return pages;
        };
        for scope in std::iter::once("shared").chain(PRESETS.iter().map(|(name, _)| *name)) {
            for (stage, label, keys) in STAGES {
                let mut path = base_path();
                path.push(i18n::text("settings_cursor.project").into());
                path.push(
                    preset(scope)
                        .map(Effect::label)
                        .unwrap_or(i18n::text("settings_cursor.shared"))
                        .into(),
                );
                path.push(i18n::text(label).into());
                let fields = keys
                    .iter()
                    .map(|key| {
                        let label = match *key {
                            "duration_ms" => "settings_cursor.duration",
                            "insert_ms" => "settings_cursor.insert",
                            _ => "settings_cursor.response",
                        };
                        let (min, max) = bounds(key);
                        Field {
                            id: (*key).into(),
                            label: i18n::text(label).into(),
                            kind: Kind::OptionalNumber { min, max },
                            value: json!(
                                project
                                    .file
                                    .opening(&stage_path(scope, stage, key))
                                    .and_then(toml_edit::Value::as_integer)
                            ),
                            default: Value::Null,
                        }
                    })
                    .collect();
                pages.push(Page {
                    id: format!("core.cursor.stage.{scope}.{stage}"),
                    path,
                    fields,
                });
            }
        }
        pages
    }

    pub(super) fn write_cursor_settings(
        &mut self,
        page: &Page,
        changes: &[(String, Value)],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let baseline = &mut self.cursor_settings_baseline;
        if page.id == "core.cursor.preset" {
            let [(key, value)] = changes else {
                bail!("Expected one cursor preset")
            };
            ensure!(key == "cursor_effect", "Unknown cursor field {key}");
            let effect = value
                .as_str()
                .and_then(preset)
                .context("Unknown cursor preset")?;
            if let Some(file) = &mut baseline.ui {
                let restore = if baseline.preset == Some(effect) {
                    &["cursor_effect"][..]
                } else {
                    &[][..]
                };
                file.write(
                    &[(&["ui", "cursor_effect"], Some(name(effect).into()))],
                    restore,
                    |_| Ok(()),
                )?;
            }
            self.cursor
                .update(cx, |cursor, cx| cursor.select_effect(effect, window, cx));
            return Ok(());
        }
        let timing = baseline
            .project
            .as_mut()
            .context("Open a project to configure cursor timing")?;
        let project = self
            .projects
            .active_project_mut()
            .filter(|project| project.root == timing.root)
            .context("The settings project changed")?;
        let (scope, stage) = page
            .id
            .strip_prefix("core.cursor.stage.")
            .and_then(|id| id.split_once('.'))
            .context("Unknown cursor page")?;
        ensure!(
            scope == "shared" || preset(scope).is_some(),
            "Unknown cursor preset {scope}"
        );
        let (_, _, allowed) = STAGES
            .iter()
            .find(|(key, _, _)| *key == stage)
            .context("Unknown cursor stage")?;
        let mut patches = Vec::new();
        let mut restore = Vec::new();
        for (key, value) in changes {
            ensure!(
                allowed.contains(&key.as_str()),
                "Unknown cursor timing {key}"
            );
            let original = page
                .fields
                .iter()
                .find(|field| field.id == *key)
                .context("Unknown cursor setting")?;
            if original.value == *value {
                restore.push(key.as_str());
            }
            let new = if value.is_null() {
                None
            } else {
                let (min, max) = bounds(key);
                Some(
                    value
                        .as_i64()
                        .filter(|v| (min..=max).contains(v))
                        .context("Invalid stage timing")?
                        .into(),
                )
            };
            patches.push((stage_path(scope, stage, key), new));
        }
        let patches: Vec<_> = patches
            .iter()
            .map(|(keys, value)| (keys.as_slice(), value.clone()))
            .collect();
        let config = timing.file.write(&patches, &restore, |source| {
            if source == timing.opening_source {
                Ok(timing.opening_config.clone())
            } else {
                CursorConfig::parse(source)
            }
        })?;
        project.cursor_config = config.clone();
        self.cursor
            .update(cx, |cursor, cx| cursor.configure(config, window, cx));
        Ok(())
    }
}
