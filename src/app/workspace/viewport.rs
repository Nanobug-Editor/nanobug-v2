//! Coordinate viewport scrolling, frame invalidation and the logical/animated cursor positions.

use super::{Editor, layout::Focus};
use crate::ui::{
    grid::{CursorCell, Rect},
    text::UNITS_PER_COLUMN,
};
use gpui::{Context, Window};
use nanobug::{
    animation::{
        cursor::{Geometry, Vec2},
        scroll::{GridScroll, ScrollInput},
    },
    editor::Mode,
};

/// Coordinate space owning the logical caret, used to rebase cursor animation during scrolling.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum CursorSpace {
    /// File manager viewport.
    Manager,
    /// One document viewport, identified by its buffer index.
    Document(usize),
    /// Fixed command/search row.
    CommandLine,
}

impl Editor {
    /// Resolve the logical caret's position and width in visual grid columns, plus its input state.
    pub(super) fn caret(&self) -> (f32, f32, f32, bool, bool) {
        if self.focus == Focus::Manager {
            let body = self.layout.tree_body();
            let y = body.y as f32 + self.tree.selected as f32 - self.tree_scroll.position();
            // The row caret starts in the same column at every nesting depth.
            return (
                body.x as f32,
                y,
                1.,
                false,
                !self.tree.entries.is_empty()
                    && y >= body.y as f32
                    && y + 1. <= (body.y + body.h) as f32 + 0.001,
            );
        }
        let buffer = &self.buffers[self.active];
        let vim = &buffer.vim;
        if matches!(vim.mode, Mode::Command | Mode::Search) {
            let slot = self.command_slot();
            let units = self
                .spacing
                .fitting_cells(&self.command_text(), slot.len() * UNITS_PER_COLUMN)
                .last()
                .map_or(0, |cell| cell.end);
            return (
                slot.start as f32 + units as f32 / UNITS_PER_COLUMN as f32,
                (self.layout.rows - 1) as f32
                    + f32::from(self.layout.footer_offset) / self.metrics.lh,
                1.,
                true,
                true,
            );
        }
        let body = self.editor_text();
        let row = buffer.cursor_row();
        let (line, _) = buffer.displayed_line(row, &self.composition);
        let span = buffer.caret_span(row, &line, &self.spacing);
        let insert = vim.mode == Mode::Insert;
        let width = (span.len() as f32 / UNITS_PER_COLUMN as f32).min(body.w.max(1) as f32);
        let retained_left = (buffer.columns.first_cell() * UNITS_PER_COLUMN) as isize;
        let x = (span.start as isize - retained_left) as f32 / UNITS_PER_COLUMN as f32
            - buffer.columns.fraction();
        let y = body.y as f32 + row as f32 - buffer.scroll.position();
        (
            body.x as f32 + x,
            y,
            width,
            insert,
            y >= body.y as f32
                && y + 1. <= (body.y + body.h) as f32 + 0.001
                && body.w > 0
                && x >= -0.001
                && x + width <= body.w as f32 + 0.001,
        )
    }

    /// Coordinate system that should move with a scrolling viewport.
    fn cursor_space(&self) -> CursorSpace {
        if self.focus == Focus::Manager {
            CursorSpace::Manager
        } else if matches!(self.vim().mode, Mode::Command | Mode::Search) {
            CursorSpace::CommandLine
        } else {
            CursorSpace::Document(self.active)
        }
    }

