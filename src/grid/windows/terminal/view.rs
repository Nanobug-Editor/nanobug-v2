//! Project the PTY grid into the editor's retained frames and smooth scrolling model.

use super::{Events, Size, Terminal};
use crate::{
    grid::render::{Frame, Rect},
    ui::theme::{SELECTION, TEXT, ThemeComponent},
};
use alacritty_terminal::{
    grid::Dimensions,
    index::{Column, Line, Point, Side},
    term::{Term, TermMode, cell::Flags, color::Colors},
    vte::ansi::{Color, CursorShape, NamedColor, Rgb},
};
use nanobug::animation::scroll::ScrollInput;

/// Base ANSI palette for a terminal whose process has not set a custom palette.
const ANSI: [u32; 16] = [
    0x1d1f21ff, 0xcc6666ff, 0xb5bd68ff, 0xf0c674ff, 0x81a2beff, 0xb294bbff, 0x8abeb7ff, 0xc5c8c6ff,
    0x666666ff, 0xd54e53ff, 0xb9ca4aff, 0xe7c547ff, 0x7aa6daff, 0xc397d8ff, 0x70c0b1ff, 0xeaeaeaff,
];

impl Terminal {
    /// Track this screen `body` and animate its viewport using `input` at `now`.
    /// Return the retained-frame invalidation bit and fractional caret displacement.
    pub(in crate::grid) fn track_view(
        &mut self,
        body: Rect,
        input: ScrollInput,
        now: f64,
    ) -> (bool, f32, f32) {
        if body.w == 0 || body.h == 0 {
            return (false, 0., 0.);
        }
        let size = Size::view(body.w, body.h);
        let resized = size != self.viewport;
        self.viewport = size;
        let session = self.session.clone();
        let term = session.screen.lock();
        self.sync_review(&term);
        let previous = self.scroll.first_cell();
        let previous_left = self.left;
        let height = self.view_rows(&term);
        if let ScrollInput::Wheel(delta) = input {
            self.set_input(false);
            self.scroll_review(&term, delta);
        }
        let target = self.target_top(&term);
        let point = cursor_cell(&term, self.review_point(&term));
        let cells = if term.grid()[point].flags.contains(Flags::WIDE_CHAR) {
            2
        } else {
            1
        };
        self.follow_column(point.column.0, cells, term.columns());
        let delta = if self.accepts_text() {
            let delta = target as f32 - self.scroll.position();
            self.scroll.snap(target, now);
            delta
        } else {
            self.scroll
                .track_rows(
                    ScrollInput::Position(target),
                    None,
                    height,
                    term.total_lines(),
                    now,
                )
                .1
        };
        (
            resized || self.scroll.first_cell() != previous || self.left != previous_left,
            delta,
            0.,
        )
    }

    /// Number of terminal rows that fit in this view of `term`.
    pub(super) fn view_rows(&self, term: &Term<Events>) -> usize {
        self.viewport.screen_lines().min(term.screen_lines())
    }

    /// Top row measured from the start of scrollback in `term`.
    pub(super) fn target_top(&self, term: &Term<Events>) -> usize {
        let maximum = term.total_lines().saturating_sub(self.view_rows(term));
        self.top
            .unwrap_or_else(|| {
                term.history_size()
                    .saturating_add_signed(term.grid().cursor.point.line.0 as isize)
            })
            .min(maximum)
    }

    /// Refresh review coordinates when `term` changes grid, width, or scrollback depth.
    pub(super) fn sync_review(&mut self, term: &Term<Events>) {
        let alternate = term.mode().contains(TermMode::ALT_SCREEN);
        let history = term.history_size();
        if alternate != self.alternate_screen {
            self.review = None;
            self.top = None;
            self.selection = None;
        } else if term.columns() != self.screen_columns {
            self.selection = None;
        } else if history != self.history {
            let delta = history as i32 - self.history as i32;
            self.selection = self.selection.take().and_then(|selection| {
                selection.rotate(term, &(Line(0)..term.bottommost_line() + 1), delta)
            });
        }
        self.alternate_screen = alternate;
        self.history = history;
        self.screen_columns = term.columns();
    }

