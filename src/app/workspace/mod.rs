//! Own the native workspace state and coordinate feature models with retained views.
//! Child modules implement input, commands, layout and rendering against this single state.

mod buffer;
mod commands;
mod explorer;
mod frames;
mod ime;
mod keyboard;
mod layout;
mod pointer;
mod shortcuts;
mod view;
mod viewport;

use self::{
    buffer::Buffer,
    layout::{Focus, Layout, ShellWindow},
    view::HelpBackdrop,
    viewport::CursorSpace,
};
use crate::{
    explorer::{Explorer, FileTransfers},
    ui::{
        Cursor,
        grid::{CloseGlyph, Metrics, Panel, Rect},
        text::TextSpacing,
        theme::{GRID_CHINESE_FONT, GRID_FONT_SIZE, GRID_RULE_FONT_FAMILY, GRID_TEXT_FONT},
    },
};
use gpui::{
    AppContext, Context, Entity, FocusHandle, PlatformTextSystem, Task, Window, font, point, px,
};
use nanobug::{
    animation::{
        cursor::{CursorConfig, Effect},
        scroll::GridScroll,
    },
    editor::{Document, Register, Vim},
};
use std::{ops::Range, path::PathBuf, sync::Arc, time::Instant};

/// Application state. The document owns logical input; cached panels and cursor only draw it.
pub(super) struct Editor {
    /// Native keyboard/IME focus for the whole character work surface.
    focus_handle: FocusHandle,
    /// Last selected pane, also used to choose the shortcut help content.
    focus: Focus,
    /// Root project directory.
    project: PathBuf,
    /// Lazily expanded filesystem manager.
    tree: Explorer,
    /// Clipboard and undo history for filesystem pastes.
    transfers: FileTransfers,
    /// Pane hidden by a close action, restored by its open shortcut or by opening a file.
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
    buffers: Vec<Buffer>,
    /// Buffer currently displayed by the sole editor viewport.
    active: usize,
    /// Shared unnamed yank/delete register.
    register: Register,
    /// Current shared font geometry.
    metrics: Metrics,
    /// Close glyph positioned to match the normal text's capital letters.
    close_glyph: CloseGlyph,
    /// Native metrics for the actual glyphs selected by the window's text layout.
    glyphs: Arc<dyn PlatformTextSystem>,
    /// Shared document advances for rendering, scrolling and pointer input.
    spacing: TextSpacing,
    /// Cell layout for the current native window.
    layout: Layout,
    /// Preferred manager width in cells.
    sidebar: usize,
    /// Cached header, manager, editor and footer views, in draw order.
    panels: [Entity<Panel>; 4],
    /// Independently clipped file rows, line numbers and text, in draw order.
    bodies: [Entity<Panel>; 3],
    /// One label painted above both panes when the selected name overflows Explorer.
    tree_name_preview: Entity<Panel>,
    /// Preview bounds in grid cells before applying Explorer's fractional scroll offset.
    tree_name_preview_rect: Option<Rect>,
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
    composition: String,
    /// Native UTF-16 selection within the preedit string.
    composition_selection: Range<usize>,
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
    pub(super) fn new(
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
                "i 开始编辑 · :w 文件名 保存 · Space e 打开 Explorer".into(),
            )
        };
        let system = window.text_system();
        let font_id = system.resolve_font(&GRID_TEXT_FONT);
        let rule_font_id = system.resolve_font(&font(GRID_RULE_FONT_FAMILY));
        let font_size = GRID_FONT_SIZE;
        let scale = window.scale_factor();
        let advance = f32::from(
            system
                .advance(font_id, px(font_size), 'M')
                .expect("font advance")
                .width,
        );
        let height = f32::from(system.ascent(rule_font_id, px(font_size))).abs()
            + f32::from(system.descent(rule_font_id, px(font_size))).abs();
        let metrics = Metrics {
            // Snap the base font's advance to whole device pixels to match the reference grid.
            cw: (advance * scale).round().max(1.) / scale,
            lh: (height * scale).ceil() / scale,
            font_size,
            origin: point(px(14.), px(5.)),
        };
        let chinese_face = system.resolve_font(&GRID_CHINESE_FONT);
        let spacing = TextSpacing::new(glyphs.clone(), chinese_face, font_size, metrics.cw);
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
            spacing,
            layout: Layout::default(),
            sidebar: 28,
            panels: [
                cx.new(|_| Panel::new(metrics)),
                cx.new(|_| Panel::new(metrics)),
                cx.new(|_| Panel::new(metrics)),
                cx.new(|_| Panel::new(metrics)),
            ],
            bodies: [
                cx.new(|_| Panel::new(metrics)),
                cx.new(|_| Panel::new(metrics)),
                cx.new(|_| Panel::new(metrics)),
            ],
            tree_name_preview: cx.new(|_| Panel::new(metrics)),
            tree_name_preview_rect: None,
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
    fn vim(&self) -> &Vim {
        &self.buffers[self.active].vim
    }
}
