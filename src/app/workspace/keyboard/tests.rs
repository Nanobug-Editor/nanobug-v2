//! Native shortcut routing regressions.

use super::*;

/// Decode native `key` for the current `focus`, Vim `mode`, hidden pane and pending `prefix`.
fn pane_shortcut(
    focus: Focus,
    mode: Mode,
    hidden_pane: Option<Focus>,
    prefix: Option<char>,
    key: &'static str,
) -> Option<PaneShortcut> {
    let key = Keystroke::parse(key).unwrap();
    PaneShortcut::resolve(
        focus,
        mode,
        hidden_pane,
        prefix,
        &key,
        normal_character(&key).as_deref(),
    )
}

/// Directional shortcuts follow the two-pane layout and can restore the right editor pane.
#[test]
fn normal_window_keys_focus_the_directional_pane() {
    let command = |focus, hidden, key| pane_shortcut(focus, Mode::Normal, hidden, None, key);
    assert_eq!(
        command(Focus::Editor, None, "ctrl-h"),
        Some(PaneShortcut::Select(Focus::Manager))
    );
    assert_eq!(
        command(Focus::Manager, Some(Focus::Editor), "ctrl-l"),
        Some(PaneShortcut::Select(Focus::Editor))
    );
    assert_eq!(
        command(Focus::Editor, Some(Focus::Manager), "ctrl-h"),
        Some(PaneShortcut::Stay)
    );
    for key in ["ctrl-j", "ctrl-k", "ctrl-l"] {
        assert_eq!(command(Focus::Editor, None, key), Some(PaneShortcut::Stay));
    }
    for key in ["tab", "h", "ctrl-shift-h"] {
        assert_eq!(command(Focus::Editor, None, key), None);
    }
}

/// Only a plain Space followed by e in Normal mode opens Explorer.
#[test]
fn explorer_open_requires_a_normal_mode_space_prefix() {
    let command =
        |mode, prefix, key| pane_shortcut(Focus::Editor, mode, Some(Focus::Manager), prefix, key);
    assert_eq!(
        command(Mode::Normal, None, "space"),
        Some(PaneShortcut::ExplorerPrefix)
    );
    assert_eq!(
        command(Mode::Normal, Some(' '), "e"),
        Some(PaneShortcut::Select(Focus::Manager))
    );
    assert_eq!(command(Mode::Normal, None, "e"), None);
    assert_eq!(command(Mode::Normal, Some(' '), "ctrl-e"), None);
    for mode in [
        Mode::Insert,
        Mode::Visual,
        Mode::VisualLine,
        Mode::Command,
        Mode::Search,
    ] {
        assert_eq!(command(mode, None, "space"), None);
        assert_eq!(command(mode, Some(' '), "e"), None);
        assert_eq!(command(mode, None, "ctrl-h"), None);
    }
}
