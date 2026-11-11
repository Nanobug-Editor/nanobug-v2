//! Own the native workspace state and coordinate feature models with retained views.
//! Child modules implement input, commands, layout and rendering against this single state.

#[cfg(windows)]
mod alt_escape;
mod commands;
mod editor_close;
mod explorer;
mod file_picker;
mod frames;
mod ime;
mod keyboard;
mod language;
mod layout;
mod leader_hint;
mod pointer;
mod projects;
mod recent_projects;
mod start_page;
mod tabs;
mod view;
mod viewport;

use crate::grid::{
    render::{Cursor, Metrics, Panel, Rect},
    windows::{help::HelpWindow, start::StartPage},
};

use self::{
    editor_close::{CloseTarget, EditorClose},
    file_picker::PickerSession,
    keyboard::PendingPrefix,
    layout::{Layout, OUTER_INSET_Y, ShellWindow, TAB_GAP_Y},
    projects::WorkspaceTabs,
    recent_projects::RecentProjects,
    start_page::StartPreview,
    tabs::TabBar,
    viewport::CursorSpace,
};
use crate::ui::{
    text::TextSpacing,
    theme::{GRID_CHINESE_FONT, GRID_FONT_SIZE, GRID_RULE_FONT_FAMILY, GRID_TEXT_FONT},
};
use gpui::{AppContext, Context, Entity, FocusHandle, PlatformTextSystem, Window, font, point, px};
use nanobug::{animation::cursor::Effect, i18n::Locale};
use std::{path::PathBuf, sync::Arc, time::Instant};

/// Application state. The document owns logical input; cached panels and cursor only draw it.
pub(crate) struct Workspace {
    /// Native keyboard/IME focus for the whole character work surface.
    focus_handle: FocusHandle,
    /// Tab order, with each start page or project retaining its own state.
    projects: WorkspaceTabs,
    /// Tab strip layout and its independent animated selection.
    tabs: TabBar,
    /// Project history shared by all start pages.
    recent_projects: RecentProjects,
    /// Retained frame for the selected start page.
    start_panel: Entity<Panel>,
    /// Directory scan shown by the active project picker.
    start_preview: Option<StartPreview>,
    /// Active picker session, including its render panels and pending operations.
    file_picker: Option<PickerSession>,
    /// Last successfully browsed directory for openings from the start page.
    file_picker_directory: Option<PathBuf>,
    /// Shortcut-help window owns popup visibility and cached content.
    help: HelpWindow,
    /// Pending decisions for closing a pane, project or the native window.
    editor_close: Option<EditorClose>,
    /// Highlighted language while the picker is open.
    language_selection: Option<Locale>,
    /// Retained choices positioned between the language picker's two half-row gaps.
    language_list: Entity<Panel>,
    /// Cached cells for the leader hint, language picker chrome or editor close dialog.
    popup_panel: Entity<Panel>,
    /// Window whose border currently shows its corner controls.
    hovered_controls: Option<ShellWindow>,
    /// Ignore repeats of a key that just dismissed a popup, pane or project.
    held_close_key: Option<String>,
    /// Windows interception of Alt+Esc while this native window is active.
    #[cfg(windows)]
    alt_escape: Option<alt_escape::AltEscape>,
    /// Pending workspace leader or Explorer sequence.
    prefix: Option<PendingPrefix>,
    /// Current shared font geometry.
    metrics: Metrics,
    /// Native metrics for the actual glyphs selected by the window's text layout.
    glyphs: Arc<dyn PlatformTextSystem>,
    /// Shared document advances for rendering, scrolling and pointer input.
    spacing: TextSpacing,
    /// Cell layout for the current native window.
    layout: Layout,
    /// Cached manager, editor and footer views, in draw order.
    panels: [Entity<Panel>; 3],
    /// Independently clipped file rows, line numbers and text, in draw order.
    bodies: [Entity<Panel>; 3],
    /// One label painted above both panes when the selected name overflows Explorer.
    tree_name_preview: Entity<Panel>,
    /// Preview bounds in grid cells before applying Explorer's fractional scroll offset.
    tree_name_preview_rect: Option<Rect>,
    /// Monotonic clock shared by viewport animations.
    scroll_epoch: Instant,
    /// Whether one viewport frame callback is pending.
    scroll_scheduled: bool,
    /// Last logical cursor space, distinguishing viewport motion from focus jumps.
    last_cursor_space: Option<CursorSpace>,
    /// Independent full-window cursor layer.
    cursor: Entity<Cursor>,
    /// Whether pointer dragging is resizing the sidebar.
    resizing: bool,
    /// Accumulated vertical wheel delta, supporting high-resolution touchpads.
    wheel: f32,
}