    /// Caret in `term` grid coordinates, clamped after history changes or a resize.
    pub(super) fn review_point(&self, term: &Term<Events>) -> Point {
        let mut point = if self.accepts_text() {
            term.grid().cursor.point
        } else {
            self.review.map_or(term.grid().cursor.point, |point| {
                Point::new(term.topmost_line() + point.line.0, point.column)
            })
        };
        let top = term.topmost_line() + self.target_top(term) as i32;
        let bottom = top + self.view_rows(term).saturating_sub(1) as i32;
        point.line = point.line.clamp(top, bottom);
        point.column = point.column.min(term.last_column());
        point
    }

    /// Reveal a review `point` in this view and include it in a local active selection.
    pub(super) fn goto_review(&mut self, term: &Term<Events>, point: Point) {
        let line = (point.line.0 - term.topmost_line().0) as usize;
        let height = self.view_rows(term);
        let top = self.target_top(term);
        let new_top = top.clamp(line.saturating_sub(height - 1), line);
        if top != new_top {
            self.top = Some(new_top);
        }
        self.review = Some(Point::new(Line(line as i32), point.column));
        if let Some(selection) = &mut self.selection {
            selection.update(point, Side::Left);
            selection.include_all();
        }
    }

    /// Move the review viewport in `term` by a signed number of rows `delta`.
    pub(super) fn scroll_review(&mut self, term: &Term<Events>, delta: isize) {
        let point = self.review_point(term);
        let maximum = term.total_lines().saturating_sub(self.view_rows(term));
        self.top = Some(
            self.target_top(term)
                .saturating_add_signed(delta)
                .min(maximum),
        );
        let top = term.topmost_line() + self.target_top(term) as i32;
        let bottom = top + self.view_rows(term).saturating_sub(1) as i32;
        self.goto_review(
            term,
            Point::new(point.line.clamp(top, bottom), point.column),
        );
    }

    /// Keep the cursor at `column` within a view narrower than the PTY's `columns`.
    /// The character at the cursor spans `cells` columns.
    fn follow_column(&mut self, column: usize, cells: usize, columns: usize) {
        let width = self.viewport.columns();
        self.left = self.left.min(columns.saturating_sub(width));
        let first = column
            .saturating_add(cells.min(width))
            .saturating_sub(width);
        self.left = self.left.clamp(first, column);
    }

    /// Produce PTY cells and an overscan line so fractional offsets remain clipped to `body`.
    pub(in crate::grid) fn frame(&self, body: Rect, composition: &str) -> Frame {
        let first = self.scroll.first_cell();
        let mut frame = Frame::viewport(body.w, body.h + 1, first);
        frame.set_theme_component(ThemeComponent::Editor);
        let term = self.session.screen.lock();
        let render = term.renderable_content();
        let left = self.left.min(term.columns());
        let selection = self
            .selection
            .as_ref()
            .and_then(|selection| selection.to_range(&term));
        let top = term.topmost_line() + first as i32;
        let mut value = String::new();
        for (row, line) in (top.0..=term.bottommost_line().0)
            .take(body.h + 1)
            .enumerate()
        {
            for column in 0..body.w.min(term.columns() - left) {
                let point = Point::new(Line(line), Column(left + column));
                let cell = &term.grid()[point];
                if cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    continue;
                }
                let width = if cell.flags.contains(Flags::WIDE_CHAR) {
                    2
                } else {
                    1
                };
                let mut fg = color(cell.fg, render.colors, TEXT);
                let mut bg = color(cell.bg, render.colors, 0);
                if cell.flags.contains(Flags::INVERSE) {
                    (fg, bg) = (if bg == 0 { ANSI[0] } else { bg }, fg);
                }
                if selection.is_some_and(|span| span.contains(point)) {
                    bg = SELECTION;
                }
                if bg != 0 {
                    frame.tint(row, column, column + width, bg);
                }
                if cell.c != ' ' && !cell.flags.contains(Flags::HIDDEN) {
                    value.clear();
                    value.push(cell.c);
                    if let Some(combining) = cell.zerowidth() {
                        value.extend(combining);
                    }
                    frame.text(column, row, &value, fg, width);
                }
            }
        }
        if !composition.is_empty() && self.accepts_text() {
            let cursor = render.cursor.point;
            let row = cursor.line.0 - top.0;
            if row >= 0
                && (row as usize) < body.h
                && let Some(column) = cursor
                    .column
                    .0
                    .checked_sub(left)
                    .filter(|&column| column < body.w)
            {
                frame.text(
                    column,
                    row as usize,
                    composition,
                    crate::ui::theme::PREEDIT,
                    body.w - column,
                );
            }
        }
        frame
    }

    /// Resolve the physical cursor and review/insert shape in the shared caret coordinates.
    pub(in crate::grid) fn caret(&self, body: Rect) -> (f32, f32, f32, bool, bool) {
        let term = self.session.screen.lock();
        let input = self.accepts_text();
        let live = term.renderable_content().cursor;
        let point = if input {
            live.point
        } else {
            cursor_cell(&term, self.review_point(&term))
        };
        let row = (point.line.0 - term.topmost_line().0) as f32 - self.scroll.position();
        let column = point.column.0.checked_sub(self.left);
        (
            (body.x + column.unwrap_or(0)) as f32,
            body.y as f32 + row,
            1.,
            input && live.shape == CursorShape::Beam,
            (!input || live.shape != CursorShape::Hidden)
                && row >= 0.
                && row + 1. <= body.h as f32 + 0.001
                && column.is_some_and(|column| column < body.w),
        )
    }

    /// Smooth viewport used by the editor frame loop and shared animated caret.
    pub(in crate::grid) fn scroll(&self) -> &nanobug::animation::scroll::GridScroll {
        &self.scroll
    }

    /// Number of lines that can be shown, including both history and the active screen.
    pub(in crate::grid) fn line_count(&self) -> usize {
        self.session.screen.lock().total_lines()
    }
}

