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
        (false, false),
        LeaderAvailability {
            manager: true,
            help: true,
            markdown: false,
            close_only: false,
            choosing_file: false,
            grid_window: true,
            tabs: true,
        },
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

/// The default leader is plain Space, with e toggling Explorer from command surfaces.
#[test]
fn explorer_toggle_requires_the_normal_mode_leader() {
    let command =
        |mode, prefix, key| shell_shortcut(Focus::Editor, mode, Some(Focus::Manager), prefix, key);
    let leader = Some(PendingPrefix::Leader(LeaderPrefix::ROOT));
    assert_eq!(
        command(Mode::Normal, None, "space"),
        Some(ShellShortcut::AwaitLeader(LeaderPrefix::ROOT))
    );
    assert_eq!(
        command(Mode::Normal, leader, "e"),
        Some(ShellShortcut::ToggleManager)
    );
    assert_eq!(command(Mode::Normal, None, "e"), None);
    assert_eq!(
        command(Mode::Normal, Some(PendingPrefix::Manager('g')), "e"),
        None
    );
    for key in ["ctrl-space", "shift-space"] {
        assert_eq!(command(Mode::Normal, None, key), None);
    }
    for key in ["ctrl-e", "ctrl-shift-e"] {
        assert_eq!(command(Mode::Normal, leader, key), None);
    }
    for mode in [
        Mode::Insert,
        Mode::Visual,
        Mode::VisualLine,
        Mode::VisualBlock,
        Mode::Command,
        Mode::Search,
    ] {
        assert_eq!(
            command(mode, None, "space"),
            matches!(mode, Mode::Visual | Mode::VisualLine | Mode::VisualBlock)
                .then_some(ShellShortcut::AwaitLeader(LeaderPrefix::ROOT))
        );
        assert_eq!(command(mode, leader, "e"), None);
        assert_eq!(command(mode, None, "ctrl-h"), None);
    }
    assert_eq!(
        shell_shortcut(Focus::Manager, Mode::Insert, None, leader, "e"),
        Some(ShellShortcut::ToggleManager)
    );
}

#[test]
fn plugin_manager_requires_space_then_shift_p_outside_text_input() {
    let leader = Some(PendingPrefix::Leader(LeaderPrefix::ROOT));
    assert_eq!(
        shell_shortcut(Focus::Editor, Mode::Normal, None, leader, "shift-p"),
        Some(ShellShortcut::OpenPluginManager)
    );
    assert_eq!(
        shell_shortcut(Focus::Editor, Mode::Normal, None, None, "shift-p"),
        None
    );
    assert_eq!(
        shell_shortcut(Focus::Editor, Mode::Insert, None, leader, "shift-p"),
        None
    );
    assert_eq!(
        shell_shortcut(Focus::Editor, Mode::Normal, None, leader, "p"),
        Some(ShellShortcut::AwaitLeader(LeaderPrefix("p")))
    );
}

/// Uppercase L opens the picker only from the root leader hint and outside editor text entry.
#[test]
fn leader_language_picker_requires_uppercase_l() {
    let leader = Some(PendingPrefix::Leader(LeaderPrefix::ROOT));
    let subgroup = Some(PendingPrefix::Leader(LeaderPrefix("p")));
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
            (false, false),
            LeaderAvailability {
                manager: true,
                help: true,
                markdown: false,
                close_only: false,
                choosing_file: false,
                grid_window: true,
                tabs: true,
            },
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
    assert_eq!(resolve("e", "E", false), Some(ShellShortcut::ToggleManager));
    assert_eq!(resolve("e", "e", true), Some(ShellShortcut::OpenFilePicker));
}

