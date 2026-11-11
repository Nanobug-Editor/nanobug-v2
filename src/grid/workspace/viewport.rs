//! Coordinate viewport scrolling, frame invalidation and the logical/animated cursor positions.

use crate::grid::{
    render::{CursorCell, Rect},
    windows::{
        help::HelpWindow,
        manager::ManagerWindow,
        start::{StartFocus, StartPage},
    },
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
    /// Start page menu, selected project row or project filter.
    Start(StartFocus),
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
    /// Highlighted row of the file picker.
    FilePicker,
    /// Editable filter, path, or filename prompt.
    FilePickerInput,
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
        if let Some(session) = &self.file_picker {
            return session
                .state
                .caret(self.file_picker_layout(), &self.spacing);
        }
        let Some(project) = self.projects.active_project() else {
            return (0., 0., 1., false, false);
        };
        if project.focus == Focus::Manager
            && let Some(manager) = &project.manager
        {
            let body = ManagerWindow::body(self.layout.tree);
            let y = body.y as f32 + manager.tree.selected as f32 - manager.scroll.position();
            // The row caret starts in the same column at every nesting depth.
            return (
                body.x as f32,
                y,
                1.,
                false,
                !manager.tree.entries.is_empty()
                    && y >= body.y as f32
                    && y + 1. <= (body.y + body.h) as f32 + 0.001,
            );
        }
        let buffer = &project.editor.buffers[project.editor.active];
        let vim = &buffer.vim;
        if matches!(vim.mode, Mode::Command | Mode::Search) {
            let slot = self.footer_slots().left;
            let units = self
                .spacing
                .fitting_cells(
                    &project.editor.command_text(),
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
        let body = project.editor.text_area(self.layout.editor);
        let row = buffer.cursor_row();
        let (line, _) = buffer.displayed_line(row, &project.editor.composition.text);
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
        } else if let Some(session) = &self.file_picker {
            if session.state.prompt.is_some() {
                CursorSpace::FilePickerInput
            } else {
                CursorSpace::FilePicker
            }
        } else if let Some(page) = self.projects.start() {
            CursorSpace::Start(page.cursor_focus())
        } else if self.projects.active().focus == Focus::Manager {
            CursorSpace::Manager
        } else if matches!(
            self.projects.active().editor.vim().mode,
            Mode::Command | Mode::Search
        ) {
            CursorSpace::CommandLine
        } else {
            CursorSpace::Document(self.projects.active().editor.active)
        }
    }

    /// Rebuild changed panel cells and retarget the independent animated cursor.
    pub(super) fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.layout = self.derive_layout(window);
        self.sync_start_preview(window, cx);
        self.repaint([ScrollInput::Reveal; 3], window, cx);
    }

    /// Apply `inputs` for the manager rows, editor rows and editor columns in order.
    /// Rebase cursor effects through `cx` and report whether retained cells need rebuilding.
    fn advance_scrolling(&mut self, inputs: [ScrollInput; 3], cx: &mut Context<Self>) -> bool {
        let Some(project) = self.projects.active_project_mut() else {
            return false;
        };
        let [tree_input, document_input, column_input] = inputs;
        let column_input = match column_input {
            ScrollInput::Reveal if project.focus == Focus::Manager => ScrollInput::Advance,
            input => input,
        };
        let cells = [
            project
                .manager
                .as_ref()
                .map_or(0, |manager| manager.scroll.first_cell()),
            project.editor.buffers[project.editor.active]
                .scroll
                .first_cell(),
            project.editor.buffers[project.editor.active]
                .columns
                .first_cell(),
        ];
        let now = self.scroll_epoch.elapsed().as_secs_f64();
        let (tree_changed, tree_delta) = project.manager.as_mut().map_or((false, 0.), |manager| {
            let (selected, delta) = manager.scroll.track_rows(
                tree_input,
                Some(manager.tree.selected),
                ManagerWindow::body(self.layout.tree).h,
                manager.tree.entries.len(),
                now,
            );
            let selected = selected.unwrap_or(0);
            let changed = manager.tree.selected != selected;
            manager.tree.selected = selected;
            (changed, delta)
        });

        let text = project.editor.text_area(self.layout.editor);
        let composition = if project.focus == Focus::Editor {
            project.editor.composition.text.as_str()
        } else {
            ""
        };
        let buffer = &mut project.editor.buffers[project.editor.active];
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
        let project = self.projects.active();
        let buffer = &project.editor.buffers[project.editor.active];
        if self.last_cursor_space == Some(space) {
            let owned = match space {
                CursorSpace::Manager => project
                    .manager
                    .as_ref()
                    .map(|manager| (&manager.scroll, tree_delta, None)),
                CursorSpace::Document(_) => Some((
                    &buffer.scroll,
                    document_delta,
                    Some((&buffer.columns, column_delta)),
                )),
                CursorSpace::Start(_)
                | CursorSpace::CommandLine
                | CursorSpace::HelpList
                | CursorSpace::HelpFilter
                | CursorSpace::FilePicker
                | CursorSpace::FilePickerInput
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
                    project
                        .manager
                        .as_ref()
                        .map_or(0, |manager| manager.scroll.first_cell()),
                    buffer.scroll.first_cell(),
                    buffer.columns.first_cell(),
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
        let space = self.cursor_space();
        let start = if matches!(space, CursorSpace::Start(_)) {
            self.start_layout()
        } else {
            None
        };
        let metrics = start.map_or(self.metrics, |_| StartPage::content_metrics(self.metrics));
        let start_caret = start
            .zip(self.projects.start())
            .map(|(layout, page)| page.caret(layout, metrics, &self.spacing));
        let (target, width, insert, visible) = if let Some((cell, width)) = start_caret {
            (
                cell.origin,
                width,
                space == CursorSpace::Start(StartFocus::Filter),
                self.editor_close.is_none(),
            )
        } else {
            (metrics.at(x, y), width, insert, visible)
        };
        let viewport = match space {
            CursorSpace::Manager => self.projects.active().manager.as_ref().map(|manager| {
                (
                    &self.bodies[0],
                    ManagerWindow::body(self.layout.tree),
                    &manager.scroll,
                    manager.tree.entries.len(),
                    0.,
                )
            }),
            CursorSpace::Document(_) => Some((
                &self.bodies[2],
                self.projects.active().editor.text_area(self.layout.editor),
                &self.projects.active().editor.buffers[self.projects.active().editor.active].scroll,
                self.projects.active().editor.vim().doc.starts.len(),
                self.projects.active().editor.buffers[self.projects.active().editor.active]
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
            CursorSpace::Start(_)
            | CursorSpace::CommandLine
            | CursorSpace::HelpFilter
            | CursorSpace::FilePicker
            | CursorSpace::FilePickerInput
            | CursorSpace::LanguageList => None,
        };
        let cell_width = metrics.cw;
        let mut painted_width = cell_width * width;
        let mut text_cell = None;
        if insert {
            painted_width = 2.;
        } else if matches!(
            space,
            CursorSpace::Manager
                | CursorSpace::HelpList
                | CursorSpace::LanguageList
                | CursorSpace::FilePicker
                | CursorSpace::Start(StartFocus::Projects)
        ) {
            painted_width *= 0.5;
        } else if visible {
            text_cell = start_caret.map(|(cell, _)| cell).or_else(|| {
                viewport.map(|(_, rect, scroll, _, displacement)| {
                    // Undo the scroll offsets to find the glyph in the retained panel.
                    CursorCell {
                        row: (y - rect.y as f32 + scroll.fraction()).round() as usize,
                        column: x - rect.x as f32 + displacement,
                        origin: target,
                    }
                })
            });
        }
        // The popup owns the cursor even while its animation crosses from the previous pane.
        let (body, clip, head_clip) = if let Some(layout) = start {
            (
                Some(self.start_panel.clone()),
                Some(layout.area().pixels(metrics)),
                None,
            )
        } else if let Some(session) = &self.file_picker
            && matches!(
                space,
                CursorSpace::FilePicker | CursorSpace::FilePickerInput
            )
        {
            let floating = self.file_picker_filter_area();
            (
                Some(if floating.is_some() {
                    self.popup_panel.clone()
                } else {
                    session.panel.clone()
                }),
                Some(
                    floating
                        .unwrap_or(self.file_picker_layout().area)
                        .pixels(self.metrics),
                ),
                None,
            )
        } else if space == CursorSpace::HelpFilter {
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
        if width > 1.
            && let (Some(cell), Some(panel)) = (text_cell, body.as_ref())
            && let Some(ink) =
                panel
                    .read(cx)
                    .glyph_width(cell.row, cell.column, window, self.glyphs.as_ref())
        {
            let scale = window.scale_factor();
            painted_width = painted_width.min((ink * scale).ceil() / scale);
        }
        let geometry = Geometry::new(
            f32::from(target.x),
            f32::from(target.y),
            painted_width,
            metrics.lh,
        );
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

    /// Apply viewport `inputs`, syncing native input and repainting `window` through `cx`.
    pub(super) fn repaint(
        &mut self,
        inputs: [ScrollInput; 3],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        #[cfg(windows)]
        self.sync_native_ime(window, cx);
        #[cfg(windows)]
        if let Some(shortcut) = &self.alt_escape {
            shortcut.set_enabled(self.editor_accepts_alt_escape());
        }
        if self.hovered_controls.is_some() {
            self.hovered_controls = if self.resizing
                || self
                    .projects
                    .active_project()
                    .is_some_and(|project| project.editor.dragging.is_some())
            {
                None
            } else {
                self.corner_controls_at(window.mouse_position())
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

    /// Apply popup `input`, syncing native input and refreshing its foreground in `window` through `cx`.
    pub(super) fn repaint_help(
        &mut self,
        input: ScrollInput,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        #[cfg(windows)]
        self.sync_native_ime(window, cx);
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
        let active = if let Some(list) = &self.help.list {
            list.scroll.is_active()
        } else if let Some(project) = self.projects.active_project() {
            let buffer = &project.editor.buffers[project.editor.active];
            project
                .manager
                .as_ref()
                .is_some_and(|manager| manager.scroll.is_active())
                || buffer.scroll.is_active()
                || buffer.columns.is_active()
        } else {
            false
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
