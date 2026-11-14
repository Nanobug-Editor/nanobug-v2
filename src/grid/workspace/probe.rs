//! Test surface on the existing grid. Input and its logical caret stay in the host.

use super::{Workspace, layout::Focus};
use crate::{
    grid::render::{Frame, GridLayer, Metrics, Panel, Rect},
    plugin_probe::{Client, Control, Options, Project, Scene, Surface},
    ui::{
        ime::Composition,
        text::{TextSpacing, UNITS_PER_COLUMN},
        theme::{ACCENT, MUTED, TEXT},
    },
};
use gpui::{AppContext, Context, Entity, KeyDownEvent, Pixels, Point, Window};
use nanobug_probe_wire::{Draft, MAX_DRAFT, Owner};
use serde_json::json;
use std::{collections::BTreeMap, path::PathBuf, time::Instant};
use unicode_segmentation::UnicodeSegmentation;

/// Retained native state of the opt-in experiment.
pub(super) struct Probe {
    /// Launch options consumed after the core's first frame.
    options: Option<Options>,
    /// Connection to the supervisor after core readiness.
    pub(super) client: Option<Client>,
    /// Project-scoped host drafts, independent of WASM process lifetime.
    projects: BTreeMap<PathBuf, Project>,
    /// Assigns a fresh scope on each project opening.
    next_scope: u64,
    /// Most recent view published by the supervisor.
    scene: Scene,
    /// Cached native grid for the active project.
    pub(super) panel: Entity<Panel>,
    /// Scope currently receiving native text, if any.
    focused: Option<u64>,
    /// Provisional input kept apart from the confirmed draft.
    pub(super) composition: Composition,
    /// Last worker frame observed in a native render pass.
    painted_guest: Option<(Owner, u64)>,
    /// Last host draft observed in a native render pass.
    painted_draft: Option<(u64, u64)>,
    /// Visible reason why a complete input could not be acknowledged.
    feedback: Option<&'static str>,
}

impl Probe {
    /// Prepare only a host panel. `options` will be consumed after the first frame.
    pub(super) fn new(options: Options, metrics: Metrics, cx: &mut Context<Workspace>) -> Self {
        Self {
            options: Some(options),
            client: None,
            projects: BTreeMap::new(),
            next_scope: 1,
            scene: Scene::default(),
            panel: cx.new(|_| Panel::new(metrics)),
            focused: None,
            composition: Composition::default(),
            painted_guest: None,
            painted_draft: None,
            feedback: None,
        }
    }

    /// Publish all open scopes with their latest confirmed input.
    fn publish(&self) {
        if let Some(client) = &self.client {
            client.projects(self.projects.values().cloned().collect());
        }
    }
}

