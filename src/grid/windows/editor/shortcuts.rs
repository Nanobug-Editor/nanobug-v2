//! Bindings shown in the editor window help.

use super::super::{
    Shortcut,
    ShortcutKeys::{Fixed, Localized},
};

/// Bindings specific to the editor, grouped by operation and input mode.
pub(in crate::grid) const SHORTCUTS: &[Shortcut] = &[
    (Fixed("Alt+Esc"), "editor_shortcuts.close"),
    (Localized("keys.editor_move"), "editor_shortcuts.move"),
    (
        Localized("keys.editor_scroll_horizontal"),
        "editor_shortcuts.scroll_horizontal",
    ),
    (Fixed("w b e  0 ^ $"), "editor_shortcuts.word_motion"),
    (Fixed("Home / End"), "editor_shortcuts.line_edges"),
    (Fixed("gg / G / 3G"), "editor_shortcuts.line_jump"),
    (Fixed("PageUp / PageDown"), "editor_shortcuts.page"),
    (Fixed("i a I A o O"), "editor_shortcuts.insert"),
    (Fixed("Esc / Ctrl+["), "editor_shortcuts.normal"),
    (
        Fixed("Backspace / Delete"),
        "editor_shortcuts.insert_delete",
    ),
    (Fixed("Enter / Tab"), "editor_shortcuts.insert_line_tab"),
    (
        Fixed("Ctrl+w / Ctrl+u"),
        "editor_shortcuts.insert_delete_word",
    ),
    (Fixed("Ctrl+v"), "editor_shortcuts.clipboard_paste"),
    (Fixed("x s D C"), "editor_shortcuts.change"),
    (
        Localized("keys.editor_operator"),
        "editor_shortcuts.operator",
    ),
    (Fixed("dd cc yy"), "editor_shortcuts.line_operator"),
    (Fixed("p / P"), "editor_shortcuts.paste"),
    (Fixed("v / V"), "editor_shortcuts.visual"),
    (Fixed("u / Ctrl+r"), "editor_shortcuts.undo_redo"),
    (Fixed("J"), "editor_shortcuts.join"),
    (Localized("keys.editor_search"), "editor_shortcuts.search"),
    (Fixed(":w / Ctrl+s"), "editor_shortcuts.save"),
    (Localized("keys.editor_open"), "editor_shortcuts.open"),
    (Fixed(":q  :qa  :q!  :wq  :x"), "editor_shortcuts.quit"),
];
