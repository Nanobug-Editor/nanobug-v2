//! Coordinate viewport scrolling, frame invalidation and the logical/animated cursor positions.

use crate::grid::{
    render::{CursorCell, Rect},
    windows::{
        editor::MarkdownViewMode,
        help::HelpWindow,
        manager::ManagerWindow,
        start::{StartFocus, StartPage},
    },
};

use super::{Workspace, language::language_list_bounds, layout::Focus};
use crate::ui::{text::UNITS_PER_COLUMN, theme::ThemeComponent};
use gpui::{Bounds, Context, Window, px, size};
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
    /// Editable command-line history viewport.
    CommandWindow,
    /// Fixed prompt row within the command-line history window.
    CommandWindowPrompt,
    /// Shortcut-help list viewport.
    HelpList,
    /// First character of the binding in the editor keyboard's description.
    HelpKeyboard,
    /// Fixed description filter prompt.
    HelpFilter,
    /// Fixed language choices in the lower-right popup.
    LanguageList,
    /// Highlighted row of the file picker.
    FilePicker,
    /// Editable filter, path, or filename prompt.
    FilePickerInput,
    /// Selected row of the plugin manager.
    PluginManager,
    /// Text prompt inside the plugin manager.
    PluginPrompt,
    /// Focused input provided by a plugin surface.
    PluginInput,
    /// The experiment's locally owned text input.
    #[cfg(feature = "plugin-probe")]
    PluginProbe,
    /// Read-only editor viewport in the two-column file preview.
    FilePreviewText,
    /// Fixed search or command line for the read-only preview.
    FilePreviewInput,
}

impl Workspace {
    /// Resolve the logical caret's position and width in visual grid columns, plus its input state.
    pub(super) fn caret(&self) -> (f32, f32, f32, bool, bool) {
        if let Some((history, area)) = self.command_window.as_ref().zip(self.command_window_area())
        {
            return history.caret(area, &self.spacing);
        }
        if let Some(caret) = self.plugin_caret() {
            return caret;
        }
        #[cfg(feature = "plugin-probe")]
        if let Some((x, y)) = self.probe_caret() {
            return (x, y, 1., true, true);
        }
        if let Some(list) = &self.help.list {
            let area = self.help_layout();
            if self.help.keyboard_visible() {
                let Some(cell) = self.help.keyboard_caret(area, self.metrics) else {
                    return (0., 0., 1., false, false);
                };
                return (
                    f32::from(cell.origin.x - self.metrics.origin.x) / self.metrics.cw,
                    f32::from(cell.origin.y - self.metrics.origin.y) / self.metrics.lh,
                    1.,
                    false,
                    true,
                );
            }
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
            if session.previewing_file() {
                return session
                    .focused_editor()
                    .map_or((0., 0., 1., false, false), |editor| {
                        self.file_preview_caret(editor)
                    });
            }
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
            let y = body.y as f32 + manager.tree().selected as f32 - manager.scroll().position();
            // The row caret starts in the same column at every nesting depth.
            return (
                body.x as f32,
                y,
                1.,
                false,
                !manager.tree().entries.is_empty()
                    && y >= body.y as f32
                    && y + 1. <= (body.y + body.h) as f32 + 0.001,
            );
        }
        let buffer = project.editor.buffer();
        let vim = &buffer.vim;
        if matches!(vim.mode, Mode::Command | Mode::Search) {
            let slot = self.footer_slots().left;
            let units = self
                .spacing
                .fitting_cells(
                    &project.editor.command_caret_text(),
                    slot.len() * UNITS_PER_COLUMN,
                )
                .last()
                .map_or(0, |cell| cell.end);
            return (
                slot.start as f32 + units as f32 / UNITS_PER_COLUMN as f32,
                f32::from(self.footer_bounds().top() - self.metrics.origin.y) / self.metrics.lh,
                1.,
                true,
                true,
            );
        }
        project
            .editor
            .body_caret(self.active_editor_area(), &self.spacing)
    }

