//! Editing-window regressions for retained buffers, native input and transient composition.

use super::{EditorWindow, input::NativeKeyResult};
use crate::{grid::render::Rect, ui::text::TextSpacing};
use gpui::{FontId, Keystroke, NoopTextSystem};
use nanobug::{document::Document, vim::Mode};
use std::{fs, path::Path, sync::Arc};

/// Ex styles compose with SDK-provided syntax spans and clear back to the live theme.
#[test]
fn ex_highlight_overlays_retained_language_plugin_spans() -> Result<(), String> {
    use crate::{
        grid::windows::WindowFrame,
        ui::theme::{TEXT, syntax_color},
    };
    use nanobug::{
        plugins::session::{DocumentPresentation, Key},
        vim::Action,
    };
    use nanobug_plugin_sdk::{
        Instance,
        language::{ByteRange, DocumentFeatures, Highlight, SyntaxKind},
    };

    let doc = Document::new("let TODO other\n");
    let version = (doc.opening, doc.revision);
    let mut editor = EditorWindow::new(doc);
    editor.buffers[0].set_presentation(Some((
        Key {
            namespace: "language".into(),
            instance: Instance {
                project: 1,
                module: "document".into(),
            },
        },
        Arc::new(DocumentPresentation {
            version,
            generation: 1,
            features: DocumentFeatures {
                highlights: vec![
                    Highlight {
                        range: ByteRange { start: 0, end: 3 },
                        kind: SyntaxKind::Keyword,
                    },
                    Highlight {
                        range: ByteRange { start: 4, end: 14 },
                        kind: SyntaxKind::Comment,
                    },
                ],
                ..Default::default()
            },
        }),
    )));
    let spacing = TextSpacing::new(Arc::new(NoopTextSystem::new()), FontId(0), 16., 9.);
    let context = WindowFrame {
        area: Rect {
            w: 28,
            h: 6,
            ..Default::default()
        },
        selected: false,
        modal_open: false,
        controls_hovered: false,
        spacing: &spacing,
    };
    let paint = |pane: &EditorWindow, column| {
        pane.content_frames(context, |_| None)
            .1
            .cell_paint_at(0, column)
    };
    let ex = |pane: &mut EditorWindow, command: &str| -> Result<(), String> {
        let edit = pane.edit(|vim, register| vim.execute_ex_line(command, register));
        match edit.action {
            Action::None if !pane.vim().ex_failed() => Ok(()),
            other => Err(format!("{command}: {other:?} {}", pane.vim().message)),
        }
    };
    assert_eq!(
        paint(&editor, 4).foreground,
        syntax_color(SyntaxKind::Comment)
    );
    ex(&mut editor, "hi Comment guifg=#112233 gui=italic")?;
    assert_eq!(
        paint(&editor, 0).foreground,
        syntax_color(SyntaxKind::Keyword)
    );
    assert_eq!(paint(&editor, 4).foreground, 0x112233);
    ex(&mut editor, "hi TodoMatch guifg=#eeaa22 guibg=#443322")?;
    ex(&mut editor, "match TodoMatch /TODO/")?;
    assert_eq!(paint(&editor, 4).foreground, 0xeeaa22);
    assert_eq!(paint(&editor, 4).background, 0x443322ff);
    assert_eq!(paint(&editor, 9).foreground, 0x112233);
    ex(&mut editor, "match")?;
    assert_eq!(paint(&editor, 4).foreground, 0x112233);
    ex(&mut editor, "hi clear Comment")?;
    assert_eq!(paint(&editor, 4).foreground, TEXT);
    ex(&mut editor, "hi clear")?;
    assert_eq!(
        paint(&editor, 4).foreground,
        syntax_color(SyntaxKind::Comment)
    );
    assert_eq!(editor.buffers[0].highlights().len(), 2);
    Ok(())
}

