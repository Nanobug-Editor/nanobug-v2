//! Exercise the editor's own modal sequences without a Neovim installation.

use super::{Action, Mode, Register, Vim, run_filter};
use crate::document::Document;

/// Create an editor and register bank for `text`.
fn editor(text: &str) -> (Vim, Register) {
    (Vim::new(Document::new(text)), Register::default())
}

/// Lock mark coordinates, but still adjust the previous-context mark when a line is deleted.
#[test]
fn ex_mark_modifiers_lock_named_marks_and_leave_context_mobile() {
    let (mut vim, mut registers) = editor("head\n  beta\n gamma\n");
    vim.set_cursor(vim.doc.starts[1] + 2);
    vim.set_command_mark('a', &mut registers);
    vim.set_command_mark('A', &mut registers);
    vim.record_jump();
    vim.set_operated_range(vim.doc.cursor..vim.doc.cursor + 1);

    vim.execute_ex_line("lockmarks 1delete", &mut registers);
    assert_eq!(vim.doc.text, "  beta\n gamma\n");
    let locked = vim.doc.starts[1] + 2;
    assert_eq!(vim.marks[&'a'], locked);
    assert_eq!(vim.marks[&'\''], 2);
    assert_eq!(vim.operated, Some(locked..locked + 1));
    assert_eq!(
        registers.file_marks.get('A').unwrap().offset_in(&vim.doc),
        Some(locked)
    );
    vim.key("Ctrl+o", &mut registers);
    assert_eq!(vim.doc.cursor, locked);

    vim.execute_ex_line("1delete", &mut registers);
    assert_eq!(vim.marks[&'a'], 2);
    assert_eq!(
        registers.file_marks.get('A').unwrap().offset_in(&vim.doc),
        Some(2)
    );
}

/// A filtered range retains marks on surviving output rows and rebases marks below the range.
#[test]
fn ex_mark_modifiers_keep_filter_marks_by_original_row() {
    use crate::document::{MarkModifiers, with_mark_modifiers};

    let (mut vim, mut registers) = editor("head\n east\n west\n tail\n");
    for (row, local, global) in [(1, 'a', 'A'), (2, 'b', 'B'), (3, 'c', 'C')] {
        vim.set_cursor(vim.doc.starts[row] + 1);
        vim.set_command_mark(local, &mut registers);
        vim.set_command_mark(global, &mut registers);
    }
    let range = vim.line_span(1, 2);
    with_mark_modifiers(
        MarkModifiers {
            keep_marks: true,
            ..MarkModifiers::default()
        },
        || vim.apply_filter(range, " north\n"),
    );

    assert_eq!(vim.doc.text, "head\n north\n tail\n");
    assert_eq!(vim.marks[&'a'], vim.doc.starts[1] + 1);
    assert_eq!(vim.marks[&'c'], vim.doc.starts[2] + 1);
    assert!(!vim.marks.contains_key(&'b'));
    assert_eq!(
        registers.file_marks.get('B').unwrap().offset_in(&vim.doc),
        Some(vim.doc.starts[1] + 1),
    );
    assert_eq!(
        registers.file_marks.get('A').unwrap().offset_in(&vim.doc),
        Some(vim.doc.starts[1] + 1),
    );
    assert_eq!(
        registers.file_marks.get('C').unwrap().offset_in(&vim.doc),
        Some(vim.doc.starts[2] + 1),
    );
}

