//! Prepare complete and incremental plugin Canvas scenes before publishing them.

use super::{Engine, Key, MAX_DRAFT, ResultEvent, resource::surface_bytes};
use crate::plugins::{
    drawing::{self, PreparedCanvas},
    session::Surface,
};
use nanobug_plugin_sdk::{
    GridSurface, PluginEvent,
    canvas::{Delta, Sync, Viewport},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Instant,
};

const CANVAS_RESOURCE_LIMIT: usize = 160 * 1024 * 1024;
const MAX_PENDING_PER_SURFACE: usize = 4;

/// Parallel complete-frame preparation publishes only the newest successful result.
#[derive(Default)]
pub(super) struct Frames {
    pending: BTreeMap<Key, Queue>,
    sync: BTreeMap<Key, Baseline>,
}

#[derive(Default)]
struct Queue {
    jobs: BTreeMap<u64, Pending>,
    /// Coalesce complete frames while this surface has its full share of preparation jobs.
    latest: Option<(u64, GridSurface)>,
}

struct Pending {
    generation: u64,
    layout: Option<u64>,
    delta_epoch: Option<u64>,
    model_bytes: usize,
}

struct Baseline {
    value: Sync,
    needs_resync: bool,
}

impl Frames {
    pub(super) fn sync(&self, key: &Key) -> Option<Sync> {
        self.sync.get(key).map(|baseline| baseline.value)
    }

    pub(super) fn resync_pending(&self, key: &Key) -> bool {
        !self.pending.contains_key(key)
            && self
                .sync
                .get(key)
                .is_some_and(|baseline| baseline.needs_resync)
    }

    pub(super) fn resync_sent(&mut self, key: &Key) {
        if let Some(baseline) = self.sync.get_mut(key) {
            baseline.needs_resync = false;
        }
    }

    pub(super) fn retain(&mut self, wanted: impl Fn(&Key) -> bool) {
        self.pending.retain(|key, _| wanted(key));
        self.sync.retain(|key, _| wanted(key));
    }

    pub(super) fn remove_plugin(&mut self, namespace: &str) {
        self.retain(|key| key.namespace != namespace);
    }

    /// Add queued and in-flight frame model bytes to the owning module's diagnostic total.
    pub(super) fn model_bytes(&self, per_scope: &mut BTreeMap<Key, usize>) {
        for (owner, queue) in &self.pending {
            let bytes = queue
                .jobs
                .values()
                .map(|job| job.model_bytes)
                .sum::<usize>()
                + queue
                    .latest
                    .as_ref()
                    .map_or(0, |(_, frame)| surface_bytes(frame));
            *per_scope.entry(owner.clone()).or_default() += bytes;
        }
    }
}

impl Engine {
    /// Record the new viewport before another callback can reuse an old baseline.
    pub(super) fn resize_canvas(&mut self, key: Key, viewport: Viewport) {
        let previous = self.layouts.insert(key.clone(), viewport);
        if previous.is_some_and(|previous| previous.id == viewport.id) {
            return;
        }
        self.frames.pending.remove(&key);
        if let Some(sync) = self.frames.sync(&key) {
            // An ordinary Layout event already asks the opted-in guest for the new layout.
            self.reset_canvas_sync(&key, sync.generation, false);
        }
    }

    pub(super) fn canvas_sync_for(&self, key: &Key, event: &PluginEvent) -> Option<Sync> {
        if matches!(event, PluginEvent::Open) {
            return None;
        }
        self.frames.sync(key)
    }

    /// Invalidating a chain changes its host epoch while keeping the last valid painted scene.
    fn reset_canvas_sync(&mut self, key: &Key, generation: u64, needs_resync: bool) {
        let epoch = self.next();
        self.frames.sync.insert(
            key.clone(),
            Baseline {
                value: Sync {
                    generation,
                    viewport: self.layout_id(key),
                    epoch,
                    sequence: None,
                },
                needs_resync,
            },
        );
    }

    pub(super) fn reject_canvas_delta(&mut self, key: &Key, generation: u64) {
        // Do not deliver new events to a legacy guest which never opted in.
        if self
            .frames
            .sync(key)
            .is_some_and(|sync| sync.generation == generation && sync.sequence.is_some())
        {
            self.reset_canvas_sync(key, generation, true);
        }
        self.log(
            &key.namespace,
            "Canvas delta rejected. A complete scene is required.",
        );
    }

