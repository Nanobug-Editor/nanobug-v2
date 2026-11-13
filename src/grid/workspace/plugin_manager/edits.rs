//! Preview edits from explicit plugin commands and apply or undo the whole group.

use super::{
    Workspace,
    rows::{Row, Target, row},
};
use crate::grid::windows::editor::EditorWindow;
use gpui::{Context, Window};
use nanobug::document::Document;
use nanobug::plugins::{
    edits,
    session::{Action, CallResult, Key, State, View},
};
use nanobug_plugin_sdk::{DocumentEdits, PluginEvent, ProjectFileEdits};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    time::Duration,
};

/// The installed version and live worker that offered an edit or file plan.
pub(super) struct PlanOwner {
    pub(super) key: Key,
    pub(super) digest: String,
    pub(super) generation: u64,
}

pub(super) struct Plan {
    owner: PlanOwner,
    root: PathBuf,
    documents: Vec<Change>,
}

enum Source {
    Open {
        opening: u64,
        revision: u64,
    },
    Disk {
        requested: String,
        resolved: PathBuf,
        version: String,
    },
}

struct Change {
    source: Source,
    title: String,
    before: String,
    after: String,
}

/// A checked editor buffer or an exact disk snapshot to add to it.
enum ApplyTarget {
    Open(usize),
    New(Box<Document>),
}

pub(super) struct Undo {
    scope: u64,
    documents: Vec<(u64, u64)>,
}

impl Undo {
    /// If any participant has changed, retain the entire group for review.
    fn apply(&self, editor: &mut EditorWindow) -> bool {
        let indices: Option<Vec<_>> = self
            .documents
            .iter()
            .map(|&(opening, revision)| buffer_index(editor, opening, revision))
            .collect();
        let Some(indices) = indices else {
            return false;
        };
        for index in indices {
            editor.buffers[index].vim.doc.undo(false);
        }
        true
    }
}

fn buffer_index(editor: &EditorWindow, opening: u64, revision: u64) -> Option<usize> {
    editor.buffers.iter().position(|buffer| {
        let doc = &buffer.vim.doc;
        doc.opening == opening && doc.revision == revision
    })
}

impl PlanOwner {
    pub(super) fn current(&self, view: &View) -> bool {
        view.modules.get(&self.key) == Some(&State::Ready)
            && view
                .runtime_status
                .get(&self.key.namespace)
                .is_some_and(|runtime| runtime.generation == self.generation)
            && view.installed.iter().any(|record| {
                record.enabled
                    && record.release.digest == self.digest
                    && record.release.manifest.identity().namespace() == self.key.namespace
            })
    }
}

impl Plan {
    /// Check every target before changing any buffer or opening a previously closed file.
    fn apply(
        self,
        editor: &mut EditorWindow,
        mut files: BTreeMap<PathBuf, Document>,
    ) -> anyhow::Result<(Undo, Vec<PathBuf>)> {
        let mut targets = Vec::with_capacity(self.documents.len());
        for change in &self.documents {
            targets.push(match &change.source {
                Source::Open { opening, revision } => {
                    ApplyTarget::Open(buffer_index(editor, *opening, *revision).ok_or_else(
                        || anyhow::anyhow!("A document changed after preview. No changes applied."),
                    )?)
                }
                Source::Disk { resolved, .. } => ApplyTarget::New(Box::new(
                    files
                        .remove(resolved)
                        .ok_or_else(|| anyhow::anyhow!("Project file was not verified"))?,
                )),
            });
        }
        let mut documents = Vec::with_capacity(targets.len());
        let mut paths = Vec::new();
        for (target, change) in targets.into_iter().zip(self.documents) {
            let index = match target {
                ApplyTarget::Open(index) => index,
                ApplyTarget::New(doc) => editor.retain_document(*doc),
            };
            let doc = &mut editor.buffers[index].vim.doc;
            edits::apply(doc, &change.after);
            documents.push((doc.opening, doc.revision));
            paths.extend(doc.path.clone());
        }
        Ok((
            Undo {
                scope: self.owner.key.instance.project,
                documents,
            },
            paths,
        ))
    }