impl Workspace {
    /// Open project `paths` in `window` through `cx`, using `glyphs` for text geometry.
    pub(crate) fn new(
        paths: Vec<PathBuf>,
        glyphs: Arc<dyn PlatformTextSystem>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let projects = WorkspaceTabs::new(paths);
        let recent_projects =
            RecentProjects::new(projects.ordered().map(|project| project.root.clone()));
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
            origin: point(px(14.), px(OUTER_INSET_Y + TAB_GAP_Y)),
        };
        let chinese_face = system.resolve_font(&GRID_CHINESE_FONT);
        let spacing = TextSpacing::new(glyphs.clone(), chinese_face, font_size, metrics.cw);
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
        let weak = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            weak.update(cx, |workspace, cx| {
                if workspace.editor_close.is_some() || workspace.picker_operation_pending() {
                    return false;
                }
                if workspace.file_picker.take().is_some() {
                    workspace.help.close();
                }
                if workspace.begin_close(CloseTarget::Quit) {
                    workspace.sync(window, cx);
                    false
                } else {
                    true
                }
            })
            .unwrap_or(true)
        });
        cx.observe_window_activation(window, |workspace, window, cx| {
            if !window.is_window_active() {
                workspace.dismiss_leader_hint(cx);
            }
            #[cfg(windows)]
            {
                if window.is_window_active() {
                    workspace.sync_native_ime(window, cx);
                }
                workspace.alt_escape = alt_escape::AltEscape::install(
                    window,
                    cx,
                    workspace.editor_accepts_alt_escape(),
                );
            }
        })
        .detach();
        #[cfg(windows)]
        let alt_escape =
            alt_escape::AltEscape::install(window, cx, projects.active_project().is_some());
        let config = projects
            .active_project()
            .map(|project| project.cursor_config.clone())
            .unwrap_or_default();
        Self {
            focus_handle,
            projects,
            tabs: TabBar::new(metrics, cx),
            recent_projects,
            start_panel: cx.new(|_| Panel::new(StartPage::content_metrics(metrics))),
            start_preview: None,
            file_picker: None,
            file_picker_directory: None,
            help: HelpWindow::new(metrics, cx),
            editor_close: None,
            language_selection: None,
            language_list: cx.new(|_| Panel::new(metrics)),
            popup_panel: cx.new(|_| Panel::new(metrics)),
            hovered_controls: None,
            held_close_key: None,
            #[cfg(windows)]
            alt_escape,
            prefix: None,
            metrics,
            glyphs,
            spacing,
            layout: Layout::default(),
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
            tree_name_preview: cx.new(|_| Panel::new(metrics)),
            tree_name_preview_rect: None,
            scroll_epoch: Instant::now(),
            scroll_scheduled: false,
            last_cursor_space: None,
            cursor: cx.new(|_| Cursor::new(Effect::Afterglow, config)),
            resizing: false,
            wheel: 0.,
        }
    }

    /// Whether a popup currently owns pane input.
    fn modal_open(&self) -> bool {
        self.file_picker.is_some() || self.help.is_open() || self.small_modal_open()
    }

    /// Whether the language picker or editor close dialog currently owns pane input.
    fn small_modal_open(&self) -> bool {
        self.language_selection.is_some() || self.editor_close.is_some()
    }

    /// Keep Alt+Escape interception limited to the selected editor pane.
    #[cfg(windows)]
    fn editor_accepts_alt_escape(&self) -> bool {
        self.projects
            .active_project()
            .is_some_and(|project| project.focus == layout::Focus::Editor)
            && !self.modal_open()
    }
}
