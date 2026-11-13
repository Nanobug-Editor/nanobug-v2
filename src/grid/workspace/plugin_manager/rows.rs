//! Character rows and their corresponding keyboard or pointer actions.

use super::{Detail, Input, Page, PluginUi, Prompt, comparison::Source};
use nanobug::{
    i18n,
    plugins::{
        file_operations::FileStepState,
        package::{Installed, Manifest, Provider},
        session::{
            Action, BackgroundToolPhase, ConfigEdit, ConfigScope, Key, PROCESS_MEMORY_LABEL, Rule,
            State, ToolOperationState,
        },
    },
};
use nanobug_plugin_sdk::{Instance, Permission};

pub(super) const HELP: &[&str] = &[
    "1–4           Switch page or detail tab",
    "/             Search the plugin list",
    "j / k         Next / previous row",
    "↑ / ↓         Next / previous row",
    "Home / End    First / last row",
    "Enter         Open or confirm selected row",
    "q / Esc       Go back or cancel",
    "?             Close this help window",
];

pub(super) enum Target {
    None,
    Details(String),
    Detail(Detail),
    Compare(Source),
    Action(Action),
    Uninstall(String),
    Zip,
    Development,
    Scope,
    Prompt(Input),
    Surface(Key),
    Retained,
    CleanCredentials,
    CleanLocal,
    RetryDraftClose,
    ExportDraft,
    DiscardDraftClose,
    CancelDraftClose,
    ApplyCredential,
    EditCredential,
    DiscardCredential,
    Command(Key, String),
    ApplyEdits,
    ApplyFiles,
    Formatter(Key),
    Completion(String),
    /// An accepted completion that replaces a byte range in the current document.
    CompletionEdit(nanobug_plugin_sdk::Edit),
    Diagnostic(usize),
    /// Jump to an outline item in the same document opening and revision.
    Symbol(usize),
}

pub(super) struct Row {
    pub label: String,
    pub description: String,
    pub target: Target,
    /// The version and status of a package in the Installed or Marketplace list.
    pub(super) package: Option<PackageColumns>,
}

/// Table cells for a package row. Action-only rows can use the full width.
pub(super) struct PackageColumns {
    pub(super) version: String,
    pub(super) status: &'static str,
    /// Whether the local catalog contains a newer release of this identity.
    pub(super) update_available: bool,
}

pub(super) fn row(label: impl Into<String>, description: impl Into<String>, target: Target) -> Row {
    let label = label.into();
    Row {
        label: if label.contains(['\r', '\n']) {
            label.lines().collect::<Vec<_>>().join(" ")
        } else {
            label
        },
        description: description.into(),
        target,
        package: None,
    }
}

fn installed(ui: &PluginUi) -> Option<&Installed> {
    let namespace = ui.manager.detail.as_ref()?;
    ui.view
        .installed
        .iter()
        .find(|record| record.release.manifest.identity().namespace() == *namespace)
}

pub(super) fn manifest(ui: &PluginUi) -> Option<&Manifest> {
    installed(ui)
        .map(|record| &record.release.manifest)
        .or_else(|| {
            ui.view
                .retained
                .get(ui.manager.detail.as_ref()?)
                .map(|release| &release.manifest)
        })
}

