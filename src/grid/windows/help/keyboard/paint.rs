//! Pixel layout and painting for the editor help keyboard.

use super::{Cap, Keyboard, Modifier, available, description_area};
use crate::{
    grid::render::{Metrics, Rect, paint_dashed_outline},
    ui::theme::{ACCENT, GRID_TEXT_FONT, MUTED, SELECTION, TEXT, style},
};
use gpui::{
    AnyElement, App, Bounds, IntoElement, PathBuilder, Pixels, Styled, TextAlign, TextRun, Window,
    canvas, fill, point, px, rgb, rgba, size,
};

/// Six rows of the editor help keyboard, with widths in key units.
const KEY_ROWS: [&[(Cap, f32)]; 6] = [
    &[
        named("Escape", "Esc", 1.),
        named("F1", "F1", 1.),
        named("F2", "F2", 1.),
        named("F3", "F3", 1.),
        named("F4", "F4", 1.),
        named("F5", "F5", 1.),
        named("F6", "F6", 1.),
        named("F7", "F7", 1.),
        named("F8", "F8", 1.),
        named("F9", "F9", 1.),
        named("F10", "F10", 1.),
        named("F11", "F11", 1.),
        named("F12", "F12", 1.),
        named("PrintScreen", "PrtSc", 1.),
        named("Insert", "Ins", 1.),
        named("Delete", "Del", 1.),
    ],
    &[
        character('`', '~'),
        character('1', '!'),
        character('2', '@'),
        character('3', '#'),
        character('4', '$'),
        character('5', '%'),
        character('6', '^'),
        character('7', '&'),
        character('8', '*'),
        character('9', '('),
        character('0', ')'),
        character('-', '_'),
        character('=', '+'),
        named("Backspace", "Backspace", 2.),
        named("Home", "Home", 1.),
    ],
    &[
        named("Tab", "Tab", 1.5),
        character('q', 'Q'),
        character('w', 'W'),
        character('e', 'E'),
        character('r', 'R'),
        character('t', 'T'),
        character('y', 'Y'),
        character('u', 'U'),
        character('i', 'I'),
        character('o', 'O'),
        character('p', 'P'),
        character('[', '{'),
        character(']', '}'),
        (
            Cap::Character {
                base: '\\',
                shifted: '|',
            },
            1.5,
        ),
        named("PageUp", "PgUp", 1.),
    ],
    &[
        named("CapsLock", "Caps", 1.75),
        character('a', 'A'),
        character('s', 'S'),
        character('d', 'D'),
        character('f', 'F'),
        character('g', 'G'),
        character('h', 'H'),
        character('j', 'J'),
        character('k', 'K'),
        character('l', 'L'),
        character(';', ':'),
        character('\'', '"'),
        named("Enter", "Enter", 2.25),
        named("PageDown", "PgDn", 1.),
    ],
    &[
        (Cap::Modifier(Modifier::Shift), 2.25),
        character('z', 'Z'),
        character('x', 'X'),
        character('c', 'C'),
        character('v', 'V'),
        character('b', 'B'),
        character('n', 'N'),
        character('m', 'M'),
        character(',', '<'),
        character('.', '>'),
        character('/', '?'),
        (Cap::Modifier(Modifier::Shift), 1.75),
        named("Up", "↑", 1.),
        named("End", "End", 1.),
    ],
    &[
        (Cap::Modifier(Modifier::Ctrl), 1.875),
        (Cap::Modifier(Modifier::Alt), 1.875),
        named("Space", "Space", 6.),
        (Cap::Modifier(Modifier::Alt), 1.5),
        (Cap::Modifier(Modifier::Ctrl), 1.75),
        named("Left", "←", 1.),
        named("Down", "↓", 1.),
        named("Right", "→", 1.),
    ],
];

/// Width of the widest row, in key units.
const KEY_COLUMNS: f32 = 16.;

/// Put the printable characters `base` and `shifted` on a square cap.
const fn character(base: char, shifted: char) -> (Cap, f32) {
    (Cap::Character { base, shifted }, 1.)
}

/// Put the keyboard legend `label` on a cap for command `name` occupying `width` key units.
const fn named(name: &'static str, label: &'static str, width: f32) -> (Cap, f32) {
    (Cap::Named { name, label }, width)
}

