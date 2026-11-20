//! Retain exact storage keys independently from the effective values shown by the form.

use nanobug::plugins::session::{ConfigScope, ScanOverride};
use serde_json::Value;
use std::collections::BTreeMap;

/// Immutable release and backend affected by an open settings page.
pub(in crate::grid::workspace) struct Source {
    pub namespace: String,
    pub digest: String,
    pub data: Data,
}

pub(in crate::grid::workspace) enum Data {
    Enabled(bool),
    Manifest {
        scope: ConfigScope,
        /// Remove a newly created empty owner record when restoring full inheritance.
        owner_was_absent: bool,
        fields: BTreeMap<String, StoredField>,
    },
    Scan {
        project: u64,
        opening: ScanOverride,
        applied: ScanOverride,
    },
}

pub(in crate::grid::workspace) struct StoredField {
    pub opening: BTreeMap<String, Value>,
    pub applied: BTreeMap<String, Value>,
    /// Raw aliases/absence before the most recent successful write, for a failed cross-page transaction.
    pub previous: BTreeMap<String, Value>,
    /// Current value inherited when this scope has no valid explicit override.
    pub fallback: Value,
    pub declaration: nanobug::plugins::package::SettingDecl,
    /// Resolved value at opening, possibly inherited from a different scope.
    pub opening_value: Value,
    /// Plugin arrays, objects, nonintegers, and unrestricted values use editable JSON.
    pub json_text: bool,
}