impl Workspace {
    /// Schedule the supervisor only after the core has laid out and rendered its first scene.
    pub(super) fn probe_after_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut options) = self.probe.as_mut().and_then(|probe| probe.options.take()) else {
            return;
        };
        options.milestone("first_scene_built");
        let view = cx.weak_entity();
        window.on_next_frame(move |window, cx| {
            let ready = Instant::now();
            let _ = view.update(cx, |workspace, cx| {
                workspace.start_probe(options, ready, window, cx)
            });
        });
    }

    /// Start workers off-thread and repaint from coalesced updates in the native window.
    fn start_probe(
        &mut self,
        options: Options,
        ready: Instant,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editable = self
            .projects
            .active_project()
            .is_some_and(|project| project.focus == Focus::Editor);
        let client = options.start(ready, editable);
        let updates = client.updates();
        self.probe.as_mut().expect("probe exists").client = Some(client);
        self.sync_probe_projects();
        cx.spawn_in(window, async move |view, cx| {
            while updates.recv().await.is_ok() {
                if cx
                    .update(|window, cx| {
                        view.update(cx, |workspace, cx| {
                            if let Some(probe) = &mut workspace.probe
                                && let Some(client) = &probe.client
                            {
                                probe.scene = client.scene();
                                workspace.sync(window, cx);
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
    }

    /// Retain a draft for every open directory tab and release it on project close.
    pub(super) fn sync_probe_projects(&mut self) {
        let Some(probe) = &mut self.probe else {
            return;
        };
        let roots: Vec<_> = self
            .projects
            .ordered()
            .filter(|project| project.manager.is_some())
            .map(|project| project.root.clone())
            .collect();
        probe.projects.retain(|root, _| roots.contains(root));
        for root in roots {
            probe.projects.entry(root.clone()).or_insert_with(|| {
                let scope = probe.next_scope;
                probe.next_scope += 1;
                Project {
                    scope,
                    root,
                    draft: Draft::default(),
                }
            });
        }
        let active = self
            .projects
            .active_project()
            .and_then(|project| probe.projects.get(&project.root));
        if probe.focused.is_some() && active.map(|project| project.scope) != probe.focused {
            probe.focused = None;
            probe.composition.clear();
        }
        probe.publish();
    }

    /// Borrow the sample scope of the active directory tab.
    fn probe_project(&self) -> Option<&Project> {
        let project = self.projects.active_project()?;
        project.manager.as_ref()?;
        self.probe.as_ref()?.projects.get(&project.root)
    }

    /// Resolve the sample surface's two grid rows above the editor's bottom border.
    fn probe_area(&self) -> Option<Rect> {
        let probe = self.probe.as_ref()?;
        let project = self.probe_project()?;
        if self.modal_open()
            || self.sequence_hint_area().is_some()
            || (probe.focused != Some(project.scope)
                && (probe.scene.status.is_empty()
                    || probe.scene.status == "Ready"
                        && !probe.scene.surfaces.contains_key(&project.scope)))
        {
            return None;
        }
        let area = self.layout.editor;
        (area.w >= 12 && area.h >= 5).then(|| Rect {
            x: area.x + 1,
            y: area.y + area.h - 3,
            w: area.w - 2,
            h: 2,
        })
    }

    /// Whether the sample input, rather than the editor or a popup, owns native text.
    pub(super) fn probe_has_focus(&self) -> bool {
        self.probe_project().is_some_and(|project| {
            self.probe_area().is_some()
                && self
                    .probe
                    .as_ref()
                    .is_some_and(|probe| probe.focused == Some(project.scope))
        })
    }

    /// Use host glyph spacing for the visible draft and the IME/caret anchor.
    pub(super) fn probe_caret(&self) -> Option<(f32, f32)> {
        if !self.probe_has_focus() {
            return None;
        }
        let area = self.probe_area()?;
        let draft = &self.probe_project()?.draft;
        let (_, advance) = visible_text(&draft.text, area.w - 3, &self.spacing);
        Some((area.x as f32 + 1. + advance, (area.y + 1) as f32))
    }

    /// Update the cached panel from a complete current guest frame and host-owned input.
    pub(super) fn refresh_probe(&self, cx: &mut Context<Self>) {
        let Some(area) = self.probe_area() else {
            return;
        };
        let probe = self.probe.as_ref().expect("visible probe");
        let project = self.probe_project().expect("visible probe project");
        let surface = probe.scene.surfaces.get(&project.scope);
        let mut frame = Frame::new(area.w, area.h);
        let color = surface.map_or(0x26364bff, |surface| surface.color);
        frame.tint(0, 0, area.w, color);
        frame.tint(1, 0, area.w, color);
        let title = match (probe.feedback, surface) {
            (Some(feedback), _) => feedback.to_owned(),
            (_, Some(surface)) => format!("{}  F8 Edit  Esc Return", surface.title),
            _ => probe.scene.status.to_owned(),
        };
        frame.label(0, 1..area.w, &title, ACCENT, &self.spacing);

        // While the worker is behind or recovering, present the input already confirmed locally.
        let text = surface
            .filter(|surface| surface.draft == project.draft)
            .map_or(project.draft.text.as_str(), |surface| {
                surface.draft.text.as_str()
            });
        let (visible, _) = visible_text(text, area.w - 3, &self.spacing);
        let mut display = visible.to_owned();
        if self.probe_has_focus() {
            display.push_str(&probe.composition.text);
        }
        let color = if self.probe_has_focus() { TEXT } else { MUTED };
        frame.label(1, 1..area.w - 1, &display, color, &self.spacing);
        probe.panel.update(cx, |panel, cx| panel.set(frame, cx));
    }

    /// Expose the panel as a normal retained layer and timestamp visible revisions after rendering.
    pub(super) fn probe_layer(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<GridLayer> {
        let area = self.probe_area()?;
        let project = self.probe_project()?;
        let (scope, revision, bytes) = (
            project.scope,
            project.draft.revision,
            project.draft.text.len(),
        );
        let (_, caret) = visible_text(&project.draft.text, area.w - 3, &self.spacing);
        let probe = self.probe.as_mut()?;
        let frame = probe
            .scene
            .surfaces
            .get(&scope)
            .filter(|surface| surface.draft.revision == revision);
        let guest = frame.map(|Surface { owner, draft, .. }| (*owner, draft.revision));
        let new_guest = guest.filter(|guest| probe.painted_guest != Some(*guest));
        let new_draft = probe.painted_draft != Some((scope, revision));
        if let Some((owner, _)) = guest
            && probe.painted_guest.is_none_or(|(prior, _)| prior != owner)
            && let Some(client) = &probe.client
        {
            client.mark(
                "plugin_input_ready",
                json!({ "scope": owner.scope, "generation": owner.generation }),
            );
        }
        if new_guest.is_some() || new_draft {
            if guest.is_some() {
                probe.painted_guest = guest;
            }
            probe.painted_draft = Some((scope, revision));
            let view = cx.weak_entity();
            window.on_next_frame(move |_, cx| {
                let _ = view.update(cx, |workspace, _| {
                    if let Some(client) = workspace.probe.as_ref().and_then(|probe| probe.client.as_ref()) {
                        if let Some((owner, revision)) = new_guest {
                            client.mark("plugin_frame", json!({ "scope": owner.scope, "generation": owner.generation, "revision": revision }));
                        }
                        if new_draft {
                            client.mark("draft_layout", json!({ "scope": scope, "revision": revision, "bytes": bytes, "caret_columns": caret }));
                        }
                    }
                });
            });
        }
        Some(GridLayer {
            panel: probe.panel.clone(),
            bounds: area.pixels(self.metrics),
            clip: None,
        })
    }

    /// Focus a ready sample on click and give other clicks back to the normal workspace.
    pub(super) fn probe_click(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(area) = self.probe_area() else {
            return false;
        };
        let (x, y) = self.metrics.cell(position);
        if area.contains(x, y)
            && self.probe_project().is_some_and(|project| {
                self.probe
                    .as_ref()
                    .is_some_and(|probe| probe.scene.surfaces.contains_key(&project.scope))
            })
        {
            self.set_probe_focus(true, window, cx);
            return true;
        }
        if self.probe_has_focus() {
            self.set_probe_focus(false, window, cx);
        }
        false
    }

    /// Give or return native input focus, clearing provisional input for the previous surface.
    pub(super) fn set_probe_focus(
        &mut self,
        focused: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(scope) = self.probe_project().map(|project| project.scope) else {
            return;
        };
        let Some(probe) = &mut self.probe else {
            return;
        };
        if focused && !probe.scene.surfaces.contains_key(&scope) {
            return;
        }
        let next = focused.then_some(scope);
        if probe.focused == next {
            return;
        }
        #[cfg(windows)]
        let had_preedit = !probe.composition.is_empty()
            || self
                .projects
                .active_project()
                .is_some_and(|project| !project.editor.composition().is_empty());
        probe.focused = next;
        probe.composition.clear();
        probe.feedback = None;
        if let Some(project) = self.projects.active_project_mut() {
            project.editor.composition_mut().clear();
        }
        self.prefix = None;
        if let Some(client) = &probe.client {
            client.mark("draft_focus", json!({ "scope": scope, "focused": focused }));
        }
        self.sync(window, cx);
        #[cfg(windows)]
        if had_preedit {
            Self::cancel_probe_preedit(window, cx);
        }
    }

    /// Handle the sample's explicit keys while leaving printable keys to the native input handler.
    pub(super) fn probe_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(scope) = self.probe_project().map(|project| project.scope) else {
            return false;
        };
        if self.probe_area().is_none() {
            return false;
        }
        let focused = self.probe_has_focus();
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        let control = match key {
            "f9" => Some(Control::Terminate),
            "f10" => Some(Control::Work(scope)),
            "f11" => Some(Control::BusyRule),
            _ => None,
        };
        if let Some(control) = control
            && !modifiers.control
            && !modifiers.alt
            && !modifiers.platform
        {
            if !event.is_held
                && let Some(client) = self.probe.as_ref().and_then(|probe| probe.client.as_ref())
            {
                client.control(control);
            }
        } else if key == "f8" && !modifiers.control && !modifiers.alt && !modifiers.platform {
            if !event.is_held {
                self.set_probe_focus(!focused, window, cx);
            }
        } else if !focused {
            return false;
        } else if key == "escape" {
            self.set_probe_focus(false, window, cx);
        } else if (modifiers.control || modifiers.platform) && key == "v" {
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                self.probe
                    .as_mut()
                    .expect("focused probe")
                    .composition
                    .clear();
                self.probe_commit(&text, window, cx);
            }
        } else if key == "backspace"
            && self
                .probe
                .as_ref()
                .is_some_and(|probe| probe.composition.is_empty())
        {
            self.edit_probe_draft(window, cx, |draft| {
                if let Some((start, _)) = draft.grapheme_indices(true).next_back() {
                    draft.truncate(start);
                    true
                } else {
                    false
                }
            });
        } else {
            return true;
        }
        cx.stop_propagation();
        true
    }

    /// Confirm one native insertion into the bounded sample draft before notifying any worker.
    pub(super) fn probe_commit(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.probe_has_focus() {
            return false;
        }
        let draft = &self.probe_project().expect("focused project").draft;
        let feedback = if text.len() > MAX_DRAFT - draft.text.len() {
            Some("Draft is limited to 1024 UTF-8 bytes")
        } else if text.chars().any(char::is_control) {
            Some("Draft accepts a single line")
        } else {
            None
        };
        if let Some(feedback) = feedback {
            self.probe.as_mut().expect("focused probe").feedback = Some(feedback);
            self.sync(window, cx);
            return true;
        }
        self.edit_probe_draft(window, cx, |draft| {
            if text.is_empty() {
                return false;
            }
            draft.push_str(text);
            true
        });
        true
    }

    /// Apply `edit` to host state first, then publish a new complete revision for the guest.
    fn edit_probe_draft(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        edit: impl FnOnce(&mut String) -> bool,
    ) {
        let Some(root) = self
            .projects
            .active_project()
            .map(|project| project.root.clone())
        else {
            return;
        };
        let Some(probe) = &mut self.probe else {
            return;
        };
        let Some(project) = probe.projects.get_mut(&root) else {
            return;
        };
        if edit(&mut project.draft.text) {
            project.draft.revision += 1;
            probe.feedback = None;
            if let Some(client) = &probe.client {
                client.mark("draft_input", json!({ "scope": project.scope, "revision": project.draft.revision, "bytes": project.draft.text.len() }));
            }
            probe.publish();
            self.cursor
                .update(cx, |cursor, cx| cursor.activity(window, cx));
        }
        self.sync(window, cx);
    }
}

/// Fit a suffix of `text` into `columns` without splitting a grapheme. Return its caret advance.
fn visible_text<'a>(text: &'a str, columns: usize, spacing: &TextSpacing) -> (&'a str, f32) {
    let end = spacing.cells(text).last().map_or(0, |cell| cell.end);
    let first_unit = end.saturating_sub(columns * UNITS_PER_COLUMN);
    let start = spacing.cells(text).find(|cell| cell.start >= first_unit);
    let (byte, first) = start.map_or((text.len(), end), |cell| (cell.byte, cell.start));
    (
        &text[byte..],
        (end - first) as f32 / UNITS_PER_COLUMN as f32,
    )
}