/// Iterate over the same keys used by the painted layout for native inspection.
pub(super) fn caps() -> impl Iterator<Item = Cap> {
    KEY_ROWS.into_iter().flatten().map(|&(cap, _)| cap)
}

/// Window-local bounds shared by painting and hit testing.
pub(super) struct Layout {
    /// Key bounds in display order, including the modifier caps.
    pub(super) caps: Vec<(Cap, Bounds<Pixels>)>,
    /// Dashed outline around the shortcut description and example.
    description: Option<Bounds<Pixels>>,
}

impl Layout {
    /// Fit the six keyboard rows above the example in `body`.
    pub(super) fn new(body: Rect, metrics: Metrics) -> Self {
        let bounds = body.pixels(metrics);
        let description = description_area(body).map(|area| {
            Bounds::new(
                metrics.at((body.x + area.x) as f32, (body.y + area.y) as f32 + 0.5),
                size(
                    px(area.w as f32 * metrics.cw),
                    px((area.h - 1) as f32 * metrics.lh),
                ),
            )
        });
        let height =
            f32::from(description.map_or(bounds.bottom(), |info| info.top()) - bounds.top());
        let width = f32::from(bounds.size.width);
        let available_width = width - (3. * metrics.cw).min(width * 0.1);
        let available_height = height - (1.8 * metrics.lh).min(height * 0.14);
        let column_gap = (0.66 * metrics.cw).min(available_width * 0.025);
        let min_row_gap = (0.55 * metrics.lh).min(height * 0.035);
        let rows = KEY_ROWS.len() as f32;
        let side = (1.92 * metrics.lh)
            .min((available_width + column_gap) / KEY_COLUMNS - column_gap)
            .min((available_height - (rows - 1.) * min_row_gap) / rows);
        let mut caps = Vec::new();
        if side < 2. {
            return Self { caps, description };
        }
        let row_gap = (0.55 * metrics.lh).min((available_height - rows * side) / (rows - 1.));
        let pitch = side + column_gap;
        let left = (width - (KEY_COLUMNS * pitch - column_gap)) / 2.;
        let top = (height - (rows * side + (rows - 1.) * row_gap)) / 2.;
        let cap_bounds = |row, x, width| {
            Bounds::new(
                bounds.origin + point(px(left + x), px(top + row as f32 * (side + row_gap))),
                size(px(width), px(side)),
            )
        };
        for (row, keys) in KEY_ROWS.into_iter().enumerate() {
            let mut column = 0.;
            for &(cap, width) in keys {
                let cap_width = side + (width - 1.) * pitch;
                caps.push((cap, cap_bounds(row, column * pitch, cap_width)));
                column += width;
            }
        }
        Self { caps, description }
    }
}

/// Whether to highlight a key or dim it because it has no example in this view.
#[derive(Clone, Copy, PartialEq, Eq)]
enum KeyState {
    /// Available for inspection.
    Normal,
    /// Currently selected example or modifier.
    Selected,
    /// No example for the current view.
    Disabled,
    /// Reserved Space key, marked with diagonal strokes.
    DisabledHatched,
}

impl Keyboard {
    /// Paint the keycaps and example outline in the same pixel space as the retained text.
    pub(in crate::grid::windows::help) fn element(
        &self,
        body: Rect,
        metrics: Metrics,
    ) -> AnyElement {
        let layout = Layout::new(body, metrics);
        let modifier = self.modifier();
        let selected_cap = self.shows_selection().then_some(self.selection.cap);
        canvas(
            |_, _, _| (),
            move |_, _, window, cx| {
                for (index, (cap, bounds)) in layout.caps.into_iter().enumerate() {
                    let selected = match cap {
                        Cap::Modifier(value) => modifier.is_some_and(|view| view.contains(value)),
                        _ => selected_cap == Some(cap),
                    };
                    let state = if !available(cap, modifier) {
                        if matches!(cap, Cap::Named { name: "Space", .. }) {
                            KeyState::DisabledHatched
                        } else {
                            KeyState::Disabled
                        }
                    } else if selected {
                        KeyState::Selected
                    } else {
                        KeyState::Normal
                    };
                    paint_cap(
                        bounds,
                        cap.label(modifier),
                        state,
                        metrics.font_size,
                        index,
                        window,
                        cx,
                    );
                }
                if let Some(bounds) = layout.description {
                    paint_dashed_outline(bounds, None, window, cx);
                }
            },
        )
        .absolute()
        .left_0()
        .top_0()
        .size_full()
        .into_any_element()
    }
}

