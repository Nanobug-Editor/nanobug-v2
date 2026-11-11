//! Native shortcut routing regressions.

use super::*;

/// Decode native `key` for the current `focus`, Vim `mode`, hidden pane and pending `prefix`.
fn shell_shortcut(
    focus: Focus,
    mode: Mode,
    hidden_pane: Option<Focus>,
    prefix: Option<PendingPrefix>,
    key: &'static str,
) -> Option<ShellShortcut> {
    let key = Keystroke::parse(key).unwrap();
    ShellShortcut::resolve(
        Some(focus),
        mode,
        hidden_pane,
        true,
        prefix,
        &key,
        normal_character(&key).as_deref(),
    )
}

/// Directional shortcuts follow the two-pane layout and can restore the right editor pane.
#[test]
fn normal_window_keys_focus_the_directional_pane() {
    let command = |focus, hidden, key| shell_shortcut(focus, Mode::Normal, hidden, None, key);
    assert_eq!(
        command(Focus::Editor, None, "ctrl-h"),
        Some(ShellShortcut::Select(Focus::Manager))
    );
    assert_eq!(
        command(Focus::Manager, Some(Focus::Editor), "ctrl-l"),
        Some(ShellShortcut::Select(Focus::Editor))
    );
    assert_eq!(
        command(Focus::Editor, Some(Focus::Manager), "ctrl-h"),
        Some(ShellShortcut::Stay)
    );
    for key in ["ctrl-j", "ctrl-k", "ctrl-l"] {
        assert_eq!(command(Focus::Editor, None, key), Some(ShellShortcut::Stay));
    }
    for key in ["tab", "h", "ctrl-shift-h"] {
        assert_eq!(command(Focus::Editor, None, key), None);
    }
}

/// The default leader is plain Space, with e opening Explorer in Normal mode.
#[test]
fn explorer_open_requires_the_normal_mode_leader() {
    let command =
        |mode, prefix, key| shell_shortcut(Focus::Editor, mode, Some(Focus::Manager), prefix, key);
    let leader = Some(PendingPrefix::Leader(LeaderPrefix::ROOT));
    assert_eq!(
        command(Mode::Normal, None, "space"),
        Some(ShellShortcut::AwaitLeader(LeaderPrefix::ROOT))
    );
    assert_eq!(
        command(Mode::Normal, leader, "e"),
        Some(ShellShortcut::Select(Focus::Manager))
    );
    assert_eq!(command(Mode::Normal, None, "e"), None);
    assert_eq!(
        command(Mode::Normal, Some(PendingPrefix::Manager('g')), "e"),
        None
    );
    for key in ["ctrl-space", "shift-space"] {
        assert_eq!(command(Mode::Normal, None, key), None);
    }
    for key in ["ctrl-e", "shift-e"] {
        assert_eq!(command(Mode::Normal, leader, key), None);
    }
    for mode in [
        Mode::Insert,
        Mode::Visual,
        Mode::VisualLine,
        Mode::Command,
        Mode::Search,
    ] {
        assert_eq!(command(mode, None, "space"), None);
        assert_eq!(command(mode, leader, "e"), None);
        assert_eq!(command(mode, None, "ctrl-h"), None);
    }
    assert_eq!(
        shell_shortcut(Focus::Manager, Mode::Insert, None, leader, "e"),
        Some(ShellShortcut::Select(Focus::Manager))
    );
}

/// Uppercase L opens the picker only from the root leader hint and outside editor text entry.
#[test]
fn leader_language_picker_requires_uppercase_l() {
    let leader = Some(PendingPrefix::Leader(LeaderPrefix::ROOT));
    let subgroup = Some(PendingPrefix::Leader(LeaderPrefix("q")));
    for (focus, mode) in [
        (Focus::Editor, Mode::Normal),
        (Focus::Manager, Mode::Insert),
    ] {
        let command = |prefix, key| shell_shortcut(focus, mode, None, prefix, key);
        assert_eq!(
            command(leader, "shift-l"),
            Some(ShellShortcut::OpenLanguagePicker)
        );
        assert_eq!(command(leader, "l"), None);
        assert_eq!(command(leader, "ctrl-shift-l"), None);
        assert_eq!(command(None, "shift-l"), None);
        assert_eq!(command(subgroup, "shift-l"), None);
    }
    for mode in [Mode::Insert, Mode::Command, Mode::Search] {
        assert_eq!(
            shell_shortcut(Focus::Editor, mode, None, leader, "shift-l"),
            None
        );
    }
}

/// Letter bindings follow Shift even when the native text has the opposite case.
#[test]
fn leader_letter_case_requires_shift() {
    let leader = Some(PendingPrefix::Leader(LeaderPrefix::ROOT));
    let resolve = |base: &'static str, text: &str, shift| {
        let mut key = Keystroke::parse(base).unwrap();
        key.key_char = Some(text.into());
        key.modifiers.shift = shift;
        ShellShortcut::resolve(
            Some(Focus::Editor),
            Mode::Normal,
            None,
            true,
            leader,
            &key,
            normal_character(&key).as_deref(),
        )
    };
    assert_eq!(resolve("l", "L", false), None);
    assert_eq!(
        resolve("l", "l", true),
        Some(ShellShortcut::OpenLanguagePicker)
    );
    assert_eq!(
        resolve("e", "E", false),
        Some(ShellShortcut::Select(Focus::Manager))
    );
    assert_eq!(resolve("e", "e", true), None);
}