pub(super) fn build(ui: &PluginUi) -> Vec<Row> {
    if ui.credential_waiting {
        return vec![row("Saving credential…", "", Target::None)];
    }
    if ui.credential_draft.is_some() {
        return vec![
            row(
                "Apply credential",
                "Save the new value in the system credential store.",
                Target::ApplyCredential,
            ),
            row(
                "Continue editing",
                "Keep the new value in this session.",
                Target::EditCredential,
            ),
            row(
                "Discard pending value",
                "Keep the currently saved credential.",
                Target::DiscardCredential,
            ),
        ];
    }
    if let Some(choices) = &ui.formatting.choices {
        return choices.rows();
    }
    if ui.file_busy {
        return vec![row("Applying file operations…", "", Target::None)];
    }
    if let Some(plan) = &ui.file_plan {
        return plan.rows();
    }
    if !ui.file_report.is_empty() {
        return ui
            .file_report
            .iter()
            .map(|line| row(line, line, Target::None))
            .collect();
    }
    if let Some(results) = &ui.language {
        return results.rows();
    }
    if let Some(outline) = &ui.outline {
        return outline.rows();
    }
    if let Some(plan) = &ui.edit_plan {
        return plan.rows();
    }
    if ui.edit_check.is_some() {
        return vec![row(
            "Checking file versions…",
            "Press Esc to cancel.",
            Target::None,
        )];
    }
    if let Some(close) = &ui.close_draft {
        if close.waiting {
            return vec![row(
                if close.draining {
                    "Waiting for plugin operations to finish…"
                } else {
                    "Saving confirmed plugin inputs…"
                },
                "",
                Target::None,
            )];
        }
        if close.draining {
            return vec![
                row(
                    "Wait for operations",
                    close
                        .error
                        .as_deref()
                        .unwrap_or("Keep the editor open while writes finish."),
                    Target::RetryDraftClose,
                ),
                row(
                    "Cancel close",
                    "Return to editing.",
                    Target::CancelDraftClose,
                ),
            ];
        }
        return vec![
            row(
                "Retry saving",
                close
                    .error
                    .as_deref()
                    .unwrap_or("Plugin inputs could not be saved."),
                Target::RetryDraftClose,
            ),
            row(
                "Export inputs and close",
                "Save a copy of confirmed ordinary input to another path.",
                Target::ExportDraft,
            ),
            row(
                "Cancel close",
                "Keep the editor open and keep inputs in memory.",
                Target::CancelDraftClose,
            ),
            row(
                "Discard unsaved plugin inputs and close",
                "Keeps the last successfully saved input.",
                Target::DiscardDraftClose,
            ),
        ];
    }
    if ui.view.waiting_for_operations {
        return vec![row(
            "Later",
            "Keep the current plugin while its operations finish.",
            Target::Action(Action::CancelInstall),
        )];
    }
    if let Some(namespace) = &ui.manager.detail {
        if ui.manager.detail_page == Detail::Operations {
            return operations(ui, namespace);
        }
        let Some(record) = installed(ui) else {
            return retained(ui, namespace);
        };
        return match ui.manager.detail_page {
            Detail::Overview => overview(ui, namespace, record),
            Detail::Modules => modules(ui, namespace, record),
            Detail::Rules => config(ui, namespace, &record.release.manifest, true),
            Detail::Settings => config(ui, namespace, &record.release.manifest, false),
            Detail::Logs => ui
                .view
                .logs
                .get(namespace)
                .into_iter()
                .flatten()
                .rev()
                .map(|line| row(line, line, Target::None))
                .collect(),
            Detail::Operations => unreachable!("handled before installed lookup"),
            Detail::Compare => compare(ui),
            Detail::Cleanup => Vec::new(),
        };
    }
    let query = ui.manager.search.to_lowercase();
    let matches = |name: &str, id: &str| {
        query.is_empty() || name.to_lowercase().contains(&query) || id.contains(&query)
    };
    match ui.manager.page {
        Page::Installed => {
            let mut rows = ui
                .view
                .installed
                .iter()
                .filter_map(|record| {
                    let manifest = &record.release.manifest;
                    let (title, description) = display(manifest);
                    if !matches(title, &manifest.id) && !matches(&manifest.name, &manifest.id) {
                        return None;
                    }
                    let identity = manifest.identity();
                    let status = if record.enabled {
                        "plugin_manager.status_enabled"
                    } else {
                        "plugin_manager.status_disabled"
                    };
                    let mut item = row(title, description, Target::Details(identity.namespace()));
                    item.package = Some(PackageColumns {
                        version: manifest.version.to_string(),
                        status: i18n::text(status),
                        update_available: ui.view.catalog.iter().any(|entry| {
                            entry.identity == identity && entry.version > manifest.version
                        }),
                    });
                    Some(item)
                })
                .collect::<Vec<_>>();
            if ui.view.core_only {
                rows.insert(
                    0,
                    row(
                        "Resume plugins",
                        "Resume the installed plugins in this editor session.",
                        Target::Action(Action::ResumePlugins),
                    ),
                );
            }
            let retained =
                ui.view
                    .retained
                    .iter()
                    .filter(|(namespace, release)| {
                        !ui.view.installed.iter().any(|record| {
                            record.release.manifest.identity().namespace() == **namespace
                        }) && matches(&release.manifest.name, &release.manifest.id)
                    })
                    .collect::<Vec<_>>();
            if !retained.is_empty() {
                rows.push(row(
                    if ui.manager.retained_open {
                        "▾ Retained data"
                    } else {
                        "▸ Retained data"
                    },
                    "Settings and input saved by uninstalled plugins.",
                    Target::Retained,
                ));
                if ui.manager.retained_open {
                    rows.extend(retained.into_iter().map(|(namespace, release)| {
                        row(
                            format!("  {}", release.manifest.name),
                            &release.manifest.description,
                            Target::Details(namespace.clone()),
                        )
                    }));
                }
            }
            rows
        }
        Page::Marketplace => ui
            .view
            .catalog
            .iter()
            .filter_map(|entry| {
                if !matches(&entry.identity.name, &entry.identity.id) {
                    return None;
                }
                let installed = ui
                    .view
                    .installed
                    .iter()
                    .find(|record| record.release.manifest.identity() == entry.identity);
                let update_available =
                    installed.is_some_and(|record| record.release.manifest.version < entry.version);
                let (status, target) = if installed.is_some() && !update_available {
                    (
                        "plugin_manager.status_installed",
                        Target::Details(entry.identity.namespace()),
                    )
                } else {
                    (
                        if update_available {
                            "plugin_manager.status_update"
                        } else {
                            "plugin_manager.status_install"
                        },
                        Target::Action(Action::InspectCatalog(entry.clone())),
                    )
                };
                let mut item = row(&entry.identity.name, &entry.description, target);
                item.package = Some(PackageColumns {
                    version: entry.version.to_string(),
                    status: i18n::text(status),
                    update_available: false,
                });
                Some(item)
            })
            .collect(),
        Page::Zip => {
            let mut rows = vec![row(
                i18n::text("plugin_manager.choose_zip"),
                i18n::text("plugin_manager.choose_zip_description"),
                Target::Zip,
            )];
            if ui.view.development {
                rows.push(row(
                    i18n::text("plugin_manager.load_build"),
                    i18n::text("plugin_manager.load_build_description"),
                    Target::Development,
                ));
            }
            rows
        }
    }
}

