//! Process startup: resolve the initial project, register assets and open the native workspace.

use crate::{
    grid::{Workspace, WorkspaceTabs},
    ui::{
        icons::IconCache,
        theme::{AppearanceSettings, GridFont},
        window_geometry,
    },
};
use gpui::{App, AppContext, TitlebarOptions, WindowBackgroundAppearance, WindowOptions, px, size};
use nanobug::app_data::settings;
use std::{ffi::OsString, path::PathBuf, sync::Mutex};

/// Exit status requested by an approved Ex close.
static EXIT_CODE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

/// A tab to reopen in its original role, even if its path has changed on disk.
pub(crate) enum RestartTab {
    Start,
    Project(PathBuf),
    File(PathBuf),
}

/// The tabs, selection and optional Ex input passed to the next native process.
pub(crate) struct Restart {
    pub(crate) tabs: Vec<RestartTab>,
    pub(crate) selected: usize,
    pub(crate) command: String,
}

static RESTART: Mutex<Option<Restart>> = Mutex::new(None);

/// Queue an approved restart to launch when the native event loop exits.
pub(crate) fn schedule_restart(restart: Restart) {
    *RESTART.lock().expect("restart request") = Some(restart);
}

/// Publish `code` after closing the native workspace has been approved.
pub(crate) fn set_exit_code(code: i32) {
    EXIT_CODE.store(code, std::sync::atomic::Ordering::Relaxed);
}

/// Open the supplied directories or files, or show the start page without arguments.
pub(crate) fn run() {
    let cwd = std::env::current_dir().unwrap_or_else(|_| env!("CARGO_MANIFEST_DIR").into());
    let mut args: Vec<_> = std::env::args_os().skip(1).collect();
    let restart = take_restart(&mut args);
    let mut plugins = match nanobug::plugins::session::Options::take(&mut args, &cwd) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("{error:#}");
            return;
        }
    };
    let plugin_trace = plugins.start_trace();
    if let Some(trace) = &plugin_trace {
        trace.mark("process_enter", serde_json::Value::Null);
    }
    #[cfg(feature = "plugin-probe")]
    let mut probe = {
        match crate::plugin_probe::Options::take(&mut args) {
            Ok(probe) => probe,
            Err(error) => {
                eprintln!("{error:#}");
                return;
            }
        }
    };
    let restore_sessions = !plugins.isolated();
    #[cfg(feature = "plugin-probe")]
    let restore_sessions = restore_sessions && probe.is_none();
    // The initial documents and Explorer rows use no native window state.
    let projects = std::thread::spawn(move || {
        crate::ui::theme::load_for_editor();
        if let Some(restart) = restart {
            WorkspaceTabs::restore_restart(restart, restore_sessions)
        } else {
            let paths = args.into_iter().map(|path| cwd.join(path)).collect();
            (
                WorkspaceTabs::with_project_sessions(paths, restore_sessions),
                None,
            )
        }
    });
    #[cfg(windows)]
    let platform: std::rc::Rc<dyn gpui::Platform> = std::rc::Rc::new(
        gpui_windows::WindowsPlatform::new_with_parallel_graphics()
            .expect("failed to initialize Windows platform")
            .with_modal_escape(),
    );
    #[cfg(not(windows))]
    let platform = gpui_platform::current_platform(false);
    let glyphs = platform.text_system();
    if let Some(trace) = &plugin_trace {
        trace.mark("platform_ready", serde_json::Value::Null);
    }
    #[cfg(feature = "plugin-probe")]
    if let Some(probe) = &mut probe {
        probe.milestone("platform_ready");
    }
    // Register the bundled text and box-drawing faces before GPUI measures the grid.
    let font_system = glyphs.clone();
    let font_trace = plugin_trace.clone();
    let fonts = std::thread::spawn(move || {
        let mut appearance = settings::path()
            .and_then(|path| AppearanceSettings::load(&path))
            .unwrap_or_default();
        font_system
            .add_fonts(vec![
                std::borrow::Cow::Borrowed(include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/assets/fonts/RecMonoCasual-Regular-1.085.ttf"
                ))),
                std::borrow::Cow::Borrowed(include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/assets/fonts/AlibabaPuHuiTi-3-55-Regular.ttf"
                ))),
                std::borrow::Cow::Borrowed(include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/assets/fonts/NanobugGrid-Regular.ttf"
                ))),
            ])
            .expect("register bundled grid fonts");
        if appearance.font == GridFont::MapleMono
            && let Err(error) = GridFont::ensure_maple(font_system.as_ref())
        {
            eprintln!("{error:#}");
            appearance.font = GridFont::default();
        }
        if let Some(trace) = font_trace {
            trace.mark("fonts_registered", serde_json::Value::Null);
        }
        appearance
    });
    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    let (platform, native_atlases) = crate::ui::native_atlas::NativeAtlases::wrap(platform);
    gpui::Application::with_platform(platform).run(move |cx: &mut App| {
        if let Some(trace) = &plugin_trace {
            trace.mark("graphics_ready", serde_json::Value::Null);
        }
        #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
        native_atlases.install(cx);
        cx.set_global(IconCache::default());
        let geometry_path = restore_sessions.then(|| settings::path().ok()).flatten();
        let (bounds, display_id) = window_geometry::startup(geometry_path.as_deref(), cx);
        let (projects, startup) = projects.join().expect("startup project preparation");
        let workspace = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(bounds),
                    display_id,
                    window_min_size: Some(size(px(800.), px(480.))),
                    window_background: WindowBackgroundAppearance::Blurred,
                    titlebar: Some(TitlebarOptions {
                        title: Some("Nanobug V2".into()),
                        appears_transparent: cfg!(windows),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                move |window, cx| {
                    let appearance = fonts.join().expect("bundled font preparation");
                    #[cfg(feature = "plugin-probe")]
                    if let Some(probe) = &mut probe {
                        probe.milestone("fonts_registered");
                    }
                    cx.new(|cx| {
                        let workspace = Workspace::new(
                            projects,
                            glyphs,
                            appearance,
                            window,
                            cx,
                            plugins,
                            plugin_trace.clone(),
                            #[cfg(feature = "plugin-probe")]
                            probe,
                        )
                        .with_window_geometry_path(geometry_path);
                        if let Some(trace) = &plugin_trace {
                            trace.mark("workspace_ready", serde_json::Value::Null);
                        }
                        workspace
                    })
                },
            )
            .expect("create Nanobug native window");
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        cx.activate(true);
        if let Some(command) = startup {
            let _ = workspace.update(cx, |workspace, window, cx| {
                workspace.run_restart_command(&command, window, cx);
            });
        }
    });
    if let Some(restart) = RESTART.lock().expect("restart request").take() {
        if let Err(error) = launch_restart(restart) {
            eprintln!("Could not restart Nanobug: {error}");
        }
    }
    let code = EXIT_CODE.load(std::sync::atomic::Ordering::Relaxed);
    if code != 0 {
        std::process::exit(code);
    }
}