/// Paint a centered key label over two subtly different hand-drawn strokes.
fn paint_cap(
    bounds: Bounds<Pixels>,
    label: String,
    state: KeyState,
    font_size: f32,
    index: usize,
    window: &mut Window,
    cx: &mut App,
) {
    let height = bounds.size.height;
    let radius = px(6.5).min(height * 0.22);
    let stroke = px(1.2).min(height * 0.1);
    let bend = px(1.15 + (index % 3) as f32 * 0.2).min(height * 0.1);
    let direction = if index.is_multiple_of(2) { 1. } else { -1. };
    let selected = state == KeyState::Selected;
    let opacity = if matches!(state, KeyState::Disabled | KeyState::DisabledHatched) {
        0.42
    } else {
        1.
    };
    let appearance = style(cx);
    let edge = rgb(appearance.foreground(if selected { ACCENT } else { MUTED })).opacity(opacity);
    let face = bounds.inset(stroke + bend * 0.5);
    if selected {
        window.paint_quad(fill(face, rgba(appearance.tint(SELECTION))).corner_radii(radius));
    }
    if state == KeyState::DisabledHatched {
        paint_hatch(
            face.inset(radius * 0.45),
            appearance.foreground(MUTED),
            window,
        );
    }
    let faint = Bounds::new(
        face.origin + point(bend * (0.75 * direction), bend * 0.58),
        face.size,
    );
    for (outline, radius, bend, stroke, color) in [
        (
            faint,
            radius * 0.83,
            -bend * direction,
            stroke * 0.7,
            edge.opacity(0.45),
        ),
        (face, radius, bend * direction, stroke, edge),
    ] {
        let path = key_outline(outline, radius, bend, stroke);
        if let Ok(path) = path.build() {
            window.paint_path(path, color);
        }
    }

    let label_width = (bounds.size.width - 4. * stroke).max(px(1.));
    let font_size = px(font_size)
        .min(height * 0.54)
        .min(label_width / (label.chars().count().max(1) as f32 * 0.65));
    let color = rgb(appearance.foreground(if selected { ACCENT } else { TEXT })).opacity(opacity);
    let runs = [TextRun {
        len: label.len(),
        font: GRID_TEXT_FONT.clone(),
        color: color.into(),
        ..Default::default()
    }];
    let system = window.text_system();
    let line = system.shape_line(label.into(), font_size, &runs, None);
    // Center the ink, including punctuation whose bounds sit above or below the baseline.
    let ink = line
        .runs
        .iter()
        .flat_map(|run| {
            run.glyphs.iter().filter_map(|glyph| {
                let character = line.text.get(glyph.index..)?.chars().next()?;
                let ink = system
                    .typographic_bounds(run.font_id, font_size, character)
                    .ok()?;
                (ink.size.height > px(0.) && ink.size.height <= font_size * 1.5).then(|| {
                    Bounds::new(
                        glyph.position + point(ink.origin.x, -ink.bottom()),
                        ink.size,
                    )
                })
            })
        })
        .reduce(|ink, glyph| ink.union(&glyph));
    let offset = ink.map_or(point(line.width() * 0.5, height * 0.5), |ink| {
        ink.center() + point(px(0.), (height + line.ascent - line.descent) * 0.5)
    });
    let _ = line.paint(
        bounds.center() - offset,
        height,
        TextAlign::Left,
        None,
        window,
        cx,
    );
}

