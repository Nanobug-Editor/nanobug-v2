//! Two-panel native shell: one filesystem manager, one editable viewport and one global cursor.
use crate::{
    backdrop::BackdropRaster,
    cursor::Cursor,
    drawing::{
        ACCENT, BACKGROUND, CURRENT, CloseGlyph, Frame, GOLD, GRID_FONT_FAMILY, MUTED, Metrics,
        Panel, Rect, SELECTION, TEXT,
    },
    explorer::Explorer,
    file_transfer::{FileTransfers, PathChange, TransferKind},
    shortcuts,
};
use gpui::{
    App, AppContext, Bounds, ClickEvent, Context, Corners, ElementInputHandler, Entity,
    FocusHandle, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, PlatformTextSystem, Point, Render,
    RenderImage, ScrollEvent, Size, StatefulInteractiveElement, StyleRefinement, Styled, Task,
    Window, canvas, div, font, point, px, rgba, size,
};
use nanobug::{
    cursor_lifecycle::CursorConfig,
    cursor_motion::{Effect, Geometry, Vec2},
    document::{Document, display_width},
    keys::normal_character,
    scroll_motion::{GridScroll, ScrollInput},
    vim::{Action, Mode, Register, Vim},
};
use std::{
    ops::Range,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use unicode_segmentation::UnicodeSegmentation;

/// Distance from the character border's centerline to the rounded cutout, in logical pixels.
const HELP_BORDER_GAP: f32 = 12.;
/// Radius of the shortcut popup's outer corners, in logical pixels.
const HELP_CORNER_RADIUS: f32 = 14.;
/// Extra pointer tolerance around the border cells, in logical pixels.
const BORDER_HOVER_SLOP: f32 = 3.;
/// Extra text columns on either side for fractional scrolling and wide glyphs.
const TEXT_OVERSCAN: usize = 4;

/// Pane that owns input when shortcut help is closed.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// Arrow/Enter filesystem navigation.
    Manager,
    /// The active buffer's native Vim state machine.
    Editor,
}

impl Focus {
    /// Return the other pane in the two-pane workspace.
    fn other(self) -> Self {
        match self {
            Self::Manager => Self::Editor,
            Self::Editor => Self::Manager,
        }
    }
}

/// One selectable window inside the editor shell.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ShellWindow {
    /// The file manager or editor pane.
    Pane(Focus),
    /// The shortcut help popup.
    ShortcutHelp,
}

impl ShellWindow {
    /// Check the sole selected window, prioritizing open help over the remembered `pane`.
    fn is_selected(self, pane: Focus, help_open: bool) -> bool {
        let selected = if help_open {
            Self::ShortcutHelp
        } else {
            Self::Pane(pane)
        };
        self == selected
    }
}

/// One open file and its retained viewport, displayed through the single editing area.
pub struct Buffer {
    /// Document and modal state.
    pub vim: Vim,
    /// Continuous viewport position with a whole-row destination.
    scroll: GridScroll,
    /// Continuous horizontal offset with a whole-column destination.
    columns: GridScroll,
}

impl Buffer {
    /// Width of the caret glyph at display column `column`, including a blank at end of line.
    fn caret_width(&self, column: usize) -> usize {
        self.vim.doc.text[self.vim.doc.cursor..]
            .graphemes(true)
            .next()
            .filter(|ch| *ch != "\n" && *ch != "\t")
            .map_or(1, |ch| display_width(ch, column))
    }

    /// Number of retained columns before the first visible text column.
    fn left_padding(&self) -> usize {
        self.columns.first_cell().min(TEXT_OVERSCAN)
    }

    /// Text panel's horizontal displacement from its clip, in display columns.
    fn text_displacement(&self) -> f32 {
        self.left_padding() as f32 + self.columns.fraction()
    }
}

/// Derived cell layout, recomputed only when the window or sidebar width changes.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Layout {
    /// Total grid columns.
    cols: usize,
    /// Total grid rows.
    rows: usize,
    /// Manager viewport, excluding global chrome.
    tree: Rect,
    /// Editing viewport, excluding global chrome.
    editor: Rect,
}

impl Layout {
    /// Scrollable file rows, excluding the title, root path and character border.
    fn tree_body(self) -> Rect {
        Rect {
            x: self.tree.x + 1,
            y: self.tree.y + 2,
            w: self.tree.w.saturating_sub(2),
            h: self.tree.h.saturating_sub(3),
        }
    }
    /// Body-relative name column at `depth`, leaving room for the mark and icon.
    fn tree_name_column(self, depth: usize) -> usize {
        (3 + depth * 2).min(self.tree_body().w.saturating_sub(5))
    }
    /// Scrollable text rows, excluding the character border.
    fn editor_body(self) -> Rect {
        Rect {
            x: self.editor.x + 1,
            y: self.editor.y + 1,
            w: self.editor.w.saturating_sub(2),
            h: self.editor.h.saturating_sub(2),
        }
    }
}

/// Coordinate space owning the logical caret, used to rebase cursor animation during scrolling.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CursorSpace {
    /// File manager viewport.
    Manager,
    /// One document viewport, identified by its buffer index.
    Document(usize),
    /// Fixed command/search row.
    CommandLine,
}

/// Placement of a retained workspace panel shared by the normal and frosted views.
struct GridLayer {
    /// Panel that owns the shaped content.
    panel: Entity<Panel>,
    /// Window-local bounds of the panel's content, including its scroll offset.
    bounds: Bounds<Pixels>,
    /// Fixed viewport for a scrollable body.
    clip: Option<Bounds<Pixels>>,
}

/// One cached, window-local backdrop while shortcut help is open.
struct HelpBackdrop {
    /// Logical size of the window the image covers.
    viewport: Size<Pixels>,
    /// Rounded popup bounds reserved in the image.
    outline: Bounds<Pixels>,
    /// Downsampled, blurred workspace texture.
    image: Arc<RenderImage>,
}

/// Application state. The document owns logical input; cached panels and cursor only draw it.
pub struct Editor {
    /// Native keyboard/IME focus for the whole character work surface.
    pub focus_handle: FocusHandle,
    /// Last selected pane, also used to choose the shortcut help content.
    pub focus: Focus,
    /// Root project directory.
    project: PathBuf,
    /// Lazily expanded filesystem manager.
    tree: Explorer,
    /// Clipboard and undo history for filesystem pastes.
    transfers: FileTransfers,
    /// Pane hidden by a close action, restored by Tab or opening a file.
    hidden_pane: Option<Focus>,
    /// Window whose border currently shows a close glyph.
    hovered_border: Option<ShellWindow>,
    /// First shortcut row shown when the help overlay owns input.
    help_top: Option<usize>,
    /// Native character panel used by shortcut help.
    help_panel: Entity<Panel>,
    /// Rebuilt when the help window's placement or viewport size changes.
    help_backdrop: Option<HelpBackdrop>,
    /// First key of a manager or global two-key sequence.
    prefix: Option<char>,
    /// Pending timeout that distinguishes standalone `z` from viewport commands.
    paste_timer: Option<Task<()>>,
    /// Count accumulated before a file-tree motion.
    tree_count: usize,
    /// Open buffers, retaining unsaved content when another file is selected.
    pub buffers: Vec<Buffer>,
    /// Buffer currently displayed by the sole editor viewport.
    pub active: usize,
    /// Shared unnamed yank/delete register.
    pub register: Register,
    /// Current shared font geometry.
    pub metrics: Metrics,
    /// Close glyph positioned to match the normal text's capital letters.
    close_glyph: CloseGlyph,
    /// Native metrics for the actual glyphs selected by the window's text layout.
    glyphs: Arc<dyn PlatformTextSystem>,
    /// Cell layout for the current native window.
    layout: Layout,
    /// Preferred manager width in cells.
    sidebar: usize,
    /// Header/status, manager and editor cached views.
    panels: [Entity<Panel>; 3],
    /// Independently clipped file rows, line numbers and text, in draw order.
    bodies: [Entity<Panel>; 3],
    /// File manager's continuous scroll position.
    tree_scroll: GridScroll,
    /// Monotonic clock shared by viewport animations.
    scroll_epoch: Instant,
    /// Whether one viewport frame callback is pending.
    scroll_scheduled: bool,
    /// Last logical cursor space, distinguishing viewport motion from focus jumps.
    last_cursor_space: Option<CursorSpace>,
    /// User-selected visual behavior, independent of the document mode.
    cursor_effect: Effect,
    /// Independent full-window cursor layer.
    cursor: Entity<Cursor>,
    /// Native IME preedit, displayed locally and committed to the model once.
    pub composition: String,
    /// Native UTF-16 selection within the preedit string.
    pub composition_selection: Range<usize>,
    /// Current operation feedback.
    status: String,
    /// Whether pointer dragging is resizing the sidebar.
    resizing: bool,
    /// Initial logical cursor and pointer origin; tiny click jitter must not start visual selection.
    dragging: Option<(usize, gpui::Point<gpui::Pixels>)>,
    /// Tree item selected by the first press of a potential double-click.
    tree_click: Option<PathBuf>,
    /// Accumulated vertical wheel delta, supporting high-resolution touchpads.
    wheel: f32,
    /// Fractional horizontal wheel steps, kept separately from vertical steps.
    horizontal_wheel: f32,
}

