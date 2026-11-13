//! Version-bound format requests and the shared save/manual deadline.

use super::{
    Workspace,
    rows::{Row, Target, row},
};
use gpui::{Context, Window};
use nanobug::{
    i18n,
    plugins::{
        edits,
        package::Provider,
        session::{Action, CallResult, ConfigScope, Formatting, Key, State},
    },
};
use nanobug_plugin_sdk::{Instance, Permission, PluginEvent};
use std::{
    collections::BTreeMap,
    ops::Range,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const SAVE_WAIT: Duration = Duration::from_secs(1);
const MANUAL_WAIT: Duration = Duration::from_secs(10);

type Result = std::result::Result<CallResult, String>;

/// Only the latest task for an opening may apply a result or fulfill a waiting save.
#[derive(Default)]
pub(super) struct Tasks {
    next: u64,
    pending: BTreeMap<u64, Pending>,
    selected: BTreeMap<(u64, String), Key>,
    pub(super) choices: Option<Choices>,
}

pub(super) struct Choices {
    version: (u64, u64),
    scope: u64,
    extension: String,
    selection: Option<Range<usize>>,
    providers: Vec<(Key, String)>,
}

impl Choices {
    pub(super) fn rows(&self) -> Vec<Row> {
        self.providers
            .iter()
            .map(|(key, name)| {
                row(
                    name,
                    "Select a formatter for this session.",
                    Target::Formatter(key.clone()),
                )
            })
            .collect()
    }
}

struct Pending {
    token: u64,
    project: u64,
    revision: u64,
    deadline: Instant,
    provider: Key,
    generation: u64,
    selection: Option<Range<usize>>,
    save: Option<Save>,
}

impl Pending {
    /// A save can join the same job and only shorten its existing deadline.
    fn attach_save(&mut self, save: Save, requested: Instant) {
        self.save = Some(save);
        self.deadline = self.deadline.min(requested + SAVE_WAIT);
    }

    fn check_deadline(&self, now: Instant) -> anyhow::Result<()> {
        anyhow::ensure!(now < self.deadline, "Formatter timed out");
        Ok(())
    }

    /// Compare the result to the document and provider originally dispatched by the editor.
    fn check_reply(
        &self,
        opening: u64,
        generation: Option<u64>,
        result: &CallResult,
        now: Instant,
    ) -> anyhow::Result<()> {
        self.check_deadline(now)?;
        anyhow::ensure!(
            result.version == Some((opening, self.revision)),
            "Document changed while the formatter was working"
        );
        anyhow::ensure!(
            generation == Some(self.generation) && result.generation == self.generation,
            "Formatter changed while the result was pending"
        );
        Ok(())
    }
}

struct Save {
    path: Option<PathBuf>,
    quit: bool,
}

impl Workspace {
    /// Resolve the active file's project overrides before falling back to global values.
    fn formatting_preferences(&self, project: u64, extension: &str) -> Formatting {
        let mut options = Formatting::default();
        for scope in [ConfigScope::Project(project), ConfigScope::AllProjects] {
            if let Some(config) = self.plugins.view.configurations.get(&scope)
                && let Some(format) = config.applied.formatters.get(extension)
            {
                options.provider = options.provider.or_else(|| format.provider.clone());
                options.on_save = options.on_save.or(format.on_save);
            }
        }
        options
    }

    fn formatters(&self, project: u64, options: &Formatting) -> Vec<(Key, String)> {
        let mut providers = Vec::new();
        for installed in self.plugins.view.installed.iter() {
            let manifest = &installed.release.manifest;
            if !installed.enabled
                || !installed.grants.contains(&Permission::ReadDocument)
                || !installed.grants.contains(&Permission::EditDocument)
                || options
                    .provider
                    .as_ref()
                    .is_some_and(|provider| *provider != manifest.identity())
            {
                continue;
            }
            let namespace = manifest.identity().namespace();
            for module in &manifest.modules {
                if !module.provides.contains(&Provider::Format) {
                    continue;
                }
                let key = Key {
                    namespace: namespace.clone(),
                    instance: Instance {
                        project,
                        module: module.id.clone(),
                    },
                };
                if self.plugins.view.modules.get(&key) == Some(&State::Ready) {
                    providers.push((
                        key,
                        format!("{} ({}) · {}", manifest.name, manifest.id, module.id),
                    ));
                }
            }
        }
        providers
    }

    pub(in crate::grid::workspace) fn manual_format(
        &mut self,
        selection: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.start_format(selection, None, None, window, cx);
    }

    /// Honor the opt-in policy. The existing save path handles every fallback.
    pub(in crate::grid::workspace) fn save_with_plugins(
        &mut self,
        path: Option<PathBuf>,
        quit: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.start_format(None, Some(Save { path, quit }), None, window, cx);
    }

    pub(in crate::grid::workspace) fn cancel_format(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.projects.active_project() else {
            return;
        };
        let opening = project.editor.vim().doc.opening;
        if let Some(job) = self.plugins.formatting.pending.get(&opening) {
            self.finish_format(
                opening,
                job.token,
                Err("Formatting cancelled".into()),
                window,
                cx,
            );
        }
    }

    fn start_format(
        &mut self,
        selection: Option<Range<usize>>,
        save: Option<Save>,
        selected: Option<Key>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let requested = Instant::now();
        let Some(project) = self.projects.active_project() else {
            return;
        };
        let doc = &project.editor.vim().doc;
        let (scope, opening, revision) = (project.plugin_scope, doc.opening, doc.revision);
        let extension = save
            .as_ref()
            .and_then(|save| save.path.as_deref())
            .or(doc.path.as_deref())
            .and_then(file_extension)
            .unwrap_or_default();
        let options = self.formatting_preferences(scope, &extension);
        if options.on_save != Some(true)
            && let Some(save) = save
        {
            self.finish_save(scope, opening, save, None, window, cx);
            return;
        }
        let mut providers = self.formatters(scope, &options);
        if let Some(selected) = &selected {
            providers.retain(|(key, _)| key == selected);
        } else if let Some(previous) = self
            .plugins
            .formatting
            .selected
            .get(&(scope, extension.clone()))
            && options
                .provider
                .as_ref()
                .is_none_or(|identity| identity.namespace() == previous.namespace)
        {
            providers.retain(|(key, _)| key == previous);
        }
        let provider = match providers.as_slice() {
            [(key, _)] => key.clone(),
            [_, _, ..] if save.is_none() => {
                self.plugins.formatting.choices = Some(Choices {
                    version: (opening, revision),
                    scope,
                    extension,
                    selection,
                    providers,
                });
                self.plugins.open = true;
                self.plugins.manager.selection = 0;
                self.plugins.manager.offset = 0;
                self.plugin_refresh(window, cx);
                return;
            }
            _ => {
                let error = if providers.is_empty() {
                    "No applicable formatter is ready"
                } else {
                    "Choose a default formatter in plugin settings"
                };
                if let Some(save) = save {
                    self.plugins.formatting.pending.remove(&opening);
                    self.finish_save(scope, opening, save, Some(error), window, cx);
                } else {
                    self.set_status(error.into());
                }
                return;
            }
        };
        if doc.text.len() > 120 * 1024 {
            let error = "This document exceeds the current plugin snapshot limit";
            if let Some(save) = save {
                self.plugins.formatting.pending.remove(&opening);
                self.finish_save(scope, opening, save, Some(error), window, cx);
            } else {
                self.set_status(error.into());
            }
            return;
        }
        let wait = if save.is_some() {
            SAVE_WAIT
        } else {
            MANUAL_WAIT
        };
        let deadline = requested + wait;
        let generation = self
            .plugins
            .view
            .runtime_status
            .get(&provider.namespace)
            .map(|status| status.generation);
        let Some(generation) = generation else {
            let error = "Formatter is unavailable";
            if let Some(save) = save {
                self.plugins.formatting.pending.remove(&opening);
                self.finish_save(scope, opening, save, Some(error), window, cx);
            } else {
                self.set_status(error.into());
            }
            return;
        };
        if let Some(prior) = self.plugins.formatting.pending.get_mut(&opening) {
            if prior.project == scope
                && prior.revision == revision
                && prior.selection == selection
                && prior.provider == provider
                && prior.generation == generation
            {
                if let Some(save) = save {
                    prior.attach_save(save, requested);
                    let (token, deadline) = (prior.token, prior.deadline);
                    self.watch_format_deadline(opening, token, deadline, window, cx);
                }
                return;
            }
            if save.is_none() && prior.save.is_some() {
                self.set_status("Save is waiting for its formatter".into());
                return;
            }
        }
        self.plugins.formatting.pending.remove(&opening);
        self.sync_plugin_projects();
        let (reply, receive) = async_channel::bounded(1);
        let event = PluginEvent::Format {
            selection: selection.as_ref().map(|range| [range.start, range.end]),
        };
        let queued = self.plugins.session.as_ref().is_some_and(|session| {
            session
                .send(Action::Invoke {
                    key: provider.clone(),
                    event,
                    reply: Some(reply),
                    timeout: deadline.saturating_duration_since(Instant::now()),
                })
                .is_ok()
        });
        if !queued {
            if let Some(save) = save {
                self.finish_save(
                    scope,
                    opening,
                    save,
                    Some("Formatter is unavailable"),
                    window,
                    cx,
                );
            } else {
                self.set_status("Formatter is unavailable".into());
            }
            return;
        }
        self.plugins.formatting.next += 1;
        let token = self.plugins.formatting.next;
        self.plugins.formatting.pending.insert(
            opening,
            Pending {
                token,
                project: scope,
                revision,
                deadline,
                provider,
                generation,
                selection,
                save,
            },
        );
        self.set_status("Formatting…".into());
        self.watch_format_deadline(opening, token, deadline, window, cx);
        cx.spawn_in(window, async move |workspace, cx| {
            let result = receive
                .recv()
                .await
                .unwrap_or_else(|_| Err("Formatter stopped".into()));
            let _ = cx.update(|window, cx| {
                workspace.update(cx, |workspace, cx| {
                    workspace.finish_format(opening, token, result, window, cx);
                })
            });
        })
        .detach();
    }

    pub(super) fn choose_formatter(
        &mut self,
        key: Key,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(choices) = self.plugins.formatting.choices.take() else {
            return;
        };
        self.plugins.open = false;
        if self.projects.active_project().is_none_or(|project| {
            let doc = &project.editor.vim().doc;
            project.plugin_scope != choices.scope || (doc.opening, doc.revision) != choices.version
        }) {
            self.set_status("Document changed while choosing its formatter".into());
            return;
        }
        self.plugins
            .formatting
            .selected
            .insert((choices.scope, choices.extension), key.clone());
        self.start_format(choices.selection, None, Some(key), window, cx);
    }

    fn watch_format_deadline(
        &self,
        opening: u64,
        token: u64,
        deadline: Instant,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let timer = cx
            .background_executor()
            .timer(deadline.saturating_duration_since(Instant::now()));
        cx.spawn_in(window, async move |workspace, cx| {
            timer.await;
            let _ = cx.update(|window, cx| {
                workspace.update(cx, |workspace, cx| {
                    workspace.finish_format(
                        opening,
                        token,
                        Err("Formatter timed out".into()),
                        window,
                        cx,
                    );
                })
            });
        })
        .detach();
    }

    fn finish_format(
        &mut self,
        opening: u64,
        token: u64,
        result: Result,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .plugins
            .formatting
            .pending
            .get(&opening)
            .is_none_or(|job| job.token != token)
        {
            return;
        }
        let job = self
            .plugins
            .formatting
            .pending
            .remove(&opening)
            .expect("current format task");
        let mut changed_path = None;
        let outcome = (|| -> anyhow::Result<()> {
            let result = result.map_err(anyhow::Error::msg)?;
            let generation = self
                .plugins
                .view
                .runtime_status
                .get(&job.provider.namespace)
                .filter(|_| self.plugins.view.modules.get(&job.provider) == Some(&State::Ready))
                .map(|status| status.generation);
            job.check_reply(opening, generation, &result, Instant::now())?;
            let project = self
                .projects
                .iter_mut()
                .find(|project| project.plugin_scope == job.project)
                .ok_or_else(|| anyhow::anyhow!("Project was closed"))?;
            let buffer = project
                .editor
                .buffers
                .iter_mut()
                .find(|buffer| buffer.vim.doc.opening == opening)
                .ok_or_else(|| anyhow::anyhow!("Document was closed"))?;
            let doc = &mut buffer.vim.doc;
            let updated = edits::prepare(
                doc,
                result.version,
                result.output.edits,
                job.selection.clone(),
            )?;
            // Validation is part of the same manual or save budget.
            job.check_deadline(Instant::now())?;
            if doc.text != updated {
                changed_path = doc.path.clone();
                edits::apply(doc, &updated);
            }
            Ok(())
        })();
        if let Some(path) = changed_path {
            for manager in self
                .projects
                .iter_mut()
                .filter_map(|project| project.manager.as_mut())
            {
                manager.transfers.invalidate(&path);
            }
        }
        let error = outcome.err().map(|error| format!("{error:#}"));
        if let Some(save) = job.save {
            self.finish_save(job.project, opening, save, error.as_deref(), window, cx);
        } else {
            if let Some(project) = self
                .projects
                .iter_mut()
                .find(|project| project.plugin_scope == job.project)
            {
                project.status = error.unwrap_or_else(|| "Formatted".into());
            }
            self.sync(window, cx);
        }
    }

    fn finish_save(
        &mut self,
        scope: u64,
        opening: u64,
        save: Save,
        format_error: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = self.projects.indexed().find_map(|(index, project)| {
            if project.plugin_scope != scope {
                return None;
            }
            project
                .editor
                .buffers
                .iter()
                .position(|buffer| buffer.vim.doc.opening == opening)
                .map(|buffer| (index, buffer))
        });
        let Some((tab, buffer)) = target else {
            return;
        };
        match self.projects.save_buffer(tab, buffer, save.path) {
            Ok(()) => {
                let project = self.projects.project_mut(tab);
                project.status = format_error.map_or_else(
                    || i18n::text("workspace.saved").into(),
                    |error| format!("Saved · {error}"),
                );
                if let Some(manager) = &mut project.manager {
                    let _ = manager.tree.refresh();
                }
                if save.quit {
                    self.request_close(super::super::editor_close::CloseTarget::Quit, window, cx);
                }
            }
            Err(error) => self.projects.project_mut(tab).status = format!("{error:#}"),
        }
        self.sync(window, cx);
    }
}

pub(super) fn file_extension(path: &Path) -> Option<String> {
    path.extension()?.to_str().map(str::to_ascii_lowercase)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nanobug_plugin_sdk::Output;

    fn manual(requested: Instant) -> Pending {
        Pending {
            token: 1,
            project: 2,
            revision: 3,
            deadline: requested + MANUAL_WAIT,
            provider: Key {
                namespace: "test".into(),
                instance: Instance {
                    project: 2,
                    module: "format".into(),
                },
            },
            generation: 4,
            selection: None,
            save: None,
        }
    }

    #[test]
    fn save_join_only_shortens_the_original_format_deadline() {
        let requested = Instant::now();
        let save = || Save {
            path: None,
            quit: false,
        };
        let mut job = manual(requested);
        job.attach_save(save(), requested + Duration::from_secs(3));
        assert_eq!(job.deadline, requested + Duration::from_secs(4));
        job.attach_save(save(), requested + Duration::from_millis(3_500));
        assert_eq!(job.deadline, requested + Duration::from_secs(4));

        let mut almost_finished = manual(requested);
        almost_finished.attach_save(save(), requested + Duration::from_millis(9_500));
        assert_eq!(almost_finished.deadline, requested + MANUAL_WAIT);
        assert!(
            job.check_deadline(requested + Duration::from_millis(3_999))
                .is_ok()
        );
        assert!(
            job.check_deadline(requested + Duration::from_secs(4))
                .is_err()
        );
    }

    #[test]
    fn formatter_reply_needs_the_dispatched_document_and_live_provider() {
        let now = Instant::now();
        let job = manual(now);
        let mut result = CallResult {
            generation: 4,
            version: Some((5, 3)),
            output: Output::default(),
        };
        assert!(job.check_reply(5, Some(4), &result, now).is_ok());
        assert!(job.check_reply(6, Some(4), &result, now).is_err());
        assert!(job.check_reply(5, Some(7), &result, now).is_err());
        assert!(
            job.check_reply(5, Some(4), &result, now + MANUAL_WAIT)
                .is_err()
        );
        result.version = Some((5, 4));
        assert!(job.check_reply(5, Some(4), &result, now).is_err());
    }
}
