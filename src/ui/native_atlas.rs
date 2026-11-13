//! Track the plugin SVG keys painted by GPUI without replacing the native rasterizer.
//! DirectX and WGPU order tile uploads after prior draws. The pinned Metal backend
//! releases the underlying tile only when the GPU finishes its last submitted use.

#[cfg(test)]
#[cfg_attr(
    target_os = "macos",
    allow(
        dead_code,
        reason = "the Cocoa probe runs in the main-thread test target"
    )
)]
#[path = "native_atlas/tests.rs"]
pub(crate) mod tests;

use anyhow::Result;
use futures_channel::oneshot;
use gpui::{
    Action, ActivationPolicy, ActivityGuard, AnyWindowHandle, App, AppLifecyclePhase,
    BackgroundExecutor, Bounds, ClipboardItem, ClipboardReadError, CursorStyle, DevicePixels,
    ForegroundExecutor, Global, GraphicalEnvironment, Keymap, Menu, MenuItem, OwnedMenu,
    PathPromptOptions, Pixels, Platform, PlatformAtlas, PlatformDisplay, PlatformGestures,
    PlatformInfo, PlatformKeyboardLayout, PlatformKeyboardMapper, PlatformTextSystem,
    PlatformWindow, RenderSvgParams, SMOOTH_SVG_SCALE_FACTOR, ScreenCaptureSource, SharedString,
    SystemNotification, SystemNotificationResponse, Task, ThermalState, Window, WindowAppearance,
    WindowButtonLayout, WindowId, WindowParams, WindowingRequest, size,
};
use smallvec::SmallVec;
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    ffi::OsString,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, Weak},
};

/// One weak atlas handle and the plugin keys in each native window's recent scenes.
#[derive(Clone, Default)]
pub(crate) struct NativeAtlases(Rc<RefCell<HashMap<WindowId, WindowAtlas>>>);

impl Global for NativeAtlases {}

impl NativeAtlases {
    /// Capture atlases from the native platform's windows, leaving the windows themselves intact.
    pub(crate) fn wrap(inner: Rc<dyn Platform>) -> (Rc<dyn Platform>, Self) {
        let atlases = Self::default();
        let platform = NativePlatform {
            inner,
            atlases: atlases.clone(),
        };
        (Rc::new(platform), atlases)
    }

    pub(crate) fn install(self, cx: &mut App) {
        cx.on_window_closed({
            let atlases = self.clone();
            move |_, id| {
                atlases.0.borrow_mut().remove(&id);
            }
        })
        .detach();
        cx.set_global(self);
    }

    /// Inspect the real atlas using the same cache lookup as a native SVG paint.
    #[cfg(all(test, windows))]
    pub(crate) fn test_atlas(
        &self,
        window: WindowId,
    ) -> Option<(Arc<dyn PlatformAtlas>, Vec<RenderSvgParams>)> {
        let windows = self.0.borrow();
        let state = windows.get(&window)?;
        Some((
            state.atlas.upgrade()?,
            state.displayed.iter().cloned().collect(),
        ))
    }
}

struct WindowAtlas {
    atlas: Weak<dyn PlatformAtlas>,
    drawing: HashSet<RenderSvgParams>,
    displayed: HashSet<RenderSvgParams>,
    retired: HashSet<RenderSvgParams>,
    retirement_queued: bool,
}

impl WindowAtlas {
    fn new(atlas: &Arc<dyn PlatformAtlas>) -> Self {
        Self {
            atlas: Arc::downgrade(atlas),
            drawing: HashSet::new(),
            displayed: HashSet::new(),
            retired: HashSet::new(),
            retirement_queued: false,
        }
    }

    /// A key may reappear in a new scene before the scheduled callback executes.
    fn retire(&mut self) {
        self.retirement_queued = false;
        if let Some(atlas) = self.atlas.upgrade() {
            for key in self.retired.drain() {
                if !self.displayed.contains(&key) {
                    atlas.remove(&key.into());
                }
            }
        } else {
            self.retired.clear();
        }
    }
}