    /// Coordinate system that should move with a scrolling viewport.
    fn cursor_space(&self) -> CursorSpace {
        if let Some(history) = &self.command_window {
            return if matches!(history.editor.vim().mode, Mode::Command | Mode::Search) {
                CursorSpace::CommandWindowPrompt
            } else {
                CursorSpace::CommandWindow
            };
        }
        if self.plugins.manager_accepts_text() {
            return CursorSpace::PluginPrompt;
        }
        if self.plugins.open {
            return CursorSpace::PluginManager;
        }
        if self.plugins.surface.focused {
            return CursorSpace::PluginInput;
        }
        #[cfg(feature = "plugin-probe")]
        if self.probe_has_focus() {
            return CursorSpace::PluginProbe;
        }
        if self.help.is_filtering() {
            CursorSpace::HelpFilter
        } else if self.help.keyboard_visible() {
            CursorSpace::HelpKeyboard
        } else if self.help.is_open() {
            CursorSpace::HelpList
        } else if self.language_selection.is_some() {
            CursorSpace::LanguageList
        } else if let Some(session) = &self.file_picker {
            if let Some(editor) = session.focused_editor() {
                if matches!(editor.vim().mode, Mode::Command | Mode::Search) {
                    CursorSpace::FilePreviewInput
                } else {
                    CursorSpace::FilePreviewText
                }
            } else if session.state.prompt.is_some() {
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
            CursorSpace::Document(self.projects.active().editor.active_index())
        }
    }

    /// Rebuild changed panel cells and retarget the independent animated cursor.
    pub(super) fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_noswapfile_to_loaded();
        self.observe_script_buffer(window, cx);
        if let Some(project) = self.projects.active_project_mut() {
            project.set_input_mappings(&self.input_mappings);
            project.refresh_shared_documents();
        }
        self.update_layout(window);
        self.refresh_document_presentation();
        #[cfg(feature = "plugin-probe")]
        self.sync_probe_projects();
        self.sync_start_preview(window, cx);
        self.repaint([ScrollInput::Reveal; 3], window, cx);
    }

    /// Apply `inputs` for the manager rows, editor rows and editor columns in order.
    /// Rebase cursor effects through `cx` and report whether retained cells need rebuilding.
    pub(super) fn advance_scrolling(
        &mut self,
        inputs: [ScrollInput; 3],
        cx: &mut Context<Self>,
    ) -> bool {
        if self.file_picker.is_some() {
            return self.advance_file_preview([inputs[1], inputs[2]], cx);
        }
        let area = self.active_editor_area();
        let Some(project) = self.projects.active_project_mut() else {
            return false;
        };
        let [tree_input, document_input, column_input] = inputs;
        let column_input = match column_input {
            ScrollInput::Reveal if project.focus == Focus::Manager => ScrollInput::Advance,
            input => input,
        };
        let first_tree_row = project
            .manager
            .as_ref()
            .map_or(0, |manager| manager.scroll().first_cell());
        let now = self.scroll_epoch.elapsed().as_secs_f64();
        let (tree_changed, tree_delta) = project.manager.as_mut().map_or((false, 0.), |manager| {
            manager.track_view(tree_input, ManagerWindow::body(self.layout.tree).h, now)
        });

        let bound_correction = if project.focus == Focus::Editor {
            project.editor.scrollbind_focus_correction()
        } else {
            0
        };
        let bound_before = (project.focus == Focus::Editor)
            .then(|| project.editor.scrollbind_target())
            .flatten();
        let (document_changed, document_delta, column_delta) = project.editor.track_view(
            area,
            project.focus == Focus::Editor,
            &self.spacing,
            [document_input, column_input],
            now,
        );
        let (preview_source_changed, preview_source_delta) = project
            .editor
            .advance_markdown_preview(&self.markdown_cache, area, now);
        let bound_shift = bound_before
            .zip(project.editor.scrollbind_target())
            .map_or(bound_correction, |(before, after)| {
                bound_correction.saturating_add(after as isize - before as isize)
            });

        let space = self.cursor_space();
        let project = self.projects.active();
        let buffer = project.editor.buffer();
        if self.last_cursor_space == Some(space) {
            let owned = match space {
                CursorSpace::Manager => project
                    .manager
                    .as_ref()
                    .map(|manager| (manager.scroll(), tree_delta, None)),
                CursorSpace::Document(_) => {
                    Some(if let Some(terminal) = project.editor.terminal() {
                        (terminal.scroll(), document_delta, None)
                    } else {
                        (
                            &buffer.scroll,
                            document_delta + preview_source_delta,
                            Some((&buffer.columns, column_delta)),
                        )
                    })
                }
                CursorSpace::Start(_)
                | CursorSpace::CommandLine
                | CursorSpace::CommandWindow
                | CursorSpace::CommandWindowPrompt
                | CursorSpace::HelpList
                | CursorSpace::HelpKeyboard
                | CursorSpace::HelpFilter
                | CursorSpace::FilePicker
                | CursorSpace::FilePickerInput
                | CursorSpace::PluginManager
                | CursorSpace::PluginPrompt
                | CursorSpace::PluginInput
                | CursorSpace::FilePreviewText
                | CursorSpace::FilePreviewInput
                | CursorSpace::LanguageList => None,
                #[cfg(feature = "plugin-probe")]
                CursorSpace::PluginProbe => None,
            };
            if let Some((rows, delta, columns)) = owned {
                self.follow_cursor_scroll(rows, delta, columns, cx);
            }
        }
        let changed = tree_changed
            || document_changed
            || preview_source_changed
            || first_tree_row
                != project
                    .manager
                    .as_ref()
                    .map_or(0, |manager| manager.scroll().first_cell());
        if bound_shift != 0 {
            self.shift_scrollbound_editors(bound_shift, None);
        }
        self.advance_inactive_editor_scrolling() || changed
    }