    /// Resolve and prepare disk-only edits away from native input and drawing.
    fn prepare_files(&mut self, groups: Vec<ProjectFileEdits>) -> anyhow::Result<()> {
        let mut seen = BTreeSet::new();
        for group in groups {
            let doc = edits::prepare_project_text(&self.root, &group.path, &group.version)?;
            let path = doc.path.clone().expect("project file path");
            anyhow::ensure!(
                seen.insert(path.clone()),
                "Plugin returned two groups for the same project file"
            );
            let after = edits::prepare(&doc, Some((doc.opening, doc.revision)), group.edits, None)?;
            if after != doc.text {
                self.documents.push(Change {
                    source: Source::Disk {
                        requested: group.path,
                        resolved: path.clone(),
                        version: group.version,
                    },
                    title: path.display().to_string(),
                    before: doc.text,
                    after,
                });
            }
        }
        self.preview_limit()
    }

    fn preview_limit(&self) -> anyhow::Result<()> {
        let mut bytes = 0;
        let mut lines = 0;
        for change in &self.documents {
            bytes += change.before.len() + change.after.len();
            lines += change.before.split('\n').count() + change.after.split('\n').count();
        }
        anyhow::ensure!(
            bytes <= 768 * 1024 && lines <= 4096,
            "Plugin edit preview is too large"
        );
        Ok(())
    }

    /// Recheck the exact bytes after confirmation, before publishing any buffer changes.
    fn verify_files(&self) -> anyhow::Result<BTreeMap<PathBuf, Document>> {
        let mut result = BTreeMap::new();
        for change in &self.documents {
            if let Source::Disk {
                requested,
                resolved,
                version,
            } = &change.source
            {
                let doc = edits::prepare_project_text(&self.root, requested, version)?;
                anyhow::ensure!(
                    doc.path.as_ref() == Some(resolved),
                    "Project file moved after preview"
                );
                result.insert(resolved.clone(), doc);
            }
        }
        Ok(result)
    }

    pub(super) fn rows(&self) -> Vec<Row> {
        let mut rows = vec![row(
            "Apply all text changes",
            "Updates buffers as one group. Unopened files open as unsaved buffers. Save files separately.",
            Target::ApplyEdits,
        )];
        for change in &self.documents {
            rows.push(row(
                &change.title,
                "Full text before and after the proposed edit.",
                Target::None,
            ));
            rows.extend(
                change
                    .before
                    .split('\n')
                    .map(|line| row(format!("− {line}"), line, Target::None)),
            );
            rows.extend(
                change
                    .after
                    .split('\n')
                    .map(|line| row(format!("+ {line}"), line, Target::None)),
            );
        }
        rows
    }
}

impl Workspace {
    /// Run the single active command with `id`, as requested from the editor command line.
    pub(in crate::grid::workspace) fn plugin_command_by_id(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.projects.active_project() else {
            return;
        };
        let scope = project.plugin_scope;
        let context = self.plugin_command_context();
        let mut candidates = self
            .plugins
            .view
            .installed
            .iter()
            .filter(|record| record.enabled)
            .filter_map(|record| {
                let manifest = &record.release.manifest;
                let command = manifest.commands.iter().find(|command| {
                    command.id == id && context.is_some_and(|context| command.applies(context))
                })?;
                let key = Key {
                    namespace: manifest.identity().namespace(),
                    instance: nanobug_plugin_sdk::Instance {
                        project: scope,
                        module: command.module.clone(),
                    },
                };
                (self.plugins.view.modules.get(&key)
                    == Some(&nanobug::plugins::session::State::Ready))
                .then_some(key)
            });
        match (candidates.next(), candidates.next()) {
            (Some(key), None) => self.plugin_command(key, id.into(), window, cx),
            (Some(_), Some(_)) => self.set_status(format!("Several active plugins declare {id}")),
            _ => self.set_status(format!("No active plugin command {id}")),
        }
    }