/// keepjumps protects jump and edit history across prompt dispatch and later confirmations.
#[test]
fn ex_mark_modifiers_keepjumps_through_prompts_and_edits() {
    let (mut vim, mut registers) = editor("first\ntarget\nthird\nfourth\n");
    vim.text("A!", &mut registers);
    vim.key("Escape", &mut registers);
    let last_change = vim.g.last_change();
    let last_insert = vim.g.insertion;
    vim.set_cursor(1);
    vim.record_jump();
    vim.set_cursor(vim.doc.starts[1]);
    let before = vim.jump_history.list(&vim.doc);
    vim.execute_ex_line("keepjumps normal! G", &mut registers);
    assert_eq!(vim.doc.location(vim.doc.cursor).0, 3);
    assert_eq!(vim.jump_history.list(&vim.doc), before);
    assert_eq!(vim.marks[&'\''], 1);

    vim.key(":", &mut registers);
    vim.text("keepjumps 2delete", &mut registers);
    vim.key("Enter", &mut registers);
    assert_eq!(vim.doc.text, "first!\nthird\nfourth\n");
    assert_eq!(vim.g.last_change(), last_change);

    vim.execute_ex_line("keepjumps 2s/third/THIRD/c", &mut registers);
    assert!(vim.substitute_confirmation_active());
    vim.key("y", &mut registers);
    assert_eq!(vim.doc.text, "first!\nTHIRD\nfourth\n");
    assert_eq!(vim.g.last_change(), last_change);
    assert_eq!(vim.g.insertion, last_insert);
    vim.text("x", &mut registers);
    assert_ne!(vim.g.last_change(), last_change);
}

/// Counted finds, repeats and operator motions consume one committed Unicode grapheme.
#[test]
fn native_literal_find_preserves_counts_operators_and_normal_keys() {
    let (mut vim, mut registers) = editor("a中b中c中d");
    vim.text("2f", &mut registers);
    assert!(vim.accepts_text());
    vim.commit_text("中文", &mut registers);
    assert!(vim.accepts_text());
    assert_eq!(vim.doc.cursor, 0);
    vim.commit_text("中", &mut registers);
    assert!(!vim.accepts_text());
    assert_eq!(vim.doc.cursor, "a中b".len());
    vim.key(";", &mut registers);
    assert_eq!(vim.doc.cursor, "a中b中c".len());
    vim.key(",", &mut registers);
    assert_eq!(vim.doc.cursor, "a中b".len());
    vim.text("0d2t", &mut registers);
    assert!(vim.accepts_text());
    vim.commit_text("中", &mut registers);
    assert_eq!(vim.doc.text, "中c中d");
    assert!(!vim.accepts_text());
    vim.key("m", &mut registers);
    assert!(!vim.accepts_text());
    vim.key("a", &mut registers);
    assert!(!vim.awaiting_key());
}

/// Committed replacement graphemes replay as edits, and Escape cancels a pending argument.
#[test]
fn native_literal_replace_replays_the_complete_grapheme() {
    let (mut vim, mut registers) = editor("abcd");
    vim.key("r", &mut registers);
    assert!(vim.accepts_text());
    vim.commit_text("e\u{301}", &mut registers);
    vim.text("l.", &mut registers);
    assert_eq!(vim.doc.text, "e\u{301}e\u{301}cd");
    vim.text("lgr", &mut registers);
    assert!(vim.accepts_text());
    vim.commit_text("中", &mut registers);
    assert_eq!(vim.doc.text, "e\u{301}e\u{301}中");
    vim.key("F", &mut registers);
    assert!(vim.accepts_text());
    vim.key("Escape", &mut registers);
    vim.commit_text("中", &mut registers);
    assert!(!vim.accepts_text());
    assert!(!vim.awaiting_key());
    assert_eq!(vim.doc.text, "e\u{301}e\u{301}中");
}

/// Preview text keeps Vim's selection and search without writes, filters or edits.
#[test]
fn read_only_vim_keeps_navigation_search_and_yank() {
    let original = "one alpha\n中文 beta\none gamma";
    let mut vim = Vim::read_only(Document::new(original));
    let mut registers = Register::default();
    for command in ["i", "o", "x", "dw", "rX", "J", "p", "u", "qaXq@a"] {
        vim.text(command, &mut registers);
        assert_eq!(vim.mode, Mode::Normal, "{command}");
        assert_eq!(vim.doc.text, original, "{command}");
    }
    for command in [":w", ":%s/one/changed/g"] {
        vim.text(command, &mut registers);
        assert_eq!(vim.key("Enter", &mut registers), Action::None);
        assert_eq!(vim.doc.text, original);
    }
    assert_eq!(vim.key("Ctrl+s", &mut registers), Action::None);
    assert_eq!(vim.apply_filter(0..3, "changed"), Action::None);
    vim.text("gg/", &mut registers);
    assert!(vim.accepts_text());
    vim.commit_text("中文", &mut registers);
    vim.key("Enter", &mut registers);
    assert_eq!(vim.mode, Mode::Normal);
    assert_eq!(vim.doc.cursor, vim.doc.starts[1]);
    vim.text("k0viwy", &mut registers);
    assert_eq!(registers.text, "one");
    assert_eq!(registers.yank_revision(), 1);
    assert_eq!(vim.doc.text, original);
    assert!(!vim.doc.modified());
}

