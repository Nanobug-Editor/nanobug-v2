//! Host-owned preview and execution of a plugin's project file operations.

use super::{
    Workspace,
    edits::PlanOwner,
    rows::{Row, Target, row},
};
use anyhow::{Context as _, Result, ensure};
use gpui::{Context, Window};
use nanobug::filesystem::{PathChange, move_exclusive};
use nanobug::plugins::{
    file_operations::{
        DestinationCheck, FileContents, FileOperationIntent, FileOperationJournal, FileStamp,
        FileStepState, stamp,
    },
    project_path,
    session::Action,
};
use nanobug_plugin_sdk::FileOperation;
use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

pub(super) struct Plan {
    owner: PlanOwner,
    root: PathBuf,
    entries: Vec<Entry>,
}

struct Entry {
    operation: FileOperation,
    sources: Vec<Observed>,
    label: String,
}

struct Observed {
    supplied: String,
    path: PathBuf,
    stamp: Option<FileStamp>,
}

impl Entry {
    fn intent(&self) -> FileOperationIntent {
        let check = match &self.operation {
            FileOperation::Create { path, text } => Some(DestinationCheck {
                target: path.clone(),
                absent_source: None,
                contents: FileContents::from_bytes(text.as_bytes()),
            }),
            FileOperation::Move { from, to } => Some(DestinationCheck {
                target: to.clone(),
                absent_source: Some(from.clone()),
                contents: self.sources[0]
                    .stamp
                    .expect("validated move source")
                    .contents,
            }),
            FileOperation::Remove { .. } => None,
        };
        FileOperationIntent {
            label: self.label.clone(),
            check,
        }
    }

    fn verify(&self, root: &Path) -> Result<()> {
        for source in &self.sources {
            ensure!(
                project_path(root, &source.supplied)? == source.path
                    && stamp(&source.path)? == source.stamp,
                "File changed after preview"
            );
        }
        Ok(())
    }

    fn apply(&self, root: &Path) -> Result<PathChange> {
        let from = &self.sources[0].path;
        match &self.operation {
            FileOperation::Create { text, .. } => {
                let mut file = tempfile::NamedTempFile::new_in(
                    from.parent().context("Invalid file destination")?,
                )?;
                file.write_all(text.as_bytes())?;
                file.as_file().sync_all()?;
                ensure!(
                    project_path(root, &self.sources[0].supplied)? == *from,
                    "File destination changed"
                );
                file.persist_noclobber(from)?;
                Ok(PathChange {
                    from: from.clone(),
                    to: None,
                })
            }
            FileOperation::Remove { .. } => {
                trash::delete(from)?;
                Ok(PathChange {
                    from: from.clone(),
                    to: None,
                })
            }
            FileOperation::Move { .. } => {
                let to = &self.sources[1].path;
                move_exclusive(from, to)?;
                Ok(PathChange {
                    from: from.clone(),
                    to: Some(to.clone()),
                })
            }
        }
    }
}