fn retained(ui: &PluginUi, namespace: &str) -> Vec<Row> {
    let Some(release) = ui.view.retained.get(namespace) else {
        return vec![row("Plugin is not installed", "", Target::None)];
    };
    if ui.manager.detail_page == Detail::Cleanup {
        let mut rows = vec![row(
            if ui.manager.clean_local {
                "✓ Local settings and input drafts"
            } else {
                "○ Local settings and input drafts"
            },
            "The project's .nb files and activation rules are kept.",
            Target::CleanLocal,
        )];
        if ui.view.credential_owners.contains(namespace) {
            rows.push(row(
                if ui.manager.clean_credentials {
                    "✓ Include saved credentials"
                } else {
                    "○ Include saved credentials"
                },
                "Turn this on to also remove the plugin's local login information.",
                Target::CleanCredentials,
            ));
        }
        rows.push(row(
            "Clear selected local data",
            "Delete the data shown above.",
            Target::Action(Action::CleanLocalData {
                namespace: namespace.into(),
                local: ui.manager.clean_local,
                credentials: ui.manager.clean_credentials,
            }),
        ));
        return rows;
    }
    vec![
        row(
            "Install",
            "Check the previous source ZIP again.",
            Target::Action(Action::InspectZip(release.source.clone())),
        ),
        row(
            "Clear local data…",
            "Review local data and choose whether to include credentials.",
            Target::Detail(Detail::Cleanup),
        ),
        review_operations(),
    ]
}

fn review_operations() -> Row {
    row(
        "External operations…",
        "Check the saved outcomes of native commands.",
        Target::Detail(Detail::Operations),
    )
}

fn operations(ui: &PluginUi, namespace: &str) -> Vec<Row> {
    let mut rows = vec![row(
        "Read operation results",
        "Read saved command outcomes again.",
        Target::Action(Action::ReviewToolOperations(namespace.into())),
    )];
    if let Some((owner, operations)) = &ui.view.tool_operations
        && owner == namespace
    {
        for operation in operations {
            let state = match operation.state {
                ToolOperationState::Pending => "Result pending confirmation".into(),
                ToolOperationState::NotStarted => "Did not start".into(),
                ToolOperationState::Exited => operation.code.map_or_else(
                    || "Process exited".into(),
                    |code| format!("Process exited with code {code}"),
                ),
            };
            rows.push(row(
                format!("{}  ·  {state}", operation.tool),
                &operation.id,
                Target::None,
            ));
        }
    }
    if let Some((owner, operations)) = &ui.view.file_operations
        && owner == namespace
    {
        for operation in operations {
            for step in &operation.steps {
                let state = match step.state {
                    FileStepState::NotRun => "Did not start",
                    FileStepState::Pending => "Result pending confirmation",
                    FileStepState::Done => "Done",
                    FileStepState::Failed => "Failed",
                };
                let evidence = if step.destination_matches {
                    format!(
                        "{} · Current file matches the requested destination",
                        operation.id
                    )
                } else {
                    operation.id.clone()
                };
                rows.push(row(
                    format!("{}  ·  {state}", step.label),
                    evidence,
                    Target::None,
                ));
            }
        }
        if rows.len() == 1
            && ui
                .view
                .tool_operations
                .as_ref()
                .is_some_and(|(owner, _)| owner == namespace)
        {
            rows.push(row("No recorded operations", "", Target::None));
        }
    }
    rows
}