/// Compare search, text-object changes, dot repeat and macros on the same buffer.
#[test]
fn backward_search_repeat_and_recorded_macros_edit_the_document() {
    let (mut vim, mut registers) = editor("one alpha\ntwo beta\none gamma");
    vim.text("G?one", &mut registers);
    assert_eq!(vim.prompt(), '?');
    vim.key("Enter", &mut registers);
    assert_eq!(vim.doc.cursor, 0);
    vim.text("ciw", &mut registers);
    assert_eq!(vim.mode, Mode::Insert);
    vim.commit_text("a", &mut registers);
    vim.key("Escape", &mut registers);
    vim.text("j0.", &mut registers);
    assert_eq!(vim.doc.line(1), "a beta");
    vim.text("qaxq@a", &mut registers);
    assert_eq!(vim.doc.line(1), "beta");
    vim.text(":%s/a/X/g", &mut registers);
    vim.key("Enter", &mut registers);
    assert_eq!(vim.doc.line(0), "X XlphX");
}

/// Keep native Unicode commits and counted insertion in single undo units.
#[test]
fn native_commits_and_replace_keep_graphemes_and_undo_units() {
    let (mut vim, mut registers) = editor("🙂a");
    vim.text("3i", &mut registers);
    vim.commit_text("猫", &mut registers);
    vim.key("Escape", &mut registers);
    assert_eq!(vim.doc.text, "猫猫猫🙂a");
    vim.key("u", &mut registers);
    assert_eq!(vim.doc.text, "🙂a");
    vim.text("0R", &mut registers);
    assert_eq!(vim.mode, Mode::Replace);
    vim.commit_text("中文", &mut registers);
    vim.key("Backspace", &mut registers);
    vim.key("Escape", &mut registers);
    assert_eq!(vim.doc.text, "中a");
    vim.key("u", &mut registers);
    assert_eq!(vim.doc.text, "🙂a");
}

/// Macro recording preserves Unicode IME text and control keys during replay.
#[test]
fn native_commit_is_replayed_as_literal_text() {
    let (mut vim, mut registers) = editor("abc\nABC");
    vim.text("qai", &mut registers);
    vim.commit_text("测<", &mut registers);
    vim.key("Escape", &mut registers);
    vim.text("qj0Q", &mut registers);
    assert_eq!(vim.doc.text, "测<abc\n测<ABC");
    vim.text("0.", &mut registers);
    assert_eq!(vim.doc.line(1), "测<测<ABC");
}

/// A supplied dot count replaces the original operator and motion counts.
#[test]
fn dot_count_replaces_the_motion_count_and_undo_does_not_replace_dot() {
    let (mut vim, mut registers) = editor("a b c d e f g h");
    vim.text("d2w3.", &mut registers);
    assert_eq!(vim.doc.text, "f g h");
    vim.text("u2.", &mut registers);
    assert_eq!(vim.doc.text, "e f g h");

    let (mut vim, mut registers) = editor("abcd");
    vim.text("rul.", &mut registers);
    assert_eq!(vim.doc.text, "uucd");
}

