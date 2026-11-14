//! Editing-window regressions for retained buffers, native input and transient composition.

use super::{EditorWindow, input::NativeKeyResult};
use crate::{grid::render::Rect, ui::text::TextSpacing};
use gpui::{FontId, Keystroke, NoopTextSystem};
use nanobug::{document::Document, vim::Mode};
use std::{fs, sync::Arc};

impl EditorWindow {
    /// Send a native `key` to a fixture with `rows` visible and optional `help` filtering.
    fn test_key(&mut self, key: &Keystroke, rows: usize, help: bool) -> Option<NativeKeyResult> {
        let spacing = TextSpacing::new(Arc::new(NoopTextSystem::new()), FontId(0), 16., 9.);
        self.key(
            key,
            Rect {
                w: 88,
                h: rows + 2,
                ..Rect::default()
            },
            &spacing,
            help,
        )
    }
}

/// Native gd and the Ex command request a definition without changing text.
#[test]
fn definition_commands_use_normal_mode_and_preserve_the_source() {
    use nanobug::vim::Action;

    let text = "fn main() {}\n";
    let mut editor = EditorWindow::new(Document::new(text));
    let prefix = editor
        .test_key(&Keystroke::parse("g").unwrap(), 4, false)
        .unwrap();
    assert_eq!(prefix.edit.action, Action::None);
    let lookup = editor
        .test_key(&Keystroke::parse("d").unwrap(), 4, false)
        .unwrap();
    assert_eq!(lookup.edit.action, Action::PluginDefinition);
    let command = editor.edit(|vim, register| {
        vim.text(":definition", register);
        vim.key("Enter", register)
    });
    assert_eq!(command.action, Action::PluginDefinition);
    assert_eq!(editor.vim().doc.text, text);
    assert!(!editor.vim().doc.modified());
}

/// The gutter reflects counted j/k steps across folded lines and updates after a move.
#[test]
fn gutter_counts_visible_steps_from_the_absolute_caret_line() {
    use crate::{
        grid::{render::Frame, windows::WindowFrame},
        ui::theme::{MUTED, ThemeComponent},
    };

    let mut doc = Document::new("a\nb\nc\nd\ne\nf\n");
    doc.collapse_rows(2..4);
    doc.cursor = doc.starts[4];
    let mut editor = EditorWindow::new(doc);
    let spacing = TextSpacing::new(Arc::new(NoopTextSystem::new()), FontId(0), 16., 9.);
    let context = WindowFrame {
        area: Rect {
            w: 20,
            h: 6,
            ..Rect::default()
        },
        selected: false,
        modal_open: false,
        controls_hovered: false,
        spacing: &spacing,
    };

    let expected_gutter = |labels: [&str; 4]| {
        let mut frame = Frame::viewport(6, 5, 0);
        frame.set_theme_component(ThemeComponent::Gutter);
        for (row, label) in labels.into_iter().enumerate() {
            frame.text(0, row, label, MUTED, 6);
        }
        frame.icon(4, 1, super::view::FOLD_ARROW);
        frame.text(2, 4, "~", MUTED, 1);
        frame
    };

    let (gutter, _) = editor.content_frames(context, |_| None);
    assert!(gutter == expected_gutter(["   2 │", "   1", "   5 │", "   1 │"]));

    for key in ["2", "k"] {
        editor
            .test_key(&Keystroke::parse(key).unwrap(), 4, false)
            .unwrap();
    }
    assert_eq!(editor.vim().doc.cursor, 0);
    let (gutter, _) = editor.content_frames(context, |_| None);
    assert!(gutter == expected_gutter(["   1 │", "   1", "   2 │", "   3 │"]));

    for key in ["2", "j"] {
        editor
            .test_key(&Keystroke::parse(key).unwrap(), 4, false)
            .unwrap();
    }
    assert_eq!(editor.vim().doc.location(editor.vim().doc.cursor).0, 4);
    let (gutter, _) = editor.content_frames(context, |_| None);
    assert!(gutter == expected_gutter(["   2 │", "   1", "   5 │", "   1 │"]));
}

