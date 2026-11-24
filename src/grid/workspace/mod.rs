//! Own the native workspace state and coordinate feature models with retained views.
//! Child modules implement input, commands, layout and rendering against this single state.

#[cfg(windows)]
mod alt_escape;
mod argument_list;
mod buffer_management;
mod command_window;
mod commands;
mod dpi;
mod editing_panes;
mod editor_close;
mod editor_splits;
mod ex_autocmd;
mod ex_batch;
mod ex_buffers;
mod ex_catalog;
mod ex_commands;
mod ex_files;
mod ex_messages;
mod ex_sign;
mod ex_windows;
mod explorer;
mod file_picker;
mod frames;
mod help_code;
mod ime;
mod input_maps;
mod keyboard;
mod language;
mod layout;
mod leader_hint;
mod markdown;
#[cfg(all(test, windows))]
mod native_test;
mod numbered_marks;
mod plugins;
mod pointer;
#[cfg(feature = "plugin-probe")]
mod probe;
mod projects;
mod quickfix;
mod recent_projects;
mod recovery;
mod session_export;
mod settings;
mod sleep;
mod start_page;
mod tabs;
mod terminal;
mod view;
mod viewport;
mod vim_config;
mod vim_menu;
mod window_keys;
mod window_modifiers;

use crate::grid::{
    render::{Cursor, Metrics, Panel, Rect},
    windows::{
        editor::MarkdownCache,
        help::HelpWindow,
        start::{RemoveProject, StartPage},
    },
};
use crate::ui::components::grid_dialog::DialogBackdrop;