/// Resolve the first cell of the character under `point` in `term`.
fn cursor_cell(term: &Term<Events>, mut point: Point) -> Point {
    if term.grid()[point].flags.contains(Flags::WIDE_CHAR_SPACER) {
        point.column = Column(point.column.0.saturating_sub(1));
    }
    point
}

/// Resolve an ANSI `color` using the child's optional `palette` overrides and UI `default`.
fn color(color: Color, palette: &Colors, default: u32) -> u32 {
    let index = match color {
        Color::Spec(rgb) => return rgba(rgb),
        Color::Indexed(index) => usize::from(index),
        Color::Named(named) => named as usize,
    };
    if let Some(rgb) = palette[index] {
        return rgba(rgb);
    }
    match index {
        0..=15 => ANSI[index],
        16..=231 => {
            let value = index - 16;
            let channel = |amount: usize| if amount == 0 { 0 } else { 55 + 40 * amount } as u8;
            rgba(Rgb {
                r: channel(value / 36),
                g: channel((value / 6) % 6),
                b: channel(value % 6),
            })
        }
        232..=255 => {
            let value = (8 + 10 * (index - 232)) as u8;
            rgba(Rgb {
                r: value,
                g: value,
                b: value,
            })
        }
        _ if matches!(
            color,
            Color::Named(NamedColor::DimForeground | NamedColor::BrightForeground)
        ) =>
        {
            TEXT
        }
        _ => default,
    }
}

/// Encode the terminal's `rgb` for the grid compositor.
fn rgba(rgb: Rgb) -> u32 {
    u32::from_be_bytes([rgb.r, rgb.g, rgb.b, 0xff])
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use gpui::Keystroke;

    /// A second view shares the screen while resize, review, and focused input remain independent.
    #[test]
    fn shared_terminal_keeps_view_state_independent() -> anyhow::Result<()> {
        let temporary = tempfile::tempdir()?;
        let (mut live, _events) = Terminal::start(
            temporary.path().to_owned(),
            Some("set /p NANOBUG_TERMINAL_TEST=".into()),
            24,
            8,
        )?;
        let mut review = live.shared_view();
        assert!(live.resize(30, 12));
        assert!(!review.resize(30, 12));
        assert_eq!(review.line_count(), 12);
        live.enter_input();
        assert!(live.accepts_text());
        assert!(!review.accepts_text());

        let area = Rect {
            x: 0,
            y: 0,
            w: 8,
            h: 3,
        };
        live.session.screen.lock().grid_mut().cursor.point = Point::new(Line(10), Column(21));
        live.track_view(area, ScrollInput::Advance, 0.);
        assert!(live.caret(area).4);
        assert!(live.left > 0);

        review.track_view(area, ScrollInput::Wheel(-6), 0.);
        review.key(&Keystroke::parse("v")?);
        assert!(review.selection.is_some());
        assert!(live.selection.is_none());
        assert_ne!(review.top, live.top);
        assert!(live.accepts_text());

        drop(live);
        review.enter_input();
        assert!(review.accepts_text());
        Ok(())
    }
}