impl Editor {
    /// Load `project` and optional `initial` file, using the window's `glyphs` for ink measurements.
    pub fn new(
        project: PathBuf,
        initial: Option<PathBuf>,
        glyphs: Arc<dyn PlatformTextSystem>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let project = std::fs::canonicalize(&project).unwrap_or(project);
        let initial = initial.unwrap_or_else(|| project.join("README.md"));
        let (doc, status) = if initial.is_file() {
            match Document::open(&initial) {
                Ok(doc) => (doc, String::new()),
                Err(error) => (Document::new(""), format!("{error:#}")),
            }
        } else {
            (
                Document::new(""),
                "i 开始编辑 · :w 文件名 保存 · Tab 切换区域".into(),
            )
        };
        let system = window.text_system();
        let font_id = system.resolve_font(&font(GRID_FONT_FAMILY));
        let font_size = 16.;
        let scale = window.scale_factor();
        let advance = f32::from(
            system
                .advance(font_id, px(font_size), 'M')
                .expect("font advance")
                .width,
        );
        let height = f32::from(system.ascent(font_id, px(font_size))).abs()
            + f32::from(system.descent(font_id, px(font_size))).abs();
        let metrics = Metrics {
            cw: (advance * scale).ceil() / scale,
            lh: (height * scale).ceil() / scale,
            font_size,
            origin: point(px(14.), px(5.)),
        };
        let close_glyph = CloseGlyph::new(metrics, font_id, glyphs.as_ref(), window);
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
        let weak = cx.entity().downgrade();
        window.on_window_should_close(cx, move |_, cx| {
            weak.update(cx, |editor, cx| {
                if editor
                    .buffers
                    .iter()
                    .any(|buffer| buffer.vim.doc.modified())
                {
                    editor.status = "有未保存的修改。使用 :w 保存，或 :q! 放弃修改并退出。".into();
                    cx.notify();
                    false
                } else {
                    true
                }
            })
            .unwrap_or(true)
        });
        let (config, status) = match CursorConfig::load(&project) {
            Ok(config) => (config, status),
            Err(error) => (CursorConfig::default(), format!("光标配置：{error}")),
        };
        let cursor_effect = Effect::Afterglow;
        Self {
            focus_handle,
            focus: Focus::Editor,
            project: project.clone(),
            tree: Explorer::new(project),
            transfers: FileTransfers::default(),
            hidden_pane: None,
            hovered_border: None,
            help_top: None,
            help_panel: cx.new(|_| Panel::new(metrics)),
            help_backdrop: None,
            prefix: None,
            paste_timer: None,
            tree_count: 0,
            buffers: vec![Buffer {
                vim: Vim::new(doc),
                scroll: GridScroll::default(),
                columns: GridScroll::default(),
            }],
            active: 0,
            register: Register::default(),
            metrics,
            close_glyph,
            glyphs,
            layout: Layout::default(),
            sidebar: 28,
            panels: [
                cx.new(|_| Panel::new(metrics)),
                cx.new(|_| Panel::new(metrics)),
                cx.new(|_| Panel::new(metrics)),
            ],
            bodies: [
                cx.new(|_| Panel::new(metrics)),
                cx.new(|_| Panel::new(metrics)),
                cx.new(|_| Panel::new(metrics)),
            ],
            tree_scroll: GridScroll::default(),
            scroll_epoch: Instant::now(),
            scroll_scheduled: false,
            last_cursor_space: None,
            cursor_effect,
            cursor: cx.new(|_| Cursor::new(cursor_effect, config)),
            composition: String::new(),
            composition_selection: 0..0,
            status,
            resizing: false,
            dragging: None,
            tree_click: None,
            wheel: 0.,
            horizontal_wheel: 0.,
        }
    }

    /// Return the currently displayed modal buffer.
    pub fn vim(&self) -> &Vim {
        &self.buffers[self.active].vim
    }

    /// Width of the dynamic line-number gutter in character cells.
    fn gutter(&self) -> usize {
        self.vim().doc.starts.len().to_string().len().max(4) + 3
    }

    /// Resolve this window's current dimensions to the two panel rectangles.
    fn derive_layout(&self, window: &Window) -> Layout {
        let size = window.viewport_size();
        let cols = ((f32::from(size.width) - 2. * f32::from(self.metrics.origin.x))
            / self.metrics.cw)
            .floor()
            .max(60.) as usize;
        let rows = ((f32::from(size.height) - 2. * f32::from(self.metrics.origin.y))
            / self.metrics.lh)
            .floor()
            .max(16.) as usize;
        let sidebar = match self.hidden_pane {
            Some(Focus::Manager) => 0,
            Some(Focus::Editor) => cols,
            None => self.sidebar.clamp(16, cols.saturating_sub(30).max(16)),
        };
        let editor_x = sidebar + usize::from(self.hidden_pane.is_none());
        Layout {
            cols,
            rows,
            tree: Rect {
                x: 0,
                y: 1,
                w: sidebar,
                h: rows - 3,
            },
            editor: Rect {
                x: editor_x,
                y: 1,
                w: cols - editor_x,
                h: rows - 3,
            },
        }
    }

    /// Text viewport excluding the line numbers, which scroll vertically only.
    fn editor_text(&self) -> Rect {
        let body = self.layout.editor_body();
        let gutter = (self.gutter() - 1).min(body.w);
        Rect {
            x: body.x + gutter,
            w: body.w - gutter,
            ..body
        }
    }

