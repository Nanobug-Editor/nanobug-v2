//! Apply the workspace text-input policy before the next Windows paint.

use super::Workspace;
use gpui::{Context, Priority, Window};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::ptr::null_mut;
use windows_sys::Win32::{
    Foundation::HWND,
    UI::Input::{
        Ime::{
            CPS_CANCEL, IACE_DEFAULT, ImmAssociateContextEx, ImmGetContext, ImmNotifyIME,
            ImmReleaseContext, NI_COMPOSITIONSTR,
        },
        KeyboardAndMouse::GetFocus,
    },
};

impl Workspace {
    /// Sync the IME for `window` on `cx`'s GUI thread without waiting for GPUI's next paint.
    pub(in crate::grid::workspace) fn sync_native_ime(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let enabled = self.accepts_native_text();
        if !enabled {
            if let Some(project) = self.projects.active_project_mut() {
                project.editor.composition.clear();
            }
            self.help.composition.clear();
        }
        let Some(hwnd) = window_hwnd(window) else {
            return;
        };
        if ime_attached(hwnd) == enabled {
            return;
        }

        // IMM can send composition messages synchronously. Run outside the GPUI borrow
        // in a posted task, which the Windows message loop services ahead of hardware input.
        let mut window = window.to_async(cx);
        let workspace = cx.weak_entity();
        cx.foreground_executor()
            .spawn_with_priority(Priority::High, async move {
                if let Ok(Some((hwnd, enabled))) = window.update(|window, cx| {
                    let workspace = workspace.upgrade()?;
                    Some((
                        window_hwnd(window)?,
                        workspace.read(cx).accepts_native_text(),
                    ))
                }) {
                    set_ime_attached(hwnd, enabled);
                }
            })
            .detach();
    }
}

/// Return the native handle of `window` when backed by Win32.
fn window_hwnd(window: &Window) -> Option<HWND> {
    match HasWindowHandle::window_handle(window).ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get() as HWND),
        _ => None,
    }
}

/// Whether Windows currently associates an input context with `hwnd`.
fn ime_attached(hwnd: HWND) -> bool {
    let context = unsafe { ImmGetContext(hwnd) };
    if context.is_null() {
        return false;
    }
    unsafe { ImmReleaseContext(hwnd, context) };
    true
}

/// On the GUI thread, restore `hwnd`'s default context when `enabled` or detach it for commands.
fn set_ime_attached(hwnd: HWND, enabled: bool) {
    let context = unsafe { ImmGetContext(hwnd) };
    let attached = !context.is_null();
    if attached {
        unsafe {
            // The default context belongs to the thread. Cancel only this window's composition.
            if !enabled && GetFocus() == hwnd {
                ImmNotifyIME(context, NI_COMPOSITIONSTR, CPS_CANCEL, 0);
            }
            ImmReleaseContext(hwnd, context);
        }
    }
    if attached != enabled {
        let flags = if enabled { IACE_DEFAULT } else { 0 };
        unsafe { ImmAssociateContextEx(hwnd, null_mut(), flags) };
    }
}