use self::{
    editor_close::{CloseTarget, EditorClose},
    file_picker::PickerSession,
    keyboard::PendingPrefix,
    layout::{Layout, ShellWindow},
    leader_hint::HintDelay,
    recent_projects::RecentProjects,
    start_page::StartPreview,
    tabs::TabBar,
    viewport::CursorSpace,
};
use crate::ui::{text::TextSpacing, theme::AppearanceSettings};
use gpui::{App, AppContext, Context, Entity, FocusHandle, PlatformTextSystem, Window};
use nanobug::{animation::cursor::Effect, i18n::Locale};
pub(crate) use projects::WorkspaceTabs;
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
    /// Clipped project rows and directory preview in the recent-project picker.
    start_bodies: [Entity<Panel>; 2],
    /// Directory scan shown by the active project picker.
    start_preview: Option<StartPreview>,
    /// Transient command or search history editor.
    command_window: Option<crate::grid::windows::editor::CommandWindow>,
    /// Context-sensitive Vim menu bound to the editor that opened it.
    vim_menu: Option<crate::grid::windows::vim_menu::VimMenu>,
    /// Interruptible input delay used by Vim's gs command.
    editor_sleep: Option<sleep::Sleep>,
    /// Definitions made without a buffer scope and shared by every editing surface.
    input_mappings: nanobug::vim::InputMappings,
    /// Portable settings for global Vim options and mappings in normal app windows.
    vim_settings_path: Option<PathBuf>,
    /// Stack of scripts currently running through the native Ex dispatcher.
    ex_source_depth: usize,
    /// Ex autocommand event dispatch and the last observed editor buffer.
    script_events: ex_autocmd::State,
    /// Canonical paths in the order first loaded by :source.
    ex_sourced_files: Vec<PathBuf>,
    /// App-wide Ex directory set by :cd, falling back to a Session's own root initially.
    ex_directory: crate::grid::windows::editor::DirectorySlot,
    /// Background draft checkpoints for loaded, modified text buffers.
    recovery: recovery::Recovery,
    /// Buffer numbers already loaded within the current `:noswapfile` dispatch.
    noswapfile_buffers: Option<std::collections::HashSet<u64>>,
    /// Placement overrides for the currently executing Ex command.
    active_ex_window_modifiers: nanobug::vim::WindowModifiers,
    /// Startup command carried into a replacement process after :restart closes this one.
    restart_ex_command: Option<String>,
    /// Stop pending Ex frames once the native window has been queued for removal.
    main_window_removing: bool,
    /// Additional directories indexed by :helptags in this application window.
    help_tag_directories: Vec<PathBuf>,
    /// The latest completed text edit batch in each retained Session.
    ex_batch_undo: std::collections::HashMap<u64, ex_batch::BatchUndo>,
    /// Prevent a list command from replacing the target list of a running batch.
    ex_batch_active: bool,
    /// Timeout belonging to one in-progress user-defined mapping.
    mapping_timer: Option<input_maps::MappingTimer>,
    /// Selected history entry awaiting confirmation from the project picker.
    remove_recent_project: Option<RemoveProject>,
    /// Active picker session, including its render panels and pending operations.
    file_picker: Option<PickerSession>,
    /// Last successfully browsed directory for openings from the start page.
    file_picker_directory: Option<PathBuf>,
    /// Shortcut-help window owns popup visibility and cached content.
    help: HelpWindow,
    /// Settings drafts and original values survive switching category pages.
    settings: Option<crate::grid::windows::settings::Settings>,
    /// Original stored overrides may differ from unsaved live preferences.
    core_settings_baseline: settings::CoreBaseline,
    /// Opening state of project cursor timings and the persisted preset.
    cursor_settings_baseline: settings::CursorBaseline,
    /// Identifies the dialog owning an asynchronous plugin catalog response.
    settings_generation: u64,
    settings_catalog_started: bool,
    /// Serialize all setting writes, including compensating writes after an error.
    settings_writing: bool,
    /// Resume a native close after outstanding settings rollbacks finish.
    close_after_settings: bool,
    /// Shared blurred workspace image for the shortcut and plugin popups.
    dialog_backdrop: DialogBackdrop,
    /// Pending decisions for closing a pane, project or the native window.
    editor_close: Option<EditorClose>,
    /// Explicit discard requested by an Ex close, pending plugin close preparation.
    forced_editor_close: Option<editor_close::CloseTarget>,
    /// Highlighted language while the picker is open.
    language_selection: Option<Locale>,
    /// Retained choices positioned between the language picker's two half-row gaps.
    language_list: Entity<Panel>,
    /// Cached cells for workspace popup chrome.
    popup_panel: Entity<Panel>,
    /// Opening timer and visibility of the current key sequence hint.
    hint_delay: HintDelay,
    /// Current page of an oversized key sequence hint.
    hint_page: usize,
    /// Window whose border currently shows its corner controls.
    hovered_controls: Option<ShellWindow>,
    /// Ignore repeats of a key that just dismissed a popup, pane or project.
    held_close_key: Option<String>,
    /// Windows interception of Alt+Esc while this native window is active.
    #[cfg(windows)]
    alt_escape: Option<alt_escape::AltEscape>,
    /// Pending workspace leader, window command, or Explorer sequence.
    prefix: Option<PendingPrefix>,
    /// Current shared font geometry.
    metrics: Metrics,
    /// Editor-wide theme, grid font and independent user zoom.
    appearance: AppearanceSettings,
    /// Settings file edited by explicit appearance commands.
    appearance_settings_path: Option<PathBuf>,
    /// Portable file backing the numbered caret marks, absent in isolated windows.
    numbered_marks_path: Option<PathBuf>,
    /// Latest saved positions ordered from mark 0 through mark 9.
    numbered_marks: Arc<[nanobug::app_data::numbered_marks::Mark]>,
    /// Portable settings file for main-window geometry, when opened by the normal app.
    window_geometry_path: Option<PathBuf>,
    /// Device scale used to snap the current logical grid geometry.
    display_scale: f32,
    /// Native metrics for the actual glyphs selected by the window's text layout.
    glyphs: Arc<dyn PlatformTextSystem>,
    /// Shared document advances for rendering, scrolling and pointer input.
    spacing: TextSpacing,
    /// Cell layout for the current native window.
    layout: Layout,
    /// Fixed Explorer border, title and project root.
    manager_chrome: Entity<Panel>,
    /// Clipped and scrolling Explorer rows.
    manager_body: Entity<Panel>,
    /// Fixed border, line numbers and text for the active editor.
    editor_panels: frames::EditorPanels,
    /// Workspace status and command line below the panes.
    footer_panel: Entity<Panel>,
    /// Retained render layers and preview caches for the other visible editors.
    inactive_editor_panels: Vec<frames::EditorPanePanels>,
    /// UI-thread text and image layout for the last rendered Markdown document.
    markdown_cache: MarkdownCache,
    /// Hover and click state for the preview links painted in this window.
    markdown_pointer: pointer::MarkdownPointer,
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
    /// The divider captured by a left-button resize gesture.
    pane_resize: Option<pointer::PaneResize>,
    /// Accumulated vertical wheel delta, supporting high-resolution touchpads.
    wheel: f32,
    /// Manager and the optional plugin surface that share the native keyboard and cursor.
    plugins: plugins::PluginCoordinator,
    /// Optional native surface and host draft for the P0 experiment.
    #[cfg(feature = "plugin-probe")]
    probe: Option<probe::Probe>,
}