    /// Resolve the logical caret using actual continuous viewport positions, never animated cursor geometry.
    pub fn caret(&self) -> (f32, f32, usize, bool, bool) {
        if self.focus == Focus::Manager {
            let body = self.layout.tree_body();
            let y = body.y as f32 + self.tree.selected as f32 - self.tree_scroll.position();
            // The row caret shares one column across every nesting depth and input method.
            return (
                body.x as f32,
                y,
                1,
                false,
                !self.tree.entries.is_empty()
                    && y >= body.y as f32
                    && y + 1. <= (body.y + body.h) as f32 + 0.001,
            );
        }
        let buffer = &self.buffers[self.active];
        let vim = &buffer.vim;
        if matches!(vim.mode, Mode::Command | Mode::Search) {
            return (
                (2 + display_width(&vim.command, 0)).min(self.layout.cols - 2) as f32,
                (self.layout.rows - 1) as f32,
                1,
                true,
                true,
            );
        }
        let body = self.editor_text();
        let (row, col) = vim.doc.location(vim.doc.cursor);
        let insert = vim.mode == Mode::Insert;
        let width = if insert {
            1
        } else {
            buffer.caret_width(col).min(body.w.max(1))
        };
        let x = (col as isize - buffer.columns.first_cell() as isize) as f32
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
                && x + width as f32 <= body.w as f32 + 0.001,
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
    pub fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        let buffer = &mut self.buffers[self.active];
        let mut location = (!matches!(buffer.vim.mode, Mode::Command | Mode::Search))
            .then(|| buffer.vim.doc.location(buffer.vim.doc.cursor));
        let row = location.map(|(row, _)| row);
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
            location = Some(buffer.vim.doc.location(buffer.vim.doc.cursor));
        }
        let caret = location.filter(|_| text.w > 0).map(|(_, column)| {
            let width = if buffer.vim.mode == Mode::Insert {
                1
            } else {
                buffer.caret_width(column)
            };
            column..column + width
        });
        let columns = match column_input {
            ScrollInput::Wheel(_) => {
                // A short caret line should not prevent panning a long visible line.
                let first = buffer.scroll.first_cell();
                let last = first + text.h + usize::from(buffer.scroll.fraction() > 0.);
                (first..last.min(buffer.vim.doc.starts.len()))
                    .map(|row| display_width(buffer.vim.doc.line(row), 0) + 1)
                    .max()
            }
            ScrollInput::Reveal => {
                location.map(|(row, _)| display_width(buffer.vim.doc.line(row), 0) + 1)
            }
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

    /// Update the workspace and shortcut-help frames through `cx`.
    fn refresh_panels(&mut self, cx: &mut Context<Self>) {
        for (panel, frame) in self
            .panels
            .iter()
            .chain(self.bodies.iter())
            .zip(self.frames())
        {
            panel.update(cx, |panel, cx| panel.set(frame, cx));
        }
        if let Some(top) = self.help_top {
            let (area, pane_rows) = self.help_layout();
            let count = pane_rows.len() + shortcuts::GLOBAL.len();
            let top = top.min(count.saturating_sub(area.h - 3));
            self.help_top = Some(top);
            let mut frame = Frame::new(area.w, area.h);
            let title = if self.focus == Focus::Manager {
                "文件管理器快捷键"
            } else {
                "编辑区快捷键"
            };
            self.window_border(&mut frame, ShellWindow::ShortcutHelp, title);
            for (row, (key, description)) in pane_rows
                .iter()
                .chain(shortcuts::GLOBAL)
                .skip(top)
                .take(area.h - 3)
                .enumerate()
            {
                frame.text(2, row + 1, key, GOLD, 20);
                frame.text(23, row + 1, description, TEXT, area.w - 25);
            }
            frame.text(
                2,
                area.h - 2,
                "j/k 或滚轮浏览  Esc / qq 关闭",
                MUTED,
                area.w - 4,
            );
            self.help_panel.update(cx, |panel, cx| panel.set(frame, cx));
        }
    }

    /// Return the centered help bounds and the focused pane's rows before shared shortcuts.
    fn help_layout(&self) -> (Rect, &'static [(&'static str, &'static str)]) {
        let pane_rows = if self.focus == Focus::Manager {
            shortcuts::MANAGER
        } else {
            shortcuts::EDITOR
        };
        let w = (self.layout.cols - 4).min(74);
        let h = (self.layout.rows - 4).min(pane_rows.len() + shortcuts::GLOBAL.len() + 3);
        (
            Rect {
                x: (self.layout.cols - w) / 2,
                y: (self.layout.rows - h) / 2,
                w,
                h,
            },
            pane_rows,
        )
    }

    /// Draw `target`'s `title` in `frame` and reserve the close slot during border hover.
    fn window_border(&self, frame: &mut Frame, target: ShellWindow, title: &str) {
        frame.border(
            title,
            target.is_selected(self.focus, self.help_top.is_some()),
            self.hovered_border == Some(target),
        );
    }

    /// Move the overlay by `delta` rows and update its retained panel through `cx`.
    fn scroll_help(&mut self, delta: isize, cx: &mut Context<Self>) {
        if let Some(top) = self.help_top {
            self.help_top = Some(top.saturating_add_signed(delta));
            self.refresh_panels(cx);
            cx.notify();
        }
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
        let mut painted_width = cell_width * width as f32;
        if insert {
            painted_width = 2.;
        } else if visible
            && width > 1
            && let Some((panel, rect, scroll, _, displacement)) = viewport
        {
            // Undo both viewport offsets to find the glyph's retained cell.
            let row = (y - rect.y as f32 + scroll.fraction()).round() as usize;
            let column = (x - rect.x as f32 + displacement).round() as usize;
            if let Some(ink) = panel
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
    fn repaint(&mut self, inputs: [ScrollInput; 3], window: &mut Window, cx: &mut Context<Self>) {
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
    fn scroll_frame(
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

    /// Derive compact chrome, fixed round character borders and independently scrollable bodies.
    fn frames(&self) -> [Frame; 6] {
        let layout = self.layout;
        let buffer = &self.buffers[self.active];
        let vim = &buffer.vim;
        let help_open = self.help_top.is_some();
        let help_selected = ShellWindow::ShortcutHelp.is_selected(self.focus, help_open);
        let active = ShellWindow::Pane(Focus::Editor).is_selected(self.focus, help_open);
        let selection_tint = if help_selected { CURRENT } else { SELECTION };
        let project_name = self
            .project
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        let filename = vim
            .doc
            .path
            .as_ref()
            .map(|p| {
                p.strip_prefix(&self.project)
                    .unwrap_or(p)
                    .to_string_lossy()
                    .into_owned()
            })
            .unwrap_or("[Untitled]".into());
        let modified = if vim.doc.modified() { " [+]" } else { "" };
        let (row, column) = vim.doc.location(vim.doc.cursor);
        let mut chrome = Frame::new(layout.cols, layout.rows);
        chrome.text(1, 0, "NANOBUG", ACCENT, 8);
        chrome.text(12, 0, &project_name, MUTED, layout.cols.saturating_sub(14));
        chrome.text(
            1,
            layout.rows - 2,
            if self.focus == Focus::Manager {
                "FILES"
            } else {
                vim.mode.label()
            },
            if help_selected { MUTED } else { ACCENT },
            8,
        );
        let position = format!(
            "{}  UTF-8 {}:{}",
            self.cursor_effect.label(),
            row + 1,
            column + 1
        );
        let position_x = layout.cols.saturating_sub(display_width(&position, 0) + 1);
        chrome.text(
            10,
            layout.rows - 2,
            &format!("{filename}{modified}"),
            TEXT,
            position_x.saturating_sub(11),
        );
        chrome.text(
            position_x,
            layout.rows - 2,
            &position,
            GOLD,
            layout.cols - position_x,
        );
        let command = if self.hidden_pane != Some(Focus::Editor)
            && matches!(vim.mode, Mode::Command | Mode::Search)
        {
            format!(
                "{}{}{}",
                if vim.mode == Mode::Command { ':' } else { '/' },
                vim.command,
                self.composition
            )
        } else if !self.status.is_empty() {
            self.status.clone()
        } else if !vim.message.is_empty() {
            vim.message.clone()
        } else {
            "Tab 区域  i 编辑  Esc 普通模式  :w 保存  :q 退出  F5 刷新  F6 动效  F7 配置".into()
        };
        chrome.text(1, layout.rows - 1, &command, ACCENT, layout.cols - 2);
        let mut tree = Frame::new(layout.tree.w, layout.tree.h);
        self.window_border(&mut tree, ShellWindow::Pane(Focus::Manager), "FILES");
        tree.text(
            2,
            1,
            &format!("{project_name}/"),
            GOLD,
            layout.tree.w.saturating_sub(4),
        );
        let tree_rect = layout.tree_body();
        let first = self.tree_scroll.first_cell();
        let mut tree_body = Frame::viewport(tree_rect.w, tree_rect.h + 1, first);
        for (i, entry) in self
            .tree
            .entries
            .iter()
            .enumerate()
            .skip(first)
            .take(tree_rect.h + 1)
        {
            let y = i - first;
            let x = layout.tree_name_column(entry.depth);
            let selected = i == self.tree.selected;
            if selected {
                tree_body.tint(y, 0, tree_rect.w, selection_tint);
            }
            if self.tree.marked.contains(&entry.path) {
                tree_body.text(0, y, "●", GOLD, 1);
            }
            let color = if selected && !help_selected {
                ACCENT
            } else {
                TEXT
            };
            let icon = entry.icon(self.tree.expanded.contains(&entry.path));
            tree_body.icon(x.saturating_sub(2), y, icon, color);
            tree_body.text(
                x,
                y,
                &entry.path.file_name().unwrap_or_default().to_string_lossy(),
                color,
                tree_rect.w.saturating_sub(x + 1),
            );
        }
        let mut edit = Frame::new(layout.editor.w, layout.editor.h);
        self.window_border(
            &mut edit,
            ShellWindow::Pane(Focus::Editor),
            &format!("{filename}{modified}"),
        );
        let body = self.editor_text();
        let gutter = body.x - layout.editor_body().x;
        let first = buffer.scroll.first_cell();
        let mut numbers = Frame::viewport(gutter, body.h + 1, first);
        let left = buffer.columns.first_cell() - buffer.left_padding();
        let width = body.w + buffer.left_padding() + TEXT_OVERSCAN;
        let right = left + width;
        let mut text = Frame::viewport(width, body.h + 1, first);
        let selection = vim.selection();
        for visible in 0..=body.h {
            let line = first + visible;
            if line >= vim.doc.starts.len() {
                numbers.text(2, visible, "~", MUTED, 1);
                continue;
            }
            let current = row == line && active;
            if current {
                numbers.tint(visible, 0, gutter, CURRENT);
                text.tint(visible, 0, width, CURRENT);
            }
            numbers.text(
                0,
                visible,
                &format!("{:>width$} │", line + 1, width = gutter.saturating_sub(2)),
                if current { GOLD } else { MUTED },
                gutter,
            );
            let mut col = 0;
            for (byte, ch) in vim.doc.line(line).grapheme_indices(true) {
                let cells = display_width(ch, col);
                let next = col + cells;
                if col >= right {
                    break;
                }
                if col >= left && next <= right {
                    let x = col - left;
                    text.text(x, visible, if ch == "\t" { " " } else { ch }, TEXT, cells);
                    if selection
                        .as_ref()
                        .is_some_and(|range| range.contains(&(vim.doc.starts[line] + byte)))
                    {
                        text.tint(visible, x, x + cells, selection_tint);
                    }
                }
                col = next;
            }
        }
        if !self.composition.is_empty() && active && vim.mode == Mode::Insert {
            let (row, col) = vim.doc.location(vim.doc.cursor);
            if row >= first && row <= first + body.h && col >= left {
                let x = col - left;
                let y = row - first;
                text.text(x, y, &self.composition, GOLD, width.saturating_sub(x));
                text.tint(y, x, x + display_width(&self.composition, col), 0xc9a3f251);
            }
        }
        [chrome, tree, edit, tree_body, numbers, text]
    }

    /// Open `path` in the sole editor viewport, retaining all previous unsaved buffers.
    fn open(&mut self, path: PathBuf) {
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        if let Some(index) = self
            .buffers
            .iter()
            .position(|b| b.vim.doc.path.as_ref() == Some(&path))
        {
            self.active = index;
        } else {
            match Document::open(&path) {
                Ok(doc) => {
                    self.buffers.push(Buffer {
                        vim: Vim::new(doc),
                        scroll: GridScroll::default(),
                        columns: GridScroll::default(),
                    });
                    self.active = self.buffers.len() - 1;
                }
                Err(error) => {
                    self.status = format!("{error:#}");
                    return;
                }
            }
        }
        let now = self.scroll_epoch.elapsed().as_secs_f64();
        let buffer = &mut self.buffers[self.active];
        buffer.scroll.snap(buffer.scroll.first_cell(), now);
        buffer.columns.snap(buffer.columns.first_cell(), now);
        self.last_cursor_space = None;
        self.focus = Focus::Editor;
        if self.hidden_pane == Some(Focus::Editor) {
            self.hidden_pane = None;
        }
        self.composition.clear();
        self.status.clear();
    }

    /// Execute `action` requested by the modal model, preserving buffers on I/O errors.
    fn action(&mut self, action: Action, window: &mut Window) {
        match action {
            Action::None => {}
            Action::Open(name) => self.open(self.project.join(name)),
            Action::Save(name) => {
                let path = name.map(|name| self.project.join(name));
                match self.save(path) {
                    Ok(()) => {
                        self.status = "已保存".into();
                        let _ = self.tree.refresh();
                    }
                    Err(error) => self.status = format!("{error:#}"),
                }
            }
            Action::SaveClose => match self.save(None) {
                Ok(()) => self.action(Action::Close(false), window),
                Err(error) => self.status = format!("{error:#}"),
            },
            Action::Close(force) => {
                if force || !self.buffers.iter().any(|buffer| buffer.vim.doc.modified()) {
                    window.remove_window();
                } else {
                    self.status = "有未保存的修改。使用 :w 保存，或 :q! 放弃修改并退出。".into();
                }
            }
        }
    }

    /// Save the active document to `path` or its existing path, then invalidate any replaced paste.
    fn save(&mut self, path: Option<PathBuf>) -> anyhow::Result<()> {
        let doc = &mut self.buffers[self.active].vim.doc;
        let previous = doc.path.clone();
        let dirty = doc.modified();
        doc.save(path.as_deref())?;
        if (dirty || doc.path != previous)
            && let Some(path) = &doc.path
        {
            self.transfers.invalidate(path);
        }
        Ok(())
    }

    /// Apply a text or key operation `input` and dispatch the resulting action in `window`.
    fn run_vim(
        &mut self,
        input: impl FnOnce(&mut Vim, &mut Register) -> Action,
        window: &mut Window,
    ) {
        let vim = &mut self.buffers[self.active].vim;
        let before = vim.doc.revision;
        let action = input(vim, &mut self.register);
        if vim.doc.revision != before
            && let Some(path) = &vim.doc.path
        {
            self.transfers.invalidate(path);
        }
        self.action(action, window);
    }

    /// Close `target` and return input to the remaining pane, guarding the last pane's exit.
    fn close_grid_window(&mut self, target: ShellWindow, window: &mut Window) {
        match target {
            ShellWindow::ShortcutHelp => self.help_top = None,
            ShellWindow::Pane(_) if self.hidden_pane.is_some() => {
                self.action(Action::Close(false), window);
                return;
            }
            ShellWindow::Pane(pane) => {
                if pane == Focus::Editor {
                    self.buffers[self.active].vim.cancel_pending();
                    self.composition.clear();
                    self.composition_selection = 0..0;
                }
                self.hidden_pane = Some(pane);
                self.focus = pane.other();
                self.status = "已关闭窗格，按 Tab 恢复".into();
            }
        }
        // Require a fresh hover before exposing the close button on an expanded pane.
        self.hovered_border = None;
    }

    /// Apply the paste operation identified by `key` and refresh the visible paths.
    fn transfer(&mut self, key: &str) {
        let dirty: Vec<_> = self
            .buffers
            .iter()
            .filter(|buffer| buffer.vim.doc.modified())
            .filter_map(|buffer| buffer.vim.doc.path.clone())
            .collect();
        let directory = self.tree.directory().to_path_buf();
        let result = if key == "v" {
            self.transfers.paste(&self.project, &directory, &dirty)
        } else {
            self.transfers.replay(key == "Z", &dirty)
        };
        self.status = match result {
            Ok(changes) => {
                self.apply_path_changes(&changes);
                if key == "v" && directory != self.project {
                    self.tree.expanded.insert(directory);
                }
                match key {
                    "v" => "已粘贴",
                    "z" => "已撤销粘贴",
                    _ => "已重做粘贴",
                }
                .into()
            }
            Err(error) => format!("{error:#}"),
        };
        if let Err(error) = self.tree.refresh() {
            self.status.push_str(&format!(" · 刷新失败：{error:#}"));
        }
    }

    /// Retarget buffers after moves in `changes` and drop stale views of replaced paths.
    fn apply_path_changes(&mut self, changes: &[PathChange]) {
        if changes.is_empty() {
            return;
        }
        let mut previous_index = 0;
        let mut active = self.active;
        self.buffers.retain_mut(|buffer| {
            let index = previous_index;
            previous_index += 1;
            if let Some((change, relative)) = buffer.vim.doc.path.as_ref().and_then(|path| {
                changes.iter().find_map(|change| {
                    path.strip_prefix(&change.from)
                        .ok()
                        .map(|relative| (change, relative))
                })
            }) {
                if let Some(to) = &change.to {
                    let path = if relative.as_os_str().is_empty() {
                        to.clone()
                    } else {
                        to.join(relative)
                    };
                    buffer.vim.doc.path = Some(path);
                    return true;
                }
            } else if !buffer.vim.doc.path.as_ref().is_some_and(|path| {
                changes
                    .iter()
                    .any(|change| change.to.as_ref().is_some_and(|to| path.starts_with(to)))
            }) {
                return true;
            }
            active -= usize::from(index < self.active);
            false
        });
        if self.buffers.is_empty() {
            self.buffers.push(Buffer {
                vim: Vim::new(Document::new("")),
                scroll: GridScroll::default(),
                columns: GridScroll::default(),
            });
        }
        self.active = active.min(self.buffers.len() - 1);
        self.last_cursor_space = None;
    }

    /// Interpret a manager `key` after a possible `prefix`, returning its viewport intent.
    /// `window` and `cx` schedule a standalone undo when the second key of `z` does not arrive.
    fn manager_key(
        &mut self,
        key: &str,
        prefix: Option<char>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<ScrollInput> {
        let height = self.layout.tree_body().h;
        if prefix == Some('z') {
            let inset = match key {
                "t" => Some(0),
                "z" => Some(height.saturating_sub(1) / 2),
                "b" => Some(height.saturating_sub(1)),
                _ => None,
            };
            if let Some(inset) = inset {
                return Some(ScrollInput::Position(
                    self.tree.selected.saturating_sub(inset),
                ));
            }
            if key != "escape" {
                self.transfer("z");
            }
        }
        if let [digit @ b'0'..=b'9'] = key.as_bytes() {
            self.tree_count = (self.tree_count * 10 + usize::from(*digit - b'0')).min(9999);
            return Some(ScrollInput::Advance);
        }
        let count = std::mem::take(&mut self.tree_count).max(1);
        match key {
            "k" | "up" => self.tree.selected = self.tree.selected.saturating_sub(count),
            "j" | "down" => {
                self.tree.selected = self
                    .tree
                    .selected
                    .saturating_add(count)
                    .min(self.tree.entries.len().saturating_sub(1))
            }
            "home" => self.tree.selected = 0,
            "g" if prefix == Some('g') => self.tree.selected = 0,
            "g" => self.prefix = Some('g'),
            "G" | "end" => self.tree.selected = self.tree.entries.len().saturating_sub(1),
            "h" | "left" => self.tree.branch(false),
            "right" => self.tree.branch(true),
            "l" | "enter" => {
                if let Some(path) = self.tree.activate() {
                    self.open(path);
                }
            }
            "s" => self.status = format!("已标记 {} 项", self.tree.toggle_mark()),
            "escape" => {
                self.tree.marked.clear();
                self.status.clear();
            }
            "c" | "x" => {
                let kind = if key == "c" {
                    TransferKind::Copy
                } else {
                    TransferKind::Cut
                };
                self.status = match self.transfers.capture(&self.tree.marked, kind) {
                    Ok(count) => format!(
                        "已{} {count} 项，到目标目录按 v",
                        if key == "c" { "复制" } else { "剪切" }
                    ),
                    Err(error) => error.to_string(),
                };
            }
            "v" | "Z" => self.transfer(key),
            "z" => {
                self.prefix = Some('z');
                let delay = cx.background_executor().timer(Duration::from_millis(350));
                self.paste_timer = Some(cx.spawn_in(window, async move |weak, cx| {
                    delay.await;
                    let _ = cx.update(|window, cx| {
                        let _ = weak.update(cx, |editor, cx| {
                            if editor.prefix.take() == Some('z') {
                                editor.transfer("z");
                                editor.repaint([ScrollInput::Reveal; 3], window, cx);
                            }
                        });
                    });
                }));
            }
            _ if prefix == Some('z') => {}
            _ => return None,
        }
        Some(ScrollInput::Reveal)
    }

    /// Route committed native `text` solely to the editor's Vim state machine.
    pub fn commit_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.focus != Focus::Editor {
            return;
        }
        self.cursor
            .update(cx, |cursor, cx| cursor.activity(window, cx));
        self.status.clear();
        let text = text.replace("\r\n", "\n");
        self.run_vim(|vim, register| vim.text(&text, register), window);
        self.sync(window, cx);
    }

    /// Dispatch normal-mode ASCII before IME; insert/search/command text uses the native input path.
    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.cursor
            .update(cx, |cursor, cx| cursor.activity(window, cx));
        let raw = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        let character = normal_character(&event.keystroke);
        let printable = character.as_deref().unwrap_or(raw);
        let vim_keys = self.focus == Focus::Manager
            || matches!(
                self.vim().mode,
                Mode::Normal | Mode::Visual | Mode::VisualLine
            );
        if event.is_held
            && ((vim_keys && matches!(character.as_deref(), Some("q" | "X")))
                || (self.focus == Focus::Manager
                    && matches!(character.as_deref(), Some("z" | "v" | "Z"))))
        {
            cx.stop_propagation();
            return;
        }
        let prefix = self.prefix.take();
        self.paste_timer = None;
        if self.help_top.is_some() {
            match printable {
                "escape" | "X" => self.close_grid_window(ShellWindow::ShortcutHelp, window),
                "q" if prefix == Some('q') => {
                    self.close_grid_window(ShellWindow::ShortcutHelp, window);
                }
                "q" => self.prefix = Some('q'),
                "j" | "down" => self.scroll_help(1, cx),
                "k" | "up" => self.scroll_help(-1, cx),
                _ => {}
            }
            if self.help_top.is_none() {
                self.repaint([ScrollInput::Advance; 3], window, cx);
            }
            cx.stop_propagation();
            return;
        }
        if vim_keys && matches!(character.as_deref(), Some("X" | "q" | "?")) {
            self.tree_count = 0;
            if self.focus == Focus::Editor {
                self.buffers[self.active].vim.cancel_pending();
            }
            match printable {
                "?" => {
                    self.help_top = Some(0);
                    self.resizing = false;
                    self.dragging = None;
                }
                "X" => self.close_grid_window(ShellWindow::Pane(self.focus), window),
                "q" if prefix == Some('q') => {
                    self.close_grid_window(ShellWindow::Pane(self.focus), window);
                }
                _ => self.prefix = Some('q'),
            }
            self.sync(window, cx);
            cx.stop_propagation();
            return;
        }
        if self.focus == Focus::Editor
            && (modifiers.control || modifiers.platform)
            && raw == "v"
            && matches!(self.vim().mode, Mode::Insert | Mode::Command | Mode::Search)
        {
            if let Some(item) = cx.read_from_clipboard()
                && let Some(text) = item.text()
            {
                self.composition.clear();
                self.composition_selection = 0..0;
                self.commit_text(&text, window, cx);
            }
            cx.stop_propagation();
            return;
        }
        if !self.composition.is_empty() && !modifiers.control && !modifiers.platform {
            return;
        }
        if raw == "f6" {
            self.cursor_effect = self
                .cursor
                .update(cx, |cursor, cx| cursor.cycle(modifiers.shift, window, cx));
            self.status = format!(
                "光标动效：{} · F6 下一种，Shift+F6 上一种",
                self.cursor_effect.label()
            );
            self.repaint([ScrollInput::Advance; 3], window, cx);
            cx.stop_propagation();
            return;
        } else if raw == "f7" {
            self.status = match CursorConfig::load(&self.project) {
                Ok(config) => {
                    self.cursor
                        .update(cx, |cursor, cx| cursor.configure(config, window, cx));
                    "已重载光标效果配置".into()
                }
                Err(error) => format!("光标配置：{error}"),
            };
            self.repaint([ScrollInput::Advance; 3], window, cx);
            cx.stop_propagation();
            return;
        } else if raw == "f5" {
            self.status = match self.tree.refresh() {
                Ok(()) => "项目已刷新".into(),
                Err(error) => format!("{error:#}"),
            };
            self.repaint([ScrollInput::Advance; 3], window, cx);
            cx.stop_propagation();
            return;
        } else if raw == "tab" && (self.focus == Focus::Manager || self.vim().mode != Mode::Insert)
        {
            self.tree_count = 0;
            if self.focus == Focus::Editor {
                self.buffers[self.active].vim.cancel_pending();
            }
            if self.hidden_pane.take().is_some() {
                self.status.clear();
            }
            self.focus = self.focus.other();
        } else if self.focus == Focus::Manager {
            let key = if modifiers.control || modifiers.alt || modifiers.platform {
                ""
            } else {
                printable
            };
            if let Some(input) = self.manager_key(key, prefix, window, cx) {
                self.layout = self.derive_layout(window);
                self.repaint(
                    [input, ScrollInput::Reveal, ScrollInput::Reveal],
                    window,
                    cx,
                );
                cx.stop_propagation();
            }
            return;
        } else {
            let key = if modifiers.control || modifiers.platform {
                format!("Ctrl+{raw}")
            } else {
                match raw {
                    "escape" => "Escape".into(),
                    "enter" => "Enter".into(),
                    "backspace" => "Backspace".into(),
                    "delete" => "Delete".into(),
                    "tab" => "Tab".into(),
                    "left" => "Left".into(),
                    "right" => "Right".into(),
                    "up" => "Up".into(),
                    "down" => "Down".into(),
                    "home" => "Home".into(),
                    "end" => "End".into(),
                    "pageup" | "pagedown" => {
                        let direction = if raw == "pageup" { "Up" } else { "Down" };
                        let height = self.layout.editor.h.saturating_sub(2);
                        self.run_vim(
                            |vim, register| {
                                for _ in 0..height {
                                    vim.key(direction, register);
                                }
                                Action::None
                            },
                            window,
                        );
                        self.sync(window, cx);
                        cx.stop_propagation();
                        return;
                    }
                    _ if matches!(
                        self.vim().mode,
                        Mode::Normal | Mode::Visual | Mode::VisualLine
                    ) =>
                    {
                        let Some(character) = character else {
                            return;
                        };
                        character
                    }
                    _ => return,
                }
            };
            self.status.clear();
            self.run_vim(|vim, register| vim.key(&key, register), window);
        }
        self.sync(window, cx);
        cx.stop_propagation();
    }

    /// Hit-test the actually displayed fractional viewport at native `position`.
    fn editor_offset(&self, position: gpui::Point<gpui::Pixels>) -> usize {
        let buffer = &self.buffers[self.active];
        let body = self.editor_text();
        let row = (f32::from(position.y - self.metrics.origin.y) / self.metrics.lh - body.y as f32
            + buffer.scroll.position())
        .floor()
        .max(0.) as usize;
        let x = f32::from(position.x - self.metrics.at(body.x as f32, 0.).x) / self.metrics.cw;
        let column =
            buffer.columns.first_cell() + (x.max(0.) + buffer.columns.fraction()).floor() as usize;
        buffer.vim.doc.offset(row, column)
    }

    /// Find the visible window whose border is near the window-local pointer `position`.
    fn border_at(&self, position: Point<Pixels>) -> Option<ShellWindow> {
        let near = |target| near_border(self.window_area(target), self.metrics, position);
        if self.help_top.is_some() {
            return near(ShellWindow::ShortcutHelp).then_some(ShellWindow::ShortcutHelp);
        }
        [Focus::Manager, Focus::Editor]
            .map(ShellWindow::Pane)
            .into_iter()
            .find(|&target| near(target))
    }

    /// Return `target`'s grid rectangle, with zero width for a hidden pane.
    fn window_area(&self, target: ShellWindow) -> Rect {
        match target {
            ShellWindow::Pane(Focus::Manager) => self.layout.tree,
            ShellWindow::Pane(Focus::Editor) => self.layout.editor,
            ShellWindow::ShortcutHelp => self.help_layout().0,
        }
    }

    /// Update the close glyph and its hit target through `cx` when the hovered `target` changes.
    fn set_border_hover(&mut self, target: Option<ShellWindow>, cx: &mut Context<Self>) {
        if self.hovered_border != target {
            self.hovered_border = target;
            self.refresh_panels(cx);
            cx.notify();
        }
    }

    /// Activate `target`'s close button from `event` and repaint `window` through `cx`.
    fn click_close_button(
        &mut self,
        target: ShellWindow,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // After a pane expands, its close button can be under the second press of a double click.
        if self.hovered_border != Some(target)
            || matches!(event, ClickEvent::Mouse(click) if click.down.click_count > 1)
        {
            return;
        }
        self.prefix = None;
        self.paste_timer = None;
        self.tree_count = 0;
        self.tree_click = None;
        self.close_grid_window(target, window);
        self.sync(window, cx);
        cx.stop_propagation();
    }

    /// Use `event` to select or activate an item, place the editor cursor, or begin resizing.
    /// `window` and `cx` update the focus and repaint the selected viewport.
    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.prefix = None;
        self.paste_timer = None;
        self.tree_count = 0;
        if self.help_top.is_some() {
            cx.stop_propagation();
            return;
        }
        self.cursor
            .update(cx, |cursor, cx| cursor.activity(window, cx));
        window.focus(&self.focus_handle, cx);
        let first_click = self.tree_click.take();
        let (x, y) = self.metrics.cell(event.position);
        if self.hidden_pane.is_none()
            && y >= self.layout.tree.y
            && y < self.layout.tree.y + self.layout.tree.h
            && (x == self.layout.tree.w || x == self.layout.tree.w.saturating_sub(1))
        {
            self.resizing = true;
        } else if self.layout.tree.contains(x, y) {
            self.focus = Focus::Manager;
            if self.layout.tree_body().contains(x, y) {
                let body = self.layout.tree_body();
                let index = (f32::from(event.position.y - self.metrics.origin.y) / self.metrics.lh
                    - body.y as f32
                    + self.tree_scroll.position())
                .floor()
                .max(0.) as usize;
                if let Some(entry) = self.tree.entries.get(index) {
                    let activate =
                        event.click_count == 2 && first_click.as_ref() == Some(&entry.path);
                    if event.click_count == 1 {
                        self.tree_click = Some(entry.path.clone());
                    }
                    self.tree.selected = index;
                    // Keep the clicked row under the pointer as a fractional scroll settles.
                    let top = index - (y - body.y);
                    self.tree_scroll
                        .snap(top, self.scroll_epoch.elapsed().as_secs_f64());
                    if activate && let Some(path) = self.tree.activate() {
                        self.open(path);
                    }
                    self.layout = self.derive_layout(window);
                    self.repaint(
                        [
                            ScrollInput::Advance,
                            ScrollInput::Reveal,
                            ScrollInput::Reveal,
                        ],
                        window,
                        cx,
                    );
                    return;
                }
            }
        } else if self.layout.editor.contains(x, y) {
            self.focus = Focus::Editor;
            if y == self.layout.editor.y || y == self.layout.editor.y + self.layout.editor.h - 1 {
                self.sync(window, cx);
                return;
            }
            let offset = self.editor_offset(event.position);
            self.buffers[self.active].vim.set_cursor(offset);
            self.dragging = Some((self.vim().doc.cursor, event.position));
        }
        self.sync(window, cx);
    }

    /// Update border hover from `event` or continue resizing or selection in `window` through `cx`.
    fn mouse_move(&mut self, event: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !event.dragging() {
            self.resizing = false;
            self.dragging = None;
        }
        if self.resizing {
            let (x, _) = self.metrics.cell(event.position);
            self.sidebar = x;
        } else if let Some((anchor, origin)) = self.dragging {
            if f32::from(event.position.x - origin.x)
                .abs()
                .max(f32::from(event.position.y - origin.y).abs())
                < 4.
            {
                return;
            }
            let at = self.editor_offset(event.position);
            let vim = &mut self.buffers[self.active].vim;
            if at != anchor {
                vim.mode = Mode::Visual;
                vim.anchor = anchor;
                vim.set_cursor(at);
            }
        } else {
            self.set_border_hover(self.border_at(event.position), cx);
            return;
        }
        self.sync(window, cx);
    }

    /// End pointer resizing/selection, including releases outside the native client bounds.
    fn mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.resizing = false;
        self.dragging = None;
    }

    /// Route `event` to the panel under the pointer and retain partial steps on each axis.
    /// Shift pans editor text horizontally. `window` and `cx` drive viewport animation.
    fn scroll(&mut self, event: &ScrollEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.cursor
            .update(cx, |cursor, cx| cursor.activity(window, cx));
        let (x, y) = self.metrics.cell(event.position);
        let delta = event.delta.pixel_delta(px(self.metrics.cw));
        let horizontal = event.modifiers.shift || (delta.x != px(0.) && delta.y == px(0.));
        if self.help_top.is_none() && self.layout.editor.contains(x, y) && horizontal {
            if self.composition.is_empty() {
                // GPUI on Windows has already converted Shift+wheel to an X delta.
                let delta = if delta.x != px(0.) { delta.x } else { delta.y };
                self.horizontal_wheel += f32::from(delta) / self.metrics.cw;
                let columns = self.horizontal_wheel.trunc() as isize;
                self.horizontal_wheel -= columns as f32;
                if columns != 0 {
                    self.scroll_frame(
                        [
                            ScrollInput::Advance,
                            ScrollInput::Advance,
                            ScrollInput::Wheel(columns.saturating_neg()),
                        ],
                        window,
                        cx,
                    );
                }
            }
            cx.stop_propagation();
            return;
        }
        self.wheel += f32::from(event.delta.pixel_delta(px(self.metrics.lh)).y) / self.metrics.lh;
        let rows = self.wheel.trunc() as isize;
        self.wheel -= rows as f32;
        if rows == 0 {
            return;
        }
        if self.help_top.is_some() {
            self.scroll_help(rows.saturating_neg(), cx);
            cx.stop_propagation();
            return;
        }
        let wheel = ScrollInput::Wheel(rows.saturating_neg());
        let inputs = if self.layout.tree.contains(x, y) {
            [wheel, ScrollInput::Advance, ScrollInput::Advance]
        } else if self.layout.editor.contains(x, y) && self.composition.is_empty() {
            [ScrollInput::Advance, wheel, ScrollInput::Advance]
        } else {
            return;
        };
        self.scroll_frame(inputs, window, cx);
    }
}

impl Editor {
    /// Place fixed panels and scrollable bodies in draw order, keeping line numbers fixed on X.
    fn grid_layers(&self) -> [GridLayer; 6] {
        let fixed = |panel: &Entity<Panel>, rect: Rect| GridLayer {
            panel: panel.clone(),
            bounds: rect.pixels(self.metrics),
            clip: None,
        };
        let scrolled = |panel: &Entity<Panel>, rect: Rect, fraction: f32| {
            let clip = rect.pixels(self.metrics);
            GridLayer {
                panel: panel.clone(),
                bounds: Bounds::new(
                    clip.origin - point(px(0.), px(fraction * self.metrics.lh)),
                    size(clip.size.width, px((rect.h + 1) as f32 * self.metrics.lh)),
                ),
                clip: Some(clip),
            }
        };
        let buffer = &self.buffers[self.active];
        let body = self.editor_text();
        let gutter = self.layout.editor_body();
        let gutter = Rect {
            w: body.x - gutter.x,
            ..gutter
        };
        let mut text = scrolled(&self.bodies[2], body, buffer.scroll.fraction());
        if body.w > 0 {
            text.bounds.origin.x -= px(buffer.text_displacement() * self.metrics.cw);
            text.bounds.size.width +=
                px((buffer.left_padding() + TEXT_OVERSCAN) as f32 * self.metrics.cw);
        }
        [
            fixed(
                &self.panels[0],
                Rect {
                    x: 0,
                    y: 0,
                    w: self.layout.cols,
                    h: self.layout.rows,
                },
            ),
            fixed(&self.panels[1], self.layout.tree),
            fixed(&self.panels[2], self.layout.editor),
            scrolled(
                &self.bodies[0],
                self.layout.tree_body(),
                self.tree_scroll.fraction(),
            ),
            scrolled(&self.bodies[1], gutter, buffer.scroll.fraction()),
            text,
        ]
    }

    /// Reuse the blurred `layers` while the `outline` and `window` size match.
    /// `cx` supplies the retained panels and their SVG renderer.
    fn help_backdrop_image(
        &mut self,
        layers: &[GridLayer],
        outline: Bounds<Pixels>,
        window: &mut Window,
        cx: &App,
    ) -> Arc<RenderImage> {
        let viewport = window.viewport_size();
        if let Some(backdrop) = &self.help_backdrop
            && backdrop.viewport == viewport
            && backdrop.outline == outline
        {
            return backdrop.image.clone();
        }
        let mut raster = BackdropRaster::new(viewport, self.glyphs.as_ref(), cx.svg_renderer());
        for layer in layers {
            let clip = layer.clip.unwrap_or(layer.bounds);
            if !clip.is_empty() {
                layer
                    .panel
                    .read(cx)
                    .stamp_backdrop(layer.bounds, clip, &mut raster, window);
            }
        }
        let image = raster.finish(outline, HELP_CORNER_RADIUS);
        if let Some(previous) = self.help_backdrop.replace(HelpBackdrop {
            viewport,
            outline,
            image: image.clone(),
        }) {
            let _ = window.drop_image(previous.image);
        }
        image
    }
}

/// Test `position` against `area`'s border cells and their pixel tolerance using `metrics`.
fn near_border(area: Rect, metrics: Metrics, position: Point<Pixels>) -> bool {
    if area.close_button().is_none() {
        return false;
    }
    let bounds = area.pixels(metrics);
    let slop = point(px(BORDER_HOVER_SLOP), px(BORDER_HOVER_SLOP));
    let inset = point(px(metrics.cw), px(metrics.lh)) + slop;
    let outer = Bounds::from_corners(bounds.origin - slop, bounds.bottom_right() + slop);
    let inner = Bounds::from_corners(bounds.origin + inset, bounds.bottom_right() - inset);
    outer.contains(&position) && !inner.contains(&position)
}

/// Return the backdrop cutout bounds with an even gap around the character border inside `grid`.
/// `metrics` locates the centerline in the first and last rows and columns.
fn help_outline(grid: Bounds<Pixels>, metrics: Metrics) -> Bounds<Pixels> {
    let padding = point(
        px(HELP_BORDER_GAP - metrics.cw * 0.5),
        px(HELP_BORDER_GAP - metrics.lh * 0.5),
    );
    Bounds::from_corners(grid.origin - padding, grid.bottom_right() + padding)
}

/// Position a cached panel using the logical `bounds` relative to its parent.
fn positioned(bounds: Bounds<Pixels>) -> StyleRefinement {
    StyleRefinement::default()
        .absolute()
        .left(bounds.origin.x)
        .top(bounds.origin.y)
        .w(bounds.size.width)
        .h(bounds.size.height)
}

impl Render for Editor {
    /// Compose the workspace and optional frosted shortcut help.
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.derive_layout(window) != self.layout {
            self.sync(window, cx);
        }
        if !window.is_window_hovered() {
            self.set_border_hover(None, cx);
        }
        let mut root = div()
            .id("nanobug")
            .relative()
            .size_full()
            .bg(rgba(BACKGROUND))
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_exit(cx.listener(|editor, _, _, cx| editor.set_border_hover(None, cx)))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_scroll(cx.listener(Self::scroll));
        let layers = self.grid_layers();
        if self.help_top.is_some() {
            let grid = self.help_layout().0.pixels(self.metrics);
            let outline = help_outline(grid, self.metrics);
            let backdrop = self.help_backdrop_image(&layers, outline, window, cx);
            root = root
                .child(
                    canvas(
                        |_, _, _| (),
                        move |bounds, _, window, _| {
                            let _ = window.paint_image(
                                bounds,
                                bounds,
                                Corners::default(),
                                backdrop.clone(),
                                0,
                                false,
                            );
                        },
                    )
                    .absolute()
                    .left_0()
                    .top_0()
                    .size_full(),
                )
                .child(self.help_panel.clone().cached(positioned(grid)));
        } else {
            if let Some(backdrop) = self.help_backdrop.take() {
                let _ = window.drop_image(backdrop.image);
            }
            for GridLayer {
                panel,
                bounds,
                clip,
            } in layers
            {
                if bounds.is_empty() {
                    continue;
                }
                root = if let Some(clip) = clip {
                    let local = Bounds::new(bounds.origin - clip.origin, bounds.size);
                    root.child(
                        div()
                            .absolute()
                            .left(clip.origin.x)
                            .top(clip.origin.y)
                            .w(clip.size.width)
                            .h(clip.size.height)
                            .overflow_hidden()
                            .child(panel.cached(positioned(local))),
                    )
                } else {
                    root.child(panel.cached(positioned(bounds)))
                };
            }
            root = root.child(
                self.cursor.clone().cached(
                    StyleRefinement::default()
                        .absolute()
                        .left_0()
                        .top_0()
                        .size_full(),
                ),
            );
        }
        if let Some(target) = self.hovered_border
            && let Some(button) = self.window_area(target).close_button()
        {
            let bounds = button.pixels(self.metrics);
            let id = match target {
                ShellWindow::Pane(Focus::Manager) => "close-files",
                ShellWindow::Pane(Focus::Editor) => "close-editor",
                ShellWindow::ShortcutHelp => "close-shortcuts",
            };
            root = root.child(
                div()
                    .id(id)
                    .absolute()
                    .left(bounds.origin.x)
                    .top(bounds.origin.y)
                    .w(bounds.size.width)
                    .h(bounds.size.height)
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |editor, event, window, cx| {
                        editor.click_close_button(target, event, window, cx);
                    }))
                    .child(self.close_glyph.element()),
            );
        }
        let view = cx.entity();
        root.child(
            canvas(
                |_, _, _| (),
                move |bounds, _, window, cx| {
                    view.update(cx, |editor, cx| {
                        window.handle_input(
                            &editor.focus_handle,
                            ElementInputHandler::new(bounds, cx.entity()),
                            cx,
                        )
                    });
                },
            )
            .absolute()
            .left_0()
            .top_0()
            .size_full(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The close label mirrors the title inset, and the border hover excludes the body.
    #[test]
    fn close_button_uses_the_border_cell_and_pixel_tolerance() {
        let metrics = Metrics {
            cw: 10.,
            lh: 20.,
            font_size: 16.,
            origin: point(px(14.), px(5.)),
        };
        let area = Rect {
            x: 4,
            y: 2,
            w: 12,
            h: 8,
        };
        let button = area.close_button().unwrap().pixels(metrics);
        assert_eq!(button.center().x, metrics.at(12.5, 2.5).x);
        assert!(!button.contains(&metrics.at(14.5, 2.5)));
        for (x, y) in [(12.5, 2.5), (9., 3.1), (16.2, 6.), (10., 9.5)] {
            assert!(near_border(area, metrics, metrics.at(x, y)));
        }
        for (x, y) in [(10., 5.), (16.5, 6.), (3.5, 6.)] {
            assert!(!near_border(area, metrics, metrics.at(x, y)));
        }
        let hidden = Rect { w: 0, ..area };
        assert!(hidden.close_button().is_none());
        assert!(!near_border(hidden, metrics, metrics.at(4., 2.)));
    }

    /// The visible character edges are equally inset despite unequal cell dimensions.
    #[test]
    fn help_outline_is_even_around_the_character_border() {
        let metrics = Metrics {
            cw: 10.,
            lh: 21.,
            font_size: 16.,
            origin: point(px(0.), px(0.)),
        };
        let grid = Bounds::new(point(px(100.), px(50.)), size(px(640.), px(462.)));
        let outline = help_outline(grid, metrics);
        assert_eq!(
            grid.left() + px(metrics.cw / 2.) - outline.left(),
            px(HELP_BORDER_GAP)
        );
        assert_eq!(
            outline.right() - (grid.right() - px(metrics.cw / 2.)),
            px(HELP_BORDER_GAP)
        );
        assert_eq!(
            grid.top() + px(metrics.lh / 2.) - outline.top(),
            px(HELP_BORDER_GAP)
        );
        assert_eq!(
            outline.bottom() - (grid.bottom() - px(metrics.lh / 2.)),
            px(HELP_BORDER_GAP)
        );
    }

    /// A shortcut popup owns the selection while open and returns it to its originating pane.
    #[test]
    fn shortcut_help_is_the_only_selected_window() {
        let windows = [
            ShellWindow::Pane(Focus::Manager),
            ShellWindow::Pane(Focus::Editor),
            ShellWindow::ShortcutHelp,
        ];
        for (pane, pane_selection) in [
            (Focus::Manager, [true, false, false]),
            (Focus::Editor, [false, true, false]),
        ] {
            assert_eq!(
                [false, true, false]
                    .map(|help_open| windows.map(|window| window.is_selected(pane, help_open))),
                [pane_selection, [false, false, true], pane_selection],
            );
        }
    }
}