/// Question mark opens help only after the root leader and on a surface with help.
#[test]
fn question_mark_requires_the_leader_and_a_help_target() {
    let leader = Some(PendingPrefix::Leader(LeaderPrefix::ROOT));
    for focus in [Focus::Editor, Focus::Manager] {
        for key in ["?", "shift-/"] {
            assert_eq!(shell_shortcut(focus, Mode::Normal, None, None, key), None);
            assert_eq!(
                shell_shortcut(focus, Mode::Normal, None, leader, key),
                Some(ShellShortcut::OpenHelp)
            );
        }
    }
    assert_eq!(
        shell_shortcut(Focus::Editor, Mode::Insert, None, leader, "shift-/"),
        None
    );

    let key = Keystroke::parse("shift-/").unwrap();
    for (help, expected) in [(false, None), (true, Some(ShellShortcut::OpenHelp))] {
        assert_eq!(
            ShellShortcut::resolve(
                None,
                Mode::Normal,
                None,
                (false, false),
                LeaderAvailability {
                    manager: false,
                    help,
                    markdown: false,
                    close_only: false,
                    choosing_file: false,
                    grid_window: help,
                    tabs: true,
                },
                leader,
                &key,
                normal_character(&key).as_deref(),
            ),
            expected
        );
    }
}

/// Both panes route q, x and X to separate close scopes after the leader.
#[test]
fn leader_close_resolves_grid_and_tab_actions() {
    let leader = Some(PendingPrefix::Leader(LeaderPrefix::ROOT));
    for (focus, mode) in [
        (Focus::Editor, Mode::Normal),
        (Focus::Editor, Mode::Visual),
        (Focus::Editor, Mode::VisualLine),
        (Focus::Editor, Mode::VisualBlock),
        (Focus::Manager, Mode::Insert),
    ] {
        let command = |prefix, key| shell_shortcut(focus, mode, None, prefix, key);
        assert_eq!(command(None, "x"), None);
        assert_eq!(command(None, "q"), None);
        assert_eq!(command(leader, "q"), Some(ShellShortcut::CloseFocused));
        assert_eq!(command(leader, "a"), None);
        assert_eq!(
            command(leader, "x"),
            Some(ShellShortcut::CloseTabs(CloseTarget::Tab))
        );
        assert_eq!(
            command(leader, "shift-x"),
            Some(ShellShortcut::CloseTabs(CloseTarget::AllTabs))
        );
        assert_eq!(command(leader, "ctrl-x"), None);
        assert_eq!(command(None, "escape"), None);
        assert_eq!(command(leader, "escape"), Some(ShellShortcut::CancelLeader));
    }
    for mode in [Mode::Insert, Mode::Command, Mode::Search] {
        for key in ["q", "x", "shift-x"] {
            assert_eq!(shell_shortcut(Focus::Editor, mode, None, leader, key), None);
        }
    }
}

/// A grouped popup exposes one close action and cannot close the workspace underneath it.
#[test]
fn popup_leader_only_closes_the_popup() {
    let availability = LeaderAvailability {
        manager: true,
        help: true,
        markdown: false,
        close_only: true,
        choosing_file: false,
        grid_window: true,
        tabs: false,
    };
    let command = |key| {
        let key = Keystroke::parse(key).unwrap();
        ShellShortcut::resolve(
            None,
            Mode::Normal,
            None,
            (false, false),
            availability,
            Some(PendingPrefix::Leader(LeaderPrefix::ROOT)),
            &key,
            normal_character(&key).as_deref(),
        )
    };
    assert_eq!(command("q"), Some(ShellShortcut::CloseFocused));
    assert_eq!(command("x"), None);
    assert_eq!(command("shift-x"), None);
    assert_eq!(
        LeaderPrefix::ROOT
            .bindings(availability)
            .map(|(key, _)| key)
            .collect::<Vec<_>>(),
        ["q"]
    );
}

