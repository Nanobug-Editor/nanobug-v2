//! Retained-grid regressions for measured text, clipping and row-cache reuse.

use super::*;
use crate::ui::{
    text::TextSpacing,
    theme::{ACCENT, CURRENT, GOLD, GRID_FONT_SIZE, SELECTION, TEXT},
};
use std::sync::Arc;

/// Keyboard SVG ink is sized in logical pixels and centered at common Windows scale factors.
#[test]
fn keyboard_icons_use_visible_path_heights() {
    use crate::ui::icons::KeyIcon;
    use gpui::{ObjectFit, SvgRenderer};

    let metrics = Metrics {
        cw: 9.,
        lh: 21.,
        font_size: GRID_FONT_SIZE,
        origin: point(px(0.), px(0.)),
    };
    let mut frame = Frame::new(16, 1);
    let icon = crate::ui::icons::Icon::new(b"", 1.8);
    frame.icon(0, 0, icon);
    let default = frame.icons[0].bounds(metrics.origin, metrics);
    assert_eq!(default.size.width, px(GRID_FONT_SIZE));
    assert_eq!(default.size.height, px(GRID_FONT_SIZE));

    let svg = SvgRenderer::new(Arc::new(()));
    for (key, target_height) in [
        (KeyIcon::ESC, 9.),
        (KeyIcon::BACKSPACE, 12.),
        (KeyIcon::ENTER, 12.),
    ] {
        frame.key_icon(2, 0, key, 0.);
        let bounds = frame.icons.last().unwrap().bounds(metrics.origin, metrics);
        for scale in [1., 1.25, 1.5] {
            let edge =
                (f32::from(bounds.size.width.max(bounds.size.height)) * scale).round() as i32;
            let raster = key.image.rasterize(&svg, edge).unwrap();
            let size = raster.size(0);
            let mut ink_rows = raster
                .as_bytes(0)
                .unwrap()
                .chunks_exact(size.width.0 as usize * 4)
                .enumerate()
                .filter_map(|(index, row)| {
                    let alpha = row.iter().skip(3).step_by(4).copied().max()?;
                    (alpha > 0).then_some((index as f32, f32::from(alpha) / 255.))
                });
            // Account for the partially covered rows at the antialiased edges.
            let (first, first_coverage) = ink_rows.next().unwrap();
            let (last, last_coverage) = ink_rows.next_back().unwrap();
            let top = first + 1. - first_coverage;
            let bottom = last + last_coverage;
            let fitted = ObjectFit::Contain.get_bounds(bounds, size);
            let pixel = f32::from(fitted.size.height) / size.height.0 as f32;
            let height = (bottom - top) * pixel;
            let center = f32::from(fitted.origin.y) + (top + bottom) * pixel / 2.;
            assert!(
                (height - target_height).abs() * scale <= 1.,
                "expected {target_height}px at scale {scale}, got {height}px (raster rows {top}..{bottom})",
            );
            assert!(
                (center - metrics.lh / 2.).abs() * scale <= 1.,
                "ink center {center}px at scale {scale}",
            );
        }
    }
}

/// A cropped row uses the measured advances for original-size glyphs and selection.
#[test]
fn measured_chinese_keeps_glyph_and_selection_positions_when_panned() {
    let glyphs = Arc::new(gpui::NoopTextSystem::new());
    let text_system = WindowTextSystem::new(Arc::new(gpui::TextSystem::new(glyphs.clone())));
    let spacing = crate::ui::text::TextSpacing::new(glyphs.clone(), FontId(0), GRID_FONT_SIZE, 6.4);
    let panel = Panel::new(Metrics {
        cw: 6.4,
        lh: 21.,
        font_size: GRID_FONT_SIZE,
        origin: point(px(0.), px(0.)),
    });
    let mut frame = Frame::new(8, 1);
    frame.viewport_line(0, spacing.cells("A中文汉，B"), 2..10, 0, |byte| {
        (TEXT, if byte == "A中".len() { SELECTION } else { 0 })
    });
    assert_eq!(
        frame.rows[0].column_at(UNITS_PER_COLUMN as isize / 2),
        Some(2)
    );

    let mut data = panel.data.borrow_mut();
    data.set_frame(frame);
    let row = data.prepare_row(0, &text_system).unwrap();
    let positions: Vec<_> = row
        .runs
        .iter()
        .flat_map(|run| {
            assert_eq!(run.line.font_size, px(GRID_FONT_SIZE));
            run.line
                .runs
                .iter()
                .flat_map(|shaped| &shaped.glyphs)
                .filter(|glyph| !run.line.text[glyph.index..].starts_with(' '))
                .map(move |glyph| run.x + run.position(glyph, run.cluster(glyph.index)).x)
        })
        .collect();
    let expected = [-6.4, 3.2, 12.8, 22.4, 32.];
    assert_eq!(positions.len(), expected.len());
    for (x, expected) in positions.into_iter().zip(expected) {
        assert!((f32::from(x) - expected).abs() < 0.001);
    }
    let [(x, width, SELECTION)] = row.backgrounds.as_slice() else {
        panic!("expected the middle Han character to be selected");
    };
    assert!((f32::from(*x) - 3.2).abs() < 0.001);
    assert!((f32::from(*width) - 9.6).abs() < 0.001);
    let first = data
        .glyph_width(0, 0, &text_system, glyphs.as_ref())
        .unwrap();
    let second = data
        .glyph_width(0, 2, &text_system, glyphs.as_ref())
        .unwrap();
    assert!((first - second).abs() < 0.01);
}

