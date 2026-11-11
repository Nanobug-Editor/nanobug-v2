//! Behavior checks for real text mutations, UTF-8 boundaries, undo units and file safety.
use nanobug::{
    document::Document,
    vim::{Action, Mode, Register, Vim},
};
use std::{
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

/// Run `keys` against `text`, using named controls such as `Escape` as separate entries.
fn edit(text: &str, keys: &[&str]) -> (Vim, Register) {
    let mut vim = Vim::new(Document::new(text));
    let mut register = Register::default();
    for key in keys {
        vim.text(key, &mut register);
    }
    (vim, register)
}

/// Send a symbolic control key `key` without splitting its name into text characters.
fn control(vim: &mut Vim, register: &mut Register, key: &str) {
    vim.key(key, register);
}

/// Make a unique fixture directory under this project's ignored test-output directory.
fn fixture() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("test-output")
        .join(format!("{}-{nonce}", std::process::id()));
    fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn insert_session_is_one_undo_unit_and_redo_restores_it() {
    let (mut vim, mut register) = edit("hello", &["i", "中文🙂e\u{301}"]);
    control(&mut vim, &mut register, "Escape");
    assert_eq!(vim.mode, Mode::Normal);
    assert_eq!(vim.doc.text, "中文🙂e\u{301}hello");
    vim.key("u", &mut register);
    assert_eq!(vim.doc.text, "hello");
    control(&mut vim, &mut register, "Ctrl+r");
    assert_eq!(vim.doc.text, "中文🙂e\u{301}hello");
}

#[test]
fn grapheme_deletion_never_splits_emoji_or_combining_marks() {
    let (mut vim, mut register) = edit("👩‍💻e\u{301}猫", &["x"]);
    assert_eq!(vim.doc.text, "e\u{301}猫");
    vim.key("x", &mut register);
    assert_eq!(vim.doc.text, "猫");
    vim.key("u", &mut register);
    assert_eq!(vim.doc.text, "e\u{301}猫");
}

#[test]
fn counts_apply_to_motions_and_pending_operators() {
    let (vim, _) = edit("one two three four five", &["2d2w"]);
    assert_eq!(vim.doc.text, "five");
    let (vim, _) = edit("a\nb\nc\nd", &["3G"]);
    assert_eq!(vim.doc.location(vim.doc.cursor).0, 2);
}

#[test]
fn delete_last_word_includes_its_final_character() {
    let (vim, register) = edit("last", &["dw"]);
    assert_eq!(vim.doc.text, "");
    assert_eq!(register.text, "last");
}

#[test]
fn change_word_preserves_the_following_separator_and_undo_group() {
    let (mut vim, mut register) = edit("hello world", &["cw", "new"]);
    control(&mut vim, &mut register, "Escape");
    assert_eq!(vim.doc.text, "new world");
    vim.key("u", &mut register);
    assert_eq!(vim.doc.text, "hello world");
}

#[test]
fn change_empty_line_still_records_insert_undo() {
    let (mut vim, mut register) = edit("", &["c$", "abc"]);
    control(&mut vim, &mut register, "Escape");
    vim.key("u", &mut register);
    assert_eq!(vim.doc.text, "");
}

#[test]
fn linewise_yank_delete_and_paste_keep_line_boundaries() {
    let (vim, register) = edit("alpha\nbeta\ngamma", &["yyjp"]);
    assert_eq!(vim.doc.text, "alpha\nbeta\nalpha\ngamma");
    assert!(register.linewise);
    let (vim, _) = edit("a\nb\nc\nd", &["2dd"]);
    assert_eq!(vim.doc.text, "c\nd");
}

#[test]
fn visual_selection_is_inclusive_and_unicode_safe() {
    let (vim, _) = edit("a猫🙂z", &["v2ld"]);
    assert_eq!(vim.doc.text, "z");
    let (vim, _) = edit("a\nb\nc", &["Vjd"]);
    assert_eq!(vim.doc.text, "c");
}

#[test]
fn vertical_motion_retains_preferred_display_column_through_short_lines() {
    let (vim, _) = edit("abcdef\nx\nabcdef", &["4ljj"]);
    assert_eq!(vim.doc.location(vim.doc.cursor), (2, 4));
}

/// Pointer placement replaces a stale goal column for both wheel and keyboard navigation.
#[test]
fn wheel_and_vertical_keys_share_the_column_after_pointer_placement() {
    let (mut vim, mut register) = edit("abcdef\nabcd猫z\nx\nabcd猫z", &["j"]);
    vim.set_cursor(vim.doc.offset(1, 4));
    vim.move_to_row(2);
    assert_eq!(vim.doc.location(vim.doc.cursor), (2, 0));
    vim.key("j", &mut register);
    assert_eq!(vim.doc.location(vim.doc.cursor), (3, 4));
    vim.key("k", &mut register);
    vim.move_to_row(1);
    assert_eq!(vim.doc.location(vim.doc.cursor), (1, 4));
}

#[test]
fn tabs_are_stored_verbatim_and_measured_at_tab_stops() {
    let doc = Document::new("a\t猫z");
    assert_eq!(doc.offset(0, 4), 2);
    assert_eq!(doc.location(5), (0, 6));
    assert_eq!(doc.text, "a\t猫z");
}

#[test]
fn search_wraps_and_reverse_search_moves_back() {
    let (mut vim, mut register) = edit("cat dog cat", &["/cat"]);
    control(&mut vim, &mut register, "Enter");
    assert_eq!(vim.doc.cursor, 8);
    vim.key("n", &mut register);
    assert_eq!(vim.doc.cursor, 0);
    vim.key("N", &mut register);
    assert_eq!(vim.doc.cursor, 8);
}

#[test]
fn commands_produce_explicit_file_actions_with_space_containing_paths() {
    let (mut vim, mut register) = edit("", &[":w hello world.txt"]);
    assert_eq!(
        vim.key("Enter", &mut register),
        Action::Save(Some("hello world.txt".into()))
    );
    vim.text(":q!", &mut register);
    assert_eq!(vim.key("Enter", &mut register), Action::Close(true));
    for command in [":qa", ":qall"] {
        vim.text(command, &mut register);
        assert_eq!(vim.key("Enter", &mut register), Action::Close(false));
    }
}

#[test]
fn save_preserves_bom_crlf_and_tabs_and_existing_file_is_replaced() {
    let dir = fixture();
    let path = dir.join("中文.txt");
    fs::write(&path, b"\xef\xbb\xbfalpha\t\r\nbeta\r\n").unwrap();
    let mut doc = Document::open(&path).unwrap();
    doc.replace(0..5, "new", true);
    doc.save(None).unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"\xef\xbb\xbfnew\t\r\nbeta\r\n");
    assert!(!doc.modified());
    doc.undo(false);
    assert!(doc.modified());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn failed_save_keeps_original_content_and_dirty_state() {
    let dir = fixture();
    let path = dir.join("safe.txt");
    fs::write(&path, "original").unwrap();
    let mut doc = Document::open(&path).unwrap();
    doc.replace(0..8, "changed", true);
    assert!(doc.save(Some(&dir.join("missing/fail.txt"))).is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), "original");
    assert!(doc.modified());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn invalid_utf8_and_binary_files_are_rejected_without_changing_a_buffer() {
    let dir = fixture();
    let path = dir.join("binary");
    fs::write(&path, [0xff, 0]).unwrap();
    assert!(Document::open(&path).is_err());
    fs::write(&path, "a\0b").unwrap();
    assert!(Document::open(&path).is_err());
    fs::remove_dir_all(dir).unwrap();
}