    /// Rebuild changed panel cells and retarget the independent animated cursor.
    pub(super) fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.layout = self.derive_layout(window);
        self.repaint([ScrollInput::Reveal; 3], window, cx);
    }

    /// Apply `inputs` for the manager rows, editor rows and editor columns in order.
    /// Rebase cursor effects through `cx` and report whether retained cells need rebuilding.
    fn advance_scrolling(&mut self, inputs: [ScrollInput; 3], cx: &mut Context<Self>) -> bool {
        let [tree_input, document_input, column_input] = inputs;
        let column_input = match column_input {
            ScrollInput::Reveal if self.focus == Focus::Manager => ScrollInput::Advance,
            input => input,
        };
        let cells = [
            self.tree_scroll.first_cell(),
            self.buffers[self.active].scroll.first_cell(),
            self.buffers[self.active].columns.first_cell(),
        ];
        let now = self.scroll_epoch.elapsed().as_secs_f64();
        let (selected, tree_delta) = self.tree_scroll.track_rows(
            tree_input,
            Some(self.tree.selected),
            self.layout.tree_body().h,
            self.tree.entries.len(),
            now,
        );
        let selected = selected.unwrap_or(0);
        let tree_changed = self.tree.selected != selected;
        self.tree.selected = selected;

        let text = self.editor_text();
        let composition = if self.focus == Focus::Editor && self.help_top.is_none() {
            self.composition.as_str()
        } else {
            ""
        };
        let buffer = &mut self.buffers[self.active];
        let row =
            (!matches!(buffer.vim.mode, Mode::Command | Mode::Search)).then(|| buffer.cursor_row());
        let (next_row, document_delta) = buffer.scroll.track_rows(
            document_input,
            row,
            text.h,
            buffer.vim.doc.starts.len(),
            now,
        );
        let document_changed = next_row != row;
        if let Some(next_row) = next_row.filter(|_| document_changed) {
            buffer.vim.move_to_row(next_row);
        }
        let caret_line = next_row.map(|row| buffer.displayed_line(row, composition).0);
        let caret = next_row
            .zip(caret_line.as_deref())
            .filter(|_| text.w > 0)
            .map(|(row, line)| {
                let span = buffer.caret_span(row, line, &self.spacing);
                span.start / UNITS_PER_COLUMN..span.end.div_ceil(UNITS_PER_COLUMN)
            });
        let columns = match column_input {
            ScrollInput::Wheel(_) => {
                // A short caret line should not prevent panning a long visible line.
                let first = buffer.scroll.first_cell();
                let last = first + text.h + usize::from(buffer.scroll.fraction() > 0.);
                (first..last.min(buffer.vim.doc.starts.len()))
                    .map(|row| {
                        self.spacing
                            .columns(&buffer.displayed_line(row, composition).0)
                            + 1
                    })
                    .max()
            }
            ScrollInput::Reveal => caret_line
                .as_deref()
                .map(|line| self.spacing.columns(line) + 1),
            _ => None,
        };
        let column_delta = buffer
            .columns
            .track_columns(column_input, caret, text.w, columns, now);

        let space = self.cursor_space();
        if self.last_cursor_space == Some(space) {
            let owned = match space {
                CursorSpace::Manager => Some((tree_delta, &self.tree_scroll, None)),
                CursorSpace::Document(_) => Some((
                    document_delta,
                    &self.buffers[self.active].scroll,
                    Some(&self.buffers[self.active].columns),
                )),
                CursorSpace::CommandLine => None,
            };
            if let Some((delta, rows, columns)) = owned {
                let shift = Vec2::new(
                    columns.map_or(0., |_| -column_delta * self.metrics.cw),
                    -delta * self.metrics.lh,
                );
                let speed = Vec2::new(
                    columns.map_or(0., |scroll| -scroll.velocity() * self.metrics.cw),
                    -rows.velocity() * self.metrics.lh,
                );
                let active = rows.is_active() || columns.is_some_and(GridScroll::is_active);
                let settling = rows.settling() && columns.is_none_or(GridScroll::settling);
                self.cursor.update(cx, |cursor, cx| {
                    cursor.follow_scroll(shift, speed, active, settling, cx)
                });
            }
        }
        tree_changed
            || document_changed
            || cells
                != [
                    self.tree_scroll.first_cell(),
                    self.buffers[self.active].scroll.first_cell(),
                    self.buffers[self.active].columns.first_cell(),
                ]
    }

    /// Retarget the cursor using logical input state and current viewport geometry.
    fn update_cursor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (x, y, width, insert, visible) = self.caret();
        let target = self.metrics.at(x, y);
        let space = self.cursor_space();
        let viewport = match space {
            CursorSpace::Manager => Some((
                &self.bodies[0],
                self.layout.tree_body(),
                &self.tree_scroll,
                self.tree.entries.len(),
                0.,
            )),
            CursorSpace::Document(_) => Some((
                &self.bodies[2],
                self.editor_text(),
                &self.buffers[self.active].scroll,
                self.vim().doc.starts.len(),
                self.buffers[self.active].text_displacement(),
            )),
            CursorSpace::CommandLine => None,
        };
        let cell_width = self.metrics.cw;
        let mut painted_width = cell_width * width;
        let mut text_cell = None;
        if insert {
            painted_width = 2.;
        } else if space == CursorSpace::Manager {
            painted_width *= 0.5;
        } else if visible && let Some((panel, rect, scroll, _, displacement)) = viewport {
            // Undo the scroll offsets and use the panel's visual cell boundary.
            let row = (y - rect.y as f32 + scroll.fraction()).round() as usize;
            let column = x - rect.x as f32 + displacement;
            text_cell = Some(CursorCell {
                row,
                column,
                origin: target,
            });
            if width > 1.
                && let Some(ink) =
                    panel
                        .read(cx)
                        .glyph_width(row, column, window, self.glyphs.as_ref())
            {
                let scale = window.scale_factor();
                painted_width = painted_width.min((ink * scale).ceil() / scale);
            }
        }
        let geometry = Geometry::new(
            f32::from(target.x),
            f32::from(target.y),
            painted_width,
            self.metrics.lh,
        );
        let (body, clip, head_clip) =
            viewport.map_or((None, None, None), |(panel, rect, scroll, rows, _)| {
                let (above, below) = scroll.wheel_row_insets(rect.h, rows);
                let head_clip = (above + below > 0).then(|| {
                    Rect {
                        y: rect.y + above,
                        h: rect.h - above - below,
                        ..rect
                    }
                    .pixels(self.metrics)
                });
                (
                    Some(panel.clone()),
                    Some(rect.pixels(self.metrics)),
                    head_clip,
                )
            });
        self.cursor.update(cx, |cursor, cx| {
            cursor.set_viewport(
                body,
                clip,
                head_clip,
                text_cell,
                self.last_cursor_space != Some(space),
                cx,
            );
            cursor.set(
                geometry,
                visible && self.help_top.is_none(),
                cell_width,
                !insert,
                window,
                cx,
            )
        });
        self.last_cursor_space = Some(space);
        window.notify_text_input_changed(&self.focus_handle);
    }

    /// Apply viewport `inputs` and repaint the panels and cursor in `window` through `cx`.
    pub(super) fn repaint(
        &mut self,
        inputs: [ScrollInput; 3],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.hovered_border.is_some() {
            self.hovered_border = if self.resizing || self.dragging.is_some() {
                None
            } else {
                self.border_at(window.mouse_position())
            };
        }
        self.advance_scrolling(inputs, cx);
        self.refresh_panels(cx);
        self.update_cursor(window, cx);
        self.schedule_scroll(window, cx);
        cx.notify();
    }

    /// Apply scroll-only `inputs` to `window`, rebuilding cells through `cx` only when they change.
    pub(super) fn scroll_frame(
        &mut self,
        inputs: [ScrollInput; 3],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.advance_scrolling(inputs, cx) {
            self.refresh_panels(cx);
        }
        self.update_cursor(window, cx);
        self.schedule_scroll(window, cx);
        cx.notify();
    }

    /// Animate body transforms in `window` through `cx`, retaining cells between grid steps.
    fn schedule_scroll(&mut self, window: &Window, cx: &mut Context<Self>) {
        let buffer = &self.buffers[self.active];
        if self.scroll_scheduled
            || !(self.tree_scroll.is_active()
                || buffer.scroll.is_active()
                || buffer.columns.is_active())
        {
            return;
        }
        self.scroll_scheduled = true;
        let weak = cx.entity().downgrade();
        window.on_next_frame(move |window, cx| {
            let _ = weak.update(cx, |editor, cx| {
                editor.scroll_scheduled = false;
                editor.scroll_frame([ScrollInput::Advance; 3], window, cx);
            });
        });
    }
}