/// A folded brace block retains its terminator and source syntax colors on the visible header.
#[test]
fn folded_brace_blocks_render_an_inline_placeholder() {
    use crate::{
        grid::{render::Frame, windows::WindowFrame},
        ui::theme::{GOLD, TEXT, ThemeComponent},
    };

    let source = "\tlet 示例 = { \t\n    value\n};\nplain\n    hidden\n";
    let mut doc = Document::new(source);
    doc.set_provider_folds(std::iter::once(0..3).collect(), false);
    assert!(doc.toggle_fold(0));
    doc.collapse_rows(4..5);
    let closing = doc.starts[2];
    let mut editor = EditorWindow::new(doc);
    let spacing = TextSpacing::new(Arc::new(NoopTextSystem::new()), FontId(0), 16., 9.);
    let context = WindowFrame {
        area: Rect {
            w: 42,
            h: 5,
            ..Rect::default()
        },
        selected: false,
        modal_open: false,
        controls_hovered: false,
        spacing: &spacing,
    };
    let foreground = |byte| (byte == closing).then_some(GOLD);
    let (_, actual) = editor.content_frames(context, foreground);
    let width = editor.text_area(context.area).w + super::TEXT_OVERSCAN;
    let mut expected = Frame::viewport(width, 4, 0);
    expected.set_theme_component(ThemeComponent::Editor);
    let header = "\tlet 示例 = {";
    let preview = "\tlet 示例 = { ... };";
    expected.viewport_line(0, spacing.cells(preview), 0..width, 0, |byte| {
        match byte.checked_sub(header.len()) {
            Some(0..5) => (0xffffff, 0, true),
            Some(5) => (GOLD, 0, false),
            _ => (TEXT, 0, false),
        }
    });
    expected.viewport_line(1, spacing.cells("plain"), 0..width, 0, |_| (TEXT, 0, false));
    assert!(actual == expected);
    assert_eq!(
        editor.buffers[0].display_columns(0, editor.vim().doc.line(0), &spacing),
        spacing.columns(preview)
    );

    editor.edit(|vim, register| vim.key("z", register));
    editor.edit(|vim, register| vim.key("a", register));
    assert!(!editor.vim().doc.folded_at(0));
    assert_eq!(
        editor.buffers[0].display_columns(0, header, &spacing),
        spacing.columns(header)
    );
    assert_eq!(editor.vim().doc.text, source);
    assert!(!editor.vim().doc.modified());
}

/// Fold hit targets follow the painted positions and open one layer without moving the caret.
#[test]
fn fold_clicks_preserve_the_caret_and_open_one_layer() {
    use crate::{grid::render::Metrics, ui::text::UNITS_PER_COLUMN};
    use gpui::{point, px};

    let source = "lead\nfn 中文() { \t\n    inner();\n}\n    outer();\n}\nafter();\n";
    let spacing = TextSpacing::new(Arc::new(NoopTextSystem::new()), FontId(0), 16., 9.);
    let metrics = Metrics {
        cw: 9.,
        lh: 21.,
        font_size: 16.,
        origin: point(px(3.), px(7.)),
    };
    let area = Rect {
        x: 5,
        y: 2,
        w: 48,
        h: 12,
    };
    let header_end = spacing.cells("fn 中文() {").last().unwrap().end;
    let header_width = header_end as f32 / UNITS_PER_COLUMN as f32;

    for dot in [None, Some(0), Some(1), Some(2)] {
        let mut doc = Document::new(source);
        doc.collapse_rows(2..6);
        doc.collapse_rows(2..4);
        doc.cursor = doc.starts[6] + 1;
        let cursor = doc.cursor;
        let mut editor = EditorWindow::new(doc);
        let buffer = editor.buffer_mut();
        buffer.scroll.snap(1, 0.);
        buffer.scroll.request(0, 1, 0.);
        buffer.scroll.advance(0.04);
        buffer.columns.snap(3, 0.);
        buffer.columns.request(0, 3, 0.);
        buffer.columns.advance(0.04);
        let (top, left) = (buffer.scroll.position(), buffer.columns.position());
        assert!((0.5..1.).contains(&top));
        assert!(buffer.columns.fraction() > 0.);
        let body = editor.text_area(area);
        // Scrolling moves the folded row's center into the first row of the viewport.
        let y = body.y as f32 + 1.5 - top;
        let text_x = body.x as f32 + header_width - left;
        for x in [
            body.x as f32 - 2.5,
            body.x as f32 - 0.5,
            text_x - 0.5,
            text_x + 0.5,
            text_x + 4.5,
            text_x + 5.5,
        ] {
            assert!(!editor.open_fold_at_position(metrics.at(x, y), area, metrics, &spacing));
        }
        let x = dot.map_or(body.x as f32 - 1.5, |dot| text_x + 1.5 + dot as f32);
        let target = metrics.at(x, y);
        assert_eq!(editor.vim().doc.folded_end_row(1), Some(5));
        editor.press(target, area, metrics, &spacing);
        assert_eq!(editor.vim().doc.folded_end_row(1), Some(3));
        assert_eq!(editor.vim().doc.cursor, cursor);
        editor.press(target, area, metrics, &spacing);
        assert_eq!(editor.vim().doc.folded_end_row(1), None);
        assert_eq!(editor.vim().doc.cursor, cursor);
        assert!(!editor.is_dragging());
        assert_eq!(editor.vim().doc.text, source);
    }
}