fn display(manifest: &Manifest) -> (&str, &str) {
    let language = if nanobug::i18n::locale() == nanobug::i18n::Locale::SimplifiedChinese {
        "zh-CN"
    } else {
        "en"
    };
    manifest.display(language)
}

fn overview(ui: &PluginUi, namespace: &str, record: &Installed) -> Vec<Row> {
    let manifest = &record.release.manifest;
    let mut rows = vec![
        row(
            format!(
                "{}  {}  ·  {}",
                manifest.name, manifest.version, manifest.id
            ),
            display(manifest).1,
            Target::None,
        ),
        row(
            format!(
                "Plugin API {}  ·  Nanobug {}",
                manifest.api, manifest.engines.nanobug
            ),
            display(manifest).1,
            Target::None,
        ),
        row(
            ui.view.workers.get(namespace).map_or_else(
                || "Runtime inactive".into(),
                |pid| format!("Runtime PID {pid}"),
            ),
            format!("{} declared modules", manifest.modules.len()),
            Target::None,
        ),
        row(
            "Settings",
            "Edit ordinary settings declared by the plugin.",
            Target::Detail(Detail::Settings),
        ),
        row(
            if record.enabled { "Disable" } else { "Enable" },
            "",
            Target::Action(Action::SetEnabled(namespace.into(), !record.enabled)),
        ),
        row(
            "Retry",
            "Retry paused rules or modules.",
            Target::Action(Action::Retry(namespace.into())),
        ),
    ];
    if let Some(stats) = ui.view.runtime_status.get(namespace) {
        rows.insert(
            3,
            row(
                format!(
                    "{} ready · {} pending · {} completed · {} errors",
                    stats.ready_modules, stats.pending, stats.completed, stats.errors
                ),
                stats.private_bytes.map_or_else(
                    || format!("Last request took {} ms", stats.last_ms),
                    |bytes| {
                        format!(
                            "Last request took {} ms · {:.1} MiB {PROCESS_MEMORY_LABEL}",
                            stats.last_ms,
                            bytes as f64 / 1_048_576.
                        )
                    },
                ),
                Target::None,
            ),
        );
        if stats.canvas_bytes != 0 {
            rows.insert(
                4,
                row(
                    format!(
                        "{:.2} MiB of prepared drawing data",
                        stats.canvas_bytes as f64 / 1_048_576.
                    ),
                    "Host memory for current plugin scenes, measured separately from the runtime.",
                    Target::None,
                ),
            );
        }
        let mut model_bytes = 0;
        let mut layout_bytes = 0;
        let mut shared_document_bytes = 0;
        for (key, usage) in &ui.view.host_resources {
            if key.namespace == namespace {
                model_bytes += usage.model_bytes;
                layout_bytes += usage.layout_queue_bytes;
                shared_document_bytes += usage.shared_document_bytes;
            }
        }
        if ui
            .surface
            .as_ref()
            .is_some_and(|key| key.namespace == namespace)
            && let Some(field) = &ui.native_field
        {
            layout_bytes += field.owned_layout_bytes();
        }
        if model_bytes + layout_bytes != 0 {
            rows.push(row(
                format!("Host models and layout · {} KiB", (model_bytes + layout_bytes).div_ceil(1024)),
                "Scene and layout data attributed to this plugin. Prepared drawings are shown separately.",
                Target::None,
            ));
        }
        if shared_document_bytes != 0 {
            rows.push(row(
                format!(
                    "Shared plugin data · {} KiB",
                    ui.view.shared_plugin_resources.div_ceil(1024)
                ),
                "Document snapshots are counted once across the session.",
                Target::None,
            ));
        }
        if stats.peak_request_bytes != 0 || stats.peak_reply_wire_bytes != 0 {
            rows.push(row(
                format!(
                    "Messages · {} KiB sending · {} KiB received",
                    stats.request_bytes.div_ceil(1024),
                    stats.reply_wire_bytes.div_ceil(1024)
                ),
                format!(
                    "Peak {} KiB sending · {} KiB received. Last pipe wait {} µs.",
                    stats.peak_request_bytes.div_ceil(1024),
                    stats.peak_reply_wire_bytes.div_ceil(1024),
                    stats.last_write_wait_us,
                ),
                Target::None,
            ));
        }
        if let Some(timing) = stats.last_worker_timing {
            rows.push(row(
                format!(
                    "Last worker request · {} µs waiting · {} µs computing",
                    timing.queue_us, timing.compute_us
                ),
                format!(
                    "Last Canvas preparation {} µs",
                    stats.last_canvas_prepare_us.unwrap_or(0)
                ),
                Target::None,
            ));
        }
        let sampling = ui.view.sampling.contains(namespace);
        rows.push(row(
            if sampling {
                "Stop detailed sampling"
            } else {
                "Start detailed sampling"
            },
            "Include per-request response times in Logs.",
            Target::Action(Action::SampleRuntime(namespace.into(), !sampling)),
        ));
    }
    if ui.view.development && record.release.development {
        rows.push(row(
            "Reload local build",
            "Review the latest complete files before replacing the running build.",
            Target::Action(Action::InspectDevelopment(record.release.source.clone())),
        ));
    }
    if let Some(entry) = ui
        .view
        .catalog
        .iter()
        .find(|entry| entry.identity == manifest.identity() && entry.version > manifest.version)
    {
        rows.push(row(
            format!("Update to {}", entry.version),
            &entry.description,
            Target::Action(Action::InspectCatalog(entry.clone())),
        ));
    }
    rows.push(row(
        "Uninstall…",
        "Removes the plugin package. Local settings and inputs are kept.",
        Target::Uninstall(namespace.into()),
    ));
    if !manifest.tools.is_empty() || manifest.permissions.contains(&Permission::ChangeFiles) {
        rows.push(review_operations());
    }
    rows
}