/// Block delete, put and change preserve rows and create one undo checkpoint.
#[test]
fn block_operations_are_rectangular() {
    let (mut vim, mut registers) = editor("abcde\nABCDE");
    vim.key("l", &mut registers);
    vim.key("Ctrl+v", &mut registers);
    vim.text("jl", &mut registers);
    assert_eq!(vim.selection_on_line(0), Some(1..3));
    assert_eq!(vim.selection_on_line(1), Some(7..9));
    vim.key("d", &mut registers);
    assert_eq!(vim.doc.text, "ade\nADE");
    vim.key("P", &mut registers);
    assert_eq!(vim.doc.text, "abcde\nABCDE");
    vim.key("Ctrl+v", &mut registers);
    vim.text("jlc", &mut registers);
    vim.commit_text("中", &mut registers);
    vim.key("Escape", &mut registers);
    assert_eq!(vim.doc.text, "a中de\nA中DE");
    vim.key("u", &mut registers);
    assert_eq!(vim.doc.text, "abcde\nABCDE");
}

/// Named line and character registers remain separate from subsequent unnamed deletes.
#[test]
fn named_registers_and_text_objects_keep_their_shapes() {
    let (mut vim, mut registers) = editor("alpha beta\ncall(中, foo)\ntail");
    vim.text("\"ayyj0di(", &mut registers);
    assert_eq!(vim.doc.line(1), "call()");
    vim.text("\"ap", &mut registers);
    assert_eq!(vim.doc.text, "alpha beta\ncall()\nalpha beta\ntail");
    vim.text("Gdd", &mut registers);
    assert_eq!(vim.doc.text, "alpha beta\ncall()\nalpha beta");
}

/// Apply Vim grouping, captures, line addresses and repeated substitutions.
#[test]
fn substitutions_use_vim_pattern_syntax_and_undo_as_one_edit() {
    let (mut vim, mut registers) = editor("foo 12 foo 13\nbar 3\nbar 5");
    vim.key(":", &mut registers);
    vim.commit_text(r"1,2s/\(foo\|bar\) \(\d\+\)/\2-\1/g", &mut registers);
    vim.key("Enter", &mut registers);
    assert_eq!(vim.doc.text, "12-foo 13-foo\n3-bar\nbar 5");
    vim.text("j&", &mut registers);
    assert_eq!(vim.doc.line(2), "5-bar");
    vim.key("u", &mut registers);
    assert_eq!(vim.doc.line(2), "bar 5");
    vim.key("u", &mut registers);
    assert_eq!(vim.doc.text, "foo 12 foo 13\nbar 3\nbar 5");
}

/// Search input edits at the prompt caret and n repeats the accepted direction.
#[test]
fn search_matches_words_in_both_directions() {
    let (mut vim, mut registers) = editor("cat cats cat\ncat");
    vim.text("G?ct", &mut registers);
    vim.key("Left", &mut registers);
    vim.commit_text("a", &mut registers);
    assert_eq!(vim.command, "cat");
    vim.key("Enter", &mut registers);
    assert_eq!(vim.doc.cursor, 9);
    vim.key("N", &mut registers);
    assert_eq!(vim.doc.location(vim.doc.cursor), (1, 0));
    vim.text("0*", &mut registers);
    assert_eq!(vim.doc.cursor, 0);
    vim.key("n", &mut registers);
    assert_eq!(vim.doc.cursor, 9);
}

/// Character finds, matching brackets and mark jumps use different range semantics.
#[test]
fn find_and_mark_arguments_retain_navigation_history() {
    let (mut vim, mut registers) = editor("a,b,c (x[y])\n  next");
    vim.text("f,;,", &mut registers);
    assert_eq!(vim.doc.cursor, 1);
    vim.text("magg%", &mut registers);
    assert_eq!(vim.doc.cursor, 11);
    vim.text("G`a", &mut registers);
    assert_eq!(vim.doc.cursor, 1);
    vim.key("Ctrl+o", &mut registers);
    assert_eq!(vim.doc.location(vim.doc.cursor), (1, 5));
    vim.key("Ctrl+i", &mut registers);
    assert_eq!(vim.doc.cursor, 1);
}

