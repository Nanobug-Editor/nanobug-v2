//! Process startup: resolve the initial project, register assets and open the native workspace.

use crate::{
    grid::{Workspace, WorkspaceTabs},
    ui::{
        icons::IconCache,
        theme::{AppearanceSettings, GridFont},
    },
};
use gpui::{
    App, AppContext, Bounds, TitlebarOptions, WindowBackgroundAppearance, WindowBounds,
    WindowOptions, px, size,
};
use nanobug::app_data::settings;

/// Open the supplied directories or files, or show the start page without arguments.
pub(crate) fn run() {
    let cwd = std::env::current_dir().unwrap_or_else(|_| env!("CARGO_MANIFEST_DIR").into());
    let mut args: Vec<_> = std::env::args_os().skip(1).collect();
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
    let paths: Vec<_> = args.into_iter().map(|path| cwd.join(path)).collect();
    let restore_sessions = !plugins.isolated();
    #[cfg(feature = "plugin-probe")]
    let restore_sessions = restore_sessions && probe.is_none();
    // The initial documents and Explorer rows use no native window state.
    let projects = std::thread::spawn(move || {
        crate::ui::theme::load_for_editor();
        WorkspaceTabs::with_project_sessions(paths, restore_sessions)
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
        let bounds = Bounds::centered(None, size(px(1320.), px(840.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
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
                let projects = projects.join().expect("startup project preparation");
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
                    );
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
    });
}