/// Keep the fixed GPUI version's SVG sizing. Rounding the logical result of `pixel_snap`
/// back to a device pixel can diverge at fractional scales, so operate in device space.
fn svg_key(window: &Window, bounds: Bounds<Pixels>, path: SharedString) -> RenderSvgParams {
    let scale = window.scale_factor();
    let snap = |value: Pixels| {
        let device = f32::from(value) * scale;
        (device.abs() - 0.5).ceil().copysign(device)
    };
    let extent = |start, end| {
        let start = snap(start);
        DevicePixels::from(((snap(end).max(start) - start) * SMOOTH_SVG_SCALE_FACTOR).ceil() as i32)
    };
    RenderSvgParams {
        path,
        size: size(
            extent(bounds.left(), bounds.right()),
            extent(bounds.top(), bounds.bottom()),
        ),
    }
}

/// Record a successfully painted host-namespaced SVG for this window's new scene.
pub(crate) fn mark_svg(window: &Window, cx: &App, bounds: Bounds<Pixels>, id: &SharedString) {
    if let Some(atlases) = cx.try_global::<NativeAtlases>()
        && let Some(atlas) = atlases
            .0
            .borrow_mut()
            .get_mut(&window.window_handle().window_id())
        && atlas.atlas.strong_count() > 0
    {
        atlas.drawing.insert(svg_key(window, bounds, id.clone()));
    }
}

/// Finalize the root's uncached plugin Canvas scene after all its paint callbacks have run.
pub(crate) fn finish_frame(window: &Window, cx: &App) {
    let Some(atlases) = cx.try_global::<NativeAtlases>() else {
        return;
    };
    let id = window.window_handle().window_id();
    let mut windows = atlases.0.borrow_mut();
    let Some(atlas) = windows.get_mut(&id) else {
        return;
    };
    if atlas.drawing.is_empty() && atlas.displayed.is_empty() {
        return;
    }
    atlas
        .retired
        .extend(atlas.displayed.difference(&atlas.drawing).cloned());
    std::mem::swap(&mut atlas.displayed, &mut atlas.drawing);
    atlas.drawing.clear();
    if !atlas.retirement_queued && !atlas.retired.is_empty() {
        atlas.retirement_queued = true;
        // The current paint has not replaced GPUI's prior scene yet. The next callback
        // runs after that swap, so an older scene cannot address the freed tiles again.
        window.on_next_frame(move |_, cx| {
            if let Some(atlases) = cx.try_global::<NativeAtlases>()
                && let Some(atlas) = atlases.0.borrow_mut().get_mut(&id)
            {
                atlas.retire();
            }
        });
    }
}

struct NativePlatform {
    inner: Rc<dyn Platform>,
    atlases: NativeAtlases,
}

