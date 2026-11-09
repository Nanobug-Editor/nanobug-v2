//! Route the Windows system chord Alt+Esc to the selected editor pane.

use gpui::{App, AsyncWindowContext, Keystroke, Window};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{
    cell::{Cell, RefCell},
    ptr::{null, null_mut},
    rc::{Rc, Weak},
};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Input::KeyboardAndMouse::{
            GetAsyncKeyState, VK_CONTROL, VK_ESCAPE, VK_LWIN, VK_RWIN, VK_SHIFT,
        },
        WindowsAndMessaging::{
            CallNextHookEx, GetForegroundWindow, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, LLKHF_ALTDOWN,
            LLKHF_UP, SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL,
        },
    },
};

thread_local! {
    /// The native callback runs on the GUI thread that installed it.
    static TARGET: RefCell<Weak<Target>> = const { RefCell::new(Weak::new()) };
}

/// Owns the keyboard hook for the active native window and unregisters it on deactivation.
pub(super) struct AltEscape {
    /// Win32 hook to remove on drop.
    hook: HHOOK,
    /// Shared callback state for the active window.
    target: Rc<Target>,
}

impl AltEscape {
    /// Install on the active `window` in `cx`; `enabled` reflects editor-pane focus.
    pub(super) fn install(window: &Window, cx: &App, enabled: bool) -> Option<Self> {
        if !window.is_window_active() {
            return None;
        }
        let RawWindowHandle::Win32(handle) = HasWindowHandle::window_handle(window).ok()?.as_raw()
        else {
            return None;
        };
        let hook = unsafe {
            SetWindowsHookExW(
                WH_KEYBOARD_LL,
                Some(keyboard_hook),
                GetModuleHandleW(null()),
                0,
            )
        };
        if hook.is_null() {
            return None;
        }
        let target = Rc::new(Target {
            hwnd: handle.hwnd.get() as HWND,
            window: window.to_async(cx),
            enabled: Cell::new(enabled),
            held: Cell::new(false),
        });
        TARGET.with(|current| current.replace(Rc::downgrade(&target)));
        Some(Self { hook, target })
    }

    /// Accept new chords when `enabled`; a chord already consumed stays held until key-up.
    pub(super) fn set_enabled(&self, enabled: bool) {
        self.target.enabled.set(enabled);
    }
}

impl Drop for AltEscape {
    fn drop(&mut self) {
        unsafe { UnhookWindowsHookEx(self.hook) };
    }
}

/// Scoped state used by the native hook without borrowing the workspace.
struct Target {
    /// Only accept input when this HWND is the OS foreground window.
    hwnd: HWND,
    /// Queue a GPUI keystroke after returning from the hook.
    window: AsyncWindowContext,
    /// Whether the editor pane owns input.
    enabled: Cell<bool>,
    /// Whether the current Escape press was intercepted.
    held: Cell<bool>,
}

impl Target {
    /// Test the Escape event's `flags` and schedule the chord only on its first key-down.
    fn consume_escape(&self, flags: u32) -> bool {
        let foreground = unsafe { GetForegroundWindow() } == self.hwnd;
        if flags & LLKHF_UP != 0 {
            return self.held.replace(false) && foreground;
        }
        if !foreground {
            return false;
        }
        if self.held.get() {
            return true;
        }
        if !self.enabled.get()
            || flags & LLKHF_ALTDOWN == 0
            || [VK_CONTROL, VK_SHIFT, VK_LWIN, VK_RWIN]
                .into_iter()
                .any(|key| unsafe { GetAsyncKeyState(i32::from(key)) } < 0)
        {
            return false;
        }
        self.held.set(true);
        self.window
            .spawn(async |cx| {
                let _ = cx.update(|window, cx| {
                    if window.is_window_active() {
                        window.dispatch_keystroke(Keystroke::parse("alt-escape").unwrap(), cx);
                    }
                });
            })
            .detach();
        true
    }
}

/// Inspect Escape in Win32's actionable `code`; forward every unhandled `message` and `data`.
unsafe extern "system" fn keyboard_hook(code: i32, message: WPARAM, data: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        // Windows supplies a KBDLLHOOKSTRUCT for HC_ACTION, valid during this callback.
        let event = unsafe { &*(data as *const KBDLLHOOKSTRUCT) };
        if event.vkCode == u32::from(VK_ESCAPE)
            && TARGET
                .with(|current| current.borrow().upgrade())
                .is_some_and(|target| target.consume_escape(event.flags))
        {
            return 1;
        }
    }
    unsafe { CallNextHookEx(null_mut(), code, message, data) }
}