/// Page keys can traverse a Markdown paragraph that spans more than one screen without newlines.
#[test]
fn markdown_pages_through_one_wrapped_source_line() {
    use nanobug::animation::scroll::ScrollInput;

    let text = "abcdefghij".repeat(14);
    let mut doc = Document::new(&text);
    doc.path = Some("long.md".into());
    let mut editor = EditorWindow::new(doc);
    editor.set_markdown_view_mode(super::MarkdownViewMode::Edit);
    let area = Rect {
        w: 16,
        h: 8,
        ..Rect::default()
    };
    let spacing = TextSpacing::new(Arc::new(NoopTextSystem::new()), FontId(0), 16., 9.);
    let page_down = Keystroke::parse("pagedown").unwrap();
    let result = editor
        .key(&page_down, area, &spacing, Path::new("."), false)
        .unwrap();
    assert_eq!(result.view_top, Some(4));
    editor.track_view(
        area,
        true,
        &spacing,
        [ScrollInput::PositionWithTail(4), ScrollInput::Reveal],
        0.,
    );
    assert_eq!(editor.buffer().scroll.target(), 4);
    assert!(editor.vim().doc.cursor > editor.text_area(area).w);
    assert_eq!(editor.vim().doc.starts, [0]);
    assert_eq!(editor.vim().doc.text, text);

    let page_up = Keystroke::parse("pageup").unwrap();
    assert_eq!(
        editor
            .key(&page_up, area, &spacing, Path::new("."), false)
            .unwrap()
            .view_top,
        Some(0),
    );
}

/// Counted screen-row motions preserve the measured column through short rows and closed folds.
#[test]
fn markdown_gj_gk_use_measured_rows_across_wide_text_and_folds() {
    let source = "ab中文cdefghié\n{\ninside\n}\nafterx";
    let mut doc = Document::new(source);
    doc.path = Some("wrapped.md".into());
    doc.cursor = source.find('中').unwrap();
    doc.collapse_rows(2..4);
    let mut editor = EditorWindow::new(doc);
    assert!(editor.set_markdown_view_mode(super::MarkdownViewMode::Edit));
    let area = Rect {
        w: 12,
        h: 7,
        ..Rect::default()
    };
    let spacing = TextSpacing::new(Arc::new(NoopTextSystem::new()), FontId(0), 16., 9.);
    assert_eq!(editor.text_area(area).w, 6);
    let input = |editor: &mut EditorWindow, keys: &[&'static str], offset| {
        for key in keys {
            fold_test_key(editor, key, area, &spacing);
        }
        assert_eq!(editor.vim().doc.cursor, offset, "{keys:?}");
    };

    input(&mut editor, &["g", "j"], source.find('f').unwrap());
    input(&mut editor, &["g", "j"], source.find('é').unwrap());
    input(&mut editor, &["g", "up"], source.find('f').unwrap());
    input(&mut editor, &["g", "k"], source.find('中').unwrap());
    input(&mut editor, &["2", "g", "down"], source.find('é').unwrap());
    input(&mut editor, &["g", "j"], source.find('{').unwrap());
    input(&mut editor, &["g", "j"], source.find("afterx").unwrap() + 2);
    assert!(editor.vim().doc.folded_end_row(1).is_some());
}

/// Visual, operators and both replay paths use the same screen-row endpoints as direct input.
#[test]
fn markdown_gj_keeps_visual_operators_and_replay_on_wrapped_rows() {
    let mut doc = Document::new("abcdefghijklmnopqr");
    doc.path = Some("wrapped.md".into());
    doc.cursor = 2;
    let mut editor = EditorWindow::new(doc);
    assert!(editor.set_markdown_view_mode(super::MarkdownViewMode::Edit));
    let area = Rect {
        w: 12,
        h: 10,
        ..Rect::default()
    };
    let spacing = TextSpacing::new(Arc::new(NoopTextSystem::new()), FontId(0), 16., 9.);
    let input = |editor: &mut EditorWindow, keys: &[&'static str]| {
        for key in keys {
            fold_test_key(editor, key, area, &spacing);
        }
    };

    input(&mut editor, &["v", "g", "j"]);
    assert_eq!(editor.vim().selection(), Some(2..9));
    input(
        &mut editor,
        &["escape", "0", "l", "l", "q", "a", "g", "j", "q"],
    );
    assert_eq!(editor.vim().doc.cursor, 8);
    input(&mut editor, &["0", "l", "l", "shift-2", "a"]);
    assert_eq!(editor.vim().doc.cursor, 8);
    input(&mut editor, &["0", "l", "l", "d", "g", "j"]);
    assert_eq!(editor.vim().doc.text, "abijklmnopqr");
    input(&mut editor, &["."]);
    assert_eq!(editor.vim().doc.text, "abopqr");
}