    /// Apply a contiguous change against the latest successfully prepared scene only.
    pub(super) fn set_canvas_delta(&mut self, key: &Key, generation: u64, delta: Delta) {
        let Some(sync) = self.frames.sync(key) else {
            self.reject_canvas_delta(key, generation);
            return;
        };
        if (delta.generation, delta.viewport, delta.epoch)
            != (generation, sync.viewport, sync.epoch)
            || sync.generation != generation
            || sync.viewport != self.layout_id(key)
        {
            return;
        }
        // A retry whose sequence is already acknowledged can be safely ignored.
        if sync.sequence.is_some_and(|base| delta.sequence <= base) {
            return;
        }
        if sync.sequence == Some(delta.base)
            && delta.base.checked_add(1) == Some(delta.sequence)
            && self.frames.pending.get(key).is_some_and(|pending| {
                pending
                    .jobs
                    .values()
                    .any(|job| job.delta_epoch == Some(delta.epoch))
            })
        {
            return;
        }
        let frame = self.view.surfaces.get(key).and_then(|surface| {
            let mut frame = surface.frame.as_ref().clone();
            let canvas = frame.canvas.take()?;
            if surface.generation != generation || sync.sequence != Some(delta.base) {
                return None;
            }
            frame.canvas = Some(delta.apply(canvas)?);
            Some(frame)
        });
        let Some(frame) = frame.filter(|_| !self.frames.pending.contains_key(key)) else {
            self.reject_canvas_delta(key, generation);
            return;
        };
        self.prepare_canvas(key, generation, frame, Some(sync.epoch));
    }

    /// Admit a complete frame without hiding the last valid frame while it is prepared.
    pub(super) fn set_surface(&mut self, key: &Key, generation: u64, frame: GridSurface) {
        if !valid_grid(&frame) {
            self.log(
                &key.namespace,
                "Plugin frame exceeds the grid drawing budget",
            );
            return;
        }
        if frame.canvas.is_none() {
            self.frames.pending.remove(key);
            let revision = self.next();
            self.publish_surface(key, generation, revision, frame, None, None);
            return;
        }
        let layout = self.layout_id(key);
        let canvas = frame.canvas.as_ref().expect("Canvas requested");
        if canvas.viewport.is_some_and(|id| Some(id) != layout)
            || canvas.sequence.is_some() && canvas.viewport != layout
        {
            return;
        }
        if let Some(queue) = self.frames.pending.get_mut(key)
            && queue.jobs.len() >= MAX_PENDING_PER_SURFACE
        {
            queue.latest = Some((generation, frame));
            return;
        }
        self.prepare_canvas(key, generation, frame, None);
    }

    fn prepare_canvas(
        &mut self,
        key: &Key,
        generation: u64,
        frame: GridSurface,
        delta_epoch: Option<u64>,
    ) {
        let store = self.store.clone();
        let release = Arc::clone(&self.releases[&key.namespace]);
        let finish = self.finish.clone();
        let owner = key.clone();
        let ticket = self.next();
        let layout = self.layout_id(key);
        let model_bytes = surface_bytes(&frame);
        if self.io.submit(move || {
            let started = Instant::now();
            let result = drawing::prepare(
                &store,
                &release,
                frame.canvas.as_ref().expect("Canvas requested"),
            );
            let elapsed = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
            let _ = finish.send(ResultEvent::Frame(
                owner, generation, ticket, frame, result, elapsed,
            ));
        }) {
            self.frames
                .pending
                .entry(key.clone())
                .or_default()
                .jobs
                .insert(
                    ticket,
                    Pending {
                        generation,
                        layout,
                        delta_epoch,
                        model_bytes,
                    },
                );
        } else if delta_epoch.is_some() {
            self.reject_canvas_delta(key, generation);
        } else {
            self.log(&key.namespace, "Canvas preparation queue is busy");
        }
    }

    /// Publish a complete prepared transaction only for the live viewport and baseline epoch.
    pub(super) fn canvas_prepared(
        &mut self,
        key: Key,
        generation: u64,
        ticket: u64,
        frame: GridSurface,
        result: anyhow::Result<PreparedCanvas>,
    ) {
        let Some(queue) = self.frames.pending.get_mut(&key) else {
            return;
        };
        let Some(pending) = queue.jobs.remove(&ticket) else {
            return;
        };
        let waiting = queue.latest.take();
        if queue.jobs.is_empty() {
            self.frames.pending.remove(&key);
        }
        self.finish_canvas_preparation(&key, generation, ticket, frame, result, pending);
        if let Some((generation, frame)) = waiting
            && self.wanted.contains_key(&key)
            && self
                .runtimes
                .get(&key.namespace)
                .is_some_and(|runtime| runtime.generation == generation && runtime.worker.is_some())
        {
            self.set_surface(&key, generation, frame);
        }
    }

    fn finish_canvas_preparation(
        &mut self,
        key: &Key,
        generation: u64,
        ticket: u64,
        frame: GridSurface,
        result: anyhow::Result<PreparedCanvas>,
        pending: Pending,
    ) {
        if pending.generation != generation
            || !self.wanted.contains_key(key)
            || !self
                .runtimes
                .get(&key.namespace)
                .is_some_and(|runtime| runtime.generation == generation && runtime.worker.is_some())
            || !pending.is_current(
                self.layout_id(key),
                ticket,
                self.view.surfaces.get(key).map(|surface| surface.revision),
                self.frames.sync(key).map(|sync| sync.epoch),
            )
        {
            return;
        }
        let published = match result {
            Ok(scene) => self.publish_surface(
                key,
                generation,
                ticket,
                frame,
                Some(Arc::new(scene)),
                pending.delta_epoch,
            ),
            Err(error) => {
                self.log(&key.namespace, &format!("Canvas frame rejected: {error:#}"));
                false
            }
        };
        if !published && pending.delta_epoch.is_some() {
            self.reject_canvas_delta(key, generation);
        }
    }

