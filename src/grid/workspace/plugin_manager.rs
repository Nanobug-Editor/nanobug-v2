//! Keyboard-first plugin manager and a single plugin-owned bottom window.

mod canvas;
mod comparison;
#[cfg(all(test, windows))]
mod credential_tests;
#[cfg(all(test, windows))]
mod cursor_tests;
mod edits;
mod field;
mod file_ops;
mod formatting;
mod interaction;
mod language;
mod manager_layout;
#[cfg(all(test, windows))]
mod native_editing_tests;
mod native_field;
mod outline;
mod rows;
#[cfg(all(test, windows))]
mod rust_plugin_test;
mod shortcuts;

use super::{Workspace, editor_close::CloseTarget, view::positioned};
use crate::{
    grid::render::{Frame, LabelStyle, Metrics, Panel, Rect},
    ui::{
        ime::Composition,
        keys::normal_character,
        text::{TextSpacing, UNITS_PER_COLUMN},
        theme::{ACCENT, GOLD, GridFont, MUTED, SELECTION, TEXT, style},
    },
};
use gpui::{
    AnyElement, AppContext, Context, Entity, IntoElement, KeyDownEvent, MouseDownEvent,
    ParentElement, Styled, Window, div, rgba,
};
use manager_layout::{ListLayout, SEARCH_BUTTONS};
use nanobug::i18n;
use nanobug::plugins::{
    package::Identity,
    session::{Action, ConfigEdit, ConfigScope, Key, Options, Project, Rule, Session, Trace, View},
};
use nanobug_plugin_sdk::{
    DocumentSnapshot, Permission,
    canvas::{Appearance, EditorFont, EditorTheme, Language, ThemeColors, Viewport},
};
use rows::{Row, Target};
use std::{collections::BTreeMap, ops::Range, path::PathBuf, rc::Rc, sync::Arc};
use unicode_segmentation::UnicodeSegmentation;
use zeroize::{Zeroize, Zeroizing};

/// First item row in a secondary dialog, below its scope, tabs, and feedback.
const LIST_START: usize = 4;
/// Minimum grid height for the secondary dialogs' content and footer.
const MIN_HEIGHT: usize = 13;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Page {
    #[default]
    Installed,
    Marketplace,
    Zip,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Detail {
    #[default]
    Overview,
    Modules,
    Rules,
    Logs,
    Operations,
    Settings,
    Compare,
    Cleanup,
}

enum Prompt {
    Search {
        original: String,
        selection: usize,
    },
    Zip,
    Development,
    DevSource {
        namespace: String,
        settings: nanobug::plugins::session::DeveloperSettings,
    },
    Rule {
        scope: ConfigScope,
        owner: Identity,
        module: String,
    },
    Setting {
        scope: ConfigScope,
        owner: Identity,
        id: String,
        renamed_from: Vec<String>,
        original: Option<serde_json::Value>,
    },
    Credential {
        namespace: String,
        slot: String,
        value: Zeroizing<String>,
    },
    ExportDraft,
    Shortcut {
        namespace: String,
        command: String,
    },
}

struct Input {
    kind: Prompt,
    value: String,
}

/// A masked setting staged only for the current editor session.
struct CredentialDraft {
    namespace: String,
    slot: String,
    value: Zeroizing<String>,
}

struct DraftClose {
    target: CloseTarget,
    project: Option<u64>,
    draining: bool,
    waiting: bool,
    error: Option<String>,
}

#[derive(Default)]
struct Manager {
    review_token: Option<u64>,
    review_page: usize,
    page: Page,
    detail: Option<String>,
    detail_page: Detail,
    comparison: Option<comparison::Comparison>,
    selection: usize,
    list_selection: usize,
    offset: usize,
    search: String,
    prompt: Option<Input>,
    help: bool,
    all_projects: bool,
    local_project: bool,
    feedback: Option<String>,
    confirm_remove: Option<String>,
    retained_open: bool,
    clean_credentials: bool,
    clean_local: bool,
}

/// Retain host-owned input and the latest scene independently of plugin processes.
pub(super) struct PluginUi {
    options: Option<Options>,
    trace: Option<Trace>,
    traced_doc: Option<(u64, u64)>,
    /// Last document revision represented by the native workspace scene.
    traced_scene: Option<(u64, u64)>,
    session: Option<Session>,
    view: Arc<View>,
    panel: Entity<Panel>,
    manager: Manager,
    snapshots: BTreeMap<u64, Arc<DocumentSnapshot>>,
    open_snapshots: BTreeMap<(u64, u64), Arc<DocumentSnapshot>>,
    active: Option<u64>,
    projects: Vec<Project>,
    pub(super) open: bool,
    pub(super) composition: Composition,
    pub(super) surface_focus: bool,
    surface: Option<Key>,
    draft: String,
    field: field::Field,
    native_field: Option<Rc<native_field::NativeField>>,
    pending_draft: Option<String>,
    credential_draft: Option<CredentialDraft>,
    credential_waiting: bool,
    credential_close: Option<CloseTarget>,
    viewport: Option<Viewport>,
    viewport_id: u64,
    interaction: interaction::State,
    images: BTreeMap<usize, Arc<gpui::RenderImage>>,
    retired_images: BTreeMap<usize, Arc<gpui::RenderImage>>,
    image_retirement_queued: bool,
    close_draft: Option<DraftClose>,
    edit_plan: Option<edits::Plan>,
    edit_check: Option<u64>,
    file_plan: Option<file_ops::Plan>,
    file_report: Vec<String>,
    pub(super) file_busy: bool,
    file_close: Option<CloseTarget>,
    undo_edits: Option<edits::Undo>,
    language: Option<language::LanguageResults>,
    /// Outline of the current version, shown by the existing plugin list popup.
    outline: Option<outline::Outline>,
    request_id: u64,
    skip_draft_close: bool,
    formatting: formatting::Tasks,
}

impl PluginUi {
    pub(super) fn new(
        options: Option<Options>,
        trace: Option<Trace>,
        metrics: Metrics,
        cx: &mut Context<Workspace>,
    ) -> Self {
        Self {
            options,
            trace,
            traced_doc: None,
            traced_scene: None,
            session: None,
            view: Arc::new(View::default()),
            panel: cx.new(|_| Panel::new(metrics)),
            manager: Manager::default(),
            snapshots: BTreeMap::new(),
            open_snapshots: BTreeMap::new(),
            active: None,
            projects: Vec::new(),
            open: false,
            composition: Composition::default(),
            surface_focus: false,
            surface: None,
            draft: String::new(),
            field: field::Field::default(),
            native_field: None,
            pending_draft: None,
            credential_draft: None,
            credential_waiting: false,
            credential_close: None,
            viewport: None,
            viewport_id: 0,
            interaction: interaction::State::default(),
            images: BTreeMap::new(),
            retired_images: BTreeMap::new(),
            image_retirement_queued: false,
            close_draft: None,
            edit_plan: None,
            edit_check: None,
            file_plan: None,
            file_report: Vec::new(),
            file_busy: false,
            file_close: None,
            undo_edits: None,
            language: None,
            outline: None,
            request_id: 0,
            skip_draft_close: false,
            formatting: formatting::Tasks::default(),
        }
    }

    pub(super) fn owns_input(&self) -> bool {
        self.open || self.surface_focus
    }

    pub(super) fn accepts_text(&self) -> bool {
        if self.open {
            self.manager.prompt.is_some()
        } else {
            self.surface_focus
        }
    }

    fn send(&mut self, action: Action) -> bool {
        let result = self
            .session
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Plugin manager is starting"))
            .and_then(|session| session.send(action));
        self.manager.feedback = result.err().map(|error| error.to_string());
        self.manager.feedback.is_none()
    }

    fn scope(&self) -> Option<ConfigScope> {
        if self.manager.all_projects {
            Some(ConfigScope::AllProjects)
        } else {
            self.active.map(|id| {
                if self.manager.local_project && self.manager.detail_page == Detail::Settings {
                    ConfigScope::ProjectLocal(id)
                } else {
                    ConfigScope::Project(id)
                }
            })
        }
    }

    fn scope_label(&self) -> &'static str {
        match self
            .manager
            .comparison
            .as_ref()
            .map(|comparison| comparison.scope)
            .or_else(|| self.scope())
        {
            Some(ConfigScope::AllProjects) => i18n::text("plugin_manager.all_projects"),
            Some(ConfigScope::ProjectLocal(_)) => {
                i18n::text("plugin_manager.current_project_local")
            }
            _ => i18n::text("plugin_manager.current_project"),
        }
    }

    fn request_draft(&mut self) {
        if let Some(key) = self.surface.clone()
            && let Some(session) = &self.session
        {
            session.draft(key, self.draft.clone());
            self.pending_draft = Some(self.draft.clone());
        }
    }

    fn update_view(&mut self) {
        let Some(session) = &self.session else {
            return;
        };
        self.view = session.view();
        let review = self.view.review.as_ref().map(|review| review.token);
        if self.manager.review_token != review {
            self.manager.review_token = review;
            self.manager.review_page = 0;
        }
        if let Some(key) = &self.surface
            && let Some(surface) = self.view.surfaces.get(key)
            && self
                .pending_draft
                .as_ref()
                .is_none_or(|value| value == &surface.draft)
        {
            if self.draft != surface.draft {
                self.draft.clone_from(&surface.draft);
                self.field = field::Field::default();
            }
            self.pending_draft = None;
        }
    }

    fn collect_retired_images(&mut self) {
        let mut live = BTreeMap::new();
        for surface in self.view.surfaces.values() {
            if let Some(drawing) = &surface.drawing {
                for asset in drawing.assets.values() {
                    if let nanobug::plugins::drawing::Asset::Image(image) = asset {
                        live.entry(image.id.0).or_insert_with(|| Arc::clone(image));
                    }
                }
            }
        }
        for (id, image) in std::mem::replace(&mut self.images, live) {
            if !self.images.contains_key(&id) {
                self.retired_images.insert(id, image);
            }
        }
    }

    fn rows(&self) -> Vec<Row> {
        rows::build(self)
    }
}