/// Horizontal g motions use only the measured current or counted Markdown screen row.
#[test]
fn markdown_g_row_edges_and_middle_follow_measured_wrapping() {
    let source = "abcdef  xyz uvwxyz12";
    let mut doc = Document::new(source);
    doc.path = Some("wrapped.md".into());
    doc.cursor = 10;
    let mut editor = EditorWindow::new(doc);
    assert!(editor.set_markdown_view_mode(super::MarkdownViewMode::Edit));
    let area = Rect {
        w: 12,
        h: 7,
        ..Rect::default()
    };
    let spacing = TextSpacing::new(Arc::new(NoopTextSystem::new()), FontId(0), 16., 9.);
    let input = |editor: &mut EditorWindow, keys: &[&'static str], offset| {
        for key in keys {
            fold_test_key(editor, key, area, &spacing);
        }
        assert_eq!(editor.vim().doc.cursor, offset, "{keys:?}");
    };
    input(&mut editor, &["g", "0"], 6);
    input(&mut editor, &["g", "shift-6"], 8);
    input(&mut editor, &["g", "m"], 9);
    input(&mut editor, &["g", "shift-4"], 11);
    input(&mut editor, &["g", "end"], 10);
    input(&mut editor, &["2", "g", "shift-4"], 17);
    input(&mut editor, &["g", "home"], 12);
    input(&mut editor, &["d", "g", "shift-4"], 12);
    assert_eq!(editor.vim().doc.text, "abcdef  xyz 12");
}

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
            Path::new("."),
            help,
        )
    }
}

/// Native Tab and Shift+Tab reach the Ex prompt while committed IME text stays intact.
#[test]
fn native_ex_tab_and_history_use_the_project_root() {
    use nanobug::vim::Action;

    let project = tempfile::tempdir().unwrap();
    fs::write(project.path().join("文 甲.txt"), "").unwrap();
    fs::write(project.path().join("文 乙.txt"), "").unwrap();
    let spacing = TextSpacing::new(Arc::new(NoopTextSystem::new()), FontId(0), 16., 9.);
    let area = Rect {
        w: 88,
        h: 25,
        ..Rect::default()
    };
    let mut editor = EditorWindow::new(Document::new(""));
    editor.edit(|vim, register| {
        vim.key(":", register);
        vim.commit_text("edit 文", register)
    });
    for (key, want) in [("shift-tab", "edit 文 甲.txt"), ("tab", "edit 文 乙.txt")] {
        editor
            .key(
                &Keystroke::parse(key).unwrap(),
                area,
                &spacing,
                project.path(),
                false,
            )
            .unwrap();
        assert_eq!(editor.vim().command, want);
    }
    assert_eq!(
        editor
            .key(
                &Keystroke::parse("enter").unwrap(),
                area,
                &spacing,
                project.path(),
                false
            )
            .unwrap()
            .edit
            .action,
        Action::Open("文 乙.txt".into())
    );
    editor.edit(|vim, register| vim.key(":", register));
    for (key, want) in [("up", "edit 文 乙.txt"), ("down", "")] {
        editor
            .key(
                &Keystroke::parse(key).unwrap(),
                area,
                &spacing,
                project.path(),
                false,
            )
            .unwrap();
        assert_eq!(editor.vim().command, want);
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
        let mut frame = Frame::viewport(4, 5, 0);
        frame.set_theme_component(ThemeComponent::Gutter);
        for (row, label) in labels.into_iter().enumerate() {
            frame.text(0, row, label, MUTED, 2);
        }
        frame.right_aligned_icon(2, 1, super::view::FOLD_ARROW, 56);
        frame.text(1, 4, "~", MUTED, 1);
        frame
    };

    let (gutter, _) = editor.content_frames(context, |_| None);
    assert!(gutter == expected_gutter([" 2", " 1", " 5", " 1"]));

    for key in ["2", "k"] {
        editor
            .test_key(&Keystroke::parse(key).unwrap(), 4, false)
            .unwrap();
    }
    assert_eq!(editor.vim().doc.cursor, 0);
    let (gutter, _) = editor.content_frames(context, |_| None);
    assert!(gutter == expected_gutter([" 1", " 1", " 2", " 3"]));

    for key in ["2", "j"] {
        editor
            .test_key(&Keystroke::parse(key).unwrap(), 4, false)
            .unwrap();
    }
    assert_eq!(editor.vim().doc.location(editor.vim().doc.cursor).0, 4);
    let (gutter, _) = editor.content_frames(context, |_| None);
    assert!(gutter == expected_gutter([" 2", " 1", " 5", " 1"]));
}

