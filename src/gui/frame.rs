//! Red frame drawn around the region that is currently being recorded.
//!
//! WGC can only capture whole items, so a region recording is a monitor capture
//! plus a crop — and on Windows 10 the compositor's "you are being captured"
//! border cannot be turned off, which makes it outline the *monitor* instead of
//! the selected area. Region recording therefore uses GDI capture (no border at
//! all) and this click-through frame marks the actual recorded rectangle.
//!
//! The frame is a hollow window: the window *region* is the ring only, so there
//! is no window over the captured pixels at all. That matters twice over — a
//! layered (colour-keyed) window over the selection makes `BitBlt` with
//! `CAPTUREBLT` miss GPU-composited content such as Electron/WebView2 text (the
//! recording then shows the page background with no text), and a border drawn
//! *inside* the selection would end up in every recorded frame.

use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CombineRgn, CreateRectRgn, CreateSolidBrush, DeleteObject, EndPaint, FillRect,
    InvalidateRect, SetWindowRgn, HGDIOBJ, PAINTSTRUCT, RGN_DIFF,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, RegisterClassW, SetWindowPos,
    ShowWindow, HWND_TOPMOST, SWP_NOACTIVATE, SW_HIDE, SW_SHOWNOACTIVATE, WM_ERASEBKGND, WM_PAINT,
    WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

use crate::geometry::Rect as Area;

const CLASS_NAME: PCWSTR = w!("RecRRecFrame");
/// Thickness of the frame, in physical pixels. It is drawn *outside* `area`.
const BORDER: i32 = 3;
/// `CreateSolidBrush` wants a COLORREF (0x00BBGGRR).
const RED: COLORREF = COLORREF(0x0000_00FF);

static REGISTERED: AtomicBool = AtomicBool::new(false);

/// Owns the frame window; dropping it removes the frame.
pub struct RecFrame {
    hwnd: HWND,
}

impl RecFrame {
    /// Create (or move) the frame so it hugs `area` (virtual-desktop pixels).
    ///
    /// The ring is drawn just outside `area` so it never appears in the recording.
    pub fn show(area: Area) -> Result<Self> {
        unsafe {
            register_class()?;
            let hinstance = GetModuleHandleW(None).context("GetModuleHandleW")?;
            let hwnd = CreateWindowExW(
                WS_EX_TRANSPARENT
                    | WS_EX_TOPMOST
                    | WS_EX_TOOLWINDOW
                    | WS_EX_NOACTIVATE,
                CLASS_NAME,
                w!("recording"),
                WS_POPUP,
                area.x - BORDER,
                area.y - BORDER,
                area.width + 2 * BORDER,
                area.height + 2 * BORDER,
                None,
                None,
                Some(hinstance.into()),
                None,
            )
            .context("CreateWindowExW(RecRRecFrame)")?;
            apply_hollow_region(hwnd, area);
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            let _ = InvalidateRect(Some(hwnd), None, true);
            Ok(Self { hwnd })
        }
    }

    /// Move the frame to another rectangle (keeps the same window).
    pub fn move_to(&self, area: Area) {
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                area.x - BORDER,
                area.y - BORDER,
                area.width + 2 * BORDER,
                area.height + 2 * BORDER,
                SWP_NOACTIVATE,
            );
            apply_hollow_region(self.hwnd, area);
            let _ = InvalidateRect(Some(self.hwnd), None, true);
        }
    }
}

/// Give the window a ring-shaped region: everything is window except `area`.
unsafe fn apply_hollow_region(hwnd: HWND, area: Area) {
    let (w, h) = (area.width + 2 * BORDER, area.height + 2 * BORDER);
    let outer = CreateRectRgn(0, 0, w, h);
    let inner = CreateRectRgn(BORDER, BORDER, BORDER + area.width, BORDER + area.height);
    if outer.is_invalid() || inner.is_invalid() {
        return;
    }
    CombineRgn(Some(outer), Some(outer), Some(inner), RGN_DIFF);
    let _ = DeleteObject(HGDIOBJ(inner.0));
    // The system owns the region from here on.
    let _ = SetWindowRgn(hwnd, Some(outer), true);
}

impl Drop for RecFrame {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

fn register_class() -> Result<()> {
    if REGISTERED.load(Ordering::Relaxed) {
        return Ok(());
    }
    unsafe {
        let hinstance = GetModuleHandleW(None).context("GetModuleHandleW")?;
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance.into(),
            lpszClassName: CLASS_NAME,
            ..Default::default()
        };
        RegisterClassW(&wc);
    }
    REGISTERED.store(true, Ordering::Relaxed);
    Ok(())
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut client = RECT::default();
            let _ = GetClientRect(hwnd, &mut client);
            // Only the ring is part of the window, so fill everything.
            let brush = CreateSolidBrush(RED);
            let _ = FillRect(hdc, &client, brush);
            let _ = DeleteObject(HGDIOBJ(brush.0));
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        // The desktop shows through the hole; no erase needed.
        WM_ERASEBKGND => LRESULT(1),
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Hide and forget a frame (used when a recording ends).
pub fn hide(frame: &mut Option<RecFrame>) {
    if let Some(f) = frame.take() {
        unsafe {
            let _ = ShowWindow(f.hwnd, SW_HIDE);
        }
    }
}