impl Plan {
    fn prepare(owner: PlanOwner, root: PathBuf, operations: Vec<FileOperation>) -> Result<Self> {
        ensure!(
            !operations.is_empty() && operations.len() <= 32,
            "Plugin file plan must contain one to 32 operations"
        );
        let mut seen = BTreeSet::new();
        let mut entries = Vec::new();
        for operation in operations {
            let (paths, label) = match &operation {
                FileOperation::Create { path, text } => {
                    ensure!(
                        text.len() <= 256 * 1024 && !text.contains('\0'),
                        "Invalid new file content"
                    );
                    (vec![(path.as_str(), false)], format!("Create {path}"))
                }
                FileOperation::Remove { path } => {
                    (vec![(path.as_str(), true)], format!("Move to trash {path}"))
                }
                FileOperation::Move { from, to } => (
                    vec![(from.as_str(), true), (to.as_str(), false)],
                    format!("Move {from} → {to}"),
                ),
            };
            let mut sources = Vec::new();
            for (input, exists) in paths {
                let supplied = Path::new(input);
                ensure!(
                    supplied
                        .components()
                        .all(|part| matches!(part, std::path::Component::Normal(_)))
                        && !input.is_empty()
                        && !input.contains(':')
                        && !input.chars().any(char::is_control),
                    "File operations use project-relative paths"
                );
                if let Ok(metadata) = fs::symlink_metadata(root.join(supplied)) {
                    ensure!(
                        !metadata.file_type().is_symlink(),
                        "File operations require a regular path"
                    );
                }
                let path = project_path(&root, input)?;
                let relative = path.strip_prefix(&root)?;
                ensure!(
                    relative
                        .components()
                        .next()
                        .is_some_and(|item| !item.as_os_str().eq_ignore_ascii_case(".nb")),
                    "Use Apply to change project editor configuration"
                );
                let unique = if cfg!(windows) {
                    path.to_string_lossy().to_lowercase()
                } else {
                    path.to_string_lossy().into_owned()
                };
                ensure!(seen.insert(unique), "A file appears twice in the same plan");
                let stamp = stamp(&path)?;
                ensure!(
                    stamp.is_some() == exists,
                    "File operation would overwrite a file or has a missing source"
                );
                ensure!(
                    path.parent().is_some_and(Path::is_dir),
                    "Destination parent does not exist"
                );
                sources.push(Observed {
                    supplied: input.to_owned(),
                    path,
                    stamp,
                });
            }
            entries.push(Entry {
                operation,
                sources,
                label,
            });
        }
        Ok(Self {
            owner,
            root,
            entries,
        })
    }

    pub(super) fn rows(&self) -> Vec<Row> {
        let mut rows = vec![row(
            "Confirm file operations",
            "Review the listed paths. Existing destinations are protected.",
            Target::ApplyFiles,
        )];
        rows.extend(
            self.entries
                .iter()
                .map(|entry| row(&entry.label, &entry.label, Target::None)),
        );
        rows
    }

    fn execute(self, profile: &Path) -> (Vec<PathChange>, Vec<String>) {
        let intents = self.entries.iter().map(Entry::intent).collect();
        let mut journal = match FileOperationJournal::start(
            profile,
            &self.owner.key.namespace,
            &self.root,
            intents,
        ) {
            Ok(journal) => journal,
            Err(error) => {
                let mut report = vec![format!("Unable to save file operation record · {error:#}")];
                report.extend(
                    self.entries
                        .iter()
                        .map(|entry| format!("Not run · {}", entry.label)),
                );
                return (Vec::new(), report);
            }
        };
        let mut changes = Vec::new();
        let mut report = Vec::new();
        let mut failed = false;
        for (index, entry) in self.entries.into_iter().enumerate() {
            if failed {
                report.push(format!("Not run · {}", entry.label));
                continue;
            }
            if let Err(error) = journal.record(index, FileStepState::Pending) {
                failed = true;
                report.push(format!(
                    "Not run · {} · Could not save pending record: {error:#}",
                    entry.label
                ));
                continue;
            }
            let mut attempted = false;
            match entry.verify(&self.root).and_then(|()| {
                attempted = true;
                entry.apply(&self.root)
            }) {
                Ok(change) => {
                    changes.push(change);
                    match journal.record(index, FileStepState::Done) {
                        Ok(()) => report.push(format!("Done · {}", entry.label)),
                        Err(error) => {
                            failed = true;
                            report.push(format!("Result pending confirmation · {} · Could not save outcome: {error:#}", entry.label));
                        }
                    }
                }
                Err(error) => {
                    failed = true;
                    let status = if (!attempted || entry.verify(&self.root).is_ok())
                        && journal.record(index, FileStepState::Failed).is_ok()
                    {
                        "Failed"
                    } else {
                        "Result pending confirmation"
                    };
                    report.push(format!("{status} · {} · {error:#}", entry.label));
                }
            }
        }
        (changes, report)
    }
}