/// Apply a native `key` to `editor`, then reveal its caret in the fixed test viewport.
fn fold_test_key(editor: &mut EditorWindow, key: &'static str, area: Rect, spacing: &TextSpacing) {
    use nanobug::animation::scroll::ScrollInput;

    editor
        .key(&Keystroke::parse(key).unwrap(), area, spacing, false)
        .unwrap();
    editor.track_view(area, true, spacing, [ScrollInput::Reveal; 2], 1.);
}

/// Enter opens only the visible fold and preserves its source cursor.
#[test]
fn enter_opens_one_fold_level_and_stays_on_the_header() {
    let source = "fn main() {\n    if ready {\n        work();\n    }\n}\nafter();\n";
    let mut doc = Document::new(source);
    doc.collapse_rows(1..5);
    doc.collapse_rows(2..4);
    doc.cursor = doc.line(0).find('{').unwrap();
    let opening = doc.cursor;
    let mut editor = EditorWindow::new(doc);
    let spacing = TextSpacing::new(Arc::new(NoopTextSystem::new()), FontId(0), 16., 9.);
    let area = Rect {
        w: 48,
        h: 12,
        ..Rect::default()
    };

    fold_test_key(&mut editor, "enter", area, &spacing);
    assert_eq!(editor.vim().doc.cursor, opening);
    assert!(!editor.vim().doc.folded_at(0));
    assert!(editor.vim().doc.folded_at(1));

    fold_test_key(&mut editor, "enter", area, &spacing);
    let inner = editor.vim().doc.starts[1] + 4;
    assert_eq!(editor.vim().doc.cursor, inner);
    assert!(editor.vim().doc.folded_at(1));

    fold_test_key(&mut editor, "enter", area, &spacing);
    assert_eq!(editor.vim().doc.cursor, inner);
    assert!(!editor.vim().doc.folded_at(1));
    assert_eq!(editor.vim().doc.text, source);
    assert!(!editor.vim().doc.modified());
}