/// Start pages offer close-all through the leader, while panes also offer q and x.
#[test]
fn project_and_close_actions_resolve_in_panes_and_on_the_start_page() {
    let root = Some(PendingPrefix::Leader(LeaderPrefix::ROOT));
    let project = Some(PendingPrefix::Leader(LeaderPrefix("p")));
    for focus in [Some(Focus::Editor), Some(Focus::Manager), None] {
        let command = |prefix, key| {
            let key = Keystroke::parse(key).unwrap();
            ShellShortcut::resolve(
                focus,
                Mode::Normal,
                None,
                (false, false),
                LeaderAvailability {
                    manager: focus.is_some(),
                    help: focus.is_some(),
                    markdown: false,
                    close_only: false,
                    choosing_file: false,
                    grid_window: focus.is_some(),
                    tabs: true,
                },
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
            command(root, "shift-e"),
            Some(ShellShortcut::OpenFilePicker)
        );
        assert_eq!(command(root, "f"), None);
        assert_eq!(
            command(root, "q"),
            focus.map(|_| ShellShortcut::CloseFocused)
        );
        assert_eq!(
            command(root, "x"),
            focus.map(|_| ShellShortcut::CloseTabs(CloseTarget::Tab))
        );
        assert_eq!(
            command(root, "shift-x"),
            Some(ShellShortcut::CloseTabs(CloseTarget::AllTabs))
        );
    }
}

/// An editor-only tab shares file and close shortcuts without offering Explorer.
#[test]
fn standalone_editor_omits_explorer_bindings() {
    let availability = LeaderAvailability {
        manager: false,
        help: true,
        markdown: false,
        close_only: false,
        choosing_file: false,
        grid_window: true,
        tabs: true,
    };
    let command = |prefix, key| {
        let key = Keystroke::parse(key).unwrap();
        ShellShortcut::resolve(
            Some(Focus::Editor),
            Mode::Normal,
            None,
            (false, false),
            availability,
            prefix,
            &key,
            normal_character(&key).as_deref(),
        )
    };
    let leader = Some(PendingPrefix::Leader(LeaderPrefix::ROOT));
    assert_eq!(command(leader, "e"), None);
    assert_eq!(command(None, "ctrl-h"), Some(ShellShortcut::Stay));
    assert_eq!(
        command(leader, "shift-e"),
        Some(ShellShortcut::OpenFilePicker)
    );
    assert_eq!(command(None, "shift-e"), None);
    assert_eq!(command(leader, "q"), Some(ShellShortcut::CloseFocused));
    assert_eq!(
        command(leader, "x"),
        Some(ShellShortcut::CloseTabs(CloseTarget::Tab))
    );
    assert!(
        !LeaderPrefix::ROOT
            .bindings(availability)
            .any(|(key, _)| key == "e")
    );
}

/// Backspace goes up one group, or dismisses the root, while a leader hint is open.
#[test]
fn backspace_returns_to_the_parent_leader_group() {
    let command = |prefix| shell_shortcut(Focus::Editor, Mode::Normal, None, prefix, "backspace");
    assert_eq!(
        command(Some(PendingPrefix::Leader(LeaderPrefix("p")))),
        Some(ShellShortcut::AwaitLeader(LeaderPrefix::ROOT))
    );
    assert_eq!(
        command(Some(PendingPrefix::Leader(LeaderPrefix::ROOT))),
        Some(ShellShortcut::CancelLeader)
    );
    assert_eq!(command(None), None);
}

/// The Markdown submenu and dispatch use the same surface-specific binding table.
#[test]
fn markdown_view_bindings_require_a_markdown_editor() {
    let availability = LeaderAvailability {
        manager: true,
        help: true,
        markdown: true,
        close_only: false,
        choosing_file: false,
        grid_window: true,
        tabs: true,
    };
    let group = LeaderPrefix("m");
    let root = LeaderPrefix::ROOT;
    let command = |mode, availability, prefix, key| {
        let key = Keystroke::parse(key).unwrap();
        ShellShortcut::resolve(
            Some(Focus::Editor),
            mode,
            None,
            (false, false),
            availability,
            Some(PendingPrefix::Leader(prefix)),
            &key,
            normal_character(&key).as_deref(),
        )
    };
    assert!(root.bindings(availability).any(|(key, _)| key == "m"));
    assert_eq!(
        command(Mode::Normal, availability, root, "m"),
        Some(ShellShortcut::AwaitLeader(group))
    );
    assert_eq!(
        group
            .bindings(availability)
            .map(|(key, _)| key)
            .collect::<Vec<_>>(),
        ["a", "e", "p"]
    );
    for (key, mode) in [
        ("a", MarkdownViewMode::Split),
        ("e", MarkdownViewMode::Edit),
        ("p", MarkdownViewMode::Preview),
    ] {
        assert_eq!(
            command(Mode::Normal, availability, group, key),
            Some(ShellShortcut::SetMarkdownView(mode))
        );
    }
    for availability in [
        LeaderAvailability {
            markdown: false,
            ..availability
        },
        LeaderAvailability {
            close_only: true,
            ..availability
        },
        LeaderAvailability {
            choosing_file: true,
            ..availability
        },
    ] {
        assert!(!root.bindings(availability).any(|(key, _)| key == "m"));
        assert!(group.bindings(availability).next().is_none());
        assert_eq!(command(Mode::Normal, availability, root, "m"), None);
        assert_eq!(command(Mode::Normal, availability, group, "a"), None);
    }
    for mode in [Mode::Visual, Mode::Command, Mode::Insert] {
        assert_eq!(command(mode, availability, root, "m"), None);
    }
}

/// Preview navigation maps to wheel and endpoint requests without reaching Vim.
#[test]
fn markdown_preview_uses_row_and_page_scroll_requests() {
    for (key, rows) in [
        ("j", 1),
        ("down", 1),
        ("k", -1),
        ("up", -1),
        ("pageup", -9),
        ("pagedown", 9),
        ("ctrl-b", -9),
        ("ctrl-f", 9),
        ("ctrl-u", -4),
        ("ctrl-d", 4),
    ] {
        assert!(matches!(
            markdown_scroll_input(&Keystroke::parse(key).unwrap(), 9),
            Some(ScrollInput::Wheel(delta)) if delta == rows
        ));
    }
    assert!(matches!(
        markdown_scroll_input(&Keystroke::parse("home").unwrap(), 9),
        Some(ScrollInput::Position(0))
    ));
    for key in ["end", "shift-g"] {
        assert!(matches!(
            markdown_scroll_input(&Keystroke::parse(key).unwrap(), 9),
            Some(ScrollInput::Position(usize::MAX))
        ));
    }
    for key in ["i", "x", "ctrl-shift-d", "shift-down"] {
        assert!(markdown_scroll_input(&Keystroke::parse(key).unwrap(), 9).is_none());
    }
}

/// Preview prefixes preserve file and tab navigation without dispatching source edits.
#[test]
fn markdown_preview_preserves_buffer_and_tab_navigation() {
    assert_eq!(
        markdown_navigation_action('g', Some("t")),
        Some(Action::TabNavigate(1, None))
    );
    assert_eq!(
        markdown_navigation_action('g', Some("T")),
        Some(Action::TabNavigate(-1, None))
    );
    assert_eq!(
        markdown_navigation_action('g', Some("Tab")),
        Some(Action::TabNavigate(0, None))
    );
    assert_eq!(
        markdown_navigation_action('[', Some("b")),
        Some(Action::BracketBuffer {
            forward: false,
            edge: false,
            count: None
        })
    );
    assert_eq!(
        markdown_navigation_action(']', Some("B")),
        Some(Action::BracketBuffer {
            forward: true,
            edge: true,
            count: None
        })
    );
    assert_eq!(markdown_navigation_action(']', Some("p")), None);
}

/// The editor's close gesture leaves plain Escape and macro keys to the modal editor.
#[test]
fn editor_close_keys_leave_normal_commands_to_vim() {
    for (binding, editor) in [
        ("q", false),
        ("alt-escape", true),
        ("escape", false),
        ("shift-x", false),
        ("shift-q", false),
        ("ctrl-q", false),
        ("ctrl-alt-escape", false),
        ("alt-shift-escape", false),
    ] {
        let key = Keystroke::parse(binding).unwrap();
        assert_eq!(EditorWindow::matches_close_key(&key), editor, "{binding}");
    }
}
