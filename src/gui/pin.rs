//! Pin a capture to the screen.
//!
//! The editor's `Pin` button freezes the current (annotated) bitmap in a
//! borderless, always-on-top window: left-drag moves it, right-click or `Esc`
//! closes it. Windows are created on the UI thread and outlive the editor; the
//! host pump keeps dispatching their messages.

use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, EndPaint, PAINTSTRUCT, SRCCOPY,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos, GetWindowLongPtrW, GetWindowRect,
    LoadCursorW, RegisterClassW, SetForegroundWindow, SetWindowLongPtrW, SetWindowPos, CS_HREDRAW,
    CS_VREDRAW, GWLP_USERDATA, IDC_SIZEALL, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, WM_DESTROY,
    WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT,
    WM_RBUTTONDOWN, WNDCLASSW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
};

use crate::capture::Bitmap;
use crate::gui::annotate::Canvas;

const CLASS_NAME: PCWSTR = w!("RecRPin");

static REGISTERED: AtomicBool = AtomicBool::new(false);

struct Pin {
    canvas: Canvas,
    /// Cursor and window origin at drag start, both in screen coordinates.
    drag: Option<(i32, i32, i32, i32)>,
}

/// Show `bmp` as a pinned window at screen position (`x`, `y`).
pub fn pin(bmp: Bitmap, x: i32, y: i32) -> Result<()> {
    unsafe {
        register_class()?;
        let hinstance = GetModuleHandleW(None)?;
        let mut canvas = Canvas::new(bmp.width, bmp.height)?;
        canvas.upload(&bmp.data);
        let state = Box::into_raw(Box::new(Pin { canvas, drag: None }));

        let hwnd = match CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            CLASS_NAME,
            w!("Rec-R pinned capture"),
            WS_POPUP | WS_VISIBLE,
            x,
            y,
            bmp.width,
            bmp.height,
            None,
            None,
            Some(hinstance.into()),
            Some(state as *const core::ffi::c_void),
        ) {
            Ok(hwnd) => hwnd,
            Err(e) => {
                let mut state = Box::from_raw(state);
                state.canvas.destroy();
                return Err(e).context("CreateWindowExW(RecRPin)");
            }
        };
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);
        tracing::info!("pinned {}x{} at {x},{y}", bmp.width, bmp.height);
        Ok(())
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Pin;
    if msg == WM_DESTROY {
        // Never PostQuitMessage: this window lives in the host's message loop.
        if !ptr.is_null() {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            let mut state = Box::from_raw(ptr);
            state.canvas.destroy();
        }
        return LRESULT(0);
    }
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let st = &mut *ptr;

    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let _ = BitBlt(
                hdc,
                0,
                0,
                st.canvas.width,
                st.canvas.height,
                Some(st.canvas.dc),
                0,
                0,
                SRCCOPY,
            );
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_LBUTTONDOWN => {
            let (mut cursor, mut window) = (POINT::default(), RECT::default());
            if GetCursorPos(&mut cursor).is_ok() && GetWindowRect(hwnd, &mut window).is_ok() {
                st.drag = Some((cursor.x, cursor.y, window.left, window.top));
                SetCapture(hwnd);
                let _ = SetForegroundWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            if let Some((sx, sy, wx, wy)) = st.drag {
                let mut cursor = POINT::default();
                if GetCursorPos(&mut cursor).is_ok() {
                    let (x, y) = drag_to(wx, wy, sx, sy, cursor.x, cursor.y);
                    let _ = SetWindowPos(
                        hwnd,
                        None,
                        x,
                        y,
                        0,
                        0,
                        SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            if st.drag.take().is_some() {
                let _ = ReleaseCapture();
            }
            LRESULT(0)
        }
        WM_RBUTTONDOWN => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_KEYDOWN => {
            if wparam.0 as u32 == 0x1B {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Window origin after the cursor moved from `(sx, sy)` to `(cx, cy)`,
/// given the origin `(wx, wy)` at grab time.
fn drag_to(wx: i32, wy: i32, sx: i32, sy: i32, cx: i32, cy: i32) -> (i32, i32) {
    (wx + cx - sx, wy + cy - sy)
}

unsafe fn register_class() -> Result<()> {
    if REGISTERED.load(Ordering::Relaxed) {
        return Ok(());
    }
    let hinstance = GetModuleHandleW(None)?;
    let wc = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(wndproc),
        hInstance: hinstance.into(),
        lpszClassName: CLASS_NAME,
        hCursor: LoadCursorW(None, IDC_SIZEALL).unwrap_or_default(),
        ..Default::default()
    };
    anyhow::ensure!(RegisterClassW(&wc) != 0, "RegisterClassW(RecRPin) failed");
    REGISTERED.store(true, Ordering::Relaxed);
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dragging_follows_the_cursor_delta() {
        assert_eq!(drag_to(100, 100, 50, 60, 50, 60), (100, 100));
        assert_eq!(drag_to(100, 100, 50, 60, 30, 20), (80, 60));
        assert_eq!(drag_to(-100, 200, 50, 60, 70, 10), (-80, 150));
    }
}