/// Search exposes closed ancestors only while the caret remains inside their bodies.
#[test]
fn search_reveals_nested_folds_temporarily() {
    let source =
        "fn main() {\n    if ready {\n        needle();\n    }\n    work();\n}\nneedle();\n";
    let mut doc = Document::new(source);
    doc.collapse_rows(1..6);
    doc.collapse_rows(2..4);
    let hit = doc.starts[2] + 8;
    let outside = doc.starts[6];
    let mut editor = EditorWindow::new(doc);
    let spacing = TextSpacing::new(Arc::new(NoopTextSystem::new()), FontId(0), 16., 9.);
    let area = Rect {
        w: 48,
        h: 12,
        ..Rect::default()
    };

    fold_test_key(&mut editor, "/", area, &spacing);
    assert_eq!(editor.vim().mode, Mode::Search);
    editor.edit(|vim, register| vim.commit_text("needle", register));
    fold_test_key(&mut editor, "enter", area, &spacing);
    assert_eq!(editor.vim().doc.cursor, hit);
    assert!(!editor.vim().doc.folded_at(0));
    assert!(!editor.vim().doc.folded_at(1));

    fold_test_key(&mut editor, "n", area, &spacing);
    assert_eq!(editor.vim().doc.cursor, outside);
    assert!(editor.vim().doc.folded_at(0));
    fold_test_key(&mut editor, "shift-n", area, &spacing);
    assert_eq!(editor.vim().doc.cursor, hit);
    assert!(!editor.vim().doc.folded_at(0));

    for key in ["j", "j"] {
        fold_test_key(&mut editor, key, area, &spacing);
    }
    assert_eq!(editor.vim().doc.location(editor.vim().doc.cursor).0, 4);
    assert!(editor.vim().doc.folded_at(1));
    assert!(!editor.vim().doc.folded_at(0));
    for key in ["j", "j"] {
        fold_test_key(&mut editor, key, area, &spacing);
    }
    assert_eq!(editor.vim().doc.location(editor.vim().doc.cursor).0, 6);
    assert!(editor.vim().doc.folded_at(0));
    assert_eq!(editor.vim().doc.text, source);
    assert!(!editor.vim().doc.modified());
}

/// A closed block keeps its visual brace targets and ordinary motion never opens it.
#[test]
fn collapsed_braces_are_navigable_without_opening_the_fold() {
    let source = "fn main() {\n    work();\n}\nafter();\n";
    let mut doc = Document::new(source);
    doc.collapse_rows(1..3);
    let opening = doc.line(0).find('{').unwrap();
    let after = doc.starts[3];
    doc.cursor = opening;
    let mut editor = EditorWindow::new(doc);
    let spacing = TextSpacing::new(Arc::new(NoopTextSystem::new()), FontId(0), 16., 9.);
    let area = Rect {
        w: 48,
        h: 12,
        ..Rect::default()
    };
    let (x, y, _, _, _) = editor.body_caret(area, &spacing);

    fold_test_key(&mut editor, "l", area, &spacing);
    let (closing_x, closing_y, _, _, visible) = editor.body_caret(area, &spacing);
    assert_eq!(closing_x - x, 6.);
    assert_eq!(closing_y, y);
    assert!(visible);
    assert_eq!(editor.vim().doc.cursor, opening);
    assert!(editor.vim().doc.folded_at(0));

    fold_test_key(&mut editor, "h", area, &spacing);
    assert_eq!(editor.body_caret(area, &spacing).0, x);
    fold_test_key(&mut editor, "j", area, &spacing);
    assert_eq!(editor.vim().doc.location(editor.vim().doc.cursor).0, 3);
    fold_test_key(&mut editor, "k", area, &spacing);
    assert_eq!(editor.vim().doc.location(editor.vim().doc.cursor).0, 0);

    for (key, start) in [
        ("%", opening),
        ("w", opening),
        ("e", opening),
        ("+", opening),
        ("-", after),
    ] {
        editor.edit(|vim, _| {
            vim.set_cursor(start);
            nanobug::vim::Action::None
        });
        fold_test_key(&mut editor, key, area, &spacing);
        assert!(editor.vim().doc.folded_at(0), "{key} opened the fold");
        let row = editor.vim().doc.location(editor.vim().doc.cursor).0;
        assert!(matches!(row, 0 | 3), "{key} landed on hidden row {row}");
    }

    editor.edit(|vim, _| {
        vim.set_cursor(opening);
        nanobug::vim::Action::None
    });
    fold_test_key(&mut editor, "l", area, &spacing);
    fold_test_key(&mut editor, "enter", area, &spacing);
    assert!(!editor.vim().doc.folded_at(0));
    assert_eq!(editor.vim().doc.cursor, opening);
    assert_eq!(editor.body_caret(area, &spacing).0, x);
    assert_eq!(editor.vim().doc.text, source);
    assert!(!editor.vim().doc.modified());
}