fn module_labels<'a>(state: Option<&'a State>, rule: Option<&'a str>) -> (&'a str, &'a str) {
    match state {
        Some(State::Failed(reason)) => (
            if rule.is_some() {
                "Rule error"
            } else {
                "Failed"
            },
            rule.unwrap_or(reason),
        ),
        Some(state) => (state.label(), rule.unwrap_or_else(|| state.label())),
        None => ("Preparing", "Preparing"),
    }
}

fn modules(ui: &PluginUi, namespace: &str, record: &Installed) -> Vec<Row> {
    let mut rows = Vec::new();
    if !record.release.manifest.tools.is_empty()
        && let Some(project) = ui.active
    {
        let allowed = ui.view.project_execution.get(&project) == Some(&true);
        rows.push(row(
            if allowed { "Block project execution" } else { "Allow project execution" },
            "Native tools run with the editor's operating system permissions and may run project code.",
            Target::Action(Action::SetProjectExecution(project, !allowed)),
        ));
    }
    for module in &record.release.manifest.modules {
        if ui.projects.is_empty() {
            rows.push(row(
                &module.id,
                "Open a project to see this module's runtime status.",
                Target::None,
            ));
        }
        for project in &ui.projects {
            let key = Key {
                namespace: namespace.into(),
                instance: Instance {
                    project: project.id,
                    module: module.id.clone(),
                },
            };
            let (state, description) = module_labels(
                ui.view.modules.get(&key),
                ui.view.rule_warnings.get(&key).map(String::as_str),
            );
            let title = project
                .root
                .file_name()
                .unwrap_or(project.root.as_os_str())
                .to_string_lossy();
            let target = if ui.active == Some(project.id) && ui.view.surfaces.contains_key(&key) {
                Target::Surface(key.clone())
            } else {
                Target::None
            };
            rows.push(row(
                format!("{}  ·  {title}  ·  {state}", module.id),
                description,
                target,
            ));
            if let Some(tools) = ui.view.background_tools.get(&key) {
                for (name, status) in tools {
                    let phase = match status.phase {
                        BackgroundToolPhase::Starting => "Starting",
                        BackgroundToolPhase::Synchronizing => "Synchronizing",
                        BackgroundToolPhase::Ready => "Ready",
                        BackgroundToolPhase::Recovering => "Recovering",
                        BackgroundToolPhase::Suspended => "Suspended",
                        BackgroundToolPhase::Paused => "Paused",
                    };
                    let retry = status.phase == BackgroundToolPhase::Paused;
                    let description = if status.memory_paused {
                        "Paused after exceeding the memory budget. Enter to retry.".into()
                    } else if retry {
                        "Enter to retry this tool.".into()
                    } else {
                        status.private_bytes.map_or_else(
                            || phase.to_owned(),
                            |bytes| {
                                format!(
                                    "{phase} · {:.1} MiB {PROCESS_MEMORY_LABEL}",
                                    bytes as f64 / 1_048_576.
                                )
                            },
                        )
                    };
                    rows.push(row(
                        format!("  {name}  ·  {phase}"),
                        description,
                        if retry {
                            Target::Action(Action::RetryBackgroundTool(key.clone(), name.clone()))
                        } else {
                            Target::None
                        },
                    ));
                }
            }
            if ui.active == Some(project.id) && state == "Ready" {
                for command in record
                    .release
                    .manifest
                    .commands
                    .iter()
                    .filter(|command| command.module == module.id)
                {
                    rows.push(row(
                        format!("  {}", command.title),
                        &module.id,
                        Target::Command(key.clone(), command.id.clone()),
                    ));
                }
            }
        }
    }
    rows
}

