//! Host-owned state, input layout and drawing for plugin-provided windows.

pub(in crate::grid) mod canvas;
pub(in crate::grid) mod field;
pub(in crate::grid) mod native_field;
mod view;

use nanobug::plugins::session::Key;
use nanobug_plugin_sdk::canvas::Viewport;
use std::rc::Rc;

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

pub(in crate::grid) use view::input_cell;