/// Switching files must preserve unsaved buffers, and failed opens must be atomic.
#[test]
fn switching_files_retains_unsaved_text_and_failed_open_preserves_state() {
    let project = tempfile::tempdir().unwrap();
    let first = project.path().join("first.txt");
    let second = project.path().join("second.txt");
    fs::write(&first, "first").unwrap();
    fs::write(&second, "second").unwrap();
    let mut editor = EditorWindow::new(Document::open(&first).unwrap());
    editor.edit(|vim, register| vim.text("i!", register));
    editor.open(second, 1.).unwrap();
    assert_eq!(editor.vim().doc.text, "second");
    editor.open(first, 2.).unwrap();
    assert_eq!(editor.vim().doc.text, "!first");
    assert!(editor.modified());
    assert_eq!(editor.buffers.len(), 2);

    let active = editor.active;
    let invalid = project.path().join("invalid.txt");
    fs::write(&invalid, [0xff]).unwrap();
    assert!(editor.open(invalid, 3.).is_err());
    assert_eq!(editor.active, active);
    assert_eq!(editor.buffers.len(), 2);
    assert_eq!(editor.vim().doc.text, "!first");
}

/// Page navigation restores each caret and scroll while sharing text, undo and the buffer ID.
#[test]
fn tabpages_share_edits_and_retain_independent_views() {
    let text = "甲乙\n丙丁\n最终";
    let mut editor = EditorWindow::new(Document::new(text));
    let id = editor.buffer().id;
    editor.jump_to(3);
    editor.buffers[editor.active].scroll.snap(1, 1.);
    editor.add_tabpage(2.);
    assert_eq!(editor.tabpage_count(), 2);
    assert_eq!(editor.active_tabpage(), 1);
    assert_eq!(editor.previous_tabpage(), Some(0));
    assert_eq!(editor.buffer().id, id);

    editor.edit(|vim, _| {
        let offset = vim.doc.offset(2, 0);
        vim.doc.replace(offset..vim.doc.text.len(), "新", true);
        nanobug::vim::Action::None
    });
    editor.jump_to(14);
    editor.buffers[editor.active].scroll.snap(2, 2.);
    assert!(editor.select_tabpage(0, 3.));
    assert_eq!(editor.buffer().id, id);
    assert_eq!(editor.vim().doc.cursor, 3);
    assert_eq!(editor.buffer().scroll.first_cell(), 1);
    assert_eq!(editor.vim().doc.text, "甲乙\n丙丁\n新");
    editor.edit(|vim, _| {
        vim.doc.undo(false);
        nanobug::vim::Action::None
    });
    assert!(editor.select_tabpage(1, 4.));
    assert_eq!(editor.vim().doc.cursor, 14);
    assert_eq!(editor.buffer().scroll.first_cell(), 2);
    assert_eq!(editor.vim().doc.text, text);
    assert_eq!(editor.buffers().len(), 1);
}

/// Closing a page transfers its orphaned draft to a reachable label, and removing a shared
/// inactive label leaves the current buffer selected.
#[test]
fn closing_tabpage_keeps_drafts_and_middle_close_detaches_labels() {
    let mut editor = EditorWindow::new(Document::new("shared"));
    let shared = editor.buffer().id;
    editor.add_tabpage(0.);
    let index = editor.retain_document(Document::new("draft"));
    let draft = editor.buffers()[index].id;
    assert!(editor.select_buffer(draft, 0.));
    editor.edit(|vim, register| vim.text("i!", register));
    editor.finish_buffer_input();
    assert!(editor.buffer_in_other_tabpage(shared));
    assert!(editor.remove_buffer_tab(shared, 0.));
    assert_eq!(editor.buffer().id, draft);
    assert!(editor.buffer_index(shared).is_some());
    assert!(editor.remove_tabpage(0.));
    assert_eq!(editor.buffer().id, shared);
    assert_eq!(
        editor
            .visible_buffers()
            .map(|buffer| buffer.id)
            .collect::<Vec<_>>(),
        [shared, draft]
    );
    assert!(!editor.remove_tabpage(0.));
    assert!(editor.select_buffer(draft, 0.));
    assert_eq!(editor.vim().doc.text, "!draft");
}