fn config(ui: &PluginUi, namespace: &str, manifest: &Manifest, rules: bool) -> Vec<Row> {
    let Some(scope) = ui.scope() else {
        return Vec::new();
    };
    let mut rows = vec![row(
        format!("Scope  ·  {}", ui.scope_label()),
        "Enter to switch between the current project and all projects.",
        Target::Scope,
    )];
    let Some(view) = ui.view.configurations.get(&scope) else {
        return rows;
    };
    let config = view.draft.as_ref().unwrap_or(&view.applied);
    let values = config.plugin(namespace);
    let owner = manifest.identity();
    if let Some(error) = &view.error {
        rows.push(row("Configuration error", error, Target::None));
    }
    if let Some(issue) = view.apply_issue {
        rows.push(row("Last Apply", issue, Target::None));
    }
    if view.external.is_some() || view.replacement_conflict {
        rows.push(row(
            i18n::text("plugin_manager.compare"),
            i18n::text(if view.replacement_conflict {
                "plugin_manager.compare_displaced"
            } else {
                "plugin_manager.compare_changed"
            }),
            Target::Detail(Detail::Compare),
        ));
    }
    if rules {
        for module in &manifest.modules {
            let override_value = values.and_then(|plugin| plugin.rules.get(&module.id));
            let (label, text) = match override_value {
                Some(Rule::Always) => ("true", "true"),
                Some(Rule::Never) => ("false", "false"),
                Some(Rule::Script(source)) => ("JS rule", source.as_str()),
                None => ("Inherited", ""),
            };
            rows.push(row(
                format!("{}  ·  {label}", module.id),
                "Enter true, false, inherit, or a JS function body.",
                Target::Prompt(Input {
                    kind: Prompt::Rule {
                        scope,
                        owner: owner.clone(),
                        module: module.id.clone(),
                    },
                    value: text.into(),
                }),
            ));
        }
    } else {
        for (key, error) in &ui.view.settings_errors {
            if key.namespace == namespace && ui.active == Some(key.instance.project) {
                rows.push(row(
                    format!("Retry saved settings · {}", key.instance.module),
                    error,
                    Target::Action(Action::RetrySettings(key.clone())),
                ));
            }
        }
        if scope == ConfigScope::AllProjects {
            if ui.view.development && installed(ui).is_some_and(|record| record.release.development)
            {
                let settings = ui
                    .view
                    .developer_settings
                    .get(namespace)
                    .cloned()
                    .unwrap_or_default();
                let value = settings
                    .source
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned())
                    .unwrap_or_default();
                rows.push(row(format!("Cargo project · {}", if value.is_empty() { "Choose a path…" } else { &value }),
                    "The selected Cargo project can run build scripts with your current system permissions.",
                    Target::Prompt(Input { kind: Prompt::DevSource { namespace: namespace.into(), settings: settings.clone() }, value })));
                if settings.source.is_some() {
                    let mut toggled = settings.clone();
                    toggled.automatic = !settings.automatic;
                    rows.push(row(format!("Auto build and reload · {}", if settings.automatic { "On" } else { "Off" }),
                        "Only this development profile watches the selected source. Failed builds keep the running plugin.",
                        Target::Action(Action::SetDeveloperSettings(namespace.into(), toggled))));
                    rows.push(row(
                        "Build and reload now",
                        "Compile the selected Cargo project and load the finished WASM.",
                        Target::Action(Action::BuildDevelopment(namespace.into())),
                    ));
                }
            }
            for command in &manifest.commands {
                let current = config
                    .shortcuts
                    .get(namespace)
                    .and_then(|commands| commands.get(&command.id));
                let value = current.cloned().unwrap_or_else(|| "inherit".into());
                let effective = current
                    .or(command.key.as_ref())
                    .filter(|key| !key.is_empty())
                    .map_or("Unassigned", String::as_str);
                rows.push(row(
                    format!("Shortcut · {} · {effective}", command.title),
                    "Global mapping for every project. Apply to change it.",
                    Target::Prompt(Input {
                        kind: Prompt::Shortcut {
                            namespace: namespace.into(),
                            command: command.id.clone(),
                        },
                        value,
                    }),
                ));
            }
        }
        if !matches!(scope, ConfigScope::ProjectLocal(_))
            && manifest
                .modules
                .iter()
                .any(|module| module.provides.contains(&Provider::Format))
            && let Some(extension) = ui
                .active
                .and_then(|active| ui.snapshots.get(&active))
                .and_then(|doc| doc.path.as_deref())
                .and_then(super::formatting::file_extension)
        {
            rows.push(row(
                format!("Use this formatter for .{extension}"),
                "Sets the default by plugin identity. Apply to use it.",
                Target::Action(Action::EditConfig(
                    scope,
                    ConfigEdit::Formatter {
                        extension: extension.clone(),
                        provider: owner.clone(),
                    },
                )),
            ));
            let prior = config
                .formatters
                .get(&extension)
                .and_then(|formatting| formatting.on_save)
                .unwrap_or(false);
            rows.push(row(
                format!(
                    "Format .{extension} on save  ·  {}",
                    if prior { "On" } else { "Off" }
                ),
                "Toggle the save setting. Apply to use it.",
                Target::Action(Action::EditConfig(
                    scope,
                    ConfigEdit::ToggleFormatOnSave(extension),
                )),
            ));
        }
        for (id, decl) in &manifest.settings {
            if decl.secret {
                if scope == ConfigScope::AllProjects {
                    let rejected = ui.view.credential_errors.iter().any(|(key, slots)| {
                        key.namespace == namespace
                            && ui.active == Some(key.instance.project)
                            && slots.contains(id)
                    });
                    rows.push(row(
                        &decl.title,
                        if rejected {
                            "Authentication rejected the saved credential. Enter a new value to update it."
                        } else {
                            "Enter a new credential, then choose Apply to save it."
                        },
                        Target::Prompt(Input {
                            kind: Prompt::Credential {
                                namespace: namespace.into(),
                                slot: id.clone(),
                                value: zeroize::Zeroizing::new(String::with_capacity(2048)),
                            },
                            value: String::new(),
                        }),
                    ));
                    rows.push(row(
                        format!("Remove saved {}", decl.title),
                        "Delete this local credential.",
                        Target::Action(Action::DeleteCredential {
                            namespace: namespace.into(),
                            slot: Some(id.clone()),
                        }),
                    ));
                }
                continue;
            }
            if scope != ConfigScope::AllProjects
                && decl.project != matches!(scope, ConfigScope::Project(_))
            {
                continue;
            }
            let current = values.and_then(|plugin| {
                plugin.settings.get(id).or_else(|| {
                    decl.renamed_from
                        .iter()
                        .find_map(|old| plugin.settings.get(old))
                })
            });
            let text = current.map(ToString::to_string).unwrap_or_default();
            rows.push(row(
                format!(
                    "{}  ·  {}",
                    decl.title,
                    if text.is_empty() { "Inherited" } else { &text }
                ),
                format!(
                    "{} Default {}. Enter a JSON value or inherit.",
                    decl.description, decl.default
                )
                .trim_start()
                .to_owned(),
                Target::Prompt(Input {
                    kind: Prompt::Setting {
                        scope,
                        owner: owner.clone(),
                        id: id.clone(),
                        renamed_from: decl.renamed_from.clone(),
                        original: current.cloned(),
                    },
                    value: text,
                }),
            ));
            if let Some(settings) = values.map(|plugin| &plugin.settings)
                && !settings.contains_key(id)
                && let Some(value) = current.filter(|value| decl.accepts_value(value))
            {
                rows.push(row(
                    format!("Prepare conversion · {}", decl.title),
                    "Copy the compatible old value to the current setting. Apply to save the draft.",
                    Target::Action(Action::EditConfig(
                        scope,
                        ConfigEdit::Setting {
                            owner: owner.clone(),
                            id: id.clone(),
                            renamed_from: decl.renamed_from.clone(),
                            original: Some(value.clone()),
                            value: Some(value.clone()),
                        },
                    )),
                ));
            }
        }
        if let Some(project) = ui.active {
            for module in manifest.modules.iter().filter(|module| module.settings_ui) {
                let key = Key {
                    namespace: namespace.into(),
                    instance: Instance {
                        project,
                        module: module.id.clone(),
                    },
                };
                if ui.view.surfaces.contains_key(&key) {
                    rows.push(row(
                        "Open custom settings",
                        "The plugin can prepare settings drafts in its own panel.",
                        Target::Surface(key),
                    ));
                }
            }
        }
        if let Some(plugin) = values {
            for (id, value) in &plugin.settings {
                let declaration = manifest.settings.get(id).or_else(|| {
                    manifest
                        .settings
                        .values()
                        .find(|setting| setting.renamed_from.contains(id))
                });
                if declaration.is_none_or(|setting| {
                    setting.secret
                        || !setting.accepts_value(value)
                        || scope != ConfigScope::AllProjects
                            && setting.project != matches!(scope, ConfigScope::Project(_))
                }) {
                    rows.push(row(
                        format!("Kept old setting · {id}"),
                        "Review this value before replacing it with a compatible setting.",
                        Target::None,
                    ));
                }
            }
        }
    }
    if view.draft.is_some() && view.external.is_none() && !view.replacement_conflict {
        rows.push(row(
            "Apply",
            "Validate, save and use the current draft.",
            Target::Action(Action::ApplyConfig(scope)),
        ));
    }
    if let Some(previous) = &view.previous {
        let previous = previous.plugin(namespace);
        let changed = if rules {
            previous
                .map(|old| &old.rules)
                .filter(|value| !value.is_empty())
                != values
                    .map(|current| &current.rules)
                    .filter(|value| !value.is_empty())
        } else {
            previous
                .map(|old| &old.settings)
                .filter(|value| !value.is_empty())
                != values
                    .map(|current| &current.settings)
                    .filter(|value| !value.is_empty())
        };
        if changed {
            rows.push(row(
                "Restore previous values to draft",
                "Review the restored values and Apply to use them.",
                Target::Action(Action::EditConfig(
                    scope,
                    ConfigEdit::Restore {
                        owner,
                        rules,
                        revision: view.revision,
                    },
                )),
            ));
        }
    }
    rows
}

