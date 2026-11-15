//! Coordinate plugin sessions, editor operations and the two independent plugin windows.

use crate::grid::windows::plugin_surface::{SurfaceWindow, canvas};

#[cfg(all(test, windows))]
mod credential_tests;
#[cfg(all(test, windows))]
mod cursor_tests;
mod edits;
mod field;
mod file_ops;
mod formatting;
mod hover;
mod interaction;
mod language;

mod closing;
mod manager_input;
#[cfg(all(test, windows))]
mod native_editing_tests;
mod native_field;
mod outline;
mod presentation;
mod prompts;
#[cfg(all(test, windows))]
mod rust_analyzer_test;
#[cfg(all(test, windows))]
mod rust_plugin_test;
mod session_link;
mod shortcuts;
mod sql_dialect;
mod surface_link;
mod workflow;

use super::{Workspace, editor_close::CloseTarget, view::positioned};
use crate::grid::windows::plugin_manager::{
    Detail, Input, ListLayout, MIN_HEIGHT, ManagerView, ManagerWindow, Page, Presentation, Prompt,
    SEARCH_BUTTONS, comparison, prompt_button_at, rows, secondary_description,
    secondary_list_entries,
};
use crate::{
    grid::render::{Metrics, Panel, Rect, dashed_outline_element},
    ui::{
        ime::Composition,
        keys::normal_character,
        text::UNITS_PER_COLUMN,
        theme::{ACCENT, GOLD, GridFont, MUTED, PLUGIN_ERROR, SELECTION, TEXT, style},
    },
};
use gpui::{
    AnyElement, AppContext, Context, Entity, IntoElement, KeyDownEvent, MouseDownEvent,
    ParentElement, Styled, Task, Window, div, rgba,
};
use nanobug::i18n;
use nanobug::plugins::session::{
    Action, ConfigEdit, ConfigScope, Options, Project, Rule, Session, Trace, View,
};
use nanobug_plugin_sdk::{
    DocumentSnapshot, Permission,
    canvas::{Appearance, EditorFont, EditorTheme, Language, ThemeColors, Viewport},
};
use rows::{Row, Target};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};
use unicode_segmentation::UnicodeSegmentation;
use zeroize::{Zeroize, Zeroizing};

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

/// Retain host-owned input and the latest scene independently of plugin processes.
pub(super) struct PluginCoordinator {
    pub(super) surface: SurfaceWindow,
    options: Option<Options>,
    trace: Option<Trace>,
    traced_doc: Option<(u64, u64)>,
    /// Last document revision represented by the native workspace scene.
    traced_scene: Option<(u64, u64)>,
    session: Option<Session>,
    view: Arc<View>,
    panel: Entity<Panel>,
    manager: ManagerWindow,
    snapshots: BTreeMap<u64, Arc<DocumentSnapshot>>,
    open_snapshots: BTreeMap<(u64, u64), Arc<DocumentSnapshot>>,
    active: Option<u64>,
    projects: Vec<Project>,
    /// SQL overrides in the last project snapshot sent to the supervisor.
    sql_dialects: BTreeMap<u64, String>,
    pub(super) open: bool,
    pub(super) composition: Composition,
    credential_draft: Option<CredentialDraft>,
    credential_waiting: bool,
    credential_close: Option<CloseTarget>,
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
    /// Definition replies accumulate without opening a list or taking editor focus.
    pending_definition: Option<language::LanguageResults>,
    /// Detail text and the native query tied to the current normal-mode caret.
    definition_hover: Option<hover::DefinitionHover>,
    /// Insert-mode results retain editor input and render beside its caret.
    completion: Option<language::CompletionPopup>,
    /// The timer for the latest eligible edit, cancelled by new input or dismissal.
    completion_delay: Option<Task<()>>,
    /// Outline of the current version, shown by the existing plugin list popup.
    outline: Option<outline::Outline>,
    request_id: u64,
    skip_draft_close: bool,
    formatting: formatting::Tasks,
}

impl PluginCoordinator {
    pub(super) fn new(
        options: Option<Options>,
        trace: Option<Trace>,
        metrics: Metrics,
        cx: &mut Context<Workspace>,
    ) -> Self {
        Self {
            surface: SurfaceWindow::default(),
            options,
            trace,
            traced_doc: None,
            traced_scene: None,
            session: None,
            view: Arc::new(View::default()),
            panel: cx.new(|_| Panel::new(metrics)),
            manager: ManagerWindow::default(),
            snapshots: BTreeMap::new(),
            open_snapshots: BTreeMap::new(),
            active: None,
            projects: Vec::new(),
            sql_dialects: BTreeMap::new(),
            open: false,
            composition: Composition::default(),
            credential_draft: None,
            credential_waiting: false,
            credential_close: None,
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
            pending_definition: None,
            definition_hover: None,
            completion: None,
            completion_delay: None,
            outline: None,
            request_id: 0,
            skip_draft_close: false,
            formatting: formatting::Tasks::default(),
        }
    }

    pub(super) fn owns_input(&self) -> bool {
        self.open || self.surface.focused
    }

    /// Whether the open manager owns a text prompt.
    pub(super) fn manager_accepts_text(&self) -> bool {
        self.open && self.manager.prompt.is_some()
    }

    pub(super) fn accepts_text(&self) -> bool {
        if self.open {
            self.manager_accepts_text()
        } else {
            self.surface.focused
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
        self.manager.scope(self.active)
    }

    /// Publish the confirmed surface draft and retain it until the session acknowledges it.
    fn request_draft(&mut self) {
        if let Some(key) = self.surface.key.clone()
            && let Some(session) = &self.session
        {
            session.draft(key, self.surface.draft.clone());
            self.surface.pending_draft = Some(self.surface.draft.clone());
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
        if let Some(key) = &self.surface.key
            && let Some(surface) = self.view.surfaces.get(key)
            && self
                .surface
                .pending_draft
                .as_ref()
                .is_none_or(|value| value == &surface.draft)
        {
            if self.surface.draft != surface.draft {
                self.surface.draft.clone_from(&surface.draft);
                self.surface.field = field::Field::default();
            }
            self.surface.pending_draft = None;
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
        if self.has_operation_rows() {
            workflow::rows(self)
        } else {
            rows::build(&self.row_context())
        }
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

impl PluginCoordinator {
    /// Whether an editor operation temporarily replaces the package browser's rows.
    fn has_operation_rows(&self) -> bool {
        self.credential_waiting
            || self.credential_draft.is_some()
            || self.formatting.choices.is_some()
            || self.file_busy
            || self.file_plan.is_some()
            || !self.file_report.is_empty()
            || self.language.is_some()
            || self.outline.is_some()
            || self.edit_plan.is_some()
            || self.edit_check.is_some()
            || self.close_draft.is_some()
    }

    /// Publish immutable package facts; native text layout contributes only its byte count.
    fn row_context(&self) -> rows::RowContext<'_> {
        rows::RowContext {
            manager: &self.manager,
            view: &self.view,
            active: self.active,
            projects: &self.projects,
            snapshots: &self.snapshots,
            surface: self.surface.key.as_ref(),
            native_field_bytes: self
                .surface
                .native_field
                .as_ref()
                .map_or(0, |field| field.owned_layout_bytes()),
        }
    }
}
