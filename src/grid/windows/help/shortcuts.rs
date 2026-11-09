//! Global shortcuts displayed in their own help scope.

use super::super::{Shortcut, ShortcutKeys::Fixed};

/// Bindings shared by both panes, including normal-mode window navigation.
pub(super) const GLOBAL: &[Shortcut] = &[
    (
        Fixed("Ctrl+h / Ctrl+l"),
        "global_shortcuts.focus_horizontal",
    ),
    (Fixed("Ctrl+j / Ctrl+k"), "global_shortcuts.focus_vertical"),
    (Fixed("<leader>e"), "global_shortcuts.open_manager"),
    (Fixed("<leader>L"), "global_shortcuts.change_language"),
    (Fixed("<leader>qq"), "global_shortcuts.close_project"),
    (Fixed("<leader>qa"), "global_shortcuts.close_all_projects"),
    (Fixed("Alt+h/l  Alt+k/j"), "global_shortcuts.switch_project"),
    (
        Fixed("Ctrl+Alt+h/l/k/j"),
        "global_shortcuts.reorder_project",
    ),
    (
        Fixed("Ctrl+Alt+Home/End"),
        "global_shortcuts.move_project_to_end",
    ),
    (Fixed("?"), "global_shortcuts.open_help"),
    (Fixed("F5"), "global_shortcuts.refresh"),
    (Fixed("F6 / Shift+F6"), "global_shortcuts.cycle_cursor"),
    (Fixed("F7"), "global_shortcuts.reload_cursor"),
];