fn compare(ui: &PluginUi) -> Vec<Row> {
    let Some(comparison) = &ui.manager.comparison else {
        return Vec::new();
    };
    let view = comparison.config();
    if view.external.is_none() && !view.replacement_conflict {
        return vec![row(
            i18n::text("plugin_manager.compare_none"),
            "",
            Target::None,
        )];
    }
    let preview = |source: Source| {
        row(
            i18n::text(source.title()),
            i18n::text("plugin_manager.compare_open"),
            Target::Compare(source),
        )
    };
    let mut rows = vec![preview(Source::Local)];
    if view.external.is_some() {
        rows.push(preview(Source::External));
    }
    if view.preserved_external.is_some() {
        rows.push(preview(Source::Preserved));
    }
    if view.replacement_conflict
        && let Some(error) = &view.error
    {
        rows.push(row(
            i18n::text("plugin_manager.compare_error"),
            error,
            Target::None,
        ));
    }
    if view.external.is_some() {
        rows.push(row(
            i18n::text("plugin_manager.compare_use_external"),
            i18n::text("plugin_manager.compare_review_draft"),
            Target::Action(Action::ResolveConfig(comparison.scope, view.revision, true)),
        ));
    }
    if !view.replacement_conflict || view.preserved_external.is_some() {
        rows.push(row(
            i18n::text("plugin_manager.compare_keep_draft"),
            i18n::text("plugin_manager.compare_review_draft"),
            Target::Action(Action::ResolveConfig(
                comparison.scope,
                view.revision,
                false,
            )),
        ));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_rows_report_rule_and_runtime_state_separately() {
        assert_eq!(module_labels(None, None), ("Preparing", "Preparing"));
        assert_eq!(module_labels(Some(&State::Ready), None), ("Ready", "Ready"));
        assert_eq!(
            module_labels(Some(&State::Unmatched), None),
            ("Condition not met", "Condition not met")
        );
        let failed = State::Failed("wasm stopped".into());
        assert_eq!(
            module_labels(Some(&failed), None),
            ("Failed", "wasm stopped")
        );
        let failed = State::Failed("test rule returned an error".into());
        assert_eq!(
            module_labels(Some(&failed), Some("test rule returned an error")),
            ("Rule error", "test rule returned an error")
        );
        assert_eq!(
            module_labels(Some(&State::Ready), Some("Rule worker recovering")),
            ("Ready", "Rule worker recovering")
        );
    }
}
