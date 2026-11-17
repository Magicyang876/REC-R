//! Win32 helpers: DPI awareness, monitor + window enumeration, GDI helpers.

pub mod monitor;
pub mod window;

use windows::Win32::UI::HiDpi::{
    GetThreadDpiAwarenessContext, GetAwarenessFromDpiAwarenessContext, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, DPI_AWARENESS_PER_MONITOR_AWARE,
    DPI_AWARENESS_SYSTEM_AWARE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

use crate::geometry::Rect;

/// Must be called once, before any window/monitor query, so that all
/// coordinates are physical pixels.
pub fn init_dpi_awareness() {
    // Fails harmlessly if already set by a manifest.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

/// True when the process is DPI-aware (per-monitor v2 sets this).
pub fn is_dpi_aware() -> bool {
    unsafe {
        let ctx = GetThreadDpiAwarenessContext();
        matches!(
            GetAwarenessFromDpiAwarenessContext(ctx),
            DPI_AWARENESS_SYSTEM_AWARE | DPI_AWARENESS_PER_MONITOR_AWARE
        )
    }
}

/// Bounding box of every monitor attached to the desktop.
pub fn virtual_desktop_rect() -> Rect {
    unsafe {
        Rect::new(
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    }
}