/// Normal-mode brackets navigate retained buffers and only current diagnostic positions.
#[test]
fn bracket_navigation_uses_native_keys_and_current_buffer_data() {
    use nanobug::{plugins::session::DocumentDiagnostics, vim::Action};
    use nanobug_plugin_sdk::Diagnostic;
    use std::path::PathBuf;

    let mut document = Document::new("first\nsecond\nthird");
    let path = PathBuf::from("bracket-fixture.txt");
    document.path = Some(path.clone());
    let mut editor = EditorWindow::new(document);
    editor.retain_document(Document::new("other"));
    let keys = |editor: &mut EditorWindow, prefix, key| {
        editor
            .test_key(&Keystroke::parse(prefix).unwrap(), 8, false)
            .unwrap();
        editor
            .test_key(&Keystroke::parse(key).unwrap(), 8, false)
            .unwrap()
            .edit
            .action
    };
    let Action::BracketBuffer {
        forward,
        edge,
        count,
    } = keys(&mut editor, "]", "b")
    else {
        panic!("expected buffer navigation");
    };
    assert!(editor.navigate_buffers(forward, edge, count));
    assert_eq!(editor.vim().doc.text, "other");
    let Action::BracketBuffer {
        forward,
        edge,
        count,
    } = keys(&mut editor, "[", "shift-b")
    else {
        panic!("expected buffer endpoint navigation");
    };
    assert!(editor.navigate_buffers(forward, edge, count));
    assert_eq!(editor.vim().doc.text, "first\nsecond\nthird");
    let version = (editor.vim().doc.opening, editor.vim().doc.revision);
    let diagnostics = [13, 6, 6].map(|start| Diagnostic {
        start,
        end: start,
        message: "fixture".into(),
    });
    editor
        .buffer_mut()
        .set_diagnostics(vec![Arc::new(DocumentDiagnostics {
            version,
            path,
            diagnostics: diagnostics.into(),
        })]);
    let Action::BracketDiagnostic {
        forward,
        edge,
        count,
    } = keys(&mut editor, "[", "d")
    else {
        panic!("expected diagnostic navigation");
    };
    assert_eq!(
        editor.buffer().diagnostic_target(forward, edge, count),
        Some(13)
    );
    editor.jump_to(13);
    assert_eq!(editor.buffer().diagnostic_target(true, false, 1), Some(6));
    assert_eq!(editor.buffer().diagnostic_target(false, true, 1), Some(6));
    editor.edit(|vim, register| vim.text("x", register));
    assert_eq!(editor.buffer().diagnostic_target(true, true, 1), None);
}

/// Preedit uses UTF-16 ranges but does not mutate the document until the single committed edit.
#[test]
fn composition_preserves_surrogate_boundaries_and_commits_one_undo_unit() {
    let mut editor = EditorWindow::new(Document::new("a"));
    editor
        .test_key(&Keystroke::parse("i").unwrap(), 8, false)
        .unwrap();
    editor.composition.mark(None, "中🙂", Some(1..3));
    assert_eq!(editor.vim().doc.text, "a");
    assert!(!editor.modified());
    let mut adjusted = None;
    assert_eq!(
        editor
            .composition
            .text_for_range(1..2, &mut adjusted)
            .as_deref(),
        Some("")
    );
    assert_eq!(adjusted, Some(1..1));
    assert_eq!(
        editor
            .composition
            .text_for_range(1..3, &mut adjusted)
            .as_deref(),
        Some("🙂")
    );
    assert_eq!(adjusted, Some(1..3));

    let text = editor.composition.take();
    editor.edit(|vim, register| vim.text(&text, register));
    assert_eq!(editor.vim().doc.text, "中🙂a");
    editor
        .test_key(&Keystroke::parse("escape").unwrap(), 8, false)
        .unwrap();
    editor
        .test_key(&Keystroke::parse("u").unwrap(), 8, false)
        .unwrap();
    assert_eq!(editor.vim().doc.text, "a");
}