impl Workspace {
    /// The plugin manager and grid surface share one retained panel.
    pub(super) fn plugin_panel(&self) -> &Entity<Panel> {
        &self.plugins.panel
    }

    /// Drop retired atlas entries only after this frame replaces the one that could use them.
    pub(super) fn retire_canvas_images_after_paint(&mut self, window: &Window, cx: &Context<Self>) {
        if self.plugins.retired_images.is_empty() || self.plugins.image_retirement_queued {
            return;
        }
        let retiring = std::mem::take(&mut self.plugins.retired_images);
        self.plugins.image_retirement_queued = true;
        let weak = cx.weak_entity();
        window.on_next_frame(move |window, cx| {
            let _ = weak.update(cx, |workspace, cx| {
                workspace.plugins.image_retirement_queued = false;
                for (id, image) in retiring {
                    if !workspace.plugins.images.contains_key(&id) {
                        let _ = window.drop_image(image);
                    }
                }
                if !workspace.plugins.retired_images.is_empty() {
                    cx.notify();
                }
            });
        });
    }

    /// Record the document revision after the workspace has emitted its scene primitives.
    pub(super) fn trace_document_scene(&mut self) {
        let Some(trace) = &self.plugins.trace else {
            return;
        };
        let Some(project) = self.projects.active_project() else {
            return;
        };
        let document = &project.editor.vim().doc;
        let revision = (document.opening, document.revision);
        if self.plugins.traced_scene.replace(revision) != Some(revision) {
            let body = project
                .editor
                .text_area(self.layout.editor)
                .pixels(self.metrics);
            trace.mark(
                "document_scene",
                serde_json::json!({
                    "revision": revision.1, "opening": revision.0,
                    "text_origin": [f32::from(body.origin.x), f32::from(body.origin.y)],
                    "cell": [self.metrics.cw, self.metrics.lh],
                }),
            );
        }
    }

    /// The supervisor starts on the first frame boundary, even on a start page with no input.
    pub(super) fn plugin_after_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(options) = self.plugins.options.take() else {
            return;
        };
        let view = cx.weak_entity();
        window.on_next_frame(move |window, cx| {
            let _ = view.update(cx, |workspace, cx| {
                if let Some(trace) = &workspace.plugins.trace {
                    trace.mark("core_ready", serde_json::json!({ "editable": workspace.projects.active_project().is_some() }));
                }
                let (session, changes) = Session::start(options);
                workspace.plugins.session = Some(session);
                workspace.sync_plugin_projects();
                cx.spawn_in(window, async move |view, cx| {
                    while changes.recv().await.is_ok() {
                        if cx
                            .update(|window, cx| {
                                view.update(cx, |workspace, cx| {
                                    workspace.plugins.update_view();
                                    if let Some(session) = &workspace.plugins.session {
                                        for (&id, request) in &workspace.plugins.view.text_measurements {
                                            let value = request.lines.iter().zip(request.prepared.iter()).map(|(line, prepared)| {
                                                prepared.clone().unwrap_or_else(|| canvas::measure(window, line, style(cx).font))
                                            }).collect();
                                            let _ = session.send(Action::TextMeasured(id, serde_json::Value::Array(value)));
                                        }
                                    }
                                    if let Some(trace) = &workspace.plugins.trace {
                                        let view = &workspace.plugins.view;
                                        let ready = view.modules.values().filter(|state| **state == nanobug::plugins::session::State::Ready).count();
                                        trace.mark("session_update", serde_json::json!({
                                            "installed": view.installed.len(), "ready": ready, "workers": view.workers.len(),
                                            "worker_pids": view.workers.values().copied().collect::<Vec<_>>(),
                                            "modules": view.modules.len(), "surfaces": view.surfaces.len(),
                                            "drawing_revision": workspace.plugins.surface.as_ref()
                                                .and_then(|key| view.surfaces.get(key))
                                                .map(|surface| surface.revision),
                                            "review": view.review.is_some(), "installing": view.installing,
                                            "unmatched": view.modules.values().filter(|state| **state == nanobug::plugins::session::State::Unmatched).count()
                                        }));
                                    }
                                    workspace.plugins.collect_retired_images();
                                    workspace.sync_plugin_projects();
                                    if workspace.refresh_document_presentation() {
                                        workspace.repaint([nanobug::animation::scroll::ScrollInput::Advance; 3], window, cx);
                                    }
                                    if workspace.plugins.open || workspace.plugins.surface.is_some()
                                        || !workspace.plugins.retired_images.is_empty()
                                    {
                                        workspace.update_cursor(window, cx);
                                        cx.notify();
                                    }
                                })
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                })
                .detach();
            });
        });
    }

    /// Publish project identity and document changes once per version, without filesystem I/O.
    pub(super) fn sync_plugin_projects(&mut self) {
        if self.plugins.session.is_none() {
            return;
        }
        let active = self
            .projects
            .active_project()
            .map(|project| project.plugin_scope);
        if let Some(trace) = &self.plugins.trace
            && let Some(doc) = self
                .projects
                .active_project()
                .map(|project| &project.editor.vim().doc)
            && self.plugins.traced_doc != Some((doc.opening, doc.revision))
        {
            self.plugins.traced_doc = Some((doc.opening, doc.revision));
            trace.mark(
                "document_revision",
                serde_json::json!({ "revision": doc.revision, "bytes": doc.text.len() }),
            );
        }
        let document_readers = self
            .plugins
            .view
            .installed
            .iter()
            .any(|record| record.enabled && record.grants.contains(&Permission::ReadDocument));
        let projects = self
            .projects
            .ordered()
            .map(|project| {
                let id = project.plugin_scope;
                let doc = &project.editor.vim().doc;
                let document = (document_readers && doc.text.len() <= 120 * 1024).then(|| {
                    let snapshot = self
                        .plugins
                        .snapshots
                        .entry(id)
                        .or_insert_with(|| Arc::new(snapshot_from(doc)));
                    if snapshot.opening != doc.opening
                        || snapshot.revision != doc.revision
                        || snapshot.path != doc.path
                    {
                        *snapshot = Arc::new(snapshot_from(doc));
                    }
                    Arc::clone(snapshot)
                });
                let mut remaining = 512 * 1024;
                let open_documents = if document_readers {
                    project
                        .editor
                        .buffers
                        .iter()
                        .filter_map(|buffer| {
                            let other = &buffer.vim.doc;
                            if other.opening == doc.opening
                                || other.text.len() > 120 * 1024
                                || other.text.len() > remaining
                            {
                                return None;
                            }
                            remaining -= other.text.len();
                            let snapshot = self
                                .plugins
                                .open_snapshots
                                .entry((id, other.opening))
                                .or_insert_with(|| Arc::new(snapshot_from(other)));
                            if snapshot.revision != other.revision || snapshot.path != other.path {
                                *snapshot = Arc::new(snapshot_from(other));
                            }
                            Some(Arc::clone(snapshot))
                        })
                        .take(32)
                        .collect()
                } else {
                    Vec::new()
                };
                Project {
                    id,
                    root: project.root.clone(),
                    document,
                    open_documents,
                }
            })
            .collect::<Vec<_>>();
        self.plugins
            .snapshots
            .retain(|id, _| document_readers && projects.iter().any(|project| project.id == *id));
        self.plugins.open_snapshots.retain(|(id, opening), _| {
            projects.iter().any(|project| {
                project.id == *id
                    && project
                        .open_documents
                        .iter()
                        .any(|snapshot| snapshot.opening == *opening)
            })
        });
        if self.plugins.active != active || self.plugins.projects != projects {
            if self
                .plugins
                .surface
                .as_ref()
                .is_some_and(|key| Some(key.instance.project) != active)
            {
                self.plugins.surface = None;
                self.plugins.surface_focus = false;
                self.plugins.composition.clear();
                self.plugins.pending_draft = None;
                self.plugins.viewport = None;
            }
            self.plugins.active = active;
            self.plugins.projects = projects;
            self.plugins
                .session
                .as_ref()
                .expect("plugin session")
                .projects(active, self.plugins.projects.clone());
        }
    }

    pub(super) fn open_plugin_manager(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.plugins.session.is_none() {
            return;
        }
        self.plugins.open = true;
        self.clear_canvas_focus();
        self.end_plugin_field_drag();
        if let Some(trace) = &self.plugins.trace {
            trace.mark("manager_open", serde_json::Value::Null);
        }
        self.plugins.surface_focus = false;
        self.plugins.composition.clear();
        self.plugins.manager.all_projects = self.plugins.active.is_none();
        self.plugins.manager.local_project = false;
        if self.plugins.manager.page == Page::Marketplace {
            self.plugins.send(Action::OpenMarketplace);
        }
        self.plugin_refresh(window, cx);
    }

    /// Finish saving confirmed plugin inputs before starting a project/window close.
    pub(super) fn prepare_plugin_close(
        &mut self,
        target: CloseTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.plugins.file_busy {
            self.plugins.file_close = Some(target);
            return true;
        }
        if std::mem::take(&mut self.plugins.skip_draft_close) || target == CloseTarget::Editor {
            return false;
        }
        if let Some(input) = self.plugins.manager.prompt.take_if(
            |input| matches!(&input.kind, Prompt::Credential { value, .. } if !value.is_empty()),
        ) && let Prompt::Credential {
            namespace,
            slot,
            value,
        } = input.kind
        {
            self.review_plugin_credential(namespace, slot, value);
        }
        if self.plugins.credential_draft.is_some() || self.plugins.credential_waiting {
            self.plugins.open = true;
        }
        if self.plugins.credential_draft.is_some() || self.plugins.credential_waiting {
            self.plugins.credential_close = Some(target);
            self.plugin_refresh(window, cx);
            return true;
        }
        if self.plugins.close_draft.is_some() {
            return true;
        }
        let project = self
            .projects
            .active_project()
            .map(|project| project.plugin_scope);
        if self.plugins.session.is_none() || target == CloseTarget::Project && project.is_none() {
            return false;
        }
        self.plugins.open = true;
        self.plugins.surface_focus = false;
        self.plugins.composition.clear();
        self.plugins.manager.prompt = None;
        self.plugins.manager.help = false;
        self.plugins.manager.selection = 0;
        self.plugins.close_draft = Some(DraftClose {
            target,
            project: if target == CloseTarget::Quit {
                None
            } else {
                project
            },
            draining: true,
            waiting: false,
            error: None,
        });
        self.advance_plugin_close(window, cx);
        true
    }

    fn advance_plugin_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(close) = self
            .plugins
            .close_draft
            .as_ref()
            .filter(|close| close.draining)
        {
            let (reply, receive) = async_channel::bounded(1);
            self.wait_draft_close(
                Action::DrainForClose {
                    project: close.project,
                    reply,
                },
                receive,
                window,
                cx,
            );
        } else {
            self.write_close_draft(None, window, cx);
        }
    }

    fn write_close_draft(
        &mut self,
        export: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (reply, receive) = async_channel::bounded(1);
        let action = if let Some(path) = export {
            Action::ExportDrafts {
                path,
                project: self
                    .plugins
                    .close_draft
                    .as_ref()
                    .and_then(|close| close.project),
                reply,
            }
        } else if let Some(project) = self
            .plugins
            .close_draft
            .as_ref()
            .and_then(|close| close.project)
        {
            Action::FlushDrafts { project, reply }
        } else {
            Action::FlushForExit(reply)
        };
        self.wait_draft_close(action, receive, window, cx);
    }

    fn wait_draft_close(
        &mut self,
        action: Action,
        receive: async_channel::Receiver<Result<(), String>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.plugins.send(action) {
            return;
        }
        if let Some(close) = &mut self.plugins.close_draft {
            close.waiting = true;
            close.error = None;
        }
        self.plugin_refresh(window, cx);
        cx.spawn_in(window, async move |workspace, cx| {
            let result = receive
                .recv()
                .await
                .unwrap_or_else(|_| Err("Plugin draft service stopped".into()));
            let _ = cx.update(|window, cx| {
                workspace.update(cx, |workspace, cx| {
                    if let Some(close) = &mut workspace.plugins.close_draft {
                        close.waiting = false;
                        match result {
                            Ok(()) if close.draining => {
                                close.draining = false;
                                workspace.write_close_draft(None, window, cx);
                            }
                            Ok(()) => workspace.finish_draft_close(window, cx),
                            Err(error) => {
                                close.error = Some(error);
                                workspace.plugin_refresh(window, cx);
                            }
                        }
                    }
                })
            });
        })
        .detach();
    }

    fn finish_draft_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(close) = self.plugins.close_draft.take() else {
            return;
        };
        self.plugins.open = false;
        self.plugins.skip_draft_close = true;
        self.request_close(close.target, window, cx);
    }

