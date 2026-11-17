//! Full-screen overlay the user drags a rectangle on.

use anyhow::{Context, Result};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleDC, CreateDIBSection, CreateFontW, CreatePen, DeleteDC,
    DeleteObject, DrawTextW, EndPaint, InvalidateRect, SelectObject, SetBkMode, SetTextColor,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CLEARTYPE_QUALITY, DEFAULT_CHARSET, DIB_RGB_COLORS,
    DT_SINGLELINE, DT_VCENTER, FF_DONTCARE, FW_BOLD, HGDIOBJ, PS_SOLID, SRCCOPY, TRANSPARENT,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetSystemMetrics, LoadCursorW, PostQuitMessage, RegisterClassW, SetCursor, SetForegroundWindow,
    ShowWindow, TranslateMessage, CS_HREDRAW, CS_VREDRAW,
    IDC_CROSS, MSG, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    SW_SHOW, WM_DESTROY, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT,
    WM_RBUTTONDOWN, WM_SETCURSOR, WNDCLASSW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
};

use crate::capture::gdi;
use crate::geometry::Rect;

const CLASS_NAME: PCWSTR = w!("RecRRegionOverlay");

struct OverlayState {
    dim_dc: windows::Win32::Graphics::Gdi::HDC,
    dim_bmp: windows::Win32::Graphics::Gdi::HBITMAP,
    orig_dc: windows::Win32::Graphics::Gdi::HDC,
    orig_bmp: windows::Win32::Graphics::Gdi::HBITMAP,
    old_dim: HGDIOBJ,
    old_orig: HGDIOBJ,
    width: i32,
    height: i32,
    origin: POINT,
    start: Option<POINT>,
    current: PointI,
    result: Option<Rect>,
    drag: bool,
}

#[derive(Clone, Copy, Default)]
struct PointI {
    x: i32,
    y: i32,
}

impl OverlayState {
    fn selection(&self) -> Option<RECT> {
        let start = self.start?;
        let (mut x0, mut y0) = (start.x, start.y);
        let (mut x1, mut y1) = (self.current.x, self.current.y);
        if x0 > x1 {
            std::mem::swap(&mut x0, &mut x1);
        }
        if y0 > y1 {
            std::mem::swap(&mut y0, &mut y1);
        }
        x0 = x0.clamp(0, self.width);
        x1 = x1.clamp(0, self.width);
        y0 = y0.clamp(0, self.height);
        y1 = y1.clamp(0, self.height);
        Some(RECT { left: x0, top: y0, right: x1, bottom: y1 })
    }
}

