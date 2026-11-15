//! Local layout snapshots keyed by canonical project directory.

use super::{
    EditorPage, EditorSplits, EditorWindow, PageLayout, PaneId, Project, Workspace, WorkspaceTabs,
};
use crate::grid::{windows::editor::EditorSnapshot, workspace::editor_close::CloseTarget};
use anyhow::{Context as _, Result, ensure};
use gpui::{Context, PromptLevel, Window};
use nanobug::{app_data, i18n};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    fs::File,
    io::{BufReader, ErrorKind},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

/// Format understood by this editor build.
const FORMAT_VERSION: u32 = 1;
/// Collect changes to project layouts and view positions at most once per interval.
const SAVE_INTERVAL: Duration = Duration::from_secs(2);

/// A project's most recent layout and editor references, without document contents.
#[derive(PartialEq, Eq, Deserialize, Serialize)]
struct ProjectSnapshot {
    /// Session format version.
    version: u32,
    /// Tabpages in display order.
    pages: Vec<PageSnapshot>,
    /// Selected page and last visited page.
    active_page: usize,
    previous_page: Option<usize>,
    /// Editor snapshots keyed by the pane identities used in the split trees.
    editors: BTreeMap<PaneId, EditorSnapshot>,
}

/// Outer pane layout and editor split geometry for one project page.
#[derive(PartialEq, Eq, Deserialize, Serialize)]
struct PageSnapshot {
    /// Explorer position, visibility, sizing and focus.
    layout: PageLayout,
    /// Nested split structure and focused editor.
    splits: EditorSplits,
}

/// Highest processed request and last successful snapshot for a project.
#[derive(Default)]
struct SavedProject {
    /// Reject an older worker even when a newer write failed.
    generation: u64,
    /// Used to avoid rewriting an unchanged layout.
    snapshot: Option<ProjectSnapshot>,
}

/// Persistence and write ordering shared by background collection and final close.
#[derive(Default)]
pub(super) struct ProjectSessions {
    /// Disabled for isolated profiles and fixtures.
    directory: Option<PathBuf>,
    /// Monotonically increasing request number assigned on the UI thread.
    generation: u64,
    /// Serialize commit order and retain the last successfully written metadata.
    saved: Arc<Mutex<HashMap<PathBuf, SavedProject>>>,
}

/// An owned set of changes that can be written without accessing the workspace.
struct WriteBatch {
    /// Local destination for every snapshot in the batch.
    directory: PathBuf,
    /// Request order relative to other batches from the same window.
    generation: u64,
    /// Snapshot commit state shared with the closing path.
    saved: Arc<Mutex<HashMap<PathBuf, SavedProject>>>,
    /// Canonical project roots and the associated layout snapshots.
    projects: Vec<(PathBuf, ProjectSnapshot)>,
}

impl ProjectSnapshot {
    /// Capture every pane of `project`, including the live outer layout of the active page.
    fn capture(project: &Project) -> Self {
        let pages = project
            .pages
            .iter()
            .enumerate()
            .map(|(index, page)| PageSnapshot {
                layout: if index == project.active_page {
                    project.page_layout()
                } else {
                    page.layout
                },
                splits: page.splits.clone(),
            })
            .collect();
        let editors = project
            .editors()
            .map(|(id, editor)| (id, editor.snapshot_for_project(&project.root)))
            .collect();
        Self {
            version: FORMAT_VERSION,
            active_page: project.active_page,
            previous_page: project.previous_page,
            pages,
            editors,
        }
    }