    /// Show document results in the native list with the cursor in command mode.
    fn open_document_list(&mut self) {
        self.plugins.open = true;
        self.plugins.manager.prompt = None;
        self.plugins.manager.help = false;
        self.plugins.manager.feedback = None;
        self.plugins.manager.selection = 0;
        self.plugins.manager.offset = 0;
        self.plugins.composition.clear();
    }

    fn plugin_refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(trace) = &self.plugins.trace {
            trace.mark(
                "manager_input",
                serde_json::json!({
                    "open": self.plugins.open,
                    "prompt": self.plugins.manager.prompt.is_some(),
                    "page": self.plugins.manager.page as usize,
                    "details": self.plugins.manager.detail.is_some(),
                    "help": self.plugins.manager.help,
                    "search_bytes": self.plugins.manager.search.len(),
                    "selection": self.plugins.manager.selection,
                    "surface_input": self.plugins.surface_focus,
                }),
            );
        }
        self.sync_text_input(window, cx);
        self.update_cursor(window, cx);
        cx.notify();
    }

    /// Observe native composition length without recording text or credential metadata.
    pub(super) fn trace_plugin_preedit(&self, utf16_units: usize) {
        if self.plugins.accepts_text()
            && !self
                .plugins
                .manager
                .prompt
                .as_ref()
                .is_some_and(|input| matches!(input.kind, Prompt::Credential { .. }))
            && let Some(trace) = &self.plugins.trace
        {
            trace.mark(
                "plugin_preedit",
                serde_json::json!({
                    "units": utf16_units,
                    "surface": self.plugins.surface_focus,
                }),
            );
        }
    }

    /// Clip the base editor under document results so they can use the existing window material.
    pub(super) fn document_results_area(&self) -> Option<Rect> {
        (self.plugins.open && (self.plugins.language.is_some() || self.plugins.outline.is_some()))
            .then(|| self.plugin_manager_area())
    }

    /// Center secondary dialogs and give the main plugin list room for its three sections.
    fn plugin_manager_area(&self) -> Rect {
        if self.uses_plugin_list_layout() {
            return ListLayout::area(
                self.layout.visible_cols,
                self.layout.visible_rows.min(self.layout.rows),
            );
        }
        let width = self.layout.cols.saturating_sub(4).min(94);
        let height = self.layout.rows.saturating_sub(3).clamp(MIN_HEIGHT, 25);
        Rect {
            x: self.layout.cols.saturating_sub(width) / 2,
            y: self.layout.rows.saturating_sub(height) / 2,
            w: width,
            h: height,
        }
    }

    fn plugin_surface_area(&self) -> Option<Rect> {
        let key = self.plugins.surface.as_ref()?;
        if Some(key.instance.project) != self.plugins.active || self.modal_open() {
            return None;
        }
        let editor = self.layout.editor;
        let height = editor.h.clamp(4, 10);
        Some(Rect {
            y: editor.y + editor.h.saturating_sub(height),
            h: height,
            ..editor
        })
    }

    /// Shared cursor effects use the same retained frame as the focused plugin surface.
    pub(super) fn plugin_cursor_panel(
        &self,
    ) -> Option<(Entity<Panel>, Rect, Option<gpui::Bounds<gpui::Pixels>>)> {
        let area = if self.plugins.open {
            self.plugin_manager_area()
        } else {
            self.plugin_surface_area()?
        };
        let field = (!self.plugins.open && self.plugins.surface_focus)
            .then(|| self.plugin_input_bounds(area))
            .flatten();
        Some((self.plugins.panel.clone(), area, field))
    }

    /// Resolve a frame whose geometry is still current for this window.
    fn current_plugin_surface(&self) -> Option<&nanobug::plugins::session::Surface> {
        let surface = self.displayed_plugin_surface()?;
        surface
            .frame
            .canvas
            .as_ref()
            .and_then(|canvas| canvas.viewport)
            .is_none_or(|id| {
                self.plugins
                    .viewport
                    .is_some_and(|viewport| viewport.id == id)
            })
            .then_some(surface)
    }

    /// Retain the last prepared drawing and its host input during a viewport transition.
    /// Guest actions still require `current_plugin_surface`.
    fn displayed_plugin_surface(&self) -> Option<&nanobug::plugins::session::Surface> {
        self.plugins
            .view
            .surfaces
            .get(self.plugins.surface.as_ref()?)
    }

    fn canvas_surface(&self) -> Option<&nanobug::plugins::session::Surface> {
        let surface = self.current_plugin_surface()?;
        surface.frame.canvas.as_ref()?;
        Some(surface)
    }

    fn input_cell(&self, area: Rect) -> (usize, usize, usize) {
        if let Some([x, y, width, _]) = self
            .displayed_plugin_surface()
            .and_then(|surface| surface.frame.canvas.as_ref()?.input)
        {
            let column = (x / self.metrics.cw).ceil().max(2.) as usize;
            let row = (y / self.metrics.lh).ceil().max(1.) as usize;
            if column < area.w.saturating_sub(3) && row < area.h - 1 {
                return (
                    column,
                    row,
                    ((width / self.metrics.cw).floor().max(1.) as usize).min(area.w - column - 2),
                );
            }
        }
        (2, area.h - 2, area.w.saturating_sub(4))
    }

    fn plugin_prompt_text(&self, columns: usize) -> (String, String) {
        let Some(input) = &self.plugins.manager.prompt else {
            return (String::new(), String::new());
        };
        let label = match &input.kind {
            Prompt::Search { .. } => "/ ",
            Prompt::Zip => "ZIP path  ",
            Prompt::Development => "Build output path  ",
            Prompt::DevSource { .. } => "Cargo project path  ",
            Prompt::Rule { .. } => "Rule  ",
            Prompt::Setting { .. } => "Value  ",
            Prompt::Credential { .. } => "New credential  ",
            Prompt::ExportDraft => "Export path  ",
            Prompt::Shortcut { .. } => "Shortcut  ",
        };
        if let Prompt::Credential { value, .. } = &input.kind {
            let count = value
                .graphemes(true)
                .count()
                .min(columns.saturating_sub(label.len() + 1));
            return (label.into(), "•".repeat(count));
        }
        let content = input.value.rsplit('\n').next().unwrap_or_default();
        (
            label.into(),
            tail(content, columns.saturating_sub(label.len() + 1)),
        )
    }

    pub(super) fn plugin_caret(&self) -> Option<(f32, f32, f32, bool, bool)> {
        if self.plugins.surface_focus && !self.plugins.open {
            let area = self.plugin_surface_area()?;
            if let Some(native) = self.canvas_input_caret_bounds() {
                return Some((
                    f32::from(native.origin.x - self.metrics.origin.x) / self.metrics.cw,
                    f32::from(native.origin.y - self.metrics.origin.y) / self.metrics.lh,
                    1.,
                    true,
                    true,
                ));
            }
            let (column, row, width) = self.input_cell(area);
            let units = self
                .plugins
                .field
                .layout(
                    &self.plugins.draft,
                    &self.plugins.composition,
                    width,
                    &self.spacing,
                )
                .caret;
            return Some((
                (area.x + column) as f32 + units as f32 / UNITS_PER_COLUMN as f32,
                (area.y + row) as f32,
                1.,
                true,
                true,
            ));
        }
        if !self.plugins.open {
            return None;
        }
        let area = self.plugin_manager_area();
        let manager = &self.plugins.manager;
        if manager.help || manager.confirm_remove.is_some() || self.plugins.view.review.is_some() {
            return Some((0., 0., 1., false, false));
        }
        let layout = self
            .uses_plugin_list_layout()
            .then(|| ListLayout::new(area));
        if manager.prompt.is_some() {
            let (column, row, units) = if let Some(layout) = &layout {
                let slot = layout.filter_text(true);
                let (_, units) = self.plugin_list_prompt(slot.len());
                (slot.start, layout.filter_row(), units)
            } else {
                let (label, text) =
                    self.plugin_prompt_text(area.w.saturating_sub(SEARCH_BUTTONS + 5));
                let units = self
                    .spacing
                    .cells(&(label + &text))
                    .last()
                    .map_or(0, |cell| cell.end);
                (2, 1, units)
            };
            return Some((
                (area.x + column) as f32 + units as f32 / UNITS_PER_COLUMN as f32,
                (area.y + row) as f32,
                1.,
                true,
                true,
            ));
        }
        let entries = layout.map_or_else(|| secondary_list_entries(area), |layout| layout.entries);
        let len = self.plugin_row_count(area);
        let selected = manager.selection.min(len.saturating_sub(1));
        let visible = self.plugin_visible_rows(entries.h, len);
        Some((
            (area.x + entries.x) as f32,
            (area.y + entries.y + selected.saturating_sub(visible.start)) as f32,
            1.,
            false,
            visible.contains(&selected),
        ))
    }

    pub(super) fn plugin_element(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (area, frame) = if self.plugins.open {
            let area = self.plugin_manager_area();
            (area, self.manager_frame(area))
        } else {
            let area = self.plugin_surface_area()?;
            if self.update_plugin_viewport(area, window, cx) {
                self.update_cursor(window, cx);
            }
            self.schedule_canvas_frame(window, cx);
            (area, self.surface_frame(area))
        };
        let panel = self.plugins.panel.clone();
        panel.update(cx, |panel, cx| panel.set(frame, cx));
        let bounds = area.pixels(self.metrics);
        let document_results = self.document_results_area().is_some();
        let inner = div()
            .absolute()
            .left(bounds.origin.x)
            .top(bounds.origin.y)
            .w(bounds.size.width)
            .h(bounds.size.height)
            .overflow_hidden();
        let inner = if document_results {
            inner
        } else {
            inner.bg(rgba(style(cx).panel()))
        };
        let inner = if self.plugins.open {
            inner
        } else {
            let input = self.plugin_surface_input_area(area);
            let report = self.canvas_underline_report(cx);
            inner
                .children(
                    self.displayed_plugin_surface()
                        .and_then(|surface| {
                            canvas::element(surface, self.plugin_input_bounds(area), report)
                        })
                        .map(|canvas| canvas.absolute().left_0().top_0().size_full()),
                )
                .children(input.and_then(|input| self.plugin_selection(area, input, cx)))
                .children(self.canvas_text_element(area, style(cx)))
        };
        let panel = panel.cached(positioned(gpui::Bounds::new(
            gpui::point(gpui::px(0.), gpui::px(0.)),
            bounds.size,
        )));
        let background = div().absolute().left_0().top_0().size_full();
        let background = if self.plugins.open && !document_results {
            background.bg(rgba(0x02070a96))
        } else {
            background
        };
        Some(background.child(inner.child(panel)).into_any_element())
    }

    /// Discard only host text layout; the active guest and its draft stay in place.
    pub(super) fn invalidate_plugin_appearance(&mut self) {
        self.plugins.native_field = None;
    }

    /// Publish the current host style and device scale with this surface's geometry.
    fn update_plugin_viewport(&mut self, area: Rect, window: &Window, cx: &gpui::App) -> bool {
        let Some(key) = self.plugins.surface.clone() else {
            return false;
        };
        let bounds = area.pixels(self.metrics);
        let (width, height, scale) = (
            f32::from(bounds.size.width),
            f32::from(bounds.size.height),
            window.scale_factor(),
        );
        let style = style(cx);
        let appearance = Some(Appearance {
            language: match i18n::locale() {
                i18n::Locale::English => Language::English,
                i18n::Locale::SimplifiedChinese => Language::SimplifiedChinese,
            },
            font_size: self.metrics.font_size,
            base_font_size: Some(self.appearance.base_size),
            font: Some(match self.appearance.font {
                GridFont::RecMono => EditorFont::RecMonoCasual,
                GridFont::MapleMono => EditorFont::MapleMonoNlCn,
            }),
            user_scale: self.appearance.zoom,
            theme: Some(if style.light {
                EditorTheme::Light
            } else {
                EditorTheme::Dark
            }),
            colors: ThemeColors {
                background: style.panel(),
                text: (style.foreground(TEXT) << 8) | 255,
                muted: (style.foreground(MUTED) << 8) | 255,
                accent: (style.foreground(ACCENT) << 8) | 255,
                selection: style.tint(SELECTION),
                warning: (style.foreground(GOLD) << 8) | 255,
                border: Some((style.foreground(MUTED) << 8) | 255),
                error: Some(if style.light { 0x9b263dff } else { 0xff899aff }),
            },
        });
        if self.plugins.viewport.is_some_and(|viewport| {
            (
                viewport.width,
                viewport.height,
                viewport.scale,
                viewport.appearance,
            ) == (width, height, scale, appearance)
        }) {
            return false;
        }
        self.plugins.viewport_id += 1;
        let viewport = Viewport {
            id: self.plugins.viewport_id,
            width,
            height,
            scale,
            apple_surface: cfg!(any(target_os = "macos", target_os = "ios")),
            shaped_line: true,
            appearance,
        };
        if self.plugins.send(Action::Resize(key, viewport)) {
            self.plugins.viewport = Some(viewport);
            if let Some(trace) = &self.plugins.trace {
                trace.mark("plugin_viewport", serde_json::json!(viewport));
            }
            return true;
        }
        false
    }

    fn manager_frame(&self, area: Rect) -> Frame {
        let manager = &self.plugins.manager;
        let mut frame = Frame::new(area.w, area.h);
        let manifest = rows::manifest(&self.plugins);
        let title = if self.plugins.formatting.choices.is_some() {
            "Choose a formatter".into()
        } else if let Some(results) = &self.plugins.language {
            results.title().into()
        } else if self.plugins.outline.is_some() {
            i18n::text("editor.document_outline").into()
        } else if self.plugins.file_plan.is_some()
            || self.plugins.file_busy
            || !self.plugins.file_report.is_empty()
        {
            "Plugin file operations".into()
        } else if self.plugins.edit_plan.is_some() {
            "Plugin edit preview".into()
        } else if self.plugins.close_draft.is_some() {
            "Save plugin inputs".into()
        } else if manager.help {
            i18n::text("plugin_manager.shortcuts").into()
        } else if self.plugins.view.review.is_some() {
            i18n::text("plugin_manager.install").into()
        } else if manager.confirm_remove.is_some() {
            i18n::text("plugin_manager.uninstall").into()
        } else if let Some(preview) = self.comparison_preview() {
            i18n::text(preview.source.title()).into()
        } else {
            manifest.map_or_else(
                || i18n::text("plugin_manager.title").into(),
                |manifest| i18n::format("plugin_manager.details", &[("name", &manifest.name)]),
            )
        };
        frame.border(&title, true, false, &self.spacing);
        if self.uses_plugin_list_layout() {
            self.draw_plugin_list(&mut frame, &ListLayout::new(area));
            return frame;
        }
        let right = area.w.saturating_sub(2);
        let document_list = self.plugins.outline.is_some() || self.plugins.language.is_some();
        if manager.help {
            for (index, line) in rows::HELP.iter().enumerate() {
                frame.label(index + 1, 2..right, line, TEXT, &self.spacing);
            }
            for (index, binding) in shortcuts::bindings(
                &self.plugins.view,
                self.plugins.active,
                self.plugin_command_context(),
            )
            .iter()
            .enumerate()
            .take(area.h.saturating_sub(rows::HELP.len() + 2))
            {
                frame.label(
                    rows::HELP.len() + index + 1,
                    2..right,
                    &format!(
                        "{:<15}{}{}",
                        binding.shortcut,
                        binding.label,
                        binding
                            .issue
                            .map_or_else(String::new, |issue| format!(" · {issue}")),
                    ),
                    TEXT,
                    &self.spacing,
                );
            }
            return frame;
        }
        if let Some(review) = &self.plugins.view.review {
            frame.label(
                1,
                2..right,
                &format!(
                    "{}  {}  ({})",
                    review.manifest.name, review.manifest.version, review.manifest.id
                ),
                TEXT,
                &self.spacing,
            );
            frame.label(
                3,
                2..right,
                if review.additional.is_empty() && review.additional_rule_paths.is_empty() {
                    i18n::text("plugin_manager.no_new_permissions")
                } else {
                    i18n::text("plugin_manager.allow_permissions")
                },
                GOLD,
                &self.spacing,
            );
            let (pages, page) = self.install_review_page(review, area);
            for (row, line) in pages[page].iter().enumerate() {
                frame.label(row + 4, 2..right, line, TEXT, &self.spacing);
            }
            if pages.len() > 1 {
                frame.label(
                    area.h - 3,
                    2..right,
                    &format!("{} / {}   ← Previous   → Next", page + 1, pages.len()),
                    MUTED,
                    &self.spacing,
                );
            }
            self.prompt_buttons(&mut frame, area.h - 2, right.saturating_sub(SEARCH_BUTTONS));
            if page + 1 < pages.len() {
                frame.label(
                    area.h - 2,
                    2..right.saturating_sub(SEARCH_BUTTONS),
                    "Enter continues review",
                    MUTED,
                    &self.spacing,
                );
            }
            return frame;
        }
        if manager.confirm_remove.is_some() {
            frame.label(
                2,
                2..right,
                "Uninstall this plugin? Local settings and drafts will be kept.",
                TEXT,
                &self.spacing,
            );
            self.prompt_buttons(&mut frame, area.h - 2, right.saturating_sub(SEARCH_BUTTONS));
            return frame;
        }
        if manager.prompt.is_some() {
            let (label, text) = self.plugin_prompt_text(area.w.saturating_sub(SEARCH_BUTTONS + 5));
            let preedit = if manager
                .prompt
                .as_ref()
                .is_some_and(|input| matches!(input.kind, Prompt::Credential { .. }))
            {
                "•".repeat(self.plugins.composition.text.graphemes(true).count())
            } else {
                self.plugins.composition.text.clone()
            };
            let displayed = label + &text + &preedit;
            frame.label(
                1,
                2..right.saturating_sub(SEARCH_BUTTONS),
                &displayed,
                TEXT,
                &self.spacing,
            );
            self.prompt_buttons(&mut frame, 1, right.saturating_sub(SEARCH_BUTTONS));
        } else if !document_list
            && matches!(
                manager.detail_page,
                Detail::Rules | Detail::Settings | Detail::Compare
            )
        {
            frame.label(
                1,
                2..right,
                self.plugins.scope_label(),
                MUTED,
                &self.spacing,
            );
        }
        let (labels, selected): (&[_], usize) = if document_list {
            (&[], 0)
        } else if manager.detail.is_some() {
            (
                &[
                    "plugin_manager.overview",
                    "plugin_manager.modules",
                    "plugin_manager.rules",
                    "plugin_manager.logs",
                ],
                match manager.detail_page {
                    Detail::Overview | Detail::Settings => 0,
                    Detail::Modules => 1,
                    Detail::Rules | Detail::Compare => 2,
                    Detail::Logs | Detail::Operations => 3,
                    Detail::Cleanup => 0,
                },
            )
        } else {
            (&[], 0)
        };
        let mut x = 2;
        for (index, label) in labels.iter().enumerate() {
            if index != 0 {
                x = frame.label(2, x..right, " | ", MUTED, &self.spacing);
            }
            x = frame.label(
                2,
                x..right,
                i18n::text(label),
                if index == selected { ACCENT } else { MUTED },
                &self.spacing,
            );
        }
        let entries = secondary_list_entries(area);
        let preview = self.comparison_preview();
        let lines = preview.map(|preview| preview.lines(right.saturating_sub(2), &self.spacing));
        let rows = if preview.is_some() {
            Vec::new()
        } else {
            self.plugins.rows()
        };
        let count = lines.as_ref().map_or(rows.len(), |lines| lines.len());
        let selected = manager.selection.min(count.saturating_sub(1));
        let visible = self.plugin_visible_rows(entries.h, count);
        let offset = visible.start;
        for index in visible {
            let text = match (preview, &lines) {
                (Some(preview), Some(lines)) => preview.line(lines[index].clone()),
                _ => &rows[index].label,
            };
            frame.label(
                entries.y + index - offset,
                2..right,
                text,
                if index == selected { ACCENT } else { TEXT },
                &self.spacing,
            );
        }
        if count == 0 {
            frame.label(
                entries.y,
                2..right,
                self.plugins.language.as_ref().map_or_else(
                    || i18n::text("plugin_manager.empty"),
                    |results| results.empty_label(),
                ),
                MUTED,
                &self.spacing,
            );
        }
        let status = manager
            .feedback
            .as_deref()
            .unwrap_or(&self.plugins.view.message);
        if !document_list && !status.is_empty() {
            frame.label(
                3,
                2..right,
                status.lines().next().unwrap_or(""),
                GOLD,
                &self.spacing,
            );
        }
        let position = preview.map(|_| {
            i18n::format(
                "plugin_manager.compare_position",
                &[("current", &(selected + 1)), ("total", &count)],
            )
        });
        let description = position
            .as_deref()
            .or_else(|| rows.get(selected).map(|row| row.description.as_str()))
            .unwrap_or_default();
        let top = area.h - 5;
        let dash = "╌".repeat(right.saturating_sub(2));
        frame.text(1, top, &format!("┌{dash}┐"), MUTED, right);
        frame.text(1, top + 3, &format!("└{dash}┘"), MUTED, right);
        for line in [top + 1, top + 2] {
            frame.text(1, line, "╎", MUTED, 1);
            frame.text(right, line, "╎", MUTED, 1);
        }
        for (index, line) in wrap_description(description, right.saturating_sub(3), &self.spacing)
            .into_iter()
            .enumerate()
        {
            frame.label(top + 1 + index, 3..right, &line, MUTED, &self.spacing);
        }
        if document_list {
            let hint = if self.plugins.outline.is_some()
                || self
                    .plugins
                    .language
                    .as_ref()
                    .is_some_and(|result| result.is_diagnostic())
            {
                "editor.document_jump_hint"
            } else {
                "editor.document_complete_hint"
            };
            frame.styled_label(
                area.h - 2,
                2..right,
                i18n::text(hint),
                TEXT,
                &self.spacing,
                LabelStyle::Dimmed,
            );
        }
        frame
    }

    /// Only show the preview while the comparison owns the manager's current focus.
    fn comparison_preview(&self) -> Option<&comparison::Preview> {
        let ui = &self.plugins;
        if ui.manager.detail_page != Detail::Compare
            || ui.credential_waiting
            || ui.credential_draft.is_some()
            || ui.formatting.choices.is_some()
            || ui.file_busy
            || ui.file_plan.is_some()
            || !ui.file_report.is_empty()
            || ui.language.is_some()
            || ui.outline.is_some()
            || ui.edit_plan.is_some()
            || ui.edit_check.is_some()
            || ui.close_draft.is_some()
            || ui.view.waiting_for_operations
        {
            return None;
        }
        ui.manager.comparison.as_ref()?.preview.as_ref()
    }

    /// Count exactly the visual rows used by rendering, the cursor and pointer hit testing.
    fn plugin_row_count(&self, area: Rect) -> usize {
        self.comparison_preview().map_or_else(
            || self.plugins.rows().len(),
            |preview| preview.lines(area.w.saturating_sub(4), &self.spacing).len(),
        )
    }

    /// Resolve visible row indices for drawing, caret placement and hit testing after list changes.
    fn plugin_visible_rows(&self, capacity: usize, count: usize) -> Range<usize> {
        let manager = &self.plugins.manager;
        let selected = manager.selection.min(count.saturating_sub(1));
        let offset =
            list_offset(manager.offset, selected, capacity).min(count.saturating_sub(capacity));
        offset..(offset + capacity).min(count)
    }

    /// Draw the prompt actions on `row`, starting at the same column used for mouse input.
    fn prompt_buttons(&self, frame: &mut Frame, row: usize, start: usize) {
        for (slot, (prefix, key)) in prompt_button_columns(start)
            .into_iter()
            .zip([("[✓ ", "Enter"), ("[✕ ", "Esc")])
        {
            let start = frame.label(row, slot.clone(), prefix, TEXT, &self.spacing);
            let end = frame.styled_label(
                row,
                start..slot.end,
                key,
                TEXT,
                &self.spacing,
                LabelStyle::Key,
            );
            frame.label(row, end..slot.end, "]", TEXT, &self.spacing);
        }
    }

    /// Keep every displayed permission and external path visible before granting access.
    fn install_review_page(
        &self,
        review: &nanobug::plugins::session::InstallReview,
        area: Rect,
    ) -> (Vec<Vec<String>>, usize) {
        let columns = area.w.saturating_sub(4).max(1) * UNITS_PER_COLUMN;
        let capacity = area.h.saturating_sub(7).max(1);
        let mut lines = Vec::new();
        for text in review
            .additional
            .iter()
            .map(|permission| permission.description().to_owned())
            .chain(
                review
                    .additional_rule_paths
                    .iter()
                    .map(|path| path.display().to_string()),
            )
        {
            let mut remaining = text.as_str();
            while !remaining.is_empty() {
                let end = self
                    .spacing
                    .fitting_cells(remaining, columns)
                    .last()
                    .map_or_else(
                        || {
                            remaining
                                .graphemes(true)
                                .next()
                                .expect("nonempty review line")
                                .len()
                        },
                        |cell| cell.byte + cell.text.len(),
                    );
                lines.push(remaining[..end].to_owned());
                remaining = &remaining[end..];
            }
        }
        let mut pages = lines
            .chunks(capacity)
            .map(<[String]>::to_vec)
            .collect::<Vec<_>>();
        if pages.is_empty() {
            pages.push(Vec::new());
        }
        let current = if self.plugins.manager.review_token == Some(review.token) {
            self.plugins.manager.review_page.min(pages.len() - 1)
        } else {
            0
        };
        (pages, current)
    }

    fn advance_install_review(&mut self, token: u64, forward: bool, approve: bool) {
        let Some(review) = self
            .plugins
            .view
            .review
            .as_ref()
            .filter(|review| review.token == token)
        else {
            return;
        };
        let (pages, current) = self.install_review_page(review, self.plugin_manager_area());
        self.plugins.manager.review_token = Some(token);
        self.plugins.manager.review_page = if forward {
            (current + 1).min(pages.len() - 1)
        } else {
            current.saturating_sub(1)
        };
        if approve && current + 1 == pages.len() {
            self.plugins.send(Action::Approve(token));
        }
    }

    fn surface_frame(&self, area: Rect) -> Frame {
        let mut frame = Frame::new(area.w, area.h);
        let surface = self.displayed_plugin_surface();
        let title = surface.map_or("Plugin", |surface| surface.frame.title.as_str());
        frame.border(title, self.plugins.surface_focus, false, &self.spacing);
        let right = area.w.saturating_sub(2);
        if let Some(surface) = surface {
            for (index, line) in surface.frame.lines.iter().take(area.h - 3).enumerate() {
                frame.label(1 + index, 2..right, line, TEXT, &self.spacing);
            }
        } else {
            let state = self
                .plugins
                .surface
                .as_ref()
                .and_then(|key| self.plugins.view.modules.get(key));
            frame.label(
                1,
                2..right,
                state.map_or("Module unavailable", |state| {
                    state.label().lines().next().unwrap_or("")
                }),
                GOLD,
                &self.spacing,
            );
        }
        if self.current_native_field().is_none()
            && (self.plugins.surface_focus
                || surface.is_some_and(|surface| {
                    surface.frame.input.is_some()
                        || surface
                            .frame
                            .canvas
                            .as_ref()
                            .is_some_and(|canvas| canvas.input.is_some())
                }))
        {
            let (column, row, width) = self.input_cell(area);
            let text = self.plugins.field.layout(
                &self.plugins.draft,
                &self.plugins.composition,
                width,
                &self.spacing,
            );
            frame.label(
                row,
                column..column + width,
                text.text(),
                TEXT,
                &self.spacing,
            );
        }
        frame
    }

    /// Use native text entry only for the active prompt or plugin text field.
    pub(super) fn plugin_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.plugins.file_busy
            || self.plugins.credential_waiting
            || self
                .plugins
                .close_draft
                .as_ref()
                .is_some_and(|close| close.waiting)
        {
            return true;
        }
        let key = &event.keystroke;
        let raw = key.key.as_str();
        if raw == "f6" && self.plugins.composition.is_empty() {
            if !event.is_held {
                self.cycle_cursor_effect(key.modifiers.shift, window, cx);
            }
            return true;
        }
        if self.plugins.accepts_text() {
            if raw == "escape" {
                if self.plugins.open {
                    self.finish_plugin_prompt(true, window, cx);
                } else {
                    self.plugins.surface_focus = false;
                    self.plugins.surface = None;
                    self.plugins.viewport = None;
                    self.end_plugin_field_drag();
                }
                self.plugins.composition.clear();
            } else if raw == "enter" && self.plugins.composition.is_empty() {
                if self.plugins.open {
                    self.finish_plugin_prompt(false, window, cx);
                } else if let Some(key) = self.plugins.surface.clone()
                    && let Some(revision) = self
                        .current_plugin_surface()
                        .map(|surface| surface.revision)
                {
                    self.plugins.send(Action::SurfaceAction {
                        key,
                        revision,
                        action: "submit".into(),
                        argument: serde_json::Value::Null,
                    });
                }
            } else if !self.plugins.open
                && self.plugins.composition.is_empty()
                && let Some(changed) = self.plugins.field.edit(&mut self.plugins.draft, key)
            {
                if changed {
                    self.plugins.request_draft();
                }
            } else if raw == "backspace" && self.plugins.composition.is_empty() && self.plugins.open
            {
                if let Some(input) = &mut self.plugins.manager.prompt {
                    if let Prompt::Credential { value, .. } = &mut input.kind {
                        if let Some((start, _)) = value.grapheme_indices(true).next_back() {
                            let mut prefix = Zeroizing::new(String::with_capacity(2048));
                            prefix.push_str(&value[..start]);
                            *value = prefix;
                        }
                    } else {
                        pop_grapheme(&mut input.value);
                    }
                    self.live_search();
                }
            } else if raw == "v" && (key.modifiers.control || key.modifiers.platform) {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    self.plugin_commit(&text, window, cx);
                }
            } else {
                return false;
            }
            self.plugin_refresh(window, cx);
            return true;
        }
        if !self.plugins.open {
            return false;
        }
        let command = normal_character(key);
        let printable = command.as_deref().unwrap_or_default();
        if self.plugins.manager.help {
            if matches!(raw, "escape" | "q") || printable == "?" {
                self.plugins.manager.help = false;
            }
        } else if let Some(review) = &self.plugins.view.review {
            let token = review.token;
            match raw {
                "enter" if !event.is_held => self.advance_install_review(token, true, true),
                "right" if !event.is_held => self.advance_install_review(token, true, false),
                "left" if !event.is_held => self.advance_install_review(token, false, false),
                "escape" | "q" => {
                    self.plugins.send(Action::CancelInstall);
                }
                _ => {}
            }
        } else if let Some(namespace) = &self.plugins.manager.confirm_remove {
            if raw == "enter" && !event.is_held {
                self.plugins.send(Action::Uninstall(namespace.clone()));
                self.plugins.manager.confirm_remove = None;
            } else if matches!(raw, "escape" | "q") {
                self.plugins.manager.confirm_remove = None;
            }
        } else {
            let count = self.plugin_row_count(self.plugin_manager_area());
            match (raw, printable) {
                ("escape", _) | (_, "q") => self.plugin_back(),
                (_, "?") => self.plugins.manager.help = true,
                (_, "/") if self.uses_plugin_list_layout() => self.open_plugin_search(),
                ("up", _) | (_, "k") => {
                    self.plugins.manager.selection =
                        self.plugins.manager.selection.saturating_sub(1)
                }
                ("down", _) | (_, "j") => {
                    self.plugins.manager.selection =
                        (self.plugins.manager.selection + 1).min(count.saturating_sub(1))
                }
                ("home", _) => self.plugins.manager.selection = 0,
                ("end", _) => self.plugins.manager.selection = count.saturating_sub(1),
                ("enter", _) if !event.is_held => self.plugin_activate(window, cx),
                (_, "1" | "2" | "3" | "4")
                    if self.plugins.outline.is_none() && self.plugins.language.is_none() =>
                {
                    self.plugin_tab(printable.as_bytes()[0] - b'1')
                }
                _ => {}
            }
        }
        self.plugin_refresh(window, cx);
        true
    }

    pub(super) fn plugin_commit(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.plugins.accepts_text() {
            return false;
        }
        if let Some(input) = &mut self.plugins.manager.prompt {
            let multiline = matches!(input.kind, Prompt::Rule { .. } | Prompt::Setting { .. });
            let clean = || {
                text.chars()
                    .filter(|&ch| !ch.is_control() || multiline && matches!(ch, '\t' | '\n'))
            };
            let (value, limit) = match &mut input.kind {
                Prompt::Credential { value, .. } => (&mut **value, 2048),
                _ => (&mut input.value, 64 * 1024),
            };
            if value.len() + clean().map(char::len_utf8).sum::<usize>() <= limit {
                value.extend(clean());
            } else {
                self.plugins.manager.feedback =
                    Some("Input is too long. The text was kept unchanged.".into());
            }
            self.live_search();
        } else {
            let clean = text
                .chars()
                .filter(|ch| !ch.is_control())
                .collect::<String>();
            if self.plugins.field.insert(&mut self.plugins.draft, &clean) {
                self.plugins.request_draft();
            } else {
                self.set_status("Plugin input is too long. The draft was kept unchanged.".into());
            }
        }
        self.plugin_refresh(window, cx);
        true
    }

    pub(super) fn plugin_click(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let (x, y) = self.metrics.cell(event.position);
        if self.plugins.open {
            if self.plugins.file_busy
                || self.plugins.credential_waiting
                || self
                    .plugins
                    .close_draft
                    .as_ref()
                    .is_some_and(|close| close.waiting)
            {
                return true;
            }
            let area = self.plugin_manager_area();
            if area.contains(x, y) && event.button == gpui::MouseButton::Left {
                let x = x - area.x;
                let row = y - area.y;
                self.plugin_manager_click(area, (x, row), event.click_count > 1, window, cx);
            }
            self.plugin_refresh(window, cx);
            return true;
        }
        if let Some(area) = self.plugin_surface_area()
            && area.contains(x, y)
        {
            if self
                .plugin_input_bounds(area)
                .is_some_and(|input| input.contains(&event.position))
            {
                self.clear_canvas_focus();
                self.plugins.surface_focus = true;
                let input = self.plugin_surface_input_area(area);
                self.press_plugin_field(input, event.position, event.modifiers.shift);
            } else if !self.plugin_canvas_down(event)
                && let Some(surface) = self.canvas_surface()
            {
                let bounds = area.pixels(self.metrics);
                let position = event.position - bounds.origin;
                let (px, py) = (f32::from(position.x), f32::from(position.y));
                if let Some(hit) = surface.frame.canvas.as_ref().and_then(|canvas| {
                    canvas.hits.iter().rev().find(|hit| {
                        let [x, y, w, h] = hit.bounds;
                        (x..x + w).contains(&px) && (y..y + h).contains(&py)
                    })
                }) {
                    let key = self.plugins.surface.clone().expect("Canvas owner");
                    self.plugins.send(Action::SurfaceAction {
                        key,
                        revision: surface.revision,
                        action: hit.action.clone(),
                        argument: serde_json::json!({"x": px, "y": py}),
                    });
                }
            }
            self.plugin_refresh(window, cx);
            return true;
        }
        self.clear_canvas_focus();
        self.end_plugin_field_drag();
        if self.plugins.surface_focus {
            self.plugins.surface_focus = false;
            self.plugins.composition.clear();
            self.plugin_refresh(window, cx);
        }
        false
    }

    /// Dispatch a panel-relative click. A prompt or confirmation owns the mouse until it closes.
    fn plugin_manager_click(
        &mut self,
        area: Rect,
        (x, row): (usize, usize),
        activate: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.plugins.manager.help {
            return;
        }
        let review = self.plugins.view.review.as_ref().map(|review| review.token);
        let removal = self.plugins.manager.confirm_remove.as_ref();
        if review.is_some() || removal.is_some() {
            if row == area.h - 2
                && let Some(cancel) = prompt_button_at(area.w.saturating_sub(SEARCH_BUTTONS + 2), x)
            {
                if let Some(token) = review {
                    if cancel {
                        self.plugins.send(Action::CancelInstall);
                    } else {
                        self.advance_install_review(token, true, true);
                    }
                } else if let Some(namespace) = self.plugins.manager.confirm_remove.take()
                    && !cancel
                {
                    self.plugins.send(Action::Uninstall(namespace));
                }
            }
            return;
        }

        let layout = self
            .uses_plugin_list_layout()
            .then(|| ListLayout::new(area));
        if self.plugins.manager.prompt.is_some() {
            let (input_row, start) = layout
                .as_ref()
                .map_or((1, area.w.saturating_sub(SEARCH_BUTTONS + 2)), |layout| {
                    (layout.filter_row(), layout.filter_text(true).end)
                });
            if row == input_row
                && let Some(cancel) = prompt_button_at(start, x)
            {
                self.finish_plugin_prompt(cancel, window, cx);
            }
            return;
        }
        let entries = if let Some(layout) = layout {
            if row == layout.filter_row() && layout.filter_text(false).contains(&x) {
                self.open_plugin_search();
                return;
            }
            if row == layout.list.y {
                if let Some(tab) = layout
                    .tabs(&self.spacing)
                    .iter()
                    .position(|slot| slot.contains(&x))
                {
                    self.plugin_tab(tab as u8);
                }
                return;
            }
            layout.entries
        } else {
            secondary_list_entries(area)
        };
        if entries.contains(x, row) {
            let visible = self.plugin_visible_rows(entries.h, self.plugin_row_count(area));
            let index = visible.start + row - entries.y;
            if visible.contains(&index) {
                self.plugins.manager.selection = index;
                if activate {
                    self.plugin_activate(window, cx);
                }
            }
        }
    }

    fn plugin_tab(&mut self, index: u8) {
        if self.plugins.credential_draft.is_some() {
            return;
        }
        let manager = &mut self.plugins.manager;
        manager.comparison = None;
        if manager.detail.is_some() {
            manager.detail_page = match index {
                0 => Detail::Overview,
                1 => Detail::Modules,
                2 => Detail::Rules,
                3 => Detail::Logs,
                _ => return,
            };
        } else {
            let page = match index {
                0 => Page::Installed,
                1 => Page::Marketplace,
                2 => Page::Zip,
                _ => return,
            };
            if manager.page == page {
                return;
            }
            manager.page = page;
            if page == Page::Marketplace {
                self.plugins.send(Action::OpenMarketplace);
            }
        }
        self.plugins.manager.selection = 0;
        self.plugins.manager.offset = 0;
    }

    fn plugin_back(&mut self) {
        if self.plugins.credential_draft.is_some() {
            self.edit_plugin_credential();
            return;
        }
        if self.plugins.formatting.choices.take().is_some() {
            self.plugins.open = false;
            return;
        }
        if self.plugins.file_plan.take().is_some() || !self.plugins.file_report.is_empty() {
            self.plugins.file_report.clear();
            return;
        }
        if self.plugins.language.take().is_some() || self.plugins.outline.take().is_some() {
            self.plugins.open = false;
            return;
        }
        if self.plugins.edit_plan.take().is_some() || self.plugins.edit_check.take().is_some() {
            return;
        }
        if self.plugins.close_draft.take().is_some() {
            self.plugins.open = false;
            return;
        }
        let manager = &mut self.plugins.manager;
        if self.plugins.view.installing {
            self.plugins.send(Action::CancelInstall);
            return;
        }
        if matches!(manager.detail_page, Detail::Settings | Detail::Cleanup) {
            manager.detail_page = Detail::Overview;
            manager.selection = 0;
        } else if manager.detail_page == Detail::Compare {
            if let Some(comparison) = &mut manager.comparison
                && let Some(preview) = comparison.preview.take()
            {
                (manager.selection, manager.offset) = preview.from;
            } else if let Some(comparison) = manager.comparison.take() {
                (manager.detail_page, manager.selection, manager.offset) = comparison.from;
            } else {
                manager.detail_page = Detail::Rules;
                manager.selection = 0;
            }
        } else if manager.detail_page == Detail::Operations {
            manager.detail_page = Detail::Overview;
            manager.selection = 0;
        } else if manager.detail.take().is_some() {
            manager.selection = manager.list_selection;
            manager.offset = 0;
            manager.detail_page = Detail::Overview;
        } else {
            self.plugins.open = false;
        }
    }

    fn plugin_activate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.comparison_preview().is_some() {
            return;
        }
        let rows = self.plugins.rows();
        let selected = self
            .plugins
            .manager
            .selection
            .min(rows.len().saturating_sub(1));
        let Some(row) = rows.into_iter().nth(selected) else {
            return;
        };
        match row.target {
            Target::None => {}
            Target::Details(namespace) => {
                self.plugins.manager.detail = Some(namespace);
                self.plugins.manager.list_selection = selected;
                self.plugins.manager.selection = 0;
                self.plugins.manager.offset = 0;
            }
            Target::Zip => {
                self.plugins.manager.prompt = Some(Input {
                    kind: Prompt::Zip,
                    value: String::new(),
                })
            }
            Target::Development => {
                self.plugins.manager.prompt = Some(Input {
                    kind: Prompt::Development,
                    value: String::new(),
                });
            }
            Target::Action(action) => {
                if let Action::ResolveConfig(scope, revision, _) = action {
                    if self
                        .plugins
                        .view
                        .configurations
                        .get(&scope)
                        .is_none_or(|view| view.revision != revision)
                    {
                        self.plugin_back();
                        self.plugins.manager.feedback =
                            Some(i18n::text("plugin_manager.compare_changed_again").into());
                        return;
                    }
                    if self.plugins.send(action) {
                        self.plugin_back();
                    }
                } else {
                    self.plugins.send(action);
                }
            }
            Target::Uninstall(key) => self.plugins.manager.confirm_remove = Some(key),
            Target::Surface(key) => {
                let Some(surface) = self.plugins.view.surfaces.get(&key) else {
                    return;
                };
                self.plugins.open = false;
                self.plugins.surface_focus = surface.frame.input.is_some()
                    || surface
                        .frame
                        .canvas
                        .as_ref()
                        .is_some_and(|canvas| canvas.has_input());
                self.plugins.draft.clone_from(&surface.draft);
                self.plugins.field = field::Field::default();
                self.plugins.surface = Some(key);
                self.plugins.pending_draft = None;
                self.plugins.viewport = None;
            }
            Target::Scope => {
                if self.plugins.active.is_some() {
                    let manager = &mut self.plugins.manager;
                    if manager.detail_page == Detail::Settings
                        && !manager.all_projects
                        && !manager.local_project
                    {
                        manager.local_project = true;
                    } else {
                        manager.all_projects = !manager.all_projects;
                        manager.local_project = false;
                    }
                }
            }
            Target::Detail(detail) => {
                if detail == Detail::Compare {
                    let configurations = Arc::clone(&self.plugins.view.configurations);
                    let Some(scope) = self
                        .plugins
                        .scope()
                        .filter(|scope| configurations.contains_key(scope))
                    else {
                        return;
                    };
                    let manager = &mut self.plugins.manager;
                    manager.comparison = Some(comparison::Comparison {
                        scope,
                        configurations,
                        from: (manager.detail_page, selected, manager.offset),
                        preview: None,
                    });
                }
                self.plugins.manager.detail_page = detail;
                self.plugins.manager.selection = 0;
                self.plugins.manager.offset = 0;
                if detail == Detail::Operations
                    && let Some(namespace) = &self.plugins.manager.detail
                {
                    self.plugins
                        .send(Action::ReviewToolOperations(namespace.clone()));
                }
                if detail == Detail::Cleanup {
                    self.plugins.manager.clean_credentials = false;
                    self.plugins.manager.clean_local = true;
                }
            }
            Target::Compare(source) => {
                let manager = &mut self.plugins.manager;
                if let Some(comparison) = &mut manager.comparison
                    && comparison.open(source, selected, manager.offset)
                {
                    manager.selection = 0;
                    manager.offset = 0;
                }
            }
            Target::Retained => {
                self.plugins.manager.retained_open = !self.plugins.manager.retained_open
            }
            Target::CleanCredentials => {
                self.plugins.manager.clean_credentials = !self.plugins.manager.clean_credentials
            }
            Target::CleanLocal => {
                self.plugins.manager.clean_local = !self.plugins.manager.clean_local
            }
            Target::Prompt(input) => self.plugins.manager.prompt = Some(input),
            Target::RetryDraftClose => self.advance_plugin_close(window, cx),
            Target::ExportDraft => {
                self.plugins.manager.prompt = Some(Input {
                    kind: Prompt::ExportDraft,
                    value: String::new(),
                })
            }
            Target::DiscardDraftClose => {
                let scope = self.plugins.close_draft.as_ref().and_then(|close| {
                    (close.target == CloseTarget::Project)
                        .then_some(self.plugins.active)
                        .flatten()
                });
                let (reply, receive) = async_channel::bounded(1);
                self.wait_draft_close(Action::DiscardDrafts(scope, reply), receive, window, cx);
            }
            Target::CancelDraftClose => self.plugin_back(),
            Target::ApplyCredential => self.save_plugin_credential(window, cx),
            Target::EditCredential => self.edit_plugin_credential(),
            Target::DiscardCredential => {
                self.plugins.credential_draft = None;
                self.finish_credential_close(window, cx);
            }
            Target::Command(key, id) => self.plugin_command(key, id, window, cx),
            Target::ApplyEdits => self.apply_plugin_edits(window, cx),
            Target::ApplyFiles => self.apply_plugin_files(window, cx),
            Target::Formatter(key) => self.choose_formatter(key, window, cx),
            Target::Completion(text) => {
                if let Some(edit) = self
                    .plugins
                    .language
                    .as_ref()
                    .map(|results| results.insertion(text))
                {
                    self.use_language_result(Some(edit), None, window, cx);
                }
            }
            Target::CompletionEdit(edit) => self.use_language_result(Some(edit), None, window, cx),
            Target::Diagnostic(offset) => self.use_language_result(None, Some(offset), window, cx),
            Target::Symbol(offset) => self.jump_to_symbol(offset, window, cx),
        }
    }

    /// Open the filter from the keyboard or by clicking its input row.
    fn open_plugin_search(&mut self) {
        let manager = &mut self.plugins.manager;
        manager.prompt = Some(Input {
            kind: Prompt::Search {
                original: manager.search.clone(),
                selection: manager.selection,
            },
            value: manager.search.clone(),
        });
    }

    fn live_search(&mut self) {
        let manager = &mut self.plugins.manager;
        if let Some(input) = &manager.prompt
            && matches!(input.kind, Prompt::Search { .. })
        {
            manager.search.clone_from(&input.value);
            manager.selection = 0;
            manager.offset = 0;
        }
    }

    fn finish_plugin_prompt(&mut self, cancel: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(input) = self.plugins.manager.prompt.take() else {
            return;
        };
        match input.kind {
            Prompt::Search {
                original,
                selection,
            } => {
                if cancel {
                    self.plugins.manager.search = original;
                    self.plugins.manager.selection = selection;
                }
            }
            Prompt::Zip if !cancel => {
                let path = PathBuf::from(input.value.trim().trim_matches('"'));
                let path = self
                    .projects
                    .active_project()
                    .map_or_else(|| path.clone(), |project| project.root.join(&path));
                self.plugins.send(Action::InspectZip(path));
            }
            Prompt::Development if !cancel => {
                let path = PathBuf::from(input.value.trim().trim_matches('"'));
                self.plugins.send(Action::InspectDevelopment(path));
            }
            Prompt::DevSource {
                namespace,
                mut settings,
            } if !cancel => {
                let value = input.value.trim().trim_matches('"');
                settings.source = (!value.is_empty()).then(|| PathBuf::from(value));
                if settings
                    .source
                    .as_ref()
                    .is_some_and(|path| !path.is_absolute())
                {
                    self.plugins.manager.feedback =
                        Some("Enter an absolute Cargo project path".into());
                    self.plugins.manager.prompt = Some(Input {
                        kind: Prompt::DevSource {
                            namespace,
                            settings,
                        },
                        value: input.value,
                    });
                    return;
                }
                if settings.source.is_none() {
                    settings.automatic = false;
                }
                self.plugins
                    .send(Action::SetDeveloperSettings(namespace, settings));
            }
            Prompt::Rule {
                scope,
                owner,
                module,
            } if !cancel => {
                let value = match input.value.trim() {
                    "" | "inherit" => None,
                    "true" => Some(Rule::Always),
                    "false" => Some(Rule::Never),
                    _ => Some(Rule::Script(input.value)),
                };
                self.plugins.send(Action::EditConfig(
                    scope,
                    ConfigEdit::Rule {
                        owner,
                        module,
                        value,
                    },
                ));
            }
            Prompt::Setting {
                scope,
                owner,
                id,
                renamed_from,
                original,
            } if !cancel => {
                let value = if matches!(input.value.trim(), "inherit" | "") {
                    None
                } else {
                    match serde_json::from_str(&input.value) {
                        Ok(parsed) => Some(parsed),
                        Err(error) => {
                            self.plugins.manager.feedback =
                                Some(format!("Invalid JSON value: {error}"));
                            self.plugins.manager.prompt = Some(Input {
                                kind: Prompt::Setting {
                                    scope,
                                    owner,
                                    id,
                                    renamed_from,
                                    original,
                                },
                                value: input.value,
                            });
                            return;
                        }
                    }
                };
                self.plugins.send(Action::EditConfig(
                    scope,
                    ConfigEdit::Setting {
                        owner,
                        id,
                        renamed_from,
                        original,
                        value,
                    },
                ));
            }
            Prompt::Credential {
                namespace,
                slot,
                value,
            } => {
                self.plugins.composition.text.zeroize();
                if !value.is_empty() {
                    self.review_plugin_credential(namespace, slot, value);
                }
            }
            Prompt::ExportDraft if !cancel => {
                let path = PathBuf::from(input.value.trim().trim_matches('"'));
                if path.is_absolute() {
                    self.write_close_draft(Some(path), window, cx);
                } else {
                    self.plugins.manager.feedback = Some("Enter an absolute export path".into());
                    self.plugins.manager.prompt = Some(input);
                }
            }
            Prompt::Shortcut { namespace, command } if !cancel => {
                let value = input.value.trim();
                let error = if value == "inherit" || value.is_empty() {
                    None
                } else if let Some(stroke) = shortcuts::parse(value) {
                    let selected = shortcuts::declaration(&self.plugins.view, &namespace, &command);
                    self.plugins
                        .view
                        .configurations
                        .get(&ConfigScope::AllProjects)
                        .and_then(|view| {
                            view.draft
                                .as_ref()
                                .unwrap_or(&view.applied)
                                .shortcuts
                                .iter()
                                .find_map(|(owner, mappings)| {
                                    mappings.iter().find_map(|(other, mapped)| {
                                        let same = owner == &namespace && other == &command;
                                        let overlaps = selected
                                            .zip(shortcuts::declaration(
                                                &self.plugins.view,
                                                owner,
                                                other,
                                            ))
                                            .is_none_or(|(current, prior)| {
                                                current.shares_context(prior)
                                            });
                                        (!same && overlaps)
                                            .then(|| shortcuts::parse(mapped))
                                            .flatten()
                                            .filter(|existing| existing == &stroke)
                                            .map(|_| {
                                                format!("This key is already assigned to {other}.")
                                            })
                                    })
                                })
                        })
                } else {
                    Some("Choose a free function key or Ctrl+Alt chord. Enter inherit or leave empty to disable.".into())
                };
                if let Some(error) = error {
                    self.plugins.manager.feedback = Some(error);
                    self.plugins.manager.prompt = Some(Input {
                        kind: Prompt::Shortcut { namespace, command },
                        value: input.value,
                    });
                    return;
                }
                self.plugins.send(Action::EditConfig(
                    ConfigScope::AllProjects,
                    ConfigEdit::Shortcut {
                        namespace,
                        command,
                        value: (value != "inherit").then(|| value.into()),
                    },
                ));
            }
            _ => {}
        }
        self.plugins.composition.clear();
    }

    fn review_plugin_credential(
        &mut self,
        namespace: String,
        slot: String,
        value: Zeroizing<String>,
    ) {
        self.plugins.credential_draft = Some(CredentialDraft {
            namespace,
            slot,
            value,
        });
        self.plugins.composition.text.zeroize();
        self.plugins.composition.clear();
        self.plugins.manager.selection = 0;
        self.plugins.manager.offset = 0;
        self.plugins.manager.feedback = None;
    }

    fn edit_plugin_credential(&mut self) {
        if let Some(draft) = self.plugins.credential_draft.take() {
            self.plugins.credential_close = None;
            self.plugins.manager.prompt = Some(Input {
                kind: Prompt::Credential {
                    namespace: draft.namespace,
                    slot: draft.slot,
                    value: draft.value,
                },
                value: String::new(),
            });
        }
    }

    fn finish_credential_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(target) = self.plugins.credential_close.take() {
            self.plugins.open = false;
            self.request_close(target, window, cx);
        }
    }

    fn save_plugin_credential(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = self.plugins.credential_draft.take() else {
            return;
        };
        let (reply, response) = async_channel::bounded(1);
        if !self.plugins.send(Action::SetCredential {
            namespace: draft.namespace.clone(),
            slot: draft.slot.clone(),
            value: draft.value.clone(),
            reply,
        }) {
            self.plugins.credential_draft = Some(draft);
            return;
        }
        self.plugins.credential_waiting = true;
        self.plugins.manager.feedback = Some("Saving credential…".into());
        cx.spawn_in(window, async move |workspace, cx| {
            let result = response
                .recv()
                .await
                .unwrap_or_else(|_| Err("Credential store stopped".into()));
            let _ = cx.update(|window, cx| {
                workspace.update(cx, |workspace, cx| {
                    workspace.plugins.credential_waiting = false;
                    match result {
                        Ok(()) => {
                            workspace.plugins.manager.feedback = Some("Credential saved".into());
                            workspace.finish_credential_close(window, cx);
                        }
                        Err(error) => {
                            workspace.plugins.manager.feedback = Some(error);
                            workspace.plugins.credential_draft = Some(draft);
                        }
                    }
                    workspace.plugin_refresh(window, cx);
                })
            });
        })
        .detach();
    }
}