/// Markdown continuations keep whole graphemes and byte-based colors with one line number.
#[test]
fn markdown_source_wraps_retained_text_at_grapheme_boundaries() {
    use crate::{
        grid::{render::Frame, windows::WindowFrame},
        ui::theme::{CURRENT, GOLD, MUTED, TEXT, ThemeComponent},
    };

    let mut doc = Document::new("abcde中\tx\nok");
    doc.path = Some("wrapped.md".into());
    doc.cursor = "abcde".len();
    let mut editor = EditorWindow::new(doc);
    assert!(editor.set_markdown_view_mode(super::MarkdownViewMode::Edit));
    let spacing = TextSpacing::new(Arc::new(NoopTextSystem::new()), FontId(0), 16., 6.4);
    let area = Rect {
        w: 12,
        h: 5,
        ..Rect::default()
    };
    let context = WindowFrame {
        area,
        selected: true,
        modal_open: false,
        controls_hovered: false,
        spacing: &spacing,
    };
    let (numbers, text) = editor.content_frames(context, |byte| (byte == 5).then_some(GOLD));
    let body = editor.text_area(area);
    assert_eq!(body.w, 6);

    let mut expected_numbers = Frame::viewport(4, 4, 0);
    expected_numbers.set_theme_component(ThemeComponent::Gutter);
    expected_numbers.text(0, 0, " 1", GOLD, 2);
    expected_numbers.tint(1, 0, 4, CURRENT);
    expected_numbers.text(0, 2, " 1", MUTED, 2);
    expected_numbers.text(1, 3, "~", MUTED, 1);
    assert!(numbers == expected_numbers);

    let width = body.w + super::TEXT_OVERSCAN;
    let mut expected_text = Frame::viewport(width, 4, 0);
    expected_text.set_theme_component(ThemeComponent::Editor);
    expected_text.keep_whole_graphemes(body.x..body.x + body.w);
    expected_text.viewport_line(0, spacing.cells("abcde"), 0..width, 0, |_| (TEXT, 0, false));
    expected_text.viewport_line(
        1,
        spacing.cells_from("中\tx", 5, 5),
        0..width,
        CURRENT,
        |byte| (if byte == 5 { GOLD } else { TEXT }, CURRENT, false),
    );
    expected_text.viewport_line(2, spacing.cells("ok"), 0..width, 0, |_| (TEXT, 0, false));
    assert!(text == expected_text);
}