/// Screen-relative commands return a logical scroll target for the native renderer.
#[test]
fn screen_motions_and_scroll_use_the_visible_viewport() {
    let (mut vim, mut registers) = editor(&"  row\n".repeat(24));
    vim.set_cursor(vim.doc.offset(6, 2));
    vim.set_view(4, 6);
    vim.key("H", &mut registers);
    assert_eq!(vim.doc.location(vim.doc.cursor), (6, 2));
    vim.key("M", &mut registers);
    assert_eq!(vim.doc.location(vim.doc.cursor), (6, 2));
    vim.text("Lzt", &mut registers);
    assert_eq!(vim.take_view_top(), Some(5));
    vim.key("Ctrl+u", &mut registers);
    assert_eq!(vim.take_view_top(), Some(2));
    assert_eq!(vim.doc.location(vim.doc.cursor).0, 4);
    vim.text("zb", &mut registers);
    assert_eq!(vim.take_view_top(), Some(1));
}

/// Screen commands skip hidden rows and counted z commands locate their visible fold header.
#[test]
fn screen_motions_and_scroll_respect_plugin_folds() {
    let (mut vim, mut registers) = editor(&"row\n".repeat(24));
    vim.doc.collapse_rows(2..8);
    vim.set_cursor(vim.doc.offset(1, 0));
    vim.key("j", &mut registers);
    assert_eq!(vim.doc.location(vim.doc.cursor).0, 8);

    vim.set_view(2, 4);
    vim.key("M", &mut registers);
    assert_eq!(vim.doc.location(vim.doc.cursor).0, 9);
    vim.text("zt", &mut registers);
    assert_eq!(vim.take_view_top(), Some(2));

    vim.text("4zt", &mut registers);
    assert_eq!(vim.doc.location(vim.doc.cursor).0, 1);
    assert_eq!(vim.take_view_top(), Some(0));
    assert!(vim.doc.folded_at(1));
}

/// Counted zt can keep the caret near the top at the end of a file.
#[test]
fn viewport_retains_explicit_placement_at_the_end_of_the_file() {
    use crate::animation::scroll::{GridScroll, ScrollInput};

    let (mut vim, mut registers) = editor(&"row\n".repeat(40));
    vim.set_view(0, 8);
    vim.text("40zt", &mut registers);
    let row = vim.doc.location(vim.doc.cursor).0;
    let top = vim.take_view_top().unwrap();
    let mut scroll = GridScroll::default();
    scroll.track_rows(
        ScrollInput::PositionWithTail(top),
        Some(row),
        8,
        vim.line_count(),
        0.,
    );
    scroll.track_rows(ScrollInput::Advance, Some(row), 8, vim.line_count(), 1.);
    assert_eq!((row, scroll.target(), scroll.first_cell()), (39, 36, 36));
}

/// Horizontal z commands place the viewport, and move the caret only as it leaves the screen.
#[test]
fn horizontal_z_scrolls_and_aligns_the_caret() {
    use super::HorizontalScroll::{CursorLeft, CursorRight, Position};

    let (mut vim, mut registers) = editor("0123456789abcdefghijklmnopqrstuvwxyz");
    vim.set_view_width(0, 10);
    vim.text("zL", &mut registers);
    assert_eq!(vim.take_horizontal_scroll(), Some(Position(5)));
    assert_eq!(vim.doc.location(vim.doc.cursor), (0, 5));
    vim.text("2z3l", &mut registers);
    assert_eq!(vim.take_horizontal_scroll(), Some(Position(11)));
    assert_eq!(vim.doc.location(vim.doc.cursor), (0, 11));
    vim.text("zs", &mut registers);
    assert_eq!(vim.take_horizontal_scroll(), Some(CursorLeft));
    vim.text("ze", &mut registers);
    assert_eq!(vim.take_horizontal_scroll(), Some(CursorRight));
    assert_eq!(vim.doc.location(vim.doc.cursor), (0, 11));
    vim.key("z", &mut registers);
    vim.key("Left", &mut registers);
    assert_eq!(vim.take_horizontal_scroll(), Some(Position(1)));
    assert_eq!(vim.doc.location(vim.doc.cursor), (0, 10));
    vim.text("500zl", &mut registers);
    assert_eq!(vim.take_horizontal_scroll(), Some(Position(35)));
    assert_eq!(vim.doc.location(vim.doc.cursor), (0, 35));
}

