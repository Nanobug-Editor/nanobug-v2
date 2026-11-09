//! Bindings shown in the file-manager window help.

use super::super::{Shortcut, ShortcutKeys::Fixed};

/// Bindings specific to the file manager.
pub(in crate::grid) const SHORTCUTS: &[Shortcut] = &[
    (Fixed("q"), "manager_shortcuts.close"),
    (Fixed("j / k  ↓ / ↑"), "manager_shortcuts.move"),
    (Fixed("h / ←"), "manager_shortcuts.collapse"),
    (Fixed("l / Enter"), "manager_shortcuts.open"),
    (Fixed("→"), "manager_shortcuts.expand"),
    (Fixed("gg / G  Home / End"), "manager_shortcuts.jump"),
    (Fixed("zz / zt / zb"), "manager_shortcuts.position"),
    (Fixed("s / Esc"), "manager_shortcuts.mark"),
    (Fixed("c / x"), "manager_shortcuts.capture"),
    (Fixed("v"), "manager_shortcuts.paste"),
    (Fixed("z / Z"), "manager_shortcuts.undo_redo"),
];