    fn layout_id(&self, key: &Key) -> Option<u64> {
        self.layouts.get(key).map(|viewport| viewport.id)
    }

    fn publish_surface(
        &mut self,
        key: &Key,
        generation: u64,
        revision: u64,
        mut frame: GridSurface,
        drawing: Option<Arc<PreparedCanvas>>,
        delta_epoch: Option<u64>,
    ) -> bool {
        if !self.frame_has_current_layout(key, &frame) {
            return false;
        }
        if let Some(canvas) = &drawing {
            let mut seen = BTreeSet::new();
            let bytes = std::iter::once(canvas)
                .chain(
                    self.view
                        .surfaces
                        .iter()
                        .filter(|(owner, _)| *owner != key)
                        .filter_map(|(_, scene)| scene.drawing.as_ref()),
                )
                .filter(|scene| seen.insert(Arc::as_ptr(scene)))
                .map(|scene| scene.resource_bytes)
                .sum::<usize>();
            if bytes > CANVAS_RESOURCE_LIMIT {
                self.log(
                    &key.namespace,
                    "Canvas frame exceeds the host resource budget",
                );
                return false;
            }
        }
        if self.draft_for(key).is_none()
            && let Some(text) = frame.input.take()
        {
            self.set_draft(key.clone(), text);
            frame.input = Some(String::new());
        }
        let draft = self.draft_for(key).cloned().unwrap_or_default();
        let sequence = frame.canvas.as_ref().and_then(|canvas| canvas.sequence);
        self.view.surfaces.insert(
            key.clone(),
            Surface {
                generation,
                revision,
                frame: Arc::new(frame),
                drawing,
                draft,
            },
        );
        if let Some(sequence) = sequence {
            self.frames.sync.insert(
                key.clone(),
                Baseline {
                    value: Sync {
                        generation,
                        viewport: self.layout_id(key),
                        epoch: delta_epoch.unwrap_or(revision),
                        sequence: Some(sequence),
                    },
                    needs_resync: false,
                },
            );
        } else {
            self.frames.sync.remove(key);
        }
        self.changed = true;
        true
    }

    /// A viewport-independent legacy frame remains usable after a resize.
    fn frame_has_current_layout(&self, key: &Key, frame: &GridSurface) -> bool {
        frame.canvas.as_ref().is_none_or(|canvas| {
            if canvas.sequence.is_some() {
                canvas.viewport == self.layout_id(key)
            } else {
                canvas
                    .viewport
                    .is_none_or(|id| Some(id) == self.layout_id(key))
            }
        })
    }

    /// The current live scene is the only source of plugin UI actions.
    pub(super) fn current_surface_revision(&self, key: &Key, revision: u64) -> Option<&Surface> {
        if self.active != Some(key.instance.project) || !self.wanted.contains_key(key) {
            return None;
        }
        self.view.surfaces.get(key).filter(|surface| {
            surface.revision == revision
                && self.frame_has_current_layout(key, &surface.frame)
                && self.runtimes.get(&key.namespace).is_some_and(|runtime| {
                    runtime.generation == surface.generation && runtime.worker.is_some()
                })
        })
    }
}

impl Pending {
    /// A newer successful frame wins even if its worker completed first.
    fn is_current(
        &self,
        layout: Option<u64>,
        ticket: u64,
        displayed: Option<u64>,
        epoch: Option<u64>,
    ) -> bool {
        self.layout == layout
            && displayed.is_none_or(|revision| revision < ticket)
            && self
                .delta_epoch
                .is_none_or(|expected| epoch == Some(expected))
    }
}

fn valid_grid(frame: &GridSurface) -> bool {
    frame.lines.len() <= 200
        && frame.title.len() <= 512
        && !frame.title.chars().any(char::is_control)
        && frame
            .lines
            .iter()
            .all(|line| line.len() <= 4096 && !line.contains(['\r', '\n']))
        && frame
            .input
            .as_ref()
            .is_none_or(|text| text.len() <= MAX_DRAFT && !text.chars().any(char::is_control))
}

#[cfg(test)]
mod tests {
    use super::Pending;

    #[test]
    fn canvas_preparations_drop_old_completions_and_keep_candidates_after_rejection() {
        let mut current = Pending {
            generation: 7,
            layout: Some(3),
            delta_epoch: None,
            model_bytes: 0,
        };
        assert!(current.is_current(Some(3), 21, Some(19), None));
        // A later request rejected off-thread does not advance the displayed revision.
        assert!(current.is_current(Some(3), 20, Some(19), None));
        assert!(!current.is_current(Some(3), 20, Some(21), None));
        assert!(!current.is_current(Some(4), 22, Some(21), None));
        current.delta_epoch = Some(9);
        assert!(current.is_current(Some(3), 22, Some(21), Some(9)));
        assert!(!current.is_current(Some(3), 22, Some(21), Some(10)));
    }
}
