//! Estimates for host-owned plugin models. Process memory and transport buffers are separate.

use super::{Engine, Key};
use crate::plugins::session::HostScopeStatus;
use nanobug_plugin_sdk::{
    GridSurface,
    canvas::{Draw, FontOptions, MeasureText, Segment},
};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    mem::size_of,
    sync::Arc,
    time::{Duration, Instant},
};

impl Engine {
    /// Account each retained allocation once, while keeping its owner scope visible.
    pub(super) fn publish_host_resources(&mut self) {
        let now = Instant::now();
        if now < self.resource_sample_due {
            return;
        }
        self.resource_sample_due = now + Duration::from_millis(100);
        let mut by_scope = BTreeMap::<Key, HostScopeStatus>::new();
        let mut seen = BTreeMap::<usize, (Key, usize, bool)>::new();
        let mut shared = 0usize;
        let mut claim = |ptr: usize, key: &Key, bytes: usize, drawing| {
            use std::collections::btree_map::Entry;
            if let Entry::Occupied(mut entry) = seen.entry(ptr) {
                let (owner, counted, was_drawing) = entry.get_mut();
                if owner == key || *counted == 0 {
                    return;
                }
                if let Some(first) = by_scope.get_mut(owner) {
                    if *was_drawing {
                        first.drawing_bytes -= *counted;
                    } else {
                        first.model_bytes -= *counted;
                    }
                }
                shared += *counted;
                *counted = 0;
                return;
            }
            seen.insert(ptr, (key.clone(), bytes, drawing));
            let entry = by_scope.entry(key.clone()).or_default();
            if drawing {
                entry.drawing_bytes += bytes;
            } else {
                entry.model_bytes += bytes;
            }
        };
        for (key, surface) in &self.view.surfaces {
            let model = &surface.frame;
            claim(
                Arc::as_ptr(model) as usize,
                key,
                surface_bytes(model),
                false,
            );
            if let Some(drawing) = &surface.drawing {
                claim(
                    Arc::as_ptr(drawing) as usize,
                    key,
                    drawing.resource_bytes,
                    true,
                );
            }
        }

        let mut preparing = BTreeMap::new();
        self.frames.model_bytes(&mut preparing);
        for (key, bytes) in preparing {
            by_scope.entry(key).or_default().model_bytes += bytes;
        }
        for request in self.view.text_measurements.values() {
            by_scope
                .entry(request.owner.clone())
                .or_default()
                .layout_queue_bytes += measurement_bytes(&request.lines, &request.prepared);
        }
        for (key, bytes) in self.pending_measurements.values() {
            by_scope.entry(key.clone()).or_default().layout_queue_bytes += bytes;
        }
        let mut snapshots = BTreeMap::new();
        for (namespace, runtime) in &self.runtimes {
            for (instance, live) in &runtime.instances {
                let key = Key {
                    namespace: namespace.clone(),
                    instance: instance.clone(),
                };
                if let Some(frame) = &live.startup_frame {
                    by_scope.entry(key.clone()).or_default().model_bytes += surface_bytes(frame);
                }
                let mut scoped = None;
                for snapshot in [&live.document_base, &live.pending_document]
                    .into_iter()
                    .flatten()
                {
                    let ptr = Arc::as_ptr(snapshot) as usize;
                    let bytes = snapshot.text.capacity()
                        + size_of::<nanobug_plugin_sdk::DocumentSnapshot>()
                        + snapshot
                            .path
                            .as_ref()
                            .map_or(0, std::path::PathBuf::capacity);
                    if scoped != Some(ptr) {
                        by_scope
                            .entry(key.clone())
                            .or_default()
                            .shared_document_bytes += bytes;
                        snapshots.insert(ptr, bytes);
                        scoped = Some(ptr);
                    }
                }
            }
        }
        shared += snapshots.values().sum::<usize>();
        if self.view.host_resources != by_scope || self.view.shared_plugin_resources != shared {
            self.view.host_resources = by_scope;
            self.view.shared_plugin_resources = shared;
            self.changed = true;
        }
    }
}

fn vec_bytes<T>(items: &Vec<T>) -> usize {
    items.capacity() * size_of::<T>()
}

fn string_bytes(value: &Option<String>) -> usize {
    value.as_ref().map_or(0, String::capacity)
}

fn font_bytes(font: &Option<FontOptions>) -> usize {
    font.as_ref().map_or(0, |font| {
        font.features.as_ref().map_or(0, |features| {
            features
                .keys()
                .map(|name| size_of::<(String, u32)>() + name.capacity())
                .sum()
        }) + font.fallbacks.as_ref().map_or(0, |fonts| {
            vec_bytes(fonts) + fonts.iter().map(String::capacity).sum::<usize>()
        })
    })
}