/// Native mode keys and page movement are handled locally, while Insert text is reserved for IME.
#[test]
fn native_keys_route_text_and_page_movement_without_duplicate_input() {
    let mut editor = EditorWindow::new(Document::new("one\ntwo\nthree\nfour\nfive"));
    editor
        .test_key(&Keystroke::parse("i").unwrap(), 2, false)
        .unwrap();
    assert_eq!(editor.vim().mode, Mode::Insert);
    assert!(
        editor
            .test_key(&Keystroke::parse("x").unwrap(), 2, false)
            .is_none()
    );
    assert_eq!(editor.vim().doc.text, "one\ntwo\nthree\nfour\nfive");
    editor
        .test_key(&Keystroke::parse("escape").unwrap(), 2, false)
        .unwrap();
    let page = editor
        .test_key(&Keystroke::parse("pagedown").unwrap(), 2, false)
        .unwrap();
    assert!(!page.clear_status);
    assert_eq!(editor.vim().doc.location(editor.vim().doc.cursor).0, 2);
}

/// A help chord can edit or save, while operations needing follow-up leave the buffer untouched.
#[test]
fn help_chords_keep_editor_mode_and_pending_input_intact() {
    let mut editor = EditorWindow::new(Document::new("one\ntwo"));
    for key in [
        "shift-a", "shift-c", "shift-i", "shift-o", "shift-r", "shift-s", "shift-v", "shift-f",
        "shift-t", "shift-z", "shift-q", "shift-/", "shift-;", "ctrl-v",
    ] {
        assert!(
            editor
                .test_key(&Keystroke::parse(key).unwrap(), 8, true)
                .is_none(),
            "{key}"
        );
        assert_eq!(editor.vim().mode, Mode::Normal, "{key}");
        assert!(!editor.vim().awaiting_key(), "{key}");
        assert_eq!(editor.vim().doc.text, "one\ntwo", "{key}");
    }
    editor
        .test_key(&Keystroke::parse("shift-j").unwrap(), 8, true)
        .unwrap();
    assert_eq!(editor.vim().doc.text, "one two");
    assert_eq!(
        editor
            .test_key(&Keystroke::parse("ctrl-s").unwrap(), 8, true)
            .unwrap()
            .edit
            .action,
        nanobug::vim::Action::Save(None),
    );
    editor
        .test_key(&Keystroke::parse("shift-a").unwrap(), 8, false)
        .unwrap();
    assert_eq!(editor.vim().mode, Mode::Insert);
}

/// Explicit modal scrolling preserves the new column as ordinary native input resumes.
#[test]
fn native_z_scroll_places_measured_columns_and_limits_rendered_rows() {
    use nanobug::animation::scroll::ScrollInput;

    let mut editor = EditorWindow::new(Document::new(
        "one abcdefghijklmnopqrstuvwxyz\ntwo\nthree\nfour",
    ));
    let spacing = TextSpacing::new(Arc::new(NoopTextSystem::new()), FontId(0), 16., 9.);
    let area = Rect {
        w: 18,
        h: 6,
        ..Rect::default()
    };
    let input = |editor: &mut EditorWindow, key: &'static str| {
        editor
            .key(&Keystroke::parse(key).unwrap(), area, &spacing, false)
            .unwrap()
    };
    input(&mut editor, "z");
    let result = input(&mut editor, "shift-l");
    assert_eq!(result.view_left, Some(5));
    editor.track_view(
        area,
        true,
        &spacing,
        [ScrollInput::Reveal, ScrollInput::PositionWithTail(5)],
        1.,
    );
    assert_eq!(editor.buffer().columns.target(), 5);

    input(&mut editor, "z");
    input(&mut editor, "2");
    input(&mut editor, "enter");
    assert_eq!(editor.text_area(area).h, 2);
    editor.track_view(area, true, &spacing, [ScrollInput::Reveal; 2], 2.);
    assert_eq!(editor.buffer().columns.target(), 5);
    assert!(editor.body_caret(area, &spacing).4);
}