const RESTART_OPTION: &str = "--nanobug-restart-command";

/// Consume the restart arguments before parsing ordinary options or filenames.
fn take_restart(args: &mut Vec<OsString>) -> Option<Restart> {
    if args.first()? != RESTART_OPTION {
        return None;
    }
    let selected = args.get(1)?.to_str()?.parse::<usize>().ok()?;
    let command = args.get(2)?.to_str()?.to_owned();
    let tab_count = args.get(3)?.to_str()?.parse::<usize>().ok()?;
    let end = tab_count.checked_mul(2)?.checked_add(4)?;
    let tabs = args
        .get(4..end)?
        .chunks_exact(2)
        .map(|pair| match pair[0].to_str()? {
            "start" => Some(RestartTab::Start),
            "project" => Some(RestartTab::Project(PathBuf::from(&pair[1]))),
            "file" => Some(RestartTab::File(PathBuf::from(&pair[1]))),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    args.drain(..end);
    Some(Restart {
        tabs,
        selected,
        command,
    })
}

/// Pass the restart state and preserve plugin options without shell interpretation.
fn launch_restart(restart: Restart) -> std::io::Result<()> {
    let mut original = std::env::args_os().skip(1).collect::<Vec<_>>();
    take_restart(&mut original);
    let mut options = Vec::<OsString>::new();
    let mut rest = original.into_iter();
    while let Some(arg) = rest.next() {
        match arg.to_str() {
            Some("--no-plugins") => options.push(arg),
            Some("--plugin-profile" | "--plugin-dev-profile" | "--plugin-trace") => {
                options.push(arg);
                options.extend(rest.next());
            }
            Some("--") => break,
            _ => {}
        }
    }
    let mut child = std::process::Command::new(std::env::current_exe()?);
    child
        .arg(RESTART_OPTION)
        .arg(restart.selected.to_string())
        .arg(restart.command)
        .arg(restart.tabs.len().to_string());
    for tab in restart.tabs {
        match tab {
            RestartTab::Start => child.args(["start", ""]),
            RestartTab::Project(path) => child.arg("project").arg(path),
            RestartTab::File(path) => child.arg("file").arg(path),
        };
    }
    child.args(options).spawn()?;
    Ok(())
}