/// Count a scene's model, excluding its separately allocated prepared geometry and images.
pub(super) fn surface_bytes(surface: &GridSurface) -> usize {
    size_of::<GridSurface>()
        + surface.title.capacity()
        + vec_bytes(&surface.lines)
        + surface.lines.iter().map(String::capacity).sum::<usize>()
        + string_bytes(&surface.input)
        + surface.canvas.as_ref().map_or(0, |canvas| {
            let interaction = &canvas.interaction;
            draw_bytes(&canvas.draw)
                + vec_bytes(&canvas.hits)
                + canvas
                    .hits
                    .iter()
                    .map(|hit| hit.action.capacity())
                    .sum::<usize>()
                + vec_bytes(&interaction.pointer)
                + interaction
                    .pointer
                    .iter()
                    .map(|region| {
                        string_bytes(&region.mouse_down)
                            + string_bytes(&region.mouse_up)
                            + string_bytes(&region.mouse_move)
                            + string_bytes(&region.wheel)
                    })
                    .sum::<usize>()
                + vec_bytes(&interaction.keys)
                + interaction
                    .keys
                    .iter()
                    .map(|key| key.key.capacity() + key.action.capacity())
                    .sum::<usize>()
                + string_bytes(&interaction.next_frame)
                + canvas.text_input.as_ref().map_or(0, |field| {
                    string_bytes(&field.font) + font_bytes(&field.font_options)
                })
        })
}

fn draw_bytes(draw: &Vec<Draw>) -> usize {
    vec_bytes(draw)
        + draw
            .iter()
            .map(|command| match command {
                Draw::Path { segments, dash, .. } => {
                    vec_bytes(segments)
                        + vec_bytes(dash)
                        + segments
                            .iter()
                            .map(|segment| {
                                if let Segment::Polygon { points, .. } = segment {
                                    vec_bytes(points)
                                } else {
                                    0
                                }
                            })
                            .sum::<usize>()
                }
                Draw::Shadows { values, .. } => vec_bytes(values),
                Draw::Text { text, runs, .. } => text.capacity() + text_run_bytes(runs),
                Draw::ShapedLine {
                    text,
                    runs,
                    underlines,
                    ..
                } => {
                    text.capacity()
                        + text_run_bytes(runs)
                        + vec_bytes(underlines)
                        + underlines
                            .iter()
                            .map(|rule| vec_bytes(&rule.exclusions))
                            .sum::<usize>()
                }
                Draw::Glyphs {
                    text,
                    font,
                    font_options,
                    ..
                } => text.capacity() + string_bytes(font) + font_bytes(font_options),
                Draw::PackageText { text, font, .. } => text.capacity() + font.asset.capacity(),
                Draw::Underline { exclusions, .. } => vec_bytes(exclusions),
                Draw::Svg { asset, .. }
                | Draw::Image { asset, .. }
                | Draw::AppleSurface { asset, .. } => asset.capacity(),
                Draw::Group { draw, .. } => draw_bytes(draw),
                Draw::Quad { .. } | Draw::Strike { .. } => 0,
            })
            .sum::<usize>()
}

/// Count text run storage, including the allocated native font descriptions.
fn text_run_bytes(runs: &Vec<nanobug_plugin_sdk::canvas::TextRun>) -> usize {
    vec_bytes(runs)
        + runs
            .iter()
            .map(|run| string_bytes(&run.font) + font_bytes(&run.font_options))
            .sum::<usize>()
}

/// The pending text descriptions plus the already shaped caret positions awaiting the UI.
pub(super) fn measurement_bytes(lines: &Vec<MeasureText>, prepared: &[Option<Value>]) -> usize {
    vec_bytes(lines)
        + lines
            .iter()
            .map(|line| {
                line.text.capacity()
                    + string_bytes(&line.font)
                    + font_bytes(&line.font_options)
                    + line
                        .package_font
                        .as_ref()
                        .map_or(0, |font| font.asset.capacity())
            })
            .sum::<usize>()
        + std::mem::size_of_val(prepared)
        + prepared
            .iter()
            .flatten()
            .map(json_heap_bytes)
            .sum::<usize>()
}

fn json_heap_bytes(value: &Value) -> usize {
    match value {
        Value::String(text) => text.capacity(),
        Value::Array(values) => {
            vec_bytes(values) + values.iter().map(json_heap_bytes).sum::<usize>()
        }
        Value::Object(fields) => fields
            .iter()
            .map(|(key, value)| {
                size_of::<(String, Value)>() + key.capacity() + json_heap_bytes(value)
            })
            .sum(),
        _ => 0,
    }
}