    pub(in crate::grid::workspace) fn plugin_command(
        &mut self,
        key: Key,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let available = self
            .plugin_command_context()
            .zip(super::shortcuts::declaration(
                &self.plugins.view,
                &key.namespace,
                &id,
            ))
            .is_some_and(|(context, command)| command.applies(context));
        if !available {
            self.plugins.manager.feedback =
                Some("Command is unavailable in this pane or mode".into());
            self.plugin_refresh(window, cx);
            return;
        }
        self.sync_plugin_projects();
        let Some(digest) = self
            .plugins
            .view
            .installed
            .iter()
            .find(|record| record.release.manifest.identity().namespace() == key.namespace)
            .map(|record| record.release.digest.clone())
        else {
            return;
        };
        let (reply, receive) = async_channel::bounded(1);
        if !self.plugins.send(Action::Invoke {
            key: key.clone(),
            event: PluginEvent::Command {
                id,
                argument: serde_json::Value::Null,
            },
            reply: Some(reply),
            timeout: Duration::from_secs(10),
        }) {
            return;
        }
        self.plugins.request_id += 1;
        let request = self.plugins.request_id;
        cx.spawn_in(window, async move |workspace, cx| {
            let result = receive
                .recv()
                .await
                .unwrap_or_else(|_| Err("Plugin command stopped".into()));
            let _ = cx.update(|window, cx| {
                workspace.update(cx, |workspace, cx| {
                    if workspace.plugins.request_id != request {
                        return;
                    }
                    let result = result.and_then(|mut result| {
                        let owner = PlanOwner {
                            key,
                            digest,
                            generation: result.generation,
                        };
                        if !owner.current(&workspace.plugins.view) {
                            return Err("Plugin changed while preparing its edit plan".into());
                        }
                        if workspace.plugins.active == Some(owner.key.instance.project)
                            && !result.output.message.is_empty()
                        {
                            workspace.set_status(result.output.message.chars().take(300).collect());
                        }
                        if !result.output.file_operations.is_empty() {
                            if !result.output.edits.is_empty()
                                || !result.output.document_edits.is_empty()
                                || !result.output.project_file_edits.is_empty()
                            {
                                return Err(
                                    "Submit text edits and file operations as separate plans"
                                        .into(),
                                );
                            }
                            workspace.preview_file_operations(
                                owner,
                                request,
                                result.output.file_operations,
                                window,
                                cx,
                            );
                            return Ok(None);
                        }
                        let files = std::mem::take(&mut result.output.project_file_edits);
                        workspace
                            .edit_plan(owner, result, files.len())
                            .map(|plan| plan.map(|plan| (plan, files)))
                            .map_err(|error| error.to_string())
                    });
                    match result {
                        Ok(Some((plan, files))) => {
                            workspace.preview_plugin_edits(plan, files, request, window, cx)
                        }
                        Ok(None) => {}
                        Err(error) => {
                            workspace.plugins.manager.feedback = Some(error.clone());
                            workspace.set_status(error);
                        }
                    }
                    workspace.plugin_refresh(window, cx);
                })
            });
        })
        .detach();
    }

    fn edit_plan(
        &self,
        owner: PlanOwner,
        result: CallResult,
        file_count: usize,
    ) -> anyhow::Result<Option<Plan>> {
        let mut groups = result.output.document_edits;
        if !result.output.edits.is_empty() {
            let (opening, revision) = result
                .version
                .ok_or_else(|| anyhow::anyhow!("Plugin edit has no document version"))?;
            groups.push(DocumentEdits {
                opening,
                revision,
                edits: result.output.edits,
            });
        }
        if groups.is_empty() && file_count == 0 {
            return Ok(None);
        }
        anyhow::ensure!(
            groups.len() + file_count <= 16,
            "Plugin edit plan has too many files"
        );
        let project = self
            .projects
            .ordered()
            .find(|project| project.plugin_scope == owner.key.instance.project)
            .ok_or_else(|| anyhow::anyhow!("Project was closed"))?;
        let mut seen = BTreeSet::new();
        let mut documents = Vec::new();
        for group in groups {
            anyhow::ensure!(
                seen.insert(group.opening),
                "Plugin returned two groups for the same document"
            );
            let doc = project
                .editor
                .buffers
                .iter()
                .find(|buffer| buffer.vim.doc.opening == group.opening)
                .map(|buffer| &buffer.vim.doc)
                .ok_or_else(|| anyhow::anyhow!("Document was closed"))?;
            let after = edits::prepare(
                doc,
                Some((group.opening, group.revision)),
                group.edits,
                None,
            )?;
            if after != doc.text {
                documents.push(Change {
                    source: Source::Open {
                        opening: doc.opening,
                        revision: doc.revision,
                    },
                    title: doc
                        .path
                        .as_ref()
                        .map_or_else(|| "Untitled".into(), |path| path.display().to_string()),
                    before: doc.text.clone(),
                    after,
                });
            }
        }
        let plan = Plan {
            owner,
            root: project.root.clone(),
            documents,
        };
        plan.preview_limit()?;
        Ok(Some(plan))
    }

    fn preview_plugin_edits(
        &mut self,
        mut plan: Plan,
        files: Vec<ProjectFileEdits>,
        request: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if files.is_empty() {
            self.show_edit_preview(plan);
            return;
        }
        let job = cx.background_executor().spawn(async move {
            plan.prepare_files(files)?;
            Ok::<_, anyhow::Error>(plan)
        });
        cx.spawn_in(window, async move |workspace, cx| {
            let result = job.await;
            let _ = cx.update(|window, cx| {
                workspace.update(cx, |workspace, cx| {
                    if workspace.plugins.request_id != request {
                        return;
                    }
                    match result {
                        Ok(plan) => workspace.show_edit_preview(plan),
                        Err(error) => workspace.set_status(format!("Plugin edit preview: {error}")),
                    }
                    workspace.plugin_refresh(window, cx);
                })
            });
        })
        .detach();
    }

