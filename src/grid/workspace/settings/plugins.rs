//! Present installed plugin settings with their real global, shared and local project scopes.

use super::super::{
    Workspace,
    plugins::settings_data::{Data, Source, StoredField},
};
use crate::grid::windows::settings::{Field, Kind, Page};
use anyhow::{Context as _, Result, ensure};
use gpui::{Context, Window};
use nanobug::{
    i18n,
    plugins::{
        package::{Installed, SettingDecl},
        session::{
            Action, ConfigScope, ScanOverride, SettingPatch, SettingsCatalog, SettingsWrite,
        },
    },
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// A submitted write that may be awaited without holding the native window or its input thread.
pub(super) struct PendingPluginWrite {
    page: String,
    result: async_channel::Receiver<Result<SettingsWrite, String>>,
}

/// Exact stored representation acknowledged by the supervisor.
pub(super) struct FinishedPluginWrite {
    page: String,
    request: SettingsWrite,
}

/// Which baseline a settings action must commit across inheritance and renamed manifest fields.
#[derive(Clone, Copy)]
pub(super) enum WriteIntent {
    Edit,
    Cancel,
    Compensate,
}

impl PendingPluginWrite {
    pub(super) async fn wait(self) -> Result<FinishedPluginWrite> {
        let request = self
            .result
            .recv()
            .await
            .context("Plugin settings service stopped")?
            .map_err(anyhow::Error::msg)?;
        Ok(FinishedPluginWrite {
            page: self.page,
            request,
        })
    }
}

impl Workspace {
    /// Refresh built-in labels after a language change while preserving manifest text and opening snapshots.
    pub(super) fn localized_plugin_settings_pages(&self, pages: &mut [Page]) {
        for page in pages.iter_mut().filter(|p| p.id.starts_with("plugin.")) {
            let Some(tail) = page.id.rsplit('.').next() else {
                continue;
            };
            page.path[0] = i18n::text("settings_plugins.root").into();
            let (key, field_key) = match tail {
                "activation" => (
                    "settings_plugins.activation",
                    Some("settings_plugins.enabled"),
                ),
                "global" => ("settings_plugins.global", None),
                "local" => ("settings_plugins.local", None),
                "project" => ("settings_plugins.project", None),
                "scan" => (
                    "settings_plugins.scan",
                    Some("settings_plugins.directories"),
                ),
                "empty" => ("settings_plugins.empty", None),
                _ => continue,
            };
            if let Some(leaf) = page.path.last_mut() {
                *leaf = i18n::text(key).into();
            }
            if let (Some(field), Some(key)) = (page.fields.first_mut(), field_key) {
                field.label = i18n::text(key).into();
            }
        }
    }

    /// Populate the plugin tree as soon as the session exists, leaving core settings immediately usable.
    pub(in crate::grid::workspace) fn begin_plugin_catalog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings_catalog_started || self.settings.is_none() {
            return;
        }
        let Some(session) = &self.plugins.session else {
            return;
        };
        let project = self.projects.active_project().map(|p| p.plugin_scope);
        let (reply, result) = async_channel::bounded(1);
        if let Err(error) = session.send(Action::SettingsCatalog { project, reply }) {
            self.settings
                .as_mut()
                .expect("dialog open")
                .set_error(error.to_string());
            return;
        }
        self.settings_catalog_started = true;
        let generation = self.settings_generation;
        cx.spawn_in(window, async move |workspace, cx| {
            let catalog = result
                .recv()
                .await
                .context("Plugin settings service stopped")
                .and_then(|result| result.map_err(anyhow::Error::msg));
            let _ = cx.update(|window, cx| {
                workspace.update(cx, |workspace, cx| {
                    if workspace.settings.is_none() || workspace.settings_generation != generation {
                        return;
                    }
                    let pages = catalog
                        .and_then(|catalog| workspace.install_plugin_catalog(project, catalog));
                    let settings = workspace
                        .settings
                        .as_mut()
                        .expect("dialog generation checked");
                    match pages {
                        Ok(pages) => settings.append_pages(pages),
                        Err(error) => settings.set_error(format!("{error:#}")),
                    }
                    workspace.repaint_settings(window, cx);
                })
            });
        })
        .detach();
    }

    fn install_plugin_catalog(
        &mut self,
        project: Option<u64>,
        catalog: SettingsCatalog,
    ) -> Result<Vec<Page>> {
        self.plugins.settings_sources.clear();
        if catalog.installed.is_empty() {
            return Ok(vec![Page {
                id: "plugin.empty".into(),
                path: vec![
                    i18n::text("settings_plugins.root").into(),
                    i18n::text("settings_plugins.empty").into(),
                ],
                fields: Vec::new(),
            }]);
        }
        let mut pages = Vec::new();
        for installed in catalog.installed.iter() {
            let manifest = &installed.release.manifest;
            let namespace = manifest.identity().namespace();
            let name = if catalog
                .installed
                .iter()
                .filter(|p| p.release.manifest.name == manifest.name)
                .count()
                > 1
            {
                format!("{} ({})", manifest.name, manifest.id)
            } else {
                manifest.name.clone()
            };
            let mut add = |key: &str, label: &str, fields: Vec<Field>, data: Data| {
                let id = format!("plugin.{namespace}.{key}");
                let path = vec![
                    i18n::text("settings_plugins.root").into(),
                    name.clone(),
                    label.into(),
                ];
                self.plugins.settings_sources.insert(
                    id.clone(),
                    Source {
                        namespace: namespace.clone(),
                        digest: installed.release.digest.clone(),
                        data,
                    },
                );
                pages.push(Page { id, path, fields });
            };
            add(
                "activation",
                i18n::text("settings_plugins.activation"),
                vec![Field {
                    id: "enabled".into(),
                    label: i18n::text("settings_plugins.enabled").into(),
                    kind: Kind::Toggle,
                    value: json!(installed.enabled),
                    default: json!(true),
                }],
                Data::Enabled(installed.enabled),
            );
            let mut scopes = vec![(
                "global",
                "settings_plugins.global",
                ConfigScope::AllProjects,
            )];
            if let Some(id) = project {
                scopes.extend([
                    (
                        "project",
                        "settings_plugins.project",
                        ConfigScope::Project(id),
                    ),
                    (
                        "local",
                        "settings_plugins.local",
                        ConfigScope::ProjectLocal(id),
                    ),
                ]);
            }
            for (key, label, scope) in scopes {
                let (fields, sources) = manifest_fields(&catalog, installed, scope);
                if !fields.is_empty() {
                    add(
                        key,
                        i18n::text(label),
                        fields,
                        Data::Manifest {
                            scope,
                            owner_was_absent: catalog
                                .scopes
                                .get(&scope)
                                .and_then(|config| config.plugin(&namespace))
                                .is_none(),
                            fields: sources,
                        },
                    );
                }
            }
            if let Some(project) = project
                && let Some(scan) = catalog.scans.get(&namespace)
            {
                match scan {
                    Ok(opening) => {
                        add(
                            "scan",
                            i18n::text("settings_plugins.scan"),
                            vec![Field {
                                id: "excludeDirs".into(),
                                label: i18n::text("settings_plugins.directories").into(),
                                kind: Kind::Text,
                                value: json!(serde_json::to_string(opening.directories())?),
                                default: json!("[]"),
                            }],
                            Data::Scan {
                                project,
                                applied: opening.clone(),
                                opening: opening.clone(),
                            },
                        );
                    }
                    Err(error) => {
                        // A damaged project file must not lock the user out of every other setting.
                        pages.push(Page {
                            id: format!("plugin.{namespace}.scan-error"),
                            path: vec![
                                i18n::text("settings_plugins.root").into(),
                                name.clone(),
                                format!(
                                    "{} · {error:#}",
                                    i18n::text("settings_plugins.unreadable")
                                ),
                            ],
                            fields: vec![],
                        });
                    }
                }
            }
        }
        Ok(pages)
    }

    /// Submit a scope-checked write; the coordinator advances the baseline only after acknowledgement.
    pub(super) fn begin_plugin_write(
        &self,
        page: &Page,
        changes: &[(String, Value)],
        intent: WriteIntent,
    ) -> Result<PendingPluginWrite> {
        let source = self
            .plugins
            .settings_sources
            .get(&page.id)
            .context("Plugin settings page is no longer open")?;
        let namespace = source.namespace.clone();
        let digest = source.digest.clone();
        let request = match &source.data {
            Data::Enabled(current) => SettingsWrite::Enabled {
                namespace,
                digest,
                before: *current,
                after: single_change(changes, "enabled")?
                    .as_bool()
                    .context("Expected a boolean")?,
            },
            Data::Manifest {
                scope,
                fields,
                owner_was_absent,
            } => {
                let patches = changes
                    .iter()
                    .map(|(id, value)| {
                        let stored = fields.get(id).context("Unknown plugin setting")?;
                        let after = stored.target(id, value, intent)?;
                        Ok(SettingPatch {
                            id: id.clone(),
                            before: stored.applied.clone(),
                            restore_snapshot: after == stored.opening
                                || matches!(intent, WriteIntent::Compensate),
                            after,
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                SettingsWrite::Manifest {
                    namespace,
                    digest,
                    scope: *scope,
                    owner_was_absent: *owner_was_absent,
                    fields: patches,
                }
            }
            Data::Scan {
                project,
                opening,
                applied,
            } => {
                let value = single_change(changes, "excludeDirs")?
                    .as_str()
                    .context("Expected directory list")?;
                let values: Vec<String> = serde_json::from_str(value)
                    .context("Expected a JSON array of directory names")?;
                let after = if values == opening.directories() {
                    opening.clone()
                } else {
                    ScanOverride::Direct(values)
                };
                SettingsWrite::Scan {
                    namespace,
                    digest,
                    project: *project,
                    before: applied.clone(),
                    after,
                }
            }
        };
        let (reply, result) = async_channel::bounded(1);
        self.plugins
            .session
            .as_ref()
            .context("Plugin session is closed")?
            .send(Action::WriteSettings(request, reply))?;
        Ok(PendingPluginWrite {
            page: page.id.clone(),
            result,
        })
    }

    /// Retain exact stored values for repeated Apply and the compensating Cancel operation.
    pub(super) fn finish_plugin_write(&mut self, finished: FinishedPluginWrite) {
        let source = self
            .plugins
            .settings_sources
            .get_mut(&finished.page)
            .expect("submitted page is retained while writing");
        let namespace = source.namespace.clone();
        let mut inherited = BTreeMap::new();
        match (&mut source.data, finished.request) {
            (Data::Enabled(current), SettingsWrite::Enabled { after, .. }) => *current = after,
            (
                Data::Manifest { scope, fields, .. },
                SettingsWrite::Manifest {
                    fields: patches, ..
                },
            ) => {
                for patch in patches {
                    let stored = fields.get_mut(&patch.id).expect("submitted field");
                    stored.previous = std::mem::replace(&mut stored.applied, patch.after);
                    if let Some(settings) = &mut self.settings {
                        settings.mark_storage_changed(
                            &finished.page,
                            &patch.id,
                            stored.applied != stored.opening,
                        );
                    }
                    if *scope == ConfigScope::AllProjects {
                        inherited.insert(
                            patch.id.clone(),
                            stored.effective(&patch.id, &stored.applied).clone(),
                        );
                    }
                }
            }
            (Data::Scan { applied, .. }, SettingsWrite::Scan { after, .. }) => *applied = after,
            _ => unreachable!("supervisor returns the submitted page write"),
        }
        if inherited.is_empty() {
            return;
        }
        for (page_id, child) in &mut self.plugins.settings_sources {
            if child.namespace != namespace {
                continue;
            }
            let Data::Manifest { scope, fields, .. } = &mut child.data else {
                continue;
            };
            if *scope == ConfigScope::AllProjects {
                continue;
            }
            for (id, fallback) in &inherited {
                if let Some(field) = fields.get_mut(id) {
                    field.fallback.clone_from(fallback);
                    if field.override_value(id, &field.applied).is_none()
                        && let Some(settings) = &mut self.settings
                    {
                        settings.refresh_inherited_value(page_id, id, field.display(fallback));
                    }
                }
            }
        }
    }
}

/// Map the manifest's JSON contract to controls without assuming an enum from a sample value.
fn manifest_fields(
    catalog: &SettingsCatalog,
    installed: &Installed,
    scope: ConfigScope,
) -> (Vec<Field>, BTreeMap<String, StoredField>) {
    let manifest = &installed.release.manifest;
    let namespace = manifest.identity().namespace();
    let mut fields = Vec::new();
    let mut sources = BTreeMap::new();
    for (id, decl) in &manifest.settings {
        if decl.secret
            || (scope != ConfigScope::AllProjects
                && decl.project != matches!(scope, ConfigScope::Project(_)))
        {
            continue;
        }
        let values_at = |scope| {
            catalog
                .scopes
                .get(&scope)
                .and_then(|c| c.plugin(&namespace))
                .map(|p| &p.settings)
        };
        let stored = values_at(scope);
        let keys = std::iter::once(id).chain(&decl.renamed_from);
        let raw: BTreeMap<_, _> = keys
            .clone()
            .filter_map(|key| stored?.get(key).map(|v| (key.clone(), v.clone())))
            .collect();
        let get = |values: Option<&BTreeMap<String, Value>>| {
            keys.clone()
                .find_map(|key| values?.get(key))
                .filter(|value| decl.accepts_value(value))
                .cloned()
        };
        let fallback = if scope == ConfigScope::AllProjects {
            None
        } else {
            get(values_at(ConfigScope::AllProjects))
        }
        .unwrap_or_else(|| decl.default.clone());
        let value = get(stored).unwrap_or_else(|| fallback.clone());
        let (kind, json_text) = control(decl, &value);
        let display = |v: &Value| {
            if json_text {
                Value::String(v.to_string())
            } else {
                v.clone()
            }
        };
        fields.push(Field {
            id: id.clone(),
            label: decl.title.clone(),
            kind,
            value: display(&value),
            default: display(&decl.default),
        });
        sources.insert(
            id.clone(),
            StoredField {
                opening: raw.clone(),
                previous: raw.clone(),
                applied: raw,
                fallback,
                declaration: decl.clone(),
                opening_value: value,
                json_text,
            },
        );
    }
    (fields, sources)
}

fn control(decl: &SettingDecl, current: &Value) -> (Kind, bool) {
    match &decl.default {
        Value::Bool(_) => (Kind::Toggle, false),
        Value::String(_) => (Kind::Text, false),
        Value::Number(n) if n.is_i64() && current.is_i64() => (
            Kind::Number {
                min: i64::MIN,
                max: i64::MAX,
            },
            false,
        ),
        _ => (Kind::Text, true),
    }
}

impl StoredField {
    /// Restore the opening representation (including inherited and renamed keys) when possible.
    fn target(
        &self,
        id: &str,
        value: &Value,
        intent: WriteIntent,
    ) -> Result<BTreeMap<String, Value>> {
        match intent {
            WriteIntent::Cancel => return Ok(self.opening.clone()),
            WriteIntent::Compensate => return Ok(self.previous.clone()),
            WriteIntent::Edit => {}
        }
        let value = if self.json_text {
            serde_json::from_str(value.as_str().context("Expected JSON text")?)?
        } else {
            value.clone()
        };
        Ok(
            if value == self.opening_value && self.effective(id, &self.opening) == &value {
                self.opening.clone()
            } else {
                BTreeMap::from([(id.to_owned(), value)])
            },
        )
    }

    fn override_value<'a>(&self, id: &str, raw: &'a BTreeMap<String, Value>) -> Option<&'a Value> {
        raw.get(id)
            .or_else(|| {
                self.declaration
                    .renamed_from
                    .iter()
                    .find_map(|key| raw.get(key))
            })
            .filter(|value| self.declaration.accepts_value(value))
    }

    fn effective<'a>(&'a self, id: &str, raw: &'a BTreeMap<String, Value>) -> &'a Value {
        self.override_value(id, raw).unwrap_or(&self.fallback)
    }

    fn display(&self, value: &Value) -> Value {
        if self.json_text {
            Value::String(value.to_string())
        } else {
            value.clone()
        }
    }
}

fn single_change<'a>(changes: &'a [(String, Value)], id: &str) -> Result<&'a Value> {
    ensure!(changes.len() == 1 && changes[0].0 == id, "Unknown setting");
    Ok(&changes[0].1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_settings_preserve_explicit_edits_when_the_inherited_value_changed() -> Result<()> {
        let field = StoredField {
            opening: BTreeMap::new(),
            previous: BTreeMap::from([("label".into(), json!("previous"))]),
            applied: BTreeMap::from([("label".into(), json!("current"))]),
            opening_value: json!("original"),
            fallback: json!("new global"),
            declaration: serde_json::from_value(json!({"title":"Label", "default":"original"}))?,
            json_text: false,
        };
        assert_eq!(
            field.target("label", &json!("original"), WriteIntent::Edit)?["label"],
            "original"
        );
        assert!(
            field
                .target("label", &json!("original"), WriteIntent::Cancel)?
                .is_empty()
        );
        assert_eq!(
            field.target("label", &json!("previous"), WriteIntent::Compensate)?,
            field.previous
        );
        Ok(())
    }
}