/// Both panes route q to the close group only after the leader has started.
#[test]
fn leader_quit_resolves_only_the_project_close_sequences() {
    let leader = Some(PendingPrefix::Leader(LeaderPrefix::ROOT));
    let quit = Some(PendingPrefix::Leader(LeaderPrefix("q")));
    for (focus, mode) in [
        (Focus::Editor, Mode::Normal),
        (Focus::Manager, Mode::Insert),
    ] {
        let command = |prefix, key| shell_shortcut(focus, mode, None, prefix, key);
        assert_eq!(command(None, "q"), None);
        assert_eq!(command(leader, "a"), None);
        assert_eq!(
            command(leader, "q"),
            Some(ShellShortcut::AwaitLeader(LeaderPrefix("q")))
        );
        assert_eq!(
            command(quit, "q"),
            Some(ShellShortcut::Close(CloseTarget::Project))
        );
        assert_eq!(
            command(quit, "a"),
            Some(ShellShortcut::Close(CloseTarget::Quit))
        );
        for key in ["ctrl-q", "shift-a", "e"] {
            assert_eq!(command(quit, key), None);
        }
        assert_eq!(command(None, "escape"), None);
        for prefix in [leader, quit] {
            assert_eq!(command(prefix, "escape"), Some(ShellShortcut::CancelLeader));
        }
    }
    for mode in [Mode::Insert, Mode::Command, Mode::Search] {
        for (prefix, key) in [(leader, "q"), (quit, "q"), (quit, "a")] {
            assert_eq!(shell_shortcut(Focus::Editor, mode, None, prefix, key), None);
        }
    }
}

/// The project group can add a start page and the quit group works on every command surface.
#[test]
fn project_and_quit_groups_resolve_in_panes_and_on_the_start_page() {
    let root = Some(PendingPrefix::Leader(LeaderPrefix::ROOT));
    let project = Some(PendingPrefix::Leader(LeaderPrefix("p")));
    let quit = Some(PendingPrefix::Leader(LeaderPrefix("q")));
    for focus in [Some(Focus::Editor), Some(Focus::Manager), None] {
        let command = |prefix, key| {
            let key = Keystroke::parse(key).unwrap();
            ShellShortcut::resolve(
                focus,
                Mode::Normal,
                None,
                focus.is_some(),
                prefix,
                &key,
                normal_character(&key).as_deref(),
            )
        };
        assert_eq!(
            command(root, "p"),
            Some(ShellShortcut::AwaitLeader(LeaderPrefix("p")))
        );
        assert_eq!(command(project, "a"), Some(ShellShortcut::OpenStart));
        assert_eq!(command(project, "q"), None);
        assert_eq!(
            command(root, "q"),
            Some(ShellShortcut::AwaitLeader(LeaderPrefix("q")))
        );
        assert_eq!(
            command(quit, "a"),
            Some(ShellShortcut::Close(CloseTarget::Quit))
        );
        assert_eq!(
            command(quit, "q"),
            focus.map(|_| ShellShortcut::Close(CloseTarget::Project))
        );
    }
}

/// An editor-only tab shares file and close shortcuts without offering Explorer.
#[test]
fn standalone_editor_omits_explorer_bindings() {
    let command = |prefix, key| {
        let key = Keystroke::parse(key).unwrap();
        ShellShortcut::resolve(
            Some(Focus::Editor),
            Mode::Normal,
            None,
            false,
            prefix,
            &key,
            normal_character(&key).as_deref(),
        )
    };
    let leader = Some(PendingPrefix::Leader(LeaderPrefix::ROOT));
    assert_eq!(command(leader, "e"), None);
    assert_eq!(command(None, "ctrl-h"), Some(ShellShortcut::Stay));
    assert_eq!(command(leader, "f"), Some(ShellShortcut::OpenFilePicker));
    assert_eq!(
        command(Some(PendingPrefix::Leader(LeaderPrefix("q"))), "q"),
        Some(ShellShortcut::Close(CloseTarget::Project)),
    );
    assert!(
        !LeaderPrefix::ROOT
            .bindings(true, false)
            .any(|(key, _)| key == "e")
    );
}

/// Backspace goes up one group, or dismisses the root, while a leader hint is open.
#[test]
fn backspace_returns_to_the_parent_leader_group() {
    let command = |prefix| shell_shortcut(Focus::Editor, Mode::Normal, None, prefix, "backspace");
    assert_eq!(
        command(Some(PendingPrefix::Leader(LeaderPrefix("q")))),
        Some(ShellShortcut::AwaitLeader(LeaderPrefix::ROOT))
    );
    assert_eq!(
        command(Some(PendingPrefix::Leader(LeaderPrefix::ROOT))),
        Some(ShellShortcut::CancelLeader)
    );
    assert_eq!(command(None), None);
}

/// Each pane recognizes only its own close gesture and leaves plain Escape to Vim.
#[test]
fn pane_close_keys_are_window_specific() {
    for (binding, manager, editor) in [
        ("q", true, false),
        ("alt-escape", false, true),
        ("escape", false, false),
        ("shift-x", false, false),
        ("shift-q", false, false),
        ("ctrl-q", false, false),
        ("ctrl-alt-escape", false, false),
        ("alt-shift-escape", false, false),
    ] {
        let key = Keystroke::parse(binding).unwrap();
        assert_eq!(
            ManagerWindow::matches_close_key(normal_character(&key).as_deref()),
            manager,
            "Explorer: {binding}"
        );
        assert_eq!(EditorWindow::matches_close_key(&key), editor, "{binding}");
    }
}