/// Paint even diagonal strokes in RGB `ink` within a disabled key's `face` in `window`.
fn paint_hatch(face: Bounds<Pixels>, ink: u32, window: &mut Window) {
    let width = f32::from(face.size.width);
    let height = f32::from(face.size.height);
    let spacing = 7.;
    let mut hatch = PathBuilder::stroke(px(1.).min(face.size.height * 0.1));
    for stripe in 1..((width + height) / spacing).ceil() as usize {
        let x = stripe as f32 * spacing;
        let start = (x - height).max(0.);
        let end = x.min(width);
        hatch.move_to(face.origin + point(px(start), px(x - start)));
        hatch.line_to(face.origin + point(px(end), px(x - end)));
    }
    if let Ok(path) = hatch.build() {
        window.paint_path(path, rgb(ink).opacity(0.25));
    }
}

/// Trace each side with a small bend, joined by rounded corners.
fn key_outline(
    bounds: Bounds<Pixels>,
    radius: Pixels,
    bend: Pixels,
    stroke: Pixels,
) -> PathBuilder {
    let (left, right, top, bottom) = (bounds.left(), bounds.right(), bounds.top(), bounds.bottom());
    let starts = [
        point(left + radius, top),
        point(right, top + radius),
        point(right - radius, bottom),
        point(left, bottom - radius),
    ];
    let ends = [
        point(right - radius, top),
        point(right, bottom - radius),
        point(left + radius, bottom),
        point(left, top + radius),
    ];
    let corners = [
        point(right, top),
        point(right, bottom),
        point(left, bottom),
        point(left, top),
    ];
    let offsets = [
        point(px(0.), bend),
        point(-bend, px(0.)),
        point(px(0.), -bend),
        point(bend, px(0.)),
    ];
    let mut path = PathBuilder::stroke(stroke);
    path.move_to(starts[0]);
    for side in 0..4 {
        let start = starts[side];
        let end = ends[side];
        let third = (end - start) / 3.;
        let offset = offsets[side];
        path.cubic_bezier_to(end, start + third + offset, end - third - offset * 0.45);
        path.curve_to(starts[(side + 1) % 4], corners[side]);
    }
    path.close();
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    /// All rows fill the same width and keep the right-side navigation column aligned.
    #[test]
    fn keycaps_fit_above_the_example_in_compact_and_full_popups() {
        assert_eq!(KEY_ROWS.map(|row| row.len()), [16, 15, 15, 14, 14, 8]);
        for row in KEY_ROWS {
            assert_eq!(row.iter().map(|(_, width)| width).sum::<f32>(), KEY_COLUMNS);
        }
        let metrics = Metrics {
            cw: 10.,
            lh: 22.,
            font_size: 16.,
            origin: point(px(14.), px(42.)),
        };
        for (w, h) in [(48, 12), (68, 14), (68, 17), (82, 26)] {
            let body = Rect { x: 3, y: 5, w, h };
            let layout = Layout::new(body, metrics);
            let viewport = body.pixels(metrics);
            let info = layout.description.unwrap();
            let caret = Keyboard::caret(body, metrics).unwrap();
            let caret_bounds = Bounds::new(caret.origin, size(px(metrics.cw), px(metrics.lh)));
            assert_eq!(layout.caps.len(), 82);
            let right = layout.caps.last().unwrap().1.right();
            for index in [15, 30, 45, 59, 73] {
                assert_eq!(layout.caps[index].1.right(), right);
            }
            assert!(viewport.contains(&info.bottom_right()));
            assert!(info.contains(&caret.origin) && info.contains(&caret_bounds.bottom_right()));
            assert_eq!(caret.origin.x - info.origin.x, px(2. * metrics.cw));
            assert_eq!(caret.origin.y - info.origin.y, px(0.5 * metrics.lh));
            for (index, (cap, bounds)) in layout.caps.iter().enumerate() {
                assert!(
                    viewport.contains(&bounds.origin) && viewport.contains(&bounds.bottom_right())
                );
                assert!(bounds.bottom() < info.top());
                match cap {
                    Cap::Character { base: '\\', .. } => {
                        assert!(bounds.size.width > bounds.size.height)
                    }
                    Cap::Character { .. } => assert_eq!(bounds.size.width, bounds.size.height),
                    Cap::Modifier(_) => assert!(bounds.size.width > bounds.size.height),
                    Cap::Named { .. } => {}
                }
                assert!(
                    layout.caps[..index]
                        .iter()
                        .all(|(_, other)| !other.intersects(bounds))
                );
            }
        }
    }
}