    /// Rebuild a project rooted at `root` from disk, preserving valid panes when files are missing.
    fn restore(mut self, root: PathBuf) -> Result<Project> {
        ensure!(
            self.version == FORMAT_VERSION,
            "Unsupported project Session version"
        );
        ensure!(
            self.active_page < self.pages.len(),
            "Invalid active Session page"
        );
        ensure!(
            self.pages.iter().all(|page| page.splits.validate_session()),
            "Invalid Session split layout"
        );
        let next_pane = self
            .editors
            .last_key_value()
            .and_then(|(&id, _)| id.checked_add(1))
            .filter(|&id| id < usize::MAX)
            .context("Invalid Session editor IDs")?;
        let active = self.pages[self.active_page].splits.active();
        let mut skipped = 0;
        let mut editors = HashMap::new();
        for pane in self.pages.iter().flat_map(|page| page.splits.ids()) {
            let snapshot = self
                .editors
                .remove(&pane)
                .context("Missing or repeated Session pane")?;
            let (editor, missing) = EditorWindow::restore_project_snapshot(snapshot, &root);
            skipped += missing;
            editors.insert(pane, editor);
        }
        ensure!(self.editors.is_empty(), "Session has unplaced panes");
        let editor = editors
            .remove(&active)
            .context("Missing active Session editor")?;
        let manager = super::ManagerWindow::new(root.clone());
        let mut project = Project::with_editor(root, editor, Some(manager), String::new());
        project.pages = self
            .pages
            .into_iter()
            .map(|page| EditorPage {
                layout: page.layout,
                splits: page.splits,
            })
            .collect();
        project.active_page = self.active_page;
        project.previous_page = self
            .previous_page
            .filter(|&index| index < project.pages.len() && index != project.active_page);
        project.next_pane = next_pane;
        project.inactive_editors = editors;
        project.restore_page_layout();
        if skipped > 0 {
            project.status = i18n::format("project_session.skipped_files", &[("count", &skipped)]);
        }
        Ok(project)
    }
}

impl ProjectSessions {
    /// Enable Session persistence only for a normal editor profile.
    pub(super) fn for_profile(enabled: bool) -> Self {
        Self {
            directory: enabled
                .then(app_data::project_sessions_directory)
                .transpose()
                .unwrap_or_else(|error| {
                    eprintln!("{error:#}");
                    None
                }),
            ..Self::default()
        }
    }

    /// Restore `root` from the most recent Session and focus an explicitly requested `file`.
    /// Fall back to normal project opening when the Session is absent or invalid.
    pub(super) fn open(&self, root: PathBuf, file: Option<PathBuf>) -> Project {
        let Some(directory) = &self.directory else {
            return Project::new(root, file);
        };
        let path = session_path(directory, &root);
        let mut project = match read_snapshot(&path).and_then(|snapshot| {
            snapshot
                .map(|snapshot| snapshot.restore(root.clone()))
                .transpose()
        }) {
            Ok(Some(project)) => project,
            Ok(None) => return Project::new(root, file),
            Err(error) => {
                eprintln!("{}: {error:#}", path.display());
                let mut project = Project::new(root, file);
                project.status = i18n::text("project_session.restore_failed").into();
                return project;
            }
        };
        if let Some(file) = file {
            if let Err(error) = project.editor.open(file, 0.) {
                project.status = format!("{error:#}");
            } else {
                project.focus_pane(super::Focus::Editor);
                if project.hidden_pane == Some(super::Focus::Editor) {
                    project.hidden_pane = None;
                }
            }
        }
        project
    }
}

impl WorkspaceTabs {
    /// Capture the directory projects in a close `target`, or all of them for an autosave.
    fn session_batch(&mut self, target: Option<CloseTarget>) -> Option<WriteBatch> {
        let directory = self.sessions.directory.clone()?;
        let projects = self
            .indexed()
            .filter(|(index, project)| {
                project.manager.is_some()
                    && (target != Some(CloseTarget::Tab) || *index == self.selected)
            })
            .map(|(_, project)| (project.root.clone(), ProjectSnapshot::capture(project)))
            .collect::<Vec<_>>();
        if projects.is_empty() {
            return None;
        }
        self.sessions.generation += 1;
        Some(WriteBatch {
            directory,
            generation: self.sessions.generation,
            saved: Arc::clone(&self.sessions.saved),
            projects,
        })
    }
}