/// A height limit is bounded by the host and can be increased without losing the viewport.
#[test]
fn z_height_can_shrink_and_restore_the_available_viewport() {
    let (mut vim, mut registers) = editor(&"row\n".repeat(24));
    vim.set_view(0, 8);
    vim.text("z28", &mut registers);
    vim.key("Delete", &mut registers);
    vim.key("Enter", &mut registers);
    assert_eq!(vim.view_height(8), 2);
    assert_eq!(vim.view.rows, 2);

    vim.text("z99", &mut registers);
    vim.key("Enter", &mut registers);
    assert_eq!(vim.view_height(8), 8);
    assert_eq!(vim.view.rows, 8);
}

/// A shell filter requests complete lines and applies output as a single undoable edit.
#[test]
fn shell_filter_round_trips_its_line_range() {
    let (mut vim, mut registers) = editor("zeta\nalpha");
    vim.text("2!!sort", &mut registers);
    let Action::Filter { range, command } = vim.key("Enter", &mut registers) else {
        panic!("expected a line filter");
    };
    assert_eq!(range, 0..10);
    let directory = tempfile::tempdir().unwrap();
    let result = run_filter(directory.path(), &command, &vim.doc.text[range.clone()]).unwrap();
    vim.apply_filter(range, &result);
    assert_eq!(vim.doc.text, "alpha\nzeta");
    vim.key("u", &mut registers);
    assert_eq!(vim.doc.text, "zeta\nalpha");
}

/// Insert control keys are separate from Normal scrolling and can insert a named register.
#[test]
fn insert_control_keys_edit_text_and_wait_for_the_register_name() {
    let (mut vim, mut registers) = editor("word\nother");
    vim.text("\"ayiwji", &mut registers);
    vim.commit_text("one two", &mut registers);
    vim.key("Ctrl+w", &mut registers);
    vim.key("Ctrl+u", &mut registers);
    vim.key("Ctrl+r", &mut registers);
    assert!(!vim.accepts_text());
    vim.key("a", &mut registers);
    assert!(vim.accepts_text());
    vim.key("Escape", &mut registers);
    assert_eq!(vim.doc.line(1), "wordother");
}

/// Omni completion requests the host while keeping Insert mode and document text.
#[test]
fn insert_omni_completion_consumes_its_prefix() {
    let (mut vim, mut register) = editor("value");
    vim.key("A", &mut register);
    assert_eq!(vim.key("Ctrl+x", &mut register), Action::None);
    assert!(vim.awaiting_key());
    assert_eq!(vim.key("Ctrl+o", &mut register), Action::PluginComplete);
    assert_eq!(vim.mode, super::Mode::Insert);
    assert_eq!(vim.doc.text, "value");
    assert!(vim.accepts_text());
    vim.key("Ctrl+x", &mut register);
    vim.key("Escape", &mut register);
    assert_eq!(vim.mode, super::Mode::Normal);
    assert!(!vim.awaiting_key());
    assert_eq!(vim.key("Ctrl+]", &mut register), Action::PluginDefinition);
    assert_eq!(vim.text("gO", &mut register), Action::PluginOutline);
}

/// The detail shortcut is a Normal-mode query and remains ordinary text in Insert mode.
#[test]
fn definition_details_key_is_normal_mode_only() {
    let (mut vim, mut register) = editor("name");
    assert_eq!(vim.key("K", &mut register), Action::PluginHover);
    vim.key("i", &mut register);
    assert_eq!(vim.key("K", &mut register), Action::None);
    assert_eq!(vim.doc.text, "Kname");
}

/// Characterwise Visual puts replace the last character at its actual byte range.
#[test]
fn visual_put_keeps_the_selected_end_of_line_position() {
    let (mut vim, mut registers) = editor("abcde");
    vim.text("yl$vp", &mut registers);
    assert_eq!(vim.doc.text, "abcda");
    assert_eq!(vim.doc.cursor, 4);
    assert_eq!(registers.text, "e");
}
