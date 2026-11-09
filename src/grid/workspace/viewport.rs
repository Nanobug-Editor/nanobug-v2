//! Coordinate viewport scrolling, frame invalidation and the logical/animated cursor positions.

use crate::grid::{
    render::{CursorCell, Rect},
    windows::{help::HelpWindow, manager::ManagerWindow},
};

use super::{
    Workspace,
    language::language_list_bounds,
    layout::{FOOTER_ROWS, Focus},
};
use crate::ui::text::UNITS_PER_COLUMN;
use gpui::{Context, Window};
use nanobug::{
    animation::{
        cursor::{Geometry, Vec2},
        scroll::{GridScroll, ScrollInput},
    },
    vim::Mode,
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
    /// Shortcut-help list viewport.
    HelpList,
    /// Fixed description filter prompt.
    HelpFilter,
    /// Fixed language choices in the lower-right popup.
    LanguageList,
}

impl Workspace {
    /// Resolve the logical caret's position and width in visual grid columns, plus its input state.
    pub(super) fn caret(&self) -> (f32, f32, f32, bool, bool) {
        if let Some(list) = &self.help.list {
            let area = self.help_layout();
            if let Some((x, y)) = self.help.filter_caret(area, &self.spacing, self.metrics) {
                return (x, y, 1., true, true);
            }
            let body = self.help.body(area);
            let origin = body
                .pixels(HelpWindow::content_metrics(self.metrics))
                .origin;
            let top = f32::from(origin.y - self.metrics.origin.y) / self.metrics.lh;
            let row = list.selected as f32 - list.scroll.position();
            return (
                body.x as f32,
                top + row,
                1.,
                false,
                list.selected < list.len() && row >= 0. && row + 1. <= body.h as f32 + 0.001,
            );
        }
        if let Some((x, y)) = self.language_caret() {
            return (x, y, 1., false, true);
        }
        if self.projects.active.focus == Focus::Manager {
            let body = ManagerWindow::body(self.layout.tree);
            let y = body.y as f32 + self.projects.active.manager.tree.selected as f32
                - self.projects.active.manager.scroll.position();
            // The row caret starts in the same column at every nesting depth.
            return (
                body.x as f32,
                y,
                1.,
                false,
                !self.projects.active.manager.tree.entries.is_empty()
                    && y >= body.y as f32
                    && y + 1. <= (body.y + body.h) as f32 + 0.001,
            );
        }
        let buffer = &self.projects.active.editor.buffers[self.projects.active.editor.active];
        let vim = &buffer.vim;
        if matches!(vim.mode, Mode::Command | Mode::Search) {
            let slot = self.command_slot(&self.footer_position());
            let units = self
                .spacing
                .fitting_cells(
                    &self.projects.active.editor.command_text(),
                    slot.len() * UNITS_PER_COLUMN,
                )
                .last()
                .map_or(0, |cell| cell.end);
            return (
                slot.start as f32 + units as f32 / UNITS_PER_COLUMN as f32,
                (self.layout.rows - FOOTER_ROWS) as f32
                    + f32::from(self.layout.footer_offset) / self.metrics.lh,
                1.,
                true,
                true,
            );
        }
        let body = self.projects.active.editor.text_area(self.layout.editor);
        let row = buffer.cursor_row();
        let (line, _) = buffer.displayed_line(row, &self.projects.active.editor.composition.text);
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
        if self.help.is_filtering() {
            CursorSpace::HelpFilter
        } else if self.help.is_open() {
            CursorSpace::HelpList
        } else if self.language_selection.is_some() {
            CursorSpace::LanguageList
        } else if self.projects.active.focus == Focus::Manager {
            CursorSpace::Manager
        } else if matches!(
            self.projects.active.editor.vim().mode,
            Mode::Command | Mode::Search
        ) {
            CursorSpace::CommandLine
        } else {
            CursorSpace::Document(self.projects.active.editor.active)
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
            ScrollInput::Reveal if self.projects.active.focus == Focus::Manager => {
                ScrollInput::Advance
            }
            input => input,
        };
        let cells = [
            self.projects.active.manager.scroll.first_cell(),
            self.projects.active.editor.buffers[self.projects.active.editor.active]
                .scroll
                .first_cell(),
            self.projects.active.editor.buffers[self.projects.active.editor.active]
                .columns
                .first_cell(),
        ];
        let now = self.scroll_epoch.elapsed().as_secs_f64();
        let (selected, tree_delta) = self.projects.active.manager.scroll.track_rows(
            tree_input,
            Some(self.projects.active.manager.tree.selected),
            ManagerWindow::body(self.layout.tree).h,
            self.projects.active.manager.tree.entries.len(),
            now,
        );
        let selected = selected.unwrap_or(0);
        let tree_changed = self.projects.active.manager.tree.selected != selected;
        self.projects.active.manager.tree.selected = selected;

        let text = self.projects.active.editor.text_area(self.layout.editor);
        let composition = if self.projects.active.focus == Focus::Editor {
            self.projects.active.editor.composition.text.as_str()
        } else {
            ""
        };
        let buffer = &mut self.projects.active.editor.buffers[self.projects.active.editor.active];
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
                CursorSpace::Manager => {
                    Some((&self.projects.active.manager.scroll, tree_delta, None))
                }
                CursorSpace::Document(_) => Some((
                    &self.projects.active.editor.buffers[self.projects.active.editor.active].scroll,
                    document_delta,
                    Some((
                        &self.projects.active.editor.buffers[self.projects.active.editor.active]
                            .columns,
                        column_delta,
                    )),
                )),
                CursorSpace::CommandLine
                | CursorSpace::HelpList
                | CursorSpace::HelpFilter
                | CursorSpace::LanguageList => None,
            };
            if let Some((rows, delta, columns)) = owned {
                self.follow_cursor_scroll(rows, delta, columns, cx);
            }
        }
        tree_changed
            || document_changed
            || cells
                != [
                    self.projects.active.manager.scroll.first_cell(),
                    self.projects.active.editor.buffers[self.projects.active.editor.active]
                        .scroll
                        .first_cell(),
                    self.projects.active.editor.buffers[self.projects.active.editor.active]
                        .columns
                        .first_cell(),
                ]
    }

    /// Keep the animated cursor in its scrolling viewport through `cx`.
    /// `rows`, `delta` and optional `columns` describe the animated movement in cells.
    fn follow_cursor_scroll(
        &self,
        rows: &GridScroll,
        delta: f32,
        columns: Option<(&GridScroll, f32)>,
        cx: &mut Context<Self>,
    ) {
        let shift = Vec2::new(
            columns.map_or(0., |(_, delta)| -delta * self.metrics.cw),
            -delta * self.metrics.lh,
        );
        let speed = Vec2::new(
            columns.map_or(0., |(scroll, _)| -scroll.velocity() * self.metrics.cw),
            -rows.velocity() * self.metrics.lh,
        );
        let active = rows.is_active() || columns.is_some_and(|(scroll, _)| scroll.is_active());
        let settling = rows.settling() && columns.is_none_or(|(scroll, _)| scroll.settling());
        self.cursor.update(cx, |cursor, cx| {
            cursor.follow_scroll(shift, speed, active, settling, cx)
        });
    }

    /// Apply popup scroll `input` and rebase its list caret through `cx`.
    /// Return whether retained rows or selection changed.
    fn advance_help_scrolling(&mut self, input: ScrollInput, cx: &mut Context<Self>) -> bool {
        let height = self.help.body(self.help_layout()).h;
        let now = self.scroll_epoch.elapsed().as_secs_f64();
        let Some(list) = &mut self.help.list else {
            return false;
        };
        let (changed, delta) = list.track(input, height, now);
        if self.last_cursor_space == Some(CursorSpace::HelpList)
            && let Some(list) = &self.help.list
            && !list.is_filtering()
        {
            self.follow_cursor_scroll(&list.scroll, delta, None, cx);
        }
        changed
    }

    /// Retarget the shared cursor in `window` using the active surface and its geometry through `cx`.
    pub(super) fn update_cursor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (x, y, width, insert, visible) = self.caret();
        let target = self.metrics.at(x, y);
        let space = self.cursor_space();
        let viewport = match space {
            CursorSpace::Manager => Some((
                &self.bodies[0],
                ManagerWindow::body(self.layout.tree),
                &self.projects.active.manager.scroll,
                self.projects.active.manager.tree.entries.len(),
                0.,
            )),
            CursorSpace::Document(_) => Some((
                &self.bodies[2],
                self.projects.active.editor.text_area(self.layout.editor),
                &self.projects.active.editor.buffers[self.projects.active.editor.active].scroll,
                self.projects.active.editor.vim().doc.starts.len(),
                self.projects.active.editor.buffers[self.projects.active.editor.active]
                    .text_displacement(),
            )),
            CursorSpace::HelpList => self.help.list.as_ref().map(|list| {
                (
                    &self.help.body,
                    self.help.body(self.help_layout()),
                    &list.scroll,
                    list.len(),
                    0.,
                )
            }),
            CursorSpace::CommandLine | CursorSpace::HelpFilter | CursorSpace::LanguageList => None,
        };
        let cell_width = self.metrics.cw;
        let mut painted_width = cell_width * width;
        let mut text_cell = None;
        if insert {
            painted_width = 2.;
        } else if matches!(
            space,
            CursorSpace::Manager | CursorSpace::HelpList | CursorSpace::LanguageList
        ) {
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
        // The popup owns the cursor even while its animation crosses from the previous pane.
        let (body, clip, head_clip) = if space == CursorSpace::HelpFilter {
            let clip = Some(self.help_layout().pixels(self.metrics));
            (None, clip, clip)
        } else if space == CursorSpace::LanguageList {
            let clip = self
                .language_picker_area()
                .map(|area| language_list_bounds(area, self.metrics));
            // The choices own nearby-text effects while the cursor can travel across the workspace.
            (Some(self.language_list.clone()), clip, None)
        } else {
            let clip_metrics = if space == CursorSpace::HelpList {
                HelpWindow::content_metrics(self.metrics)
            } else {
                self.metrics
            };
            viewport.map_or((None, None, None), |(panel, rect, scroll, rows, _)| {
                let (above, below) = scroll.wheel_row_insets(rect.h, rows);
                let head_clip = (above + below > 0 || space == CursorSpace::HelpList).then(|| {
                    Rect {
                        y: rect.y + above,
                        h: rect.h - above - below,
                        ..rect
                    }
                    .pixels(clip_metrics)
                });
                (
                    Some(panel.clone()),
                    Some(rect.pixels(clip_metrics)),
                    head_clip,
                )
            })
        };
        self.cursor.update(cx, |cursor, cx| {
            cursor.set_viewport(
                body,
                clip,
                head_clip,
                text_cell,
                self.last_cursor_space != Some(space),
                cx,
            );
            cursor.set(geometry, visible, cell_width, !insert, window, cx)
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
        #[cfg(windows)]
        if let Some(shortcut) = &self.alt_escape {
            shortcut.set_enabled(self.projects.active.focus == Focus::Editor && !self.modal_open());
        }
        if self.hovered_close.is_some() {
            self.hovered_close = if self.resizing || self.projects.active.editor.dragging.is_some()
            {
                None
            } else {
                self.close_button_at(window.mouse_position())
            };
        }
        if self.help.is_open() {
            self.advance_help_scrolling(ScrollInput::Advance, cx);
        } else {
            self.advance_scrolling(inputs, cx);
        }
        self.refresh_panels(cx);
        self.finish_scroll_frame(window, cx);
    }

    /// Apply popup `input` and refresh only its foreground and cursor in `window` through `cx`.
    pub(super) fn repaint_help(
        &mut self,
        input: ScrollInput,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.advance_help_scrolling(input, cx);
        self.refresh_help_panels(cx);
        self.finish_scroll_frame(window, cx);
    }

    /// Dismiss the help filter in `window` through `cx`, restoring its old query on `cancel`.
    pub(super) fn finish_help_filter(
        &mut self,
        cancel: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .help
            .finish_filter(cancel, self.scroll_epoch.elapsed().as_secs_f64())
        {
            self.repaint_help(ScrollInput::Advance, window, cx);
        }
    }

    /// Apply scroll-only `inputs` to `window`, rebuilding cells through `cx` only when they change.
    pub(super) fn scroll_frame(
        &mut self,
        inputs: [ScrollInput; 3],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.help.is_open() {
            if self.advance_help_scrolling(ScrollInput::Advance, cx) {
                self.refresh_help_panels(cx);
            }
        } else if self.advance_scrolling(inputs, cx) {
            self.refresh_panels(cx);
        }
        self.finish_scroll_frame(window, cx);
    }

    /// Retarget the active cursor and publish the viewport update in `window` through `cx`.
    fn finish_scroll_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.update_cursor(window, cx);
        self.schedule_scroll(window, cx);
        cx.notify();
    }

    /// Animate body transforms in `window` through `cx`, retaining cells between grid steps.
    fn schedule_scroll(&mut self, window: &Window, cx: &mut Context<Self>) {
        let buffer = &self.projects.active.editor.buffers[self.projects.active.editor.active];
        let active = if let Some(list) = &self.help.list {
            list.scroll.is_active()
        } else {
            self.projects.active.manager.scroll.is_active()
                || buffer.scroll.is_active()
                || buffer.columns.is_active()
        };
        if self.scroll_scheduled || !active {
            return;
        }
        self.scroll_scheduled = true;
        let weak = cx.entity().downgrade();
        window.on_next_frame(move |window, cx| {
            let _ = weak.update(cx, |workspace, cx| {
                workspace.scroll_scheduled = false;
                workspace.scroll_frame([ScrollInput::Advance; 3], window, cx);
            });
        });
    }
}