fn make_dib(
    width: i32,
    height: i32,
    data: &[u8],
) -> Result<(windows::Win32::Graphics::Gdi::HDC, windows::Win32::Graphics::Gdi::HBITMAP, HGDIOBJ)> {
    unsafe {
        let dc = CreateCompatibleDC(None);
        anyhow::ensure!(!dc.is_invalid(), "CreateCompatibleDC failed");
        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let bmp = CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0)
            .context("CreateDIBSection failed")?;
        anyhow::ensure!(!bits.is_null(), "no DIB bits");
        std::ptr::copy_nonoverlapping(data.as_ptr(), bits as *mut u8, data.len());
        let old = SelectObject(dc, bmp.into());
        Ok((dc, bmp, old))
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let ptr = windows::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(
        hwnd,
        windows::Win32::UI::WindowsAndMessaging::GWLP_USERDATA,
    ) as *mut OverlayState;

    match msg {
        WM_PAINT => {
            if ptr.is_null() {
                return DefWindowProcW(hwnd, msg, wparam, lparam);
            }
            let st = &mut *ptr;
            let mut ps = windows::Win32::Graphics::Gdi::PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let _ = BitBlt(hdc, 0, 0, st.width, st.height, Some(st.dim_dc), 0, 0, SRCCOPY);
            if let Some(r) = st.selection() {
                let w = (r.right - r.left).max(1);
                let h = (r.bottom - r.top).max(1);
                if w > 1 && h > 1 {
                    let _ = BitBlt(hdc, r.left, r.top, w, h, Some(st.orig_dc), r.left, r.top, SRCCOPY);
                    let pen = CreatePen(PS_SOLID, 2, COLORREF(0x00FF8040));
                    let old_pen = SelectObject(hdc, pen.into());
                    let brush = windows::Win32::Graphics::Gdi::GetStockObject(
                        windows::Win32::Graphics::Gdi::NULL_BRUSH,
                    );
                    let old_brush = SelectObject(hdc, brush);
                    let _ = windows::Win32::Graphics::Gdi::Rectangle(hdc, r.left, r.top, r.right, r.bottom);
                    SelectObject(hdc, old_brush);
                    SelectObject(hdc, old_pen);
                    let _ = DeleteObject(pen.into());

                    // Size badge
                    let label = format!("{} x {}", w, h);
                    let mut text: Vec<u16> = label.encode_utf16().collect();
                    let font = CreateFontW(
                        -18,
                        0,
                        0,
                        0,
                        FW_BOLD.0 as i32,
                        0,
                        0,
                        0,
                        DEFAULT_CHARSET,
                        windows::Win32::Graphics::Gdi::OUT_DEFAULT_PRECIS,
                        windows::Win32::Graphics::Gdi::CLIP_DEFAULT_PRECIS,
                        CLEARTYPE_QUALITY,
                        (FF_DONTCARE.0 | windows::Win32::Graphics::Gdi::VARIABLE_PITCH.0) as u32,
                        OutputPcwstr(b"Segoe UI\0").as_ref(),
                    );
                    let old_font = SelectObject(hdc, font.into());
                    let _ = SetBkMode(hdc, TRANSPARENT);
                    let _ = SetTextColor(hdc, COLORREF(0x00FFFFFF));
                    let mut box_ = RECT {
                        left: r.left + 4,
                        top: if r.top > 24 { r.top - 22 } else { r.top + 4 },
                        right: r.left + 140,
                        bottom: if r.top > 24 { r.top - 2 } else { r.top + 24 },
                    };
                    DrawTextW(hdc, &mut text, &mut box_, DT_SINGLELINE | DT_VCENTER);
                    SelectObject(hdc, old_font);
                    let _ = DeleteObject(font.into());
                }
            }
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            tracing::debug!("overlay: LBUTTONDOWN at {},{}", lo(lparam), hi(lparam));
            if !ptr.is_null() {
                let st = &mut *ptr;
                st.drag = true;
                st.start = Some(POINT { x: lo(lparam), y: hi(lparam) });
                st.current = PointI { x: lo(lparam), y: hi(lparam) };
                unsafe {
                    SetCapture(hwnd);
                }
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            if !ptr.is_null() {
                let st = &mut *ptr;
                st.current = PointI { x: lo(lparam), y: hi(lparam) };
                if st.drag {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            tracing::debug!("overlay: LBUTTONUP at {},{}", lo(lparam), hi(lparam));
            if !ptr.is_null() {
                let st = &mut *ptr;
                st.current = PointI { x: lo(lparam), y: hi(lparam) };
                st.drag = false;
                unsafe {
                    let _ = ReleaseCapture();
                }
                if let Some(r) = st.selection() {
                    let w = r.right - r.left;
                    let h = r.bottom - r.top;
                    if w >= 8 && h >= 8 {
                        st.result = Some(Rect::new(
                            r.left + st.origin.x,
                            r.top + st.origin.y,
                            w,
                            h,
                        ));
                        tracing::debug!("overlay: result {:?}", st.result);
                        unsafe {
                            let _ = DestroyWindow(hwnd);
                        }
                        return LRESULT(0);
                    }
                }
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            if wparam.0 == 0x1B {
                // Esc
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
            }
            LRESULT(0)
        }
        WM_RBUTTONDOWN => {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_SETCURSOR => {
            unsafe {
                if let Ok(cursor) = LoadCursorW(None, IDC_CROSS) {
                    SetCursor(Some(cursor));
                }
            }
            LRESULT(1)
        }
        WM_DESTROY => {
            // Keep the state alive: the caller reads `result` out of it after
            // the message loop ends and then frees it.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn lo(lparam: LPARAM) -> i32 {
    (lparam.0 & 0xFFFF) as i16 as i32
}

fn hi(lparam: LPARAM) -> i32 {
    ((lparam.0 >> 16) & 0xFFFF) as i16 as i32
}

/// Dimmed copy of the desktop for the overlay background.
fn dim(data: &[u8]) -> Vec<u8> {
    let mut out = data.to_vec();
    for px in out.chunks_exact_mut(4) {
        px[0] = (px[0] as u32 * 110 / 255) as u8;
        px[1] = (px[1] as u32 * 110 / 255) as u8;
        px[2] = (px[2] as u32 * 110 / 255) as u8;
        px[3] = 255;
    }
    out
}

/// Show the selection overlay over every monitor and wait for the result.
pub fn select_region_blocking() -> Result<Option<Rect>> {
    unsafe {
        let origin_x = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let origin_y = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let width = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let height = GetSystemMetrics(SM_CYVIRTUALSCREEN);
        anyhow::ensure!(width > 0 && height > 0, "virtual desktop has no size");

        let shot = gdi::capture_screen(Rect::new(origin_x, origin_y, width, height))
            .context("capturing the desktop for the overlay failed")?;
        let (dim_dc, dim_bmp, old_dim) = make_dib(width, height, &dim(&shot.data))?;
        let (orig_dc, orig_bmp, old_orig) = make_dib(width, height, &shot.data)?;

        let hinstance = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)?;
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance.into(),
            lpszClassName: CLASS_NAME,
            hCursor: LoadCursorW(None, IDC_CROSS).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&wc);

        let state = Box::new(OverlayState {
            dim_dc,
            dim_bmp,
            orig_dc,
            orig_bmp,
            old_dim,
            old_orig,
            width,
            height,
            origin: POINT { x: origin_x, y: origin_y },
            start: None,
            current: PointI::default(),
            result: None,
            drag: false,
        });
        let state_ptr = Box::into_raw(state);

        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            CLASS_NAME,
            w!("Select a region  (Esc to cancel)"),
            WS_POPUP | WS_VISIBLE,
            origin_x,
            origin_y,
            width,
            height,
            None,
            None,
            Some(hinstance.into()),
            Some(state_ptr as *const core::ffi::c_void),
        )?;
        windows::Win32::UI::WindowsAndMessaging::SetWindowLongPtrW(
            hwnd,
            windows::Win32::UI::WindowsAndMessaging::GWLP_USERDATA,
            state_ptr as isize,
        );
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(hwnd));

        let mut msg = MSG::default();
        loop {
            let r = GetMessageW(&mut msg, None, 0, 0);
            if r.0 <= 0 {
                break;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        // The window (and therefore its GWLP_USERDATA) is already gone once a
        // selection destroyed it, so reclaim the state through the pointer we
        // still hold.
        let result = {
            let boxed = Box::from_raw(state_ptr);
            let r = boxed.result;
            let _ = SelectObject(boxed.dim_dc, boxed.old_dim);
            let _ = SelectObject(boxed.orig_dc, boxed.old_orig);
            let _ = DeleteObject(boxed.dim_bmp.into());
            let _ = DeleteObject(boxed.orig_bmp.into());
            let _ = DeleteDC(boxed.dim_dc);
            let _ = DeleteDC(boxed.orig_dc);
            r
        };
        Ok(result)
    }
}

/// Small helper so `CreateFontW` gets a pointer that lives long enough.
struct OutputPcwstr(&'static [u8]);
impl OutputPcwstr {
    fn as_ref(&self) -> PCWSTR {
        PCWSTR(self.0.as_ptr() as *const u16)
    }
}