// Forward defaults as well as required methods so platform-specific overrides survive.
macro_rules! forward_platform {
    ($($(#[$attr:meta])* fn $method:ident($($arg:ident: $ty:ty),*) $(-> $result:ty)?;)*) => {
        $($(#[$attr])* fn $method(&self $(, $arg: $ty)*) $(-> $result)? {
            self.inner.$method($($arg),*)
        })*
    };
}

impl Platform for NativePlatform {
    fn open_window(
        &self,
        handle: AnyWindowHandle,
        options: WindowParams,
    ) -> Result<Box<dyn PlatformWindow>> {
        let window = self.inner.open_window(handle, options)?;
        self.atlases
            .0
            .borrow_mut()
            .insert(handle.window_id(), WindowAtlas::new(&window.sprite_atlas()));
        Ok(window)
    }

    forward_platform! {
        fn platform_info() -> PlatformInfo;
        fn background_executor() -> BackgroundExecutor;
        fn foreground_executor() -> ForegroundExecutor;
        fn text_system() -> Arc<dyn PlatformTextSystem>;
        fn run(on_finish_launching: Box<dyn 'static + FnOnce()>);
        fn quit();
        fn set_initial_windowing(request: WindowingRequest);
        fn request_windowing(request: WindowingRequest) -> Task<Result<()>>;
        fn restart(binary_path: Option<PathBuf>, arguments: Vec<OsString>);
        fn activate(ignoring_other_apps: bool);
        fn set_activation_policy(policy: ActivationPolicy);
        fn hide();
        fn hide_other_apps();
        fn unhide_other_apps();
        fn displays() -> Vec<Rc<dyn PlatformDisplay>>;
        fn primary_display() -> Option<Rc<dyn PlatformDisplay>>;
        fn on_displays_changed(callback: Box<dyn FnMut()>);
        fn active_window() -> Option<AnyWindowHandle>;
        fn window_stack() -> Option<Vec<AnyWindowHandle>>;
        fn is_screen_capture_supported() -> bool;
        fn screen_capture_sources() -> oneshot::Receiver<Result<Vec<Rc<dyn ScreenCaptureSource>>>>;
        fn window_appearance() -> WindowAppearance;
        fn set_window_appearance(appearance: Option<WindowAppearance>);
        fn button_layout() -> Option<WindowButtonLayout>;
        fn open_url(url: &str);
        fn on_open_urls(callback: Box<dyn FnMut(Vec<String>)>);
        fn register_url_scheme(url: &str) -> Task<Result<()>>;
        fn prompt_for_paths(options: PathPromptOptions) -> oneshot::Receiver<Result<Option<Vec<PathBuf>>>>;
        fn prompt_for_new_path(directory: &Path, suggested_name: Option<&str>) -> oneshot::Receiver<Result<Option<PathBuf>>>;
        fn can_select_mixed_files_and_dirs() -> bool;
        fn reveal_path(path: &Path);
        fn open_with_system(path: &Path);
        fn on_quit(callback: Box<dyn FnMut() -> bool>);
        fn on_reopen(callback: Box<dyn FnMut()>);
        fn on_system_sleep(callback: Box<dyn FnMut()>);
        fn on_system_wake(callback: Box<dyn FnMut()>);
        fn on_app_lifecycle(callback: Box<dyn FnMut(AppLifecyclePhase)>);
        fn on_memory_warning(callback: Box<dyn FnMut()>);
        fn gestures() -> Option<Rc<dyn PlatformGestures>>;
        fn set_menus(menus: Vec<Menu>, keymap: &Keymap);
        fn get_menus() -> Option<Vec<OwnedMenu>>;
        fn set_dock_menu(menu: Vec<MenuItem>, keymap: &Keymap);
        fn perform_dock_menu_action(action: usize);
        fn add_recent_document(path: &Path);
        fn update_jump_list(menus: Vec<MenuItem>, entries: Vec<SmallVec<[PathBuf; 2]>>) -> Task<Vec<SmallVec<[PathBuf; 2]>>>;
        fn on_app_menu_action(callback: Box<dyn FnMut(&dyn Action)>);
        fn on_will_open_app_menu(callback: Box<dyn FnMut()>);
        fn on_validate_app_menu_command(callback: Box<dyn FnMut(&dyn Action) -> bool>);
        fn thermal_state() -> ThermalState;
        fn on_thermal_state_change(callback: Box<dyn FnMut()>);
        fn prevent_idle_sleep(reason: &str) -> Task<Result<ActivityGuard>>;
        fn set_app_identity(identifier: &str, name: &str);
        fn show_system_notification(notification: SystemNotification);
        fn dismiss_system_notification(tag: &str);
        fn on_system_notification_response(callback: Box<dyn FnMut(SystemNotificationResponse)>);
        fn compositor_name() -> &'static str;
        fn graphical_environment() -> Option<GraphicalEnvironment>;
        fn app_path() -> Result<PathBuf>;
        fn path_for_auxiliary_executable(name: &str) -> Result<PathBuf>;
        fn set_cursor_style(style: CursorStyle);
        fn hide_cursor_until_mouse_moves();
        fn is_cursor_visible() -> bool;
        fn should_auto_hide_scrollbars() -> bool;
        fn read_from_clipboard() -> Option<ClipboardItem>;
        fn write_to_clipboard(item: ClipboardItem);
        fn read_from_clipboard_async() -> Task<Result<Option<ClipboardItem>, ClipboardReadError>>;
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        fn read_from_primary() -> Option<ClipboardItem>;
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        fn write_to_primary(item: ClipboardItem);
        #[cfg(target_os = "macos")]
        fn read_from_find_pasteboard() -> Option<ClipboardItem>;
        #[cfg(target_os = "macos")]
        fn write_to_find_pasteboard(item: ClipboardItem);
        fn write_credentials(url: &str, username: &str, password: &[u8]) -> Task<Result<()>>;
        #[cfg(target_os = "macos")]
        fn write_credentials_if_not_exists(url: &str, username: &str, password: &[u8]) -> Task<Result<bool>>;
        fn read_credentials(url: &str) -> Task<Result<Option<(String, Vec<u8>)>>>;
        fn delete_credentials(url: &str) -> Task<Result<()>>;
        fn keyboard_layout() -> Box<dyn PlatformKeyboardLayout>;
        fn keyboard_mapper() -> Rc<dyn PlatformKeyboardMapper>;
        fn on_keyboard_layout_change(callback: Box<dyn FnMut()>);
    }
}