    /// Synchronize bound editors' relative offsets in the selected Tabpage.
    pub(super) fn sync_scrollbound_editors(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project) = self.projects.active_project() else {
            return;
        };
        let align = project.editor.scrollbind_target().or_else(|| {
            project
                .page_editors()
                .find_map(|(_, editor)| editor.scrollbind_target())
        });
        if let Some(row) = align {
            self.shift_scrollbound_editors(0, Some(row));
            self.repaint([ScrollInput::Advance; 3], window, cx);
        }
    }

    /// Retarget independently animated views using the focused editor's scroll delta.
    fn shift_scrollbound_editors(&mut self, shift: isize, align: Option<usize>) {
        let areas = self.editor_rects();
        let now = self.scroll_epoch.elapsed().as_secs_f64();
        let project = self.projects.active_mut();
        let active = project.active_editor_id();
        for (pane, area) in areas {
            if (pane != active || align.is_some())
                && let Some(editor) = project.editor_by_id_mut(pane)
            {
                editor.scrollbind_rows(area, &self.spacing, shift, align, now);
            }
        }
    }

    /// Keep the animated cursor in its scrolling viewport through `cx`.
    /// `rows`, `delta` and optional `columns` describe the animated movement in cells.
    pub(super) fn follow_cursor_scroll(
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
        if self.help.keyboard_visible() {
            return false;
        }
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
        self.update_canvas_text_layout(window);
        let (x, y, width, insert, visible) = self.caret();
        let space = self.cursor_space();
        let start = if matches!(space, CursorSpace::Start(_)) {
            self.start_layout()
        } else {
            None
        };
        let metrics = start.map_or(self.metrics, |_| StartPage::content_metrics(self.metrics));
        let keyboard_caret = self.help.keyboard_caret(self.help_layout(), self.metrics);
        let start_caret = start
            .zip(self.projects.start())
            .map(|(layout, page)| page.caret(layout, metrics, &self.spacing));
        let (target, width, insert, visible) = if let Some((cell, width)) = start_caret {
            (
                cell.origin,
                width,
                space == CursorSpace::Start(StartFocus::Filter),
                !self.small_modal_open() && space != CursorSpace::Start(StartFocus::Preview),
            )
        } else {
            let target = keyboard_caret.map_or_else(|| metrics.at(x, y), |cell| cell.origin);
            (target, width, insert, visible)
        };
        let viewport = match space {
            CursorSpace::Manager => self.projects.active().manager.as_ref().map(|manager| {
                (
                    &self.bodies[0],
                    ManagerWindow::body(self.layout.tree),
                    manager.scroll(),
                    manager.tree().entries.len(),
                    0.,
                )
            }),
            CursorSpace::Document(_) => {
                let editor = &self.projects.active().editor;
                let area = editor.text_area(self.active_editor_area());
                Some(if let Some(terminal) = editor.terminal() {
                    (
                        &self.bodies[2],
                        area,
                        terminal.scroll(),
                        terminal.line_count(),
                        0.,
                    )
                } else {
                    (
                        &self.bodies[2],
                        area,
                        &editor.buffer().scroll,
                        editor.vim().line_count(),
                        editor.buffer().text_displacement(),
                    )
                })
            }
            CursorSpace::CommandWindow => self
                .command_window
                .as_ref()
                .zip(self.command_window_area())
                .map(|(history, area)| {
                    let buffer = history.editor.buffer();
                    (
                        history.text_panel(),
                        history.editor.text_area(area),
                        &buffer.scroll,
                        buffer.vim.line_count(),
                        buffer.text_displacement(),
                    )
                }),
            CursorSpace::FilePreviewText => self.file_picker.as_ref().and_then(|session| {
                let editor = session.focused_editor()?;
                let buffer = editor.buffer();
                Some((
                    &session.text_panel,
                    editor.text_area(self.file_picker_layout().preview_editor_area()),
                    &buffer.scroll,
                    buffer.vim.line_count(),
                    buffer.text_displacement(),
                ))
            }),
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
            | CursorSpace::CommandWindowPrompt
            | CursorSpace::HelpKeyboard
            | CursorSpace::HelpFilter
            | CursorSpace::FilePicker
            | CursorSpace::FilePickerInput
            | CursorSpace::PluginManager
            | CursorSpace::PluginPrompt
            | CursorSpace::PluginInput
            | CursorSpace::FilePreviewInput
            | CursorSpace::LanguageList => None,
            #[cfg(feature = "plugin-probe")]
            CursorSpace::PluginProbe => None,
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
                | CursorSpace::PluginManager
                | CursorSpace::Start(StartFocus::Projects)
        ) {
            painted_width *= 0.5;
        } else if visible {
            text_cell = keyboard_caret
                .or(start_caret.map(|(cell, _)| cell))
                .or_else(|| {
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
            let floating = self.file_picker_prompt_area();
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
        } else if space == CursorSpace::CommandWindowPrompt {
            let clip = self
                .command_window_area()
                .map(|area| area.pixels(self.metrics));
            (None, clip, clip)
        } else if space == CursorSpace::FilePreviewInput {
            let bounds = self.footer_bounds();
            (
                self.file_picker
                    .as_ref()
                    .map(|session| session.footer_panel.clone()),
                Some(bounds),
                Some(bounds),
            )
        } else if let Some(cell) = keyboard_caret {
            // Clip the binding glyph to its cell while the cursor travels to it.
            let glyph_clip = Some(Bounds::new(
                cell.origin,
                size(px(metrics.cw), px(metrics.lh)),
            ));
            (Some(self.help.body.clone()), glyph_clip, None)
        } else if space == CursorSpace::HelpFilter {
            let clip = Some(self.help_layout().pixels(self.metrics));
            (None, clip, clip)
        } else if space == CursorSpace::LanguageList {
            let clip = self
                .language_picker_area()
                .map(|area| language_list_bounds(area, self.metrics));
            // The choices own nearby-text effects while the cursor can travel across the workspace.
            (Some(self.language_list.clone()), clip, None)
        } else if matches!(
            space,
            CursorSpace::PluginManager | CursorSpace::PluginPrompt | CursorSpace::PluginInput
        ) {
            self.plugin_cursor_panel()
                .map_or((None, None, None), |(panel, area, field)| {
                    (Some(panel), Some(area.pixels(self.metrics)), field)
                })
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
        let height = self
            .canvas_input_caret_bounds()
            .map_or(metrics.lh, |bounds| f32::from(bounds.size.height));
        let geometry = Geometry::new(
            f32::from(target.x),
            f32::from(target.y),
            painted_width,
            height,
        );
        self.cursor.update(cx, |cursor, cx| {
            let component = match space {
                CursorSpace::Document(_)
                | CursorSpace::FilePreviewText
                | CursorSpace::CommandWindow => Some(ThemeComponent::Editor),
                CursorSpace::Manager => Some(ThemeComponent::ExplorerTree),
                CursorSpace::CommandLine
                | CursorSpace::FilePreviewInput
                | CursorSpace::CommandWindowPrompt => Some(ThemeComponent::Footer),
                _ => None,
            };
            cursor.set_theme_component(component, cx);
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
                visible && !self.sleep_hides_caret(),
                cell_width,
                !insert,
                window,
                cx,
            )
        });
        self.last_cursor_space = Some(space);
        window.notify_text_input_changed(&self.focus_handle);
    }

    /// Publish the current document to plugins and repaint `window` through `cx` using `inputs`.
    pub(super) fn repaint(
        &mut self,
        inputs: [ScrollInput; 3],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Explorer opens files through this path to preserve its own scroll position.
        self.sync_plugin_projects();
        self.sync_text_input(window, cx);
        self.resize_visible_terminals();
        #[cfg(windows)]
        if let Some(shortcut) = &self.alt_escape {
            shortcut.set_enabled(self.editor_accepts_alt_escape());
        }
        if self.hovered_controls.is_some() {
            self.hovered_controls = if self.pane_resize.is_some()
                || self
                    .projects
                    .active_project()
                    .is_some_and(|project| project.editor.is_dragging())
            {
                None
            } else {
                self.corner_controls_at(window.mouse_position())
            };
        }
        if self.vim_menu.is_some() {
            self.advance_vim_menu(ScrollInput::Reveal);
        } else if self.command_window.is_some() {
            self.advance_command_window(inputs, cx);
        } else if self.help.is_open() {
            self.advance_help_scrolling(ScrollInput::Advance, cx);
        } else {
            self.advance_scrolling(inputs, cx);
        }
        self.refresh_panels(cx);
        #[cfg(feature = "plugin-probe")]
        self.refresh_probe(cx);
        self.finish_scroll_frame(window, cx);
    }

    /// Apply popup `input`, syncing native input and refreshing its foreground in `window` through `cx`.
    pub(super) fn repaint_help(
        &mut self,
        input: ScrollInput,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sync_text_input(window, cx);
        #[cfg(windows)]
        if let Some(shortcut) = &self.alt_escape {
            shortcut.set_enabled(self.editor_accepts_alt_escape());
        }
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
        self.advance_definition_hover_scroll();
        if self.vim_menu.is_some() {
            self.advance_vim_menu(inputs[1]);
            self.refresh_vim_menu(cx);
        } else if self.command_window.is_some() {
            if self.advance_command_window(inputs, cx) {
                self.refresh_panels(cx);
            }
        } else if self.help.is_open() {
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
    pub(super) fn schedule_scroll(&mut self, window: &Window, cx: &mut Context<Self>) {
        let active = if let Some(menu) = &self.vim_menu {
            menu.scrolling()
        } else if let Some(history) = &self.command_window {
            let buffer = history.editor.buffer();
            buffer.scroll.is_active() || buffer.columns.is_active()
        } else if let Some(list) = &self.help.list {
            list.scroll.is_active()
        } else if let Some(session) = &self.file_picker {
            session.focused_editor().is_some_and(|editor| {
                let buffer = editor.buffer();
                buffer.scroll.is_active() || buffer.columns.is_active()
            })
        } else if let Some(project) = self.projects.active_project() {
            let buffer = project.editor.buffer();
            project
                .manager
                .as_ref()
                .is_some_and(|manager| manager.scroll().is_active())
                || project
                    .editor
                    .terminal()
                    .is_some_and(|terminal| terminal.scroll().is_active())
                || project.editor.markdown_view_mode() != Some(MarkdownViewMode::Preview)
                    && (buffer.scroll.is_active() || buffer.columns.is_active())
                || project
                    .editor
                    .markdown_scroll_active(self.active_editor_area())
                || self.inactive_editor_scroll_active()
        } else {
            false
        };
        if self.scroll_scheduled || !(active || self.definition_hover_scroll_active()) {
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
