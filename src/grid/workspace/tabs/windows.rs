//! Resolve tab clicks after Windows finishes processing a native caption press.

use super::{TabPress, Workspace};
use gpui::{AsyncWindowContext, Context, Pixels, Point, WeakEntity, Window, point, px};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{cell::Cell, rc::Rc};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
    Graphics::Gdi::ScreenToClient,
    UI::{
        Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_RBUTTON},
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
        WindowsAndMessaging::{
            GetCursorPos, GetForegroundWindow, GetSystemMetrics, HTCAPTION, SM_SWAPBUTTON,
            WM_CANCELMODE, WM_KILLFOCUS, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOVE, WM_MOVING,
            WM_NCDESTROY, WM_NCLBUTTONDBLCLK, WM_NCLBUTTONDOWN, WM_NCLBUTTONUP,
        },
    },
};

/// Identity of the subclass that observes presses on the shared caption strip.
const TAB_SUBCLASS: usize = 1;

/// State shared by a caption press and any messages sent by the native move loop.
struct CaptionTabs {
    /// Workspace receiving a completed click.
    workspace: WeakEntity<Workspace>,
    /// GPUI window that owns this subclass and its GUI thread.
    window: AsyncWindowContext,
    /// Current click candidate, cleared as soon as a native move begins.
    press: Cell<Option<TabPress>>,
}

impl Workspace {
    /// Observe caption presses for this `window` using the workspace in `cx`.
    pub(in crate::grid::workspace) fn install_caption_tabs(window: &Window, cx: &Context<Self>) {
        let Ok(handle) = HasWindowHandle::window_handle(window) else {
            return;
        };
        let RawWindowHandle::Win32(handle) = handle.as_raw() else {
            return;
        };
        let hwnd = handle.hwnd.get() as HWND;
        let state = Rc::into_raw(Rc::new(CaptionTabs {
            workspace: cx.weak_entity(),
            window: window.to_async(cx),
            press: Cell::new(None),
        }));
        // The subclass owns this reference until WM_NCDESTROY on the same GUI thread.
        unsafe {
            if SetWindowSubclass(hwnd, Some(caption_message), TAB_SUBCLASS, state as usize) == 0 {
                drop(Rc::from_raw(state));
            }
        }
    }
}

impl CaptionTabs {
    /// Record the tab hit by a press at `position` in native client pixels.
    fn begin(&self, position: POINT) {
        let press = self
            .window
            .clone()
            .update(|window, cx| {
                let workspace = self.workspace.upgrade()?;
                workspace
                    .read(cx)
                    .caption_tab_press(logical_position(position, window.scale_factor()))
            })
            .ok()
            .flatten();
        self.press.set(press);
    }

    /// Complete a surviving press at `position` in `hwnd`'s client pixels.
    fn release(&self, hwnd: HWND, position: POINT) {
        let Some(press) = self.press.take() else {
            return;
        };
        if unsafe { GetForegroundWindow() } != hwnd {
            return;
        }
        let workspace = self.workspace.clone();
        self.window
            .spawn(async move |cx| {
                let _ = cx.update(|window, cx| {
                    let position = logical_position(position, window.scale_factor());
                    let _ = workspace.update(cx, |workspace, cx| {
                        workspace.finish_caption_tab_click(press, position, window, cx);
                    });
                });
            })
            .detach();
    }
}

/// Unpack Win32's signed point in the low and high words of a mouse message's `data`.
fn mouse_point(data: LPARAM) -> POINT {
    POINT {
        x: data as i16 as i32,
        y: (data >> 16) as i16 as i32,
    }
}

/// Convert a screen `position` to `hwnd`'s client coordinates.
fn client_position(hwnd: HWND, mut position: POINT) -> Option<POINT> {
    (unsafe { ScreenToClient(hwnd, &mut position) } != 0).then_some(position)
}

/// Convert a device-pixel client `position` to GPUI pixels using the window's `scale`.
fn logical_position(position: POINT, scale: f32) -> Point<Pixels> {
    point(px(position.x as f32 / scale), px(position.y as f32 / scale))
}

/// Observe `message` for `hwnd` while forwarding the native `hit` and `data` to Windows.
/// `id` and `state` identify the registered subclass and its retained state.
unsafe extern "system" fn caption_message(
    hwnd: HWND,
    message: u32,
    hit: WPARAM,
    data: LPARAM,
    id: usize,
    state: usize,
) -> LRESULT {
    let state = state as *const CaptionTabs;
    match message {
        WM_NCLBUTTONDOWN if hit == HTCAPTION as WPARAM => {
            // DefSubclassProc can run a nested loop that destroys the window.
            unsafe { Rc::increment_strong_count(state) };
            let state = unsafe { Rc::from_raw(state) };
            if let Some(position) = client_position(hwnd, mouse_point(data)) {
                state.begin(position);
            } else {
                state.press.set(None);
            }
            let result = unsafe { DefSubclassProc(hwnd, message, hit, data) };
            // Windows can consume the release inside its modal caption handler.
            if state.press.get().is_some() {
                let primary = unsafe {
                    if GetSystemMetrics(SM_SWAPBUTTON) == 0 {
                        VK_LBUTTON
                    } else {
                        VK_RBUTTON
                    }
                };
                let mut cursor = POINT::default();
                if unsafe {
                    GetAsyncKeyState(i32::from(primary)) >= 0 && GetCursorPos(&mut cursor) != 0
                } && let Some(position) = client_position(hwnd, cursor)
                {
                    state.release(hwnd, position);
                }
            }
            return result;
        }
        WM_NCLBUTTONUP => {
            if let Some(position) = client_position(hwnd, mouse_point(data)) {
                unsafe { &*state }.release(hwnd, position);
            }
        }
        WM_LBUTTONUP => unsafe { &*state }.release(hwnd, mouse_point(data)),
        WM_MOVING | WM_MOVE | WM_CANCELMODE | WM_KILLFOCUS | WM_LBUTTONDOWN
        | WM_NCLBUTTONDBLCLK => {
            unsafe { &*state }.press.set(None);
        }
        WM_NCDESTROY => unsafe {
            RemoveWindowSubclass(hwnd, Some(caption_message), id);
            let state = Rc::from_raw(state);
            state.press.set(None);
        },
        _ => {}
    }
    unsafe { DefSubclassProc(hwnd, message, hit, data) }
}