fn pop_grapheme(text: &mut String) {
    if let Some((start, _)) = text.grapheme_indices(true).next_back() {
        text.truncate(start);
    }
}

fn snapshot_from(doc: &nanobug::document::Document) -> DocumentSnapshot {
    DocumentSnapshot {
        path: doc.path.clone(),
        opening: doc.opening,
        revision: doc.revision,
        text: doc.text.clone(),
    }
}

fn list_offset(offset: usize, selected: usize, capacity: usize) -> usize {
    offset
        .min(selected)
        .max((selected + 1).saturating_sub(capacity))
}

/// Place secondary dialog entries below the tabs and above the two-line description.
fn secondary_list_entries(area: Rect) -> Rect {
    Rect {
        x: 2,
        y: LIST_START,
        w: area.w.saturating_sub(4),
        h: area.h.saturating_sub(LIST_START + 5),
    }
}

/// Locate the Enter and Esc buttons from the prompt's reserved left edge.
fn prompt_button_columns(left: usize) -> [Range<usize>; 2] {
    [left..left + 9, left + 11..left + 18]
}

/// Resolve a click at `column` to its visible button. `true` means cancel.
fn prompt_button_at(left: usize, column: usize) -> Option<bool> {
    prompt_button_columns(left)
        .iter()
        .position(|slot| slot.contains(&column))
        .map(|button| button == 1)
}

fn tail(text: &str, columns: usize) -> String {
    let mut width = 0;
    let mut start = text.len();
    for (index, grapheme) in text.grapheme_indices(true).rev() {
        width += unicode_width::UnicodeWidthStr::width(grapheme).max(1);
        if width > columns {
            break;
        }
        start = index;
    }
    text[start..].to_owned()
}

/// Wrap the `text` to two rows using native glyph advances. Truncate the second with an ellipsis.
fn wrap_description(text: &str, columns: usize, spacing: &TextSpacing) -> Vec<String> {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let end = spacing
        .fitting_cells(&text, columns * UNITS_PER_COLUMN)
        .last()
        .map_or(0, |cell| cell.byte + cell.text.len());
    if end == 0 || end == text.len() {
        return vec![spacing.ellipsize(&text, columns).into_owned()];
    }
    let split = if text[end..].starts_with(' ') {
        end
    } else {
        text[..end]
            .rfind(' ')
            .filter(|index| *index > end / 2)
            .unwrap_or(end)
    };
    vec![
        text[..split].to_owned(),
        spacing
            .ellipsize(text[split..].trim_start(), columns)
            .into_owned(),
    ]
}