impl Workspace {
    pub(super) fn preview_file_operations(
        &mut self,
        owner: PlanOwner,
        request: u64,
        operations: Vec<FileOperation>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let scope = owner.key.instance.project;
        let Some(root) = self
            .projects
            .ordered()
            .find(|project| project.plugin_scope == scope)
            .map(|project| project.root.clone())
        else {
            return;
        };
        let job = cx
            .background_executor()
            .spawn(async move { Plan::prepare(owner, root.canonicalize()?, operations) });
        cx.spawn_in(window, async move |workspace, cx| {
            let result = job.await;
            let _ = cx.update(|window, cx| {
                workspace.update(cx, |workspace, cx| {
                    if workspace.plugins.request_id != request
                        || workspace.plugins.active != Some(scope)
                    {
                        return;
                    }
                    match result {
                        Ok(plan) if plan.owner.current(&workspace.plugins.view) => {
                            workspace.plugins.file_plan = Some(plan);
                            workspace.plugins.file_report.clear();
                            workspace.plugins.open = true;
                            workspace.plugins.manager.selection = 0;
                            workspace.plugins.manager.offset = 0;
                        }
                        Ok(_) => {
                            workspace.plugins.manager.feedback =
                                Some("Plugin changed after it prepared the file plan".into())
                        }
                        Err(error) => {
                            workspace.plugins.manager.feedback = Some(format!("{error:#}"))
                        }
                    }
                    workspace.plugin_refresh(window, cx);
                })
            });
        })
        .detach();
    }

