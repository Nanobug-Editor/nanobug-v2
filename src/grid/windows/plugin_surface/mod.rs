//! Host-owned state, input layout and drawing for plugin-provided windows.

pub(in crate::grid) mod canvas;
use crate::grid::text_field as field;
pub(in crate::grid) mod native_field;
mod view;

use nanobug::plugins::session::{Key, Surface};
use nanobug_plugin_sdk::{Instance, canvas::Viewport};
use serde::{Deserialize, Serialize};
use std::rc::Rc;

/// A plugin-provided view identified without its process-local project ID.
#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
pub(in crate::grid) struct SurfaceSnapshot {
    /// Owning package namespace and manifest module ID.
    namespace: String,
    module: String,
    /// Whether the user was interacting with the window.
    pub(in crate::grid) focused: bool,
}

impl SurfaceSnapshot {
    /// Capture the stable owner from a live `key` and its current `focused` state.
    pub(in crate::grid) fn new(key: &Key, focused: bool) -> Self {
        Self {
            namespace: key.namespace.clone(),
            module: key.instance.module.clone(),
            focused,
        }
    }

    /// Match the same plugin and module in the reopened `project`.
    pub(in crate::grid) fn key(&self, project: u64) -> Key {
        Key {
            namespace: self.namespace.clone(),
            instance: Instance {
                project,
                module: self.module.clone(),
            },
        }
    }
}

/// Preserve confirmed text and its layout while the plugin publishes replacement scenes.
#[derive(Default)]
pub(in crate::grid) struct SurfaceWindow {
    /// Session instance supplying the retained plugin window.
    pub(in crate::grid) key: Option<Key>,
    /// Whether input is routed to the plugin's editable field.
    pub(in crate::grid) focused: bool,
    /// Host-confirmed text, preserved across replacement scenes and worker restarts.
    pub(in crate::grid) draft: String,
    /// Grapheme caret, selection and pointer interaction within the draft.
    pub(in crate::grid) field: field::Field,
    /// Shaped text cache for the current scene, font and draft.
    pub(in crate::grid) native_field: Option<Rc<native_field::NativeField>>,
    /// Latest submitted draft awaiting acknowledgement in the published session view.
    pub(in crate::grid) pending_draft: Option<String>,
    /// Most recent drawable viewport sent to the plugin.
    pub(in crate::grid) viewport: Option<Viewport>,
    /// Monotonic identity used to reject scenes prepared for an older viewport.
    pub(in crate::grid) viewport_id: u64,
}

impl SurfaceWindow {
    /// Reset the host view when leaving its owner, keeping viewport IDs monotonic.
    pub(in crate::grid) fn clear_attachment(&mut self) {
        *self = Self {
            viewport_id: self.viewport_id,
            ..Self::default()
        };
    }

    /// Show `surface` from `key`, requesting focus only if the frame supports input.
    pub(in crate::grid) fn attach(&mut self, key: Key, surface: &Surface, focus: bool) {
        self.clear_attachment();
        self.focused = focus
            && (surface.frame.input.is_some()
                || surface
                    .frame
                    .canvas
                    .as_ref()
                    .is_some_and(|canvas| canvas.has_input()));
        self.draft.clone_from(&surface.draft);
        self.key = Some(key);
    }
}

pub(in crate::grid) use view::input_cell;