impl Workspace {
    /// Open `projects` in `window` through `cx`, measuring `glyphs` with the startup `appearance`.
    pub(crate) fn new(
        projects: WorkspaceTabs,
        glyphs: Arc<dyn PlatformTextSystem>,
        appearance: AppearanceSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
        plugins: nanobug::plugins::session::Options,
        plugin_trace: Option<nanobug::plugins::session::Trace>,
        #[cfg(feature = "plugin-probe")] probe: Option<crate::plugin_probe::Options>,
    ) -> Self {
        #[cfg(feature = "plugin-probe")]
        let isolated = probe.is_some();
        #[cfg(not(feature = "plugin-probe"))]
        let isolated = false;
        let isolate_data = isolated || plugins.isolated();
        let numbered_marks_path = (!isolate_data)
            .then(|| nanobug::app_data::numbered_marks::path().ok())
            .flatten();
        let numbered_marks = numbered_marks::load(numbered_marks_path.as_deref());
        let cursor_effect = if isolate_data {
            Effect::Afterglow
        } else {
            settings::saved_cursor_effect()
        };
        let recent_projects = if isolate_data {
            RecentProjects::in_memory()
        } else {
            RecentProjects::new(projects.ordered().map(|project| project.root.clone()))
        };
        let style = appearance.style(window.appearance());
        cx.set_global(style);
        let (metrics, spacing) = dpi::measure(window, &glyphs, appearance);
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
        Self::start_project_sessions(&projects, window, cx);
        #[cfg(windows)]
        Self::install_caption_tabs(window, cx);
        let weak = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            weak.update(cx, |workspace, cx| {
                if workspace.settings.is_some() {
                    if workspace.settings_writing {
                        return false;
                    }
                    workspace.settings_action(
                        crate::grid::windows::settings::Action::Cancel,
                        window,
                        cx,
                    );
                    if workspace.settings.is_some() {
                        workspace.close_after_settings = workspace.settings_writing;
                        return false;
                    }
                }
                if workspace.editor_close.is_some()
                    || workspace.picker_operation_pending()
                    || workspace.plugins.file_busy
                {
                    return false;
                }
                if workspace.file_picker.is_some() {
                    workspace.help.close();
                    workspace.finish_file_picker(None, window, cx);
                }
                if workspace.prepare_plugin_close(CloseTarget::Quit, window, cx) {
                    return false;
                }
                if workspace.begin_close(CloseTarget::Quit) {
                    workspace.sync(window, cx);
                    false
                } else if !workspace.save_closing_project_sessions(CloseTarget::Quit) {
                    workspace.warn_project_session_close(CloseTarget::Quit, window, cx);
                    false
                } else {
                    workspace.save_numbered_mark();
                    workspace.save_window_geometry(window, cx);
                    workspace.recovery.finish();
                    true
                }
            })
            .unwrap_or(true)
        });
        cx.observe_window_activation(window, |workspace, window, cx| {
            if !window.is_window_active() {
                workspace.dismiss_sequence_hint(cx);
                if workspace.pane_resize.take().is_some() {
                    cx.notify();
                }
            }
            workspace
                .help
                .update_keyboard_modifiers(window.modifiers(), false);
            #[cfg(windows)]
            {
                if window.is_window_active() {
                    workspace.sync_text_input(window, cx);
                }
                workspace.alt_escape = alt_escape::AltEscape::install(
                    window,
                    cx,
                    workspace.editor_accepts_alt_escape(),
                );
            }
        })
        .detach();
        cx.observe_window_appearance(window, |workspace, window, cx| {
            workspace.apply_appearance(workspace.appearance, window, cx);
        })
        .detach();
        #[cfg(windows)]
        let alt_escape =
            alt_escape::AltEscape::install(window, cx, projects.active_project().is_some());
        let config = projects
            .active_project()
            .map(|project| project.cursor_config.clone())
            .unwrap_or_default();
        let input_mappings = nanobug::vim::InputMappings::default();
        style.update_ex_highlight_palette(&input_mappings);
        let mut workspace = Self {
            focus_handle,
            projects,
            tabs: TabBar::new(metrics, cx),
            recent_projects,
            start_panel: cx.new(|_| Panel::new(StartPage::content_metrics(metrics))),
            start_bodies: std::array::from_fn(|_| {
                cx.new(|_| Panel::new(StartPage::content_metrics(metrics)))
            }),
            start_preview: None,
            command_window: None,
            vim_menu: None,
            editor_sleep: None,
            input_mappings,
            vim_settings_path: (!isolate_data)
                .then(|| nanobug::app_data::settings::path().ok())
                .flatten(),
            ex_source_depth: 0,
            script_events: Default::default(),
            ex_sourced_files: Vec::new(),
            ex_directory: Default::default(),
            recovery: recovery::Recovery::new((!isolate_data && !cfg!(test)).then(|| {
                nanobug::app_data::project_sessions_directory()
                    .map(|path| path.with_file_name("recovery").join("editor"))
            })),
            noswapfile_buffers: None,
            active_ex_window_modifiers: Default::default(),
            restart_ex_command: None,
            main_window_removing: false,
            help_tag_directories: Vec::new(),
            ex_batch_undo: std::collections::HashMap::new(),
            ex_batch_active: false,
            mapping_timer: None,
            remove_recent_project: None,
            file_picker: None,
            file_picker_directory: None,
            help: HelpWindow::new(metrics, cx),
            settings: None,
            core_settings_baseline: Default::default(),
            cursor_settings_baseline: Default::default(),
            settings_generation: 0,
            settings_catalog_started: false,
            settings_writing: false,
            close_after_settings: false,
            dialog_backdrop: DialogBackdrop::default(),
            editor_close: None,
            forced_editor_close: None,
            language_selection: None,
            language_list: cx.new(|_| Panel::new(metrics)),
            popup_panel: cx.new(|_| Panel::new(metrics)),
            hint_delay: HintDelay::default(),
            hint_page: 0,
            hovered_controls: None,
            held_close_key: None,
            #[cfg(windows)]
            alt_escape,
            prefix: None,
            metrics,
            appearance,
            appearance_settings_path: nanobug::app_data::settings::path().ok(),
            numbered_marks_path,
            numbered_marks,
            window_geometry_path: None,
            display_scale: window.scale_factor(),
            glyphs,
            spacing,
            layout: Layout::default(),
            manager_chrome: cx.new(|_| Panel::new(metrics)),
            manager_body: cx.new(|_| Panel::new(metrics)),
            editor_panels: frames::EditorPanels::new(metrics, cx),
            footer_panel: cx.new(|_| Panel::new(metrics)),
            inactive_editor_panels: Vec::new(),
            markdown_cache: MarkdownCache::default(),
            markdown_pointer: pointer::MarkdownPointer::default(),
            tree_name_preview: cx.new(|_| Panel::new(metrics)),
            tree_name_preview_rect: None,
            scroll_epoch: Instant::now(),
            scroll_scheduled: false,
            last_cursor_space: None,
            cursor: cx.new(|_| Cursor::new(cursor_effect, config)),
            pane_resize: None,
            wheel: 0.,
            plugins: plugins::PluginCoordinator::new(
                (!isolated).then_some(plugins),
                plugin_trace,
                metrics,
                cx,
            ),
            #[cfg(feature = "plugin-probe")]
            probe: probe.map(|options| probe::Probe::new(options, metrics, cx)),
        };
        workspace.load_vim_config();
        workspace.start_recovery(window, cx);
        workspace
    }

    /// Enable geometry persistence to `path` for the application's main window.
    pub(crate) fn with_window_geometry_path(mut self, path: Option<PathBuf>) -> Self {
        self.window_geometry_path = path;
        self
    }

    /// Execute the startup `command` after a replacement process restores its project views.
    pub(crate) fn run_restart_command(
        &mut self,
        command: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.projects.active_project().is_some() {
            self.execute_ex_input(command, ":restart", 1, false, window, cx);
        }
    }

    /// Save the approved main-window close placement from `window` in `cx`.
    fn save_window_geometry(&self, window: &Window, cx: &App) {
        if let Some(path) = &self.window_geometry_path
            && let Err(error) = crate::ui::window_geometry::save(path, window, cx)
        {
            eprintln!("Could not save main window placement: {error:#}");
        }
    }

    /// Persist the placement and remove `window` after an in-app close completes in `cx`.
    fn remove_main_window(&mut self, window: &mut Window, cx: &App) {
        self.main_window_removing = true;
        self.recovery.finish();
        self.save_numbered_mark();
        self.save_window_geometry(window, cx);
        window.remove_window();
    }

    /// Whether a popup currently owns pane input.
    fn modal_open(&self) -> bool {
        self.plugins.open
            || self.file_picker.is_some()
            || self.help.is_open()
            || self.small_modal_open()
    }

    /// Whether an overlay over the panes currently owns input.
    fn small_modal_open(&self) -> bool {
        self.settings.is_some()
            || self.vim_menu.is_some()
            || self.command_window.is_some()
            || self.language_selection.is_some()
            || self.editor_close.is_some()
            || self.remove_recent_project.is_some()
    }

    /// Enable Alt+Escape when the editor pane or its keyboard help owns input.
    #[cfg(windows)]
    fn editor_accepts_alt_escape(&self) -> bool {
        self.help.keyboard_visible()
            || !self.modal_open()
                && self
                    .projects
                    .active_project()
                    .is_some_and(|project| project.focus == layout::Focus::Editor)
    }
}