    pub(super) fn apply_plugin_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(plan) = self.plugins.file_plan.take() else {
            return;
        };
        if !plan.owner.current(&self.plugins.view) {
            self.plugins.manager.feedback =
                Some("The plugin changed after it prepared the file plan".into());
            return;
        }
        let Some(profile) = self.plugins.view.operation_root.clone() else {
            self.plugins.manager.feedback = Some("File operation history is unavailable".into());
            self.plugins.file_plan = Some(plan);
            return;
        };
        let dirty = self.projects.dirty_paths().collect::<BTreeSet<_>>();
        if plan
            .entries
            .iter()
            .flat_map(|entry| &entry.sources)
            .any(|source| dirty.contains(&source.path))
        {
            self.plugins.manager.feedback =
                Some("Save affected buffers before applying file operations".into());
            self.plugins.file_plan = Some(plan);
            return;
        }
        let (lease, ended) = async_channel::bounded(1);
        let (reply, response) = async_channel::bounded(1);
        if !self.plugins.send(Action::BeginFileWrite {
            owner: plan.owner.key.clone(),
            digest: plan.owner.digest.clone(),
            generation: plan.owner.generation,
            ended,
            reply,
        }) {
            self.plugins.file_plan = Some(plan);
            return;
        }
        self.plugins.file_busy = true;
        let background = cx.background_executor().clone();
        cx.spawn_in(window, async move |workspace, cx| {
            let permitted = response.recv().await.unwrap_or_else(|_| {
                Err("Plugin manager stopped before applying the file plan".into())
            });
            let (changes, report) = match permitted {
                Ok(()) => {
                    background
                        .spawn(async move {
                            let outcome = plan.execute(&profile);
                            drop(lease);
                            outcome
                        })
                        .await
                }
                Err(error) => (Vec::new(), vec![error]),
            };
            let _ = cx.update(|window, cx| {
                workspace.update(cx, |workspace, cx| {
                    workspace.plugins.file_busy = false;
                    workspace.projects.apply_path_changes(&changes);
                    for manager in workspace
                        .projects
                        .iter_mut()
                        .filter_map(|project| project.manager.as_mut())
                    {
                        let _ = manager.refresh();
                    }
                    workspace.plugins.file_report = report;
                    workspace.plugins.manager.selection = 0;
                    workspace.plugins.manager.offset = 0;
                    workspace.sync(window, cx);
                    if let Some(target) = workspace.plugins.file_close.take() {
                        workspace.request_close(target, window, cx);
                    }
                })
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nanobug::plugins::session::Key;
    use nanobug_plugin_sdk::Instance;

    fn owner() -> PlanOwner {
        PlanOwner {
            key: Key {
                namespace: "1".repeat(64),
                instance: Instance {
                    project: 1,
                    module: "test".into(),
                },
            },
            digest: "digest".into(),
            generation: 1,
        }
    }

    #[test]
    fn a_changed_file_stops_the_remaining_confirmed_operations() -> Result<()> {
        let project = tempfile::tempdir()?;
        let root = project.path().canonicalize()?;
        let source = root.join("old.txt");
        fs::write(&source, "before")?;
        let modified = fs::metadata(&source)?.modified()?;
        let plan = Plan::prepare(
            owner(),
            root.clone(),
            vec![
                FileOperation::Create {
                    path: "first.txt".into(),
                    text: "created".into(),
                },
                FileOperation::Move {
                    from: "old.txt".into(),
                    to: "moved.txt".into(),
                },
                FileOperation::Create {
                    path: "last.txt".into(),
                    text: "must remain absent".into(),
                },
            ],
        )?;
        fs::write(&source, "edited")?;
        fs::OpenOptions::new()
            .write(true)
            .open(&source)?
            .set_times(fs::FileTimes::new().set_modified(modified))?;
        let profile = tempfile::tempdir()?;
        let (changes, status) = plan.execute(profile.path());
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].from, root.join("first.txt"));
        assert!(changes[0].to.is_none());
        assert!(status[0].starts_with("Done"));
        assert!(status[1].starts_with("Failed"));
        assert!(status[2].starts_with("Not run"));
        assert_eq!(fs::read_to_string(root.join("first.txt"))?, "created");
        assert!(!root.join("last.txt").exists());
        assert_eq!(fs::read_to_string(&source)?, "edited");
        let receipts =
            nanobug::plugins::file_operations::review(profile.path(), &owner().key.namespace)?;
        assert_eq!(receipts.len(), 1);
        let states: Vec<_> = receipts[0].steps.iter().map(|step| step.state).collect();
        assert_eq!(
            states,
            [
                FileStepState::Done,
                FileStepState::Failed,
                FileStepState::NotRun
            ]
        );
        Ok(())
    }

    #[test]
    fn a_destination_created_after_preview_is_never_overwritten() -> Result<()> {
        let project = tempfile::tempdir()?;
        let root = project.path().canonicalize()?;
        let from = root.join("source.txt");
        let to = root.join("target.txt");
        fs::write(&from, "source text")?;
        let operation = FileOperation::Move {
            from: "source.txt".into(),
            to: "target.txt".into(),
        };
        let plan = Plan::prepare(owner(), root.clone(), vec![operation.clone()])?;
        fs::write(&to, "user's target")?;
        assert!(Plan::prepare(owner(), root, vec![operation]).is_err());
        let profile = tempfile::tempdir()?;
        let (changes, status) = plan.execute(profile.path());
        assert!(changes.is_empty());
        assert!(status[0].starts_with("Failed"));
        assert_eq!(fs::read_to_string(from)?, "source text");
        assert_eq!(fs::read_to_string(to)?, "user's target");
        Ok(())
    }

    #[test]
    fn an_unwritable_receipt_prevents_the_confirmed_file_plan_from_starting() -> Result<()> {
        let project = tempfile::tempdir()?;
        let root = project.path().canonicalize()?;
        let profile = tempfile::tempdir()?;
        fs::write(profile.path().join("operations"), "not a directory")?;
        let plan = Plan::prepare(
            owner(),
            root.clone(),
            vec![FileOperation::Create {
                path: "new.txt".into(),
                text: "planned".into(),
            }],
        )?;
        let (changes, report) = plan.execute(profile.path());
        assert!(changes.is_empty());
        assert!(report.iter().any(|line| line.starts_with("Not run")));
        assert!(!root.join("new.txt").exists());
        Ok(())
    }
}