/// Selection recolors names without changing the glyph coordinates used to paint them.
#[test]
fn tree_names_keep_glyph_positions_when_selected() {
    let glyphs = Arc::new(gpui::NoopTextSystem::new());
    let text_system = WindowTextSystem::new(Arc::new(gpui::TextSystem::new(glyphs.clone())));
    let metrics = Metrics {
        cw: 10.,
        lh: 21.,
        font_size: 16.,
        origin: point(px(0.), px(0.)),
    };
    let spacing = TextSpacing::new(glyphs, FontId(0), metrics.font_size, metrics.cw);
    let name = "目录example.txt";
    let positions = |color, marked| {
        let mut frame = Frame::new(24, 1);
        if marked {
            frame.text(0, 0, "●", GOLD, 1);
        }
        frame.label(0, 5..23, name, color, &spacing);
        let mut data = PanelData {
            frame,
            metrics,
            cache: vec![None],
            waves: Vec::new(),
        };
        data.prepare_row(0, &text_system)
            .unwrap()
            .label_runs
            .iter()
            .flat_map(|run| {
                assert_eq!(run.color, color);
                run.line
                    .runs
                    .iter()
                    .flat_map(|shaped| &shaped.glyphs)
                    .map(move |glyph| run.x + run.position(glyph, run.cluster(glyph.index)).x)
            })
            .collect::<Vec<_>>()
    };
    for marked in [false, true] {
        assert_eq!(positions(TEXT, marked), positions(ACCENT, marked));
    }
}

/// Chinese labels fit by measured width while border and right-aligned fields stay anchored.
#[test]
fn shell_labels_use_measured_widths_with_fixed_anchors() {
    let glyphs = Arc::new(gpui::NoopTextSystem::new());
    let text_system = WindowTextSystem::new(Arc::new(gpui::TextSystem::new(glyphs.clone())));
    let metrics = Metrics {
        cw: 6.4,
        lh: 21.,
        font_size: GRID_FONT_SIZE,
        origin: point(px(0.), px(0.)),
    };
    let spacing = TextSpacing::new(glyphs, FontId(0), metrics.font_size, metrics.cw);
    let mut frame = Frame::new(25, 3);
    frame.border("中文.txt", true, true, &spacing);
    assert!(
        frame.rows[0].cells[2..11]
            .iter()
            .all(|cell| cell.text == " ")
    );
    assert_eq!(frame.rows[0].cells[11].text, "─");
    assert_eq!(frame.rows[0].cells[16].text, "─");
    assert_eq!(frame.rows[0].cells[18].text, "?");
    assert!(
        frame.rows[0].cells[20..23]
            .iter()
            .all(|cell| cell.text == " ")
    );
    assert_eq!(frame.rows[0].cells[23].text, "─");
    assert_eq!(frame.label(1, 1..4, "中文X", TEXT, &spacing), 4);
    assert_eq!(frame.right_label(1, 24, "余 UTF-8", GOLD, &spacing), 16);
    assert_eq!(
        frame.labels[1][1].x + frame.labels[1][1].width(),
        24 * UNITS_PER_COLUMN
    );
    let mut data = PanelData {
        frame,
        metrics,
        cache: vec![None, None, None],
        waves: Vec::new(),
    };
    let runs = &data.prepare_row(1, &text_system).unwrap().label_runs;
    assert_eq!(runs[0].line.text.as_str(), "中文");
    let glyphs = &runs[0].line.runs[0].glyphs;
    let second = runs[0].x + runs[0].position(&glyphs[1], 1).x;
    assert!((f32::from(second) - 16.).abs() < 0.001);
    assert!((f32::from(runs[1].x) - 16.5 * metrics.cw).abs() < 0.001);
}