/// An ordinary source pane can opt into wrapping, hide digits and measure its own tabs.
#[test]
fn local_display_options_control_source_layout() {
    let mut editor = EditorWindow::new(Document::new("\tabcdefghijk"));
    let spacing = TextSpacing::new(Arc::new(NoopTextSystem::new()), FontId(0), 16., 6.4);
    let area = Rect {
        w: 14,
        h: 5,
        ..Rect::default()
    };
    let original = editor.text_area(area);
    assert!(!editor.visual_rows(original.w, &spacing).is_wrapped());
    editor.edit(|vim, register| {
        vim.text(
            ":setlocal wrap nonumber norelativenumber tabstop=8",
            register,
        );
        vim.key("Enter", register)
    });
    let body = editor.text_area(area);
    assert_eq!(body.w, original.w + 1);
    assert!(editor.visual_rows(body.w, &spacing).is_wrapped());
    editor.edit(|vim, register| vim.key("l", register));
    assert_eq!(
        editor.buffer().caret_span(0, "\tabc", &spacing).start,
        8 * crate::ui::text::UNITS_PER_COLUMN
    );
    assert_eq!(editor.vim().doc.location(1), (0, 8));
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
    let source_area = editor.text_area(context.area);
    let width = source_area.w + super::TEXT_OVERSCAN;
    let mut expected = Frame::viewport(width, 4, 0);
    expected.set_theme_component(ThemeComponent::Editor);
    expected.keep_whole_graphemes(source_area.x..source_area.x + source_area.w);
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
        .key(
            &Keystroke::parse(key).unwrap(),
            area,
            spacing,
            Path::new("."),
            false,
        )
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

/// Buffer switches restore each document's own caret, scroll position and undo state.
#[test]
fn buffer_switches_retain_independent_views() {
    let text = "甲乙\n丙丁\n最终";
    let mut editor = EditorWindow::new(Document::new(text));
    let id = editor.buffer().id;
    editor.jump_to(3);
    editor.buffers[editor.active].scroll.snap(1, 1.);
    let other = editor.retain_document(Document::new(text));
    let other = editor.buffers()[other].id;
    assert!(editor.select_buffer(other, 2.));

    editor.edit(|vim, _| {
        let offset = vim.doc.offset(2, 0);
        vim.doc.replace(offset..vim.doc.text.len(), "新", true);
        nanobug::vim::Action::None
    });
    editor.jump_to(14);
    editor.buffers[editor.active].scroll.snap(2, 2.);
    assert!(editor.select_buffer(id, 3.));
    assert_eq!(editor.buffer().id, id);
    assert_eq!(editor.vim().doc.cursor, 3);
    assert_eq!(editor.buffer().scroll.first_cell(), 1);
    assert_eq!(editor.vim().doc.text, text);
    assert!(editor.select_buffer(other, 4.));
    assert_eq!(editor.vim().doc.cursor, 14);
    assert_eq!(editor.buffer().scroll.first_cell(), 2);
    assert_eq!(editor.vim().doc.text, "甲乙\n丙丁\n新");
    editor.edit(|vim, _| {
        vim.doc.undo(false);
        nanobug::vim::Action::None
    });
    assert_eq!(editor.vim().doc.text, text);
}

/// Removing an inactive file label frees its buffer and leaves the active draft selected.
#[test]
fn closing_an_inactive_buffer_preserves_the_active_draft() {
    let mut editor = EditorWindow::new(Document::new("shared"));
    let shared = editor.buffer().id;
    let index = editor.retain_document(Document::new("draft"));
    let draft = editor.buffers()[index].id;
    assert!(editor.select_buffer(draft, 0.));
    editor.edit(|vim, register| vim.text("i!", register));
    editor.finish_buffer_input();
    assert!(editor.remove_buffer_tab(shared, 0.));
    assert_eq!(editor.buffer().id, draft);
    assert!(editor.buffer_index(shared).is_none());
    assert_eq!(
        editor
            .visible_buffers()
            .map(|buffer| buffer.id)
            .collect::<Vec<_>>(),
        [draft]
    );
    assert_eq!(editor.vim().doc.text, "!draft");
}

/// Keep the alternate label keyed to its buffer when unrelated labels are removed or reindexed.
#[test]
fn alternate_buffer_follows_identity_until_its_label_closes() {
    let mut editor = EditorWindow::new(Document::new("one"));
    let one = editor.buffer().id;
    let two = editor.retain_document(Document::new("two"));
    let two = editor.buffers()[two].id;
    let three = editor.retain_document(Document::new("three"));
    let three = editor.buffers()[three].id;
    assert_eq!(editor.alternate_buffer_id(), None);

    assert!(editor.select_buffer(two, 0.));
    assert!(editor.select_buffer(three, 0.));
    assert_eq!(editor.alternate_buffer_id(), Some(two));
    assert!(editor.remove_buffer_tab(one, 0.));
    assert_eq!(editor.alternate_buffer_id(), Some(two));

    assert!(editor.select_buffer(two, 0.));
    assert_eq!(editor.alternate_buffer_id(), Some(three));
    assert!(editor.remove_buffer_tab(three, 0.));
    assert_eq!(editor.alternate_buffer_id(), None);
    assert_eq!(editor.buffer().id, two);
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
            .key(
                &Keystroke::parse(key).unwrap(),
                area,
                &spacing,
                Path::new("."),
                false,
            )
            .unwrap()
    };
    input(&mut editor, "z");
    let result = input(&mut editor, "shift-l");
    assert_eq!(result.view_left, Some(6));
    editor.track_view(
        area,
        true,
        &spacing,
        [ScrollInput::Reveal, ScrollInput::PositionWithTail(6)],
        1.,
    );
    assert_eq!(editor.buffer().columns.target(), 6);

    input(&mut editor, "z");
    input(&mut editor, "2");
    input(&mut editor, "enter");
    assert_eq!(editor.text_area(area).h, 2);
    editor.track_view(area, true, &spacing, [ScrollInput::Reveal; 2], 2.);
    assert_eq!(editor.buffer().columns.target(), 6);
    assert!(editor.body_caret(area, &spacing).4);
}