impl Workspace {
    /// Coalesce view changes and persist them off the UI thread until the workspace closes.
    pub(in crate::grid::workspace) fn start_project_sessions(
        projects: &WorkspaceTabs,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if projects.sessions.directory.is_none() {
            return;
        }
        let executor = cx.background_executor().clone();
        cx.spawn_in(window, async move |weak, cx| {
            loop {
                executor.timer(SAVE_INTERVAL).await;
                let Ok(Ok(batch)) = cx.update(|_, cx| {
                    weak.update(cx, |workspace, _| {
                        if workspace.editor_close.is_some() {
                            return None;
                        }
                        workspace.projects.session_batch(None)
                    })
                }) else {
                    break;
                };
                let Some(batch) = batch else { continue };
                let failed = executor.spawn(async move { batch.write() }).await;
                if !failed.is_empty() {
                    let _ = cx.update(|window, cx| {
                        weak.update(cx, |workspace, cx| {
                            if workspace
                                .projects
                                .active_project()
                                .is_some_and(|project| failed.contains(&project.root))
                            {
                                workspace
                                    .set_status(i18n::text("project_session.save_failed").into());
                                workspace.sync(window, cx);
                            }
                        })
                    });
                }
            }
        })
        .detach();
    }

    /// Flush the projects affected by a confirmed close `target` before dropping them.
    pub(in crate::grid::workspace) fn save_closing_project_sessions(
        &mut self,
        target: CloseTarget,
    ) -> bool {
        self.projects
            .session_batch(Some(target))
            .is_none_or(|batch| batch.write().is_empty())
    }

    /// Acknowledge a failed Session write, then finish the already confirmed `target` close.
    pub(in crate::grid::workspace) fn warn_project_session_close(
        &mut self,
        target: CloseTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |weak, cx| {
            let Ok(answer) = cx.update(|window, cx| {
                window.prompt(
                    PromptLevel::Warning,
                    i18n::text("project_session.save_failed"),
                    None,
                    &[i18n::text("project_session.close")],
                    cx,
                )
            }) else {
                return;
            };
            let _ = answer.await;
            let _ = cx.update(|window, cx| {
                weak.update(cx, |workspace, cx| {
                    workspace.finish_close_after_session(target, window, cx);
                    workspace.sync(window, cx);
                })
            });
        })
        .detach();
    }
}

impl WriteBatch {
    /// Publish snapshots in request order and return the project roots that failed to save.
    fn write(self) -> Vec<PathBuf> {
        let mut failures = Vec::new();
        for (root, snapshot) in self.projects {
            let mut saved = self.saved.lock().expect("project Session writer");
            let project = saved.entry(root.clone()).or_default();
            if self.generation < project.generation {
                continue;
            }
            project.generation = self.generation;
            if project.snapshot.as_ref() == Some(&snapshot) {
                continue;
            }
            let path = session_path(&self.directory, &root);
            match write_snapshot(&path, &snapshot) {
                Ok(()) => project.snapshot = Some(snapshot),
                Err(error) => {
                    eprintln!("{}: {error:#}", path.display());
                    failures.push(root);
                }
            }
        }
        failures
    }
}

/// Resolve the file in `directory` belonging to canonical project `root`.
fn session_path(directory: &Path, root: &Path) -> PathBuf {
    let key = Sha256::digest(root.as_os_str().as_encoded_bytes());
    directory.join(format!("{key:x}.json"))
}

/// Decode a snapshot at `path`, treating the absence of a previous Session as normal.
fn read_snapshot(path: &Path) -> Result<Option<ProjectSnapshot>> {
    match File::open(path) {
        Ok(file) => Ok(Some(serde_json::from_reader(BufReader::new(file))?)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Atomically replace the snapshot at `path` after the new metadata reaches disk.
fn write_snapshot(path: &Path, snapshot: &ProjectSnapshot) -> Result<()> {
    let directory = path
        .parent()
        .context("Project Session has no parent directory")?;
    std::fs::create_dir_all(directory)?;
    let mut staged = tempfile::NamedTempFile::new_in(directory)?;
    serde_json::to_writer(staged.as_file_mut(), snapshot)?;
    staged.as_file().sync_all()?;
    staged.persist(path)?;
    Ok(())
}

#[cfg(test)]
mod tests;