    fn show_edit_preview(&mut self, plan: Plan) {
        if !plan.documents.is_empty()
            && plan.owner.current(&self.plugins.view)
            && self.plugins.active == Some(plan.owner.key.instance.project)
        {
            self.plugins.edit_plan = Some(plan);
            self.plugins.open = true;
            self.plugins.manager.selection = 0;
            self.plugins.manager.offset = 0;
        }
    }

    pub(super) fn apply_plugin_edits(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(plan) = self.plugins.edit_plan.take() else {
            return;
        };
        if plan
            .documents
            .iter()
            .all(|change| matches!(change.source, Source::Open { .. }))
        {
            self.publish_plugin_edits(plan, BTreeMap::new(), window, cx);
            return;
        }
        self.plugins.request_id += 1;
        let id = self.plugins.request_id;
        self.plugins.edit_check = Some(id);
        let job = cx.background_executor().spawn(async move {
            let checked = plan.verify_files();
            (plan, checked)
        });
        cx.spawn_in(window, async move |workspace, cx| {
            let (plan, checked) = job.await;
            let _ = cx.update(|window, cx| {
                workspace.update(cx, |workspace, cx| {
                    if workspace.plugins.edit_check != Some(id) {
                        return;
                    }
                    workspace.plugins.edit_check = None;
                    match checked {
                        Ok(files) => workspace.publish_plugin_edits(plan, files, window, cx),
                        Err(error) => {
                            workspace.plugins.manager.feedback =
                                Some(format!("No edits applied. {error}"));
                            workspace.plugin_refresh(window, cx);
                        }
                    }
                })
            });
        })
        .detach();
    }

    fn publish_plugin_edits(
        &mut self,
        plan: Plan,
        files: BTreeMap<PathBuf, Document>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let scope = plan.owner.key.instance.project;
        let result = (|| -> anyhow::Result<Undo> {
            anyhow::ensure!(
                plan.owner.current(&self.plugins.view),
                "Plugin changed after it prepared the preview"
            );
            let overlap = self
                .projects
                .ordered()
                .flat_map(|project| project.editor.buffers.iter())
                .any(|buffer| {
                    buffer
                        .vim
                        .doc
                        .path
                        .as_ref()
                        .is_some_and(|path| files.contains_key(path))
                });
            anyhow::ensure!(
                !overlap,
                "A file in the plan is now open. Prepare the edits using its open buffer."
            );
            let project = self
                .projects
                .iter_mut()
                .find(|project| project.plugin_scope == scope && project.root == plan.root)
                .ok_or_else(|| anyhow::anyhow!("Project was closed"))?;
            let (undo, paths) = plan.apply(&mut project.editor, files)?;
            for manager in self
                .projects
                .iter_mut()
                .filter_map(|project| project.manager.as_mut())
            {
                for path in &paths {
                    manager.transfers.invalidate(path);
                }
            }
            Ok(undo)
        })();
        match result {
            Ok(undo) => {
                self.plugins.undo_edits = Some(undo);
                self.plugins.open = false;
                self.set_status(
                    "Plugin edits applied. Save changed files or use :undo-plugin.".into(),
                );
            }
            Err(error) => self.plugins.manager.feedback = Some(error.to_string()),
        }
        self.sync(window, cx);
    }

