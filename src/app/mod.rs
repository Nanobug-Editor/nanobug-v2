//! Process startup: resolve the initial project, register assets and open the native workspace.

use crate::{grid::Workspace, ui::icons::IconCache};
use gpui::{
    App, AppContext, Bounds, TitlebarOptions, WindowBackgroundAppearance, WindowBounds,
    WindowOptions, px, size,
};

/// Open the supplied directories or files, or show the start page without arguments.
pub(crate) fn run() {
    let cwd = std::env::current_dir().unwrap_or_else(|_| env!("CARGO_MANIFEST_DIR").into());
    let paths: Vec<_> = std::env::args_os()
        .skip(1)
        .map(|path| cwd.join(path))
        .collect();
    let platform = gpui_platform::current_platform(false);
    let glyphs = platform.text_system();
    // Register the bundled text and box-drawing faces before GPUI measures the grid.
    glyphs
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
    gpui::Application::with_platform(platform).run(move |cx: &mut App| {
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
            move |window, cx| cx.new(|cx| Workspace::new(paths, glyphs, window, cx)),
        )
        .expect("create Nanobug native window");
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        cx.activate(true);
    });
}
