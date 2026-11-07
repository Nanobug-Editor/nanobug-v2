//! Native GPUI entry point. The project has no terminal, webview or external editing core.
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
mod app;
mod backdrop;
mod cursor;
mod drawing;
mod explorer;
mod file_transfer;
mod input;
mod shortcuts;

use gpui::{
    App, AppContext, Bounds, TitlebarOptions, WindowBackgroundAppearance, WindowBounds,
    WindowOptions, px, size,
};

/// Open the supplied directory/file, or the current working directory when no argument is given.
fn main() {
    let path = std::env::args_os()
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::env::current_dir().unwrap_or_else(|_| env!("CARGO_MANIFEST_DIR").into())
        });
    let path = if path.is_relative() {
        std::env::current_dir().unwrap_or_default().join(path)
    } else {
        path
    };
    let initial = path.is_file().then(|| path.clone());
    let project = if initial.is_some() {
        path.parent().unwrap().to_path_buf()
    } else {
        path
    };
    let platform = gpui_platform::current_platform(false);
    let glyphs = platform.text_system();
    // Register the bundled font before GPUI measures or shapes the first grid row.
    glyphs
        .add_fonts(vec![std::borrow::Cow::Borrowed(include_bytes!(
            "../assets/fonts/NanobugGrid-Regular.ttf"
        ))])
        .expect("register bundled grid font");
    gpui::Application::with_platform(platform).run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1320.), px(840.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(800.), px(480.))),
                window_background: WindowBackgroundAppearance::Blurred,
                titlebar: Some(TitlebarOptions {
                    title: Some("Nanobug V2".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            move |window, cx| cx.new(|cx| app::Editor::new(project, initial, glyphs, window, cx)),
        )
        .expect("create Nanobug native window");
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        cx.activate(true);
    });
}