/// One-pixel tree strokes meet at row and elbow joins even during fractional scrolling.
#[test]
fn tree_guides_snap_to_single_device_pixels() {
    let origin = point(px(14.2), px(27.35));
    let metrics = Metrics {
        cw: 10.,
        lh: 21.,
        font_size: 16.,
        origin,
    };
    let mut frame = Frame::new(8, 2);
    frame.tree_guide(1, 0, true, None);
    frame.tree_guide(1, 1, false, Some(2));

    for scale in [1., 1.25, 1.5, 2.] {
        let trunk = frame.guides[0].bounds(origin, metrics, scale);
        let end = frame.guides[1].bounds(origin, metrics, scale);
        let arm = frame.guides[2].bounds(origin, metrics, scale);
        let device = |value: Pixels| f32::from(value) * scale;
        assert!((device(trunk.size.width) - 1.).abs() < 0.001);
        assert!((device(arm.size.height) - 1.).abs() < 0.001);
        assert!(device(trunk.bottom() - end.top()).abs() < 0.001);
        assert!(device(end.bottom() - arm.top()).abs() < 0.001);
        assert_eq!(trunk.left(), arm.left());
    }
}

/// Scrolling one row should only prepare the newly exposed row and any changed content.
#[test]
fn scrolled_panel_keeps_prepared_rows_that_stay_visible() {
    let height = 40;
    let make_frame = |first| {
        let mut frame = Frame::viewport(24, height, first);
        for row in 0..height {
            frame.text(0, row, &format!("Line {}", first + row), TEXT, 24);
        }
        frame
    };
    let glyphs = Arc::new(gpui::NoopTextSystem::new());
    let text_system = WindowTextSystem::new(Arc::new(gpui::TextSystem::new(glyphs)));
    let panel = Panel::new(Metrics {
        cw: 8.,
        lh: 20.,
        font_size: 16.,
        origin: point(px(0.), px(0.)),
    });
    let mut data = panel.data.borrow_mut();
    data.set_frame(make_frame(0));
    for row in 0..height {
        data.prepare_row(row, &text_system).unwrap();
    }

    let mut next = make_frame(1);
    next.tint(3, 0, 24, CURRENT);
    data.set_frame(next);
    assert_eq!(data.cache.iter().flatten().count(), height - 2);
    assert!(data.cache[3].is_none());
    assert!(data.cache[height - 1].is_none());
    assert!(
        data.cache[0].as_ref().unwrap().runs[0]
            .line
            .text
            .starts_with("Line 1 ")
    );

    for row in 0..height {
        data.prepare_row(row, &text_system).unwrap();
    }
    data.set_frame(make_frame(0));
    assert_eq!(data.cache.iter().flatten().count(), height - 2);
    assert!(data.cache[0].is_none());
    assert!(data.cache[4].is_none());
}

/// Wide cells use the cached glyph's ink extent, including later glyphs in the same run.
#[test]
fn wide_cursor_uses_ink_without_counting_grid_padding() {
    let glyphs = Arc::new(gpui::NoopTextSystem::new());
    let text_system = WindowTextSystem::new(Arc::new(gpui::TextSystem::new(glyphs.clone())));
    let metrics = Metrics {
        cw: 8.,
        lh: 20.,
        font_size: 16.,
        origin: point(px(0.), px(0.)),
    };
    let mut frame = Frame::new(7, 1);
    frame.text(0, 0, "a定位\u{3000}", TEXT, 7);
    let mut data = PanelData {
        frame,
        metrics,
        cache: vec![None],
        waves: Vec::new(),
    };
    for column in [1, 3] {
        let ink = data
            .glyph_width(0, column, &text_system, glyphs.as_ref())
            .unwrap();
        assert!(
            (ink - 7.136).abs() < 0.01,
            "unexpected width {ink} at column {column}"
        );
    }
    assert_eq!(data.glyph_width(0, 0, &text_system, glyphs.as_ref()), None);
    assert_eq!(data.glyph_width(0, 5, &text_system, glyphs.as_ref()), None);
}