    pub(in crate::grid::workspace) fn undo_plugin_edits(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(undo) = &self.plugins.undo_edits else {
            self.set_status("No plugin edit group to undo".into());
            return;
        };
        let undone = self
            .projects
            .iter_mut()
            .find(|project| project.plugin_scope == undo.scope)
            .is_some_and(|project| undo.apply(&mut project.editor));
        if undone {
            self.plugins.undo_edits = None;
        }
        self.set_status(
            if undone {
                "Plugin edit group undone"
            } else {
                "A document changed. The plugin edit group was kept."
            }
            .into(),
        );
        self.sync(window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nanobug_plugin_sdk::{Edit, Instance};

    fn owner() -> PlanOwner {
        PlanOwner {
            key: Key {
                namespace: "fixture".into(),
                instance: Instance {
                    project: 1,
                    module: "editor".into(),
                },
            },
            digest: "fixture".into(),
            generation: 1,
        }
    }

    #[test]
    fn changing_one_unopened_file_invalidates_the_whole_preview() -> anyhow::Result<()> {
        let folder = tempfile::tempdir()?;
        let root = folder.path();
        let groups = ["first.txt", "second.txt"]
            .into_iter()
            .map(|path| -> anyhow::Result<_> {
                std::fs::write(root.join(path), "original")?;
                let snapshot = edits::read_project_text(root, path.into())?;
                Ok(ProjectFileEdits {
                    path: snapshot.path,
                    version: snapshot.version,
                    edits: vec![Edit {
                        start: 0,
                        end: 8,
                        text: "changed".into(),
                    }],
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let mut plan = Plan {
            owner: owner(),
            root: root.to_path_buf(),
            documents: Vec::new(),
        };
        plan.prepare_files(groups)?;
        assert_eq!(plan.verify_files()?.len(), 2);
        std::fs::write(root.join("second.txt"), "new text")?;
        assert!(plan.verify_files().is_err());
        assert_eq!(std::fs::read_to_string(root.join("first.txt"))?, "original");
        Ok(())
    }

    #[test]
    fn mixed_plan_checks_every_buffer_and_undoes_the_group_together() -> anyhow::Result<()> {
        let folder = tempfile::tempdir()?;
        let root = folder.path();
        let open_path = root.join("open.txt");
        let closed_path = root.join("closed.txt");
        std::fs::write(&open_path, "open")?;
        std::fs::write(&closed_path, "closed")?;
        let mut doc = Document::open(&open_path)?;
        doc.replace(4..4, " unsaved", true);
        let mut editor = EditorWindow::new(doc);
        let make_plan = |doc: &Document| -> anyhow::Result<Plan> {
            let mut plan = Plan {
                owner: owner(),
                root: root.to_path_buf(),
                documents: vec![Change {
                    source: Source::Open {
                        opening: doc.opening,
                        revision: doc.revision,
                    },
                    title: "open.txt".into(),
                    before: doc.text.clone(),
                    after: format!("{} formatted", doc.text),
                }],
            };
            let closed = edits::read_project_text(root, "closed.txt".into())?;
            plan.prepare_files(vec![ProjectFileEdits {
                path: closed.path,
                version: closed.version,
                edits: vec![Edit {
                    start: 6,
                    end: 6,
                    text: " formatted".into(),
                }],
            }])?;
            Ok(plan)
        };
        let stale = make_plan(&editor.vim().doc)?;
        let files = stale.verify_files()?;
        editor.buffers[0].vim.doc.replace(0..0, "user ", true);
        assert!(stale.apply(&mut editor, files).is_err());
        assert_eq!(editor.buffers.len(), 1);

        let fresh = make_plan(&editor.vim().doc)?;
        let files = fresh.verify_files()?;
        let (undo, _) = fresh.apply(&mut editor, files)?;
        assert_eq!(
            editor.buffers[0].vim.doc.text,
            "user open unsaved formatted"
        );
        assert_eq!(editor.buffers[1].vim.doc.text, "closed formatted");
        assert_eq!(std::fs::read_to_string(&open_path)?, "open");
        assert_eq!(std::fs::read_to_string(&closed_path)?, "closed");
        let permissions = std::fs::metadata(&closed_path)?.permissions();
        let mut locked = permissions.clone();
        locked.set_readonly(true);
        std::fs::set_permissions(&closed_path, locked)?;
        let save = editor.buffers[1].vim.doc.save(None);
        std::fs::set_permissions(&closed_path, permissions)?;
        assert!(save.is_err());
        assert_eq!(editor.buffers[1].vim.doc.text, "closed formatted");
        assert_eq!(std::fs::read_to_string(&closed_path)?, "closed");
        assert!(undo.apply(&mut editor));
        assert_eq!(editor.buffers[0].vim.doc.text, "user open unsaved");
        assert_eq!(editor.buffers[1].vim.doc.text, "closed");

        let versioned = editor
            .buffers
            .iter()
            .map(|buffer| {
                let doc = &buffer.vim.doc;
                (doc.opening, doc.revision)
            })
            .collect();
        let undo = Undo {
            scope: 1,
            documents: versioned,
        };
        editor.buffers[1].vim.doc.replace(0..0, "changed ", true);
        assert!(!undo.apply(&mut editor));
        assert_eq!(editor.buffers[0].vim.doc.text, "user open unsaved");
        assert_eq!(editor.buffers[1].vim.doc.text, "changed closed");
        Ok(())
    }
}
