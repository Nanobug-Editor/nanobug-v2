//! Open an offscreen native Workspace for tests using Windows DirectWrite and DirectX.

use super::Workspace;
use crate::ui::icons::IconCache;
use anyhow::{Context as _, Result};
use gpui::{
    AppContext, Application, AsyncApp, Bounds, WindowBounds, WindowHandle, WindowOptions, px, size,
};
use nanobug::plugins::session::Options;
use std::{borrow::Cow, cell::RefCell, path::PathBuf, rc::Rc};

/// Let real workspace callbacks run while a native assertion waits on the UI thread.
pub(super) fn run_async(
    project: PathBuf,
    options: Options,
    initialize: impl FnOnce(&mut Workspace) + 'static,
    test: impl AsyncFnOnce(WindowHandle<Workspace>, &mut AsyncApp) -> Result<()> + 'static,
) -> Result<()> {
    run_async_with_visibility(project, options, initialize, test, false)
}

/// Let Windows deliver real client geometry for tests that assert painted positions.
pub(super) fn run_async_visible(
    project: PathBuf,
    options: Options,
    initialize: impl FnOnce(&mut Workspace) + 'static,
    test: impl AsyncFnOnce(WindowHandle<Workspace>, &mut AsyncApp) -> Result<()> + 'static,
) -> Result<()> {
    run_async_with_visibility(project, options, initialize, test, true)
}

/// Build the same native workspace with an optionally visible, unfocused window.
fn run_async_with_visibility(
    project: PathBuf,
    options: Options,
    initialize: impl FnOnce(&mut Workspace) + 'static,
    test: impl AsyncFnOnce(WindowHandle<Workspace>, &mut AsyncApp) -> Result<()> + 'static,
    show: bool,
) -> Result<()> {
    use gpui::Platform;

    let settings_dir = tempfile::tempdir()?;
    let settings_path = settings_dir.path().join("settings.toml");
    let platform = Rc::new(gpui_windows::WindowsPlatform::new(false)?);
    let glyphs = platform.text_system();
    glyphs.add_fonts(vec![
        Cow::Borrowed(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/fonts/RecMonoCasual-Regular-1.085.ttf"
        ))),
        Cow::Borrowed(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/fonts/AlibabaPuHuiTi-3-55-Regular.ttf"
        ))),
        Cow::Borrowed(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/fonts/NanobugGrid-Regular.ttf"
        ))),
    ])?;
    let result = Rc::new(RefCell::new(None));
    let saved = Rc::clone(&result);
    let (platform, atlases) = crate::ui::native_atlas::NativeAtlases::wrap(platform);
    Application::new_inaccessible(platform).run(move |cx| {
        atlases.install(cx);
        cx.set_global(IconCache::default());
        let bounds = Bounds::centered(None, size(px(920.), px(580.)), cx);
        let window = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                focus: false,
                show,
                ..WindowOptions::default()
            },
            move |window, cx| {
                cx.new(|cx| {
                    let mut workspace = Workspace::new(
                        super::projects::WorkspaceTabs::new(vec![project]),
                        glyphs,
                        Default::default(),
                        window,
                        cx,
                        options,
                        None,
                        #[cfg(feature = "plugin-probe")]
                        None,
                    );
                    workspace.appearance_settings_path = Some(settings_path);
                    initialize(&mut workspace);
                    workspace
                })
            },
        );
        match window {
            Ok(window) => cx
                .spawn(async move |cx| {
                    let result = test(window, cx).await;
                    *saved.borrow_mut() = Some(result);
                    cx.update(|app| app.quit());
                })
                .detach(),
            Err(error) => {
                *saved.borrow_mut() = Some(Err(error));
                cx.quit();
            }
        }
    });
    result
        .borrow_mut()
        .take()
        .context("GPUI did not open the native test workspace")?
}
