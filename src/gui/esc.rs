//! Global Esc watcher used to cancel an in-progress recording.
//!
//! A low-level keyboard hook is installed only while something is recording and
//! is removed the moment the last recording ends. It never swallows the key:
//! Esc still reaches whatever window has focus, and the tray window additionally
//! receives [`WM_CANCEL_RECORDING`].

use std::sync::atomic::{AtomicIsize, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, PostMessageW, SetWindowsHookExW, UnhookWindowsHookEx, HHOOK, KBDLLHOOKSTRUCT,
    WH_KEYBOARD_LL, WM_APP, WM_KEYDOWN, WM_SYSKEYDOWN,
};

/// Sent to the tray window when Esc is pressed while a recording runs.
pub const WM_CANCEL_RECORDING: u32 = WM_APP + 2;

const VK_ESCAPE: u32 = 0x1B;

static HOOK: AtomicIsize = AtomicIsize::new(0);
static TARGET: AtomicIsize = AtomicIsize::new(0);

pub fn installed() -> bool {
    HOOK.load(Ordering::Relaxed) != 0
}

/// Install the watcher, posting [`WM_CANCEL_RECORDING`] to `target`.
///
/// Returns `true` when the hook is now installed.
pub fn install(target: HWND) -> bool {
    if installed() {
        return true;
    }
    TARGET.store(target.0 as isize, Ordering::Relaxed);
    match unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0) } {
        Ok(hook) => {
            HOOK.store(hook.0 as isize, Ordering::Relaxed);
            tracing::debug!("esc cancel hook installed");
            true
        }
        Err(e) => {
            TARGET.store(0, Ordering::Relaxed);
            tracing::warn!("esc cancel hook unavailable: {e}");
            false
        }
    }
}

pub fn uninstall() {
    let raw = HOOK.swap(0, Ordering::Relaxed);
    TARGET.store(0, Ordering::Relaxed);
    if raw != 0 {
        unsafe {
            let _ = UnhookWindowsHookEx(HHOOK(raw as *mut _));
        }
        tracing::debug!("esc cancel hook removed");
    }
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // A negative code means "do not process, pass it on".
    if code >= 0 && matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN) {
        let info = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
        tracing::debug!("esc hook: vk={} flags=0x{:x}", info.vkCode, info.flags.0);
        if info.vkCode == VK_ESCAPE {
            let target = TARGET.load(Ordering::Relaxed);
            if target != 0 {
                let _ = PostMessageW(
                    Some(HWND(target as *mut _)),
                    WM_CANCEL_RECORDING,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}