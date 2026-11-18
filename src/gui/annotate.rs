//! Post-capture annotation editor: text boxes and block mosaic.
//!
//! Shown after a region screenshot (`Screenshot -> Region`, `Ctrl+Alt+A`). The
//! captured bitmap is displayed 1:1 unless it does not fit on screen; mouse
//! coordinates are mapped back into image space, so annotations always land in
//! the pixels that get saved.
//!
//! Tools: `T` text, `M` mosaic, `B` box. Keys: `Ctrl+Z` undo, `Ctrl+C` copy,
//! `Ctrl+S` save, `Enter` copy and save, `Esc` cancel.

use anyhow::{Context, Result};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, ClientToScreen, CreateCompatibleDC, CreateDIBSection, CreateFontW, CreatePen, CreateSolidBrush,
    DeleteDC, DeleteObject, DrawFocusRect, DrawTextW, EndPaint, FillRect, GetDC, GetMonitorInfoW, GetStockObject,
    GetTextExtentPoint32W, InvalidateRect, IntersectClipRect, MonitorFromRect, Rectangle, ReleaseDC, RoundRect, SelectObject,
    SetBkMode,
    SetStretchBltMode, SetTextColor, StretchBlt, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    CLEARTYPE_QUALITY, DEFAULT_CHARSET, DEFAULT_GUI_FONT, DIB_RGB_COLORS, DT_CENTER, DT_END_ELLIPSIS, DT_LEFT, DT_NOCLIP,
    DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, FF_DONTCARE, FW_BOLD, HALFTONE, HBRUSH, HDC, HGDIOBJ, NULL_BRUSH,
    OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, MONITORINFO, MONITOR_DEFAULTTONEAREST, PS_DOT, PS_SOLID, SRCCOPY, TRANSPARENT,
    VARIABLE_PITCH,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{DRAWITEMSTRUCT, ODS_DISABLED, ODS_FOCUS, ODS_SELECTED};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetActiveWindow, SetCapture, SetFocus, TrackMouseEvent, TME_LEAVE,
    TRACKMOUSEEVENT, VK_CONTROL,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GetClientRect, GetMessageW,
    GetSystemMetrics, GetWindow, GetWindowLongPtrW, GetWindowTextW, HMENU, IsChild, LoadCursorW, MessageBoxW, PostMessageW,
    PostQuitMessage,
    RegisterClassW, SendMessageW, SetCursor, SetForegroundWindow, SetWindowLongPtrW,
    SetWindowTextW, ShowWindow,
    TranslateMessage, CS_HREDRAW, CS_VREDRAW, ES_AUTOHSCROLL, GWLP_USERDATA, GW_OWNER, IDC_ARROW, IDC_CROSS,
    IDC_IBEAM, MB_ICONERROR, MB_OK, MSG, SM_CXSCREEN, SM_CYSCREEN, SW_SHOW, WM_CLOSE, WM_COMMAND, WM_DESTROY,
    WM_DRAWITEM, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WM_SETFONT,
    WM_SETCURSOR,
    WINDOW_EX_STYLE, WINDOW_STYLE, WNDCLASSW, WS_CAPTION, WS_CHILD, WS_EX_CLIENTEDGE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
    WS_SYSMENU, WS_VISIBLE, BS_DEFPUSHBUTTON, BS_OWNERDRAW, BS_PUSHBUTTON,
};

use crate::capture::Bitmap;
use crate::geometry::Rect;

const CLASS_NAME: PCWSTR = w!("RecRAnnotate");
const TOOLBAR_CLASS: PCWSTR = w!("RecRAnnotateToolbar");
const PROMPT_CLASS: PCWSTR = w!("RecRAnnotateText");

/// Height of the separate toolbar window.
const TOOLBAR: i32 = 40;
/// UI-only border outside the screenshot pixels.
const BORDER: i32 = 2;

const BTN_TEXT: u32 = 6101;
const BTN_MOSAIC: u32 = 6102;
const BTN_BLOCK: u32 = 6103;
const BTN_UNDO: u32 = 6104;
const BTN_SAVE: u32 = 6105;
const BTN_CANCEL: u32 = 6106;
const BTN_COLOR: u32 = 6107;
const BTN_BOX: u32 = 6108;

/// `WM_MOUSELEAVE`; only exported from `Win32::UI::Controls`, not worth a feature.
const WM_MOUSELEAVE: u32 = 0x02A3;

/// Text colours as `COLORREF` (`0x00BBGGRR`); the first one is the default.
const TEXT_COLORS: [(&str, u32); 7] = [
    ("Orange", 0x00008AFF),
    ("Red", 0x003030FF),
    ("Yellow", 0x000AD6FF),
    ("Green", 0x0059C734),
    ("Blue", 0x00FF840A),
    ("White", 0x00FFFFFF),
    ("Black", 0x00000000),
];

/// Dark text needs a light outline and vice versa.
fn outline_for(color: COLORREF) -> COLORREF {
    let r = color.0 & 0xFF;
    let g = (color.0 >> 8) & 0xFF;
    let b = (color.0 >> 16) & 0xFF;
    let luma = (r * 299 + g * 587 + b * 114) / 1000;
    if luma > 140 {
        COLORREF(0x0000_0000)
    } else {
        COLORREF(0x00FF_FFFF)
    }
}

const PROMPT_OK: u32 = 1;
const PROMPT_CANCEL: u32 = 2;

const BLOCK_SIZES: [i32; 3] = [8, 16, 28];
const TEXT_SIZE: i32 = 22;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tool {
    Mosaic,
    Text,
    Box,
}

#[derive(Debug, PartialEq, Eq)]
enum EditorKey { Cancel, Save, Copy, Text, Mosaic, Box, Undo }

fn editor_key(key: u32, ctrl: bool) -> Option<EditorKey> {
    match (key, ctrl) {
        (0x1B, _) => Some(EditorKey::Cancel),
        (0x0D, _) | (0x53, true) => Some(EditorKey::Save),
        (0x43, true) => Some(EditorKey::Copy),
        (0x54, false) => Some(EditorKey::Text),
        (0x4D, false) => Some(EditorKey::Mosaic),
        (0x42, false) => Some(EditorKey::Box),
        (0x5A, true) => Some(EditorKey::Undo),
        _ => None,
    }
}

enum Op {
    Mosaic { x: i32, y: i32, w: i32, h: i32, block: i32 },
    Text { x: i32, y: i32, text: String, color: u32 },
    Box { x: i32, y: i32, w: i32, h: i32, color: u32 },
}

/// A `DIB` section we can draw into and read back.
struct Canvas {
    dc: HDC,
    bmp: windows::Win32::Graphics::Gdi::HBITMAP,
    bits: *mut u8,
    old: HGDIOBJ,
    width: i32,
    height: i32,
}

impl Canvas {
    fn new(width: i32, height: i32) -> Result<Self> {
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
            let old = SelectObject(dc, bmp.into());
            Ok(Self { dc, bmp, bits: bits as *mut u8, old, width, height })
        }
    }

    unsafe fn upload(&mut self, data: &[u8]) {
        let len = (self.width * self.height * 4) as usize;
        std::ptr::copy_nonoverlapping(data.as_ptr(), self.bits, len.min(data.len()));
    }

    fn snapshot(&self) -> Bitmap {
        let len = (self.width * self.height * 4) as usize;
        let data = unsafe { std::slice::from_raw_parts(self.bits, len) }.to_vec();
        Bitmap { width: self.width, height: self.height, data }
    }

    unsafe fn destroy(&mut self) {
        SelectObject(self.dc, self.old);
        let _ = DeleteObject(self.bmp.into());
        let _ = DeleteDC(self.dc);
    }
}

pub enum EditResult {
    Save(Bitmap),
    Copied,
}

struct Editor {
    base: Bitmap,
    canvas: Canvas,
    ops: Vec<Op>,
    tool: Tool,
    toolbar: HWND,
    tool_buttons: [HWND; 3],
    block: i32,
    block_btn: HWND,
    /// Index into [`TEXT_COLORS`].
    color: usize,
    color_btn: HWND,
    /// Pointer position over the image, used to preview where text lands.
    hover: Option<(i32, i32)>,
    /// Insertion point of the text box that is being typed right now.
    pending: Option<(i32, i32)>,
    tracking_leave: bool,
    scale: f64,
    disp_w: i32,
    disp_h: i32,
    /// Drag in progress, in image coordinates.
    drag: Option<(i32, i32, i32, i32)>,
    result: Option<EditResult>,
}

impl Editor {
    fn to_image(&self, x: i32, y: i32) -> (i32, i32) {
        let ix = ((x - BORDER) as f64 / self.scale).floor() as i32;
        let iy = ((y - BORDER) as f64 / self.scale).floor() as i32;
        (
            ix.clamp(0, self.base.width - 1),
            iy.clamp(0, self.base.height - 1),
        )
    }

    fn to_disp(&self, x: i32, y: i32) -> (i32, i32) {
        (
            (x as f64 * self.scale).round() as i32 + BORDER,
            (y as f64 * self.scale).round() as i32 + BORDER,
        )
    }

    /// Where the caret marker should be drawn, if anywhere.
    fn caret(&self) -> Option<(i32, i32)> {
        match self.tool {
            Tool::Text => self.pending.or(self.hover),
            Tool::Mosaic | Tool::Box => None,
        }
    }

    /// Re-render base bitmap + every operation into the canvas.
    fn compose(&mut self) {
        unsafe {
            self.canvas.upload(&self.base.data);
            for op in &self.ops {
                match op {
                    Op::Mosaic { x, y, w, h, block } => mosaic(
                        self.canvas.bits,
                        self.canvas.width,
                        self.canvas.height,
                        (*x, *y, *x + *w, *y + *h),
                        *block,
                    ),
                    Op::Text { x, y, text, color } => {
                        draw_text(self.canvas.dc, *x, *y, text, TEXT_SIZE, COLORREF(*color))
                    }
                    Op::Box { x, y, w, h, color } => {
                        draw_box(self.canvas.dc, *x, *y, *w, *h, COLORREF(*color))
                    }
                }
            }
        }
    }
}

/// Average `block`x`block` cells inside `rect` and paint each cell flat.
unsafe fn mosaic(bits: *mut u8, w: i32, h: i32, rect: (i32, i32, i32, i32), block: i32) {
    let bytes = std::slice::from_raw_parts_mut(bits, (w * h * 4) as usize);
    let (x0, y0) = (rect.0.clamp(0, w), rect.1.clamp(0, h));
    let (x1, y1) = (rect.2.clamp(0, w), rect.3.clamp(0, h));
    let block = block.max(2);
    let mut by = y0;
    while by < y1 {
        let mut bx = x0;
        while bx < x1 {
            let ex = (bx + block).min(x1);
            let ey = (by + block).min(y1);
            let (mut sb, mut sg, mut sr, mut n) = (0u32, 0u32, 0u32, 0u32);
            for y in by..ey {
                for x in bx..ex {
                    let i = ((y * w + x) * 4) as usize;
                    sb += bytes[i] as u32;
                    sg += bytes[i + 1] as u32;
                    sr += bytes[i + 2] as u32;
                    n += 1;
                }
            }
            if n > 0 {
                let (ab, ag, ar) = ((sb / n) as u8, (sg / n) as u8, (sr / n) as u8);
                for y in by..ey {
                    for x in bx..ex {
                        let i = ((y * w + x) * 4) as usize;
                        bytes[i] = ab;
                        bytes[i + 1] = ag;
                        bytes[i + 2] = ar;
                    }
                }
            }
            bx += block;
        }
        by += block;
    }
}

/// Text with a contrasting outline, so it stays readable on any screenshot.
unsafe fn draw_text(dc: HDC, x: i32, y: i32, text: &str, size: i32, color: COLORREF) {
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    let face: Vec<u16> = "Segoe UI\0".encode_utf16().collect();
    let font = CreateFontW(
        -size,
        0,
        0,
        0,
        FW_BOLD.0 as i32,
        0,
        0,
        0,
        DEFAULT_CHARSET,
        OUT_DEFAULT_PRECIS,
        CLIP_DEFAULT_PRECIS,
        CLEARTYPE_QUALITY,
        (FF_DONTCARE.0 | VARIABLE_PITCH.0) as u32,
        PCWSTR(face.as_ptr()),
    );
    let old_font = SelectObject(dc, font.into());
    let _ = SetBkMode(dc, TRANSPARENT);
    let outline = outline_for(color);
    for (dx, dy) in [(-1, 0), (1, 0), (0, -1), (0, 1), (-1, -1), (1, 1)] {
        let _ = SetTextColor(dc, outline);
        let mut r = RECT { left: x + dx, top: y + dy, right: x + 100_000, bottom: y + 100_000 };
        DrawTextW(dc, &mut wide, &mut r, DT_LEFT | DT_SINGLELINE | DT_NOPREFIX | DT_NOCLIP);
    }
    let _ = SetTextColor(dc, color);
    let mut r = RECT { left: x, top: y, right: x + 100_000, bottom: y + 100_000 };
    DrawTextW(dc, &mut wide, &mut r, DT_LEFT | DT_SINGLELINE | DT_NOPREFIX | DT_NOCLIP);
    SelectObject(dc, old_font);
    let _ = DeleteObject(font.into());
}

/// Caret-sized bar marking where a text box will be inserted. Uses the current
/// text colour, so the marker doubles as a colour preview.
unsafe fn draw_caret(dc: HDC, x: i32, y: i32, height: i32, color: COLORREF) {
    let border = outline_for(color);
    let brush = CreateSolidBrush(border);
    FillRect(dc, &RECT { left: x - 2, top: y - 2, right: x + 4, bottom: y + height + 2 }, brush);
    let _ = DeleteObject(brush.into());
    let brush = CreateSolidBrush(color);
    FillRect(dc, &RECT { left: x - 1, top: y - 1, right: x + 3, bottom: y + height + 1 }, brush);
    let _ = DeleteObject(brush.into());
}

/// Client-space box the insertion marker would occupy, padded for its outline.
///
/// Repainting only these few pixels (instead of the whole window) is what stops
/// the image from flickering while the pointer moves over it.
fn caret_box(st: &Editor, at: Option<(i32, i32)>) -> Option<RECT> {
    let (x, y) = at?;
    if x >= st.base.width || y >= st.base.height {
        return None;
    }
    let (dx, dy) = st.to_disp(x, y);
    Some(RECT {
        left: dx - 4,
        top: dy - 4,
        right: dx + 6,
        bottom: dy + TEXT_SIZE + 8,
    })
}

/// Client-space box of the dashed mosaic rubber band, padded for its pen.
fn band_box(st: &Editor) -> Option<RECT> {
    let (x0, y0, x1, y1) = st.drag?;
    let (ax, ay) = st.to_disp(x0.min(x1), y0.min(y1));
    let (bx, by) = st.to_disp(x0.max(x1), y0.max(y1));
    Some(RECT {
        left: ax - 3,
        top: ay - 3,
        right: bx + 3,
        bottom: by + 3,
    })
}

unsafe fn invalidate(hwnd: HWND, boxes: [Option<RECT>; 2]) {
    for b in boxes.into_iter().flatten() {
        let _ = InvalidateRect(Some(hwnd), Some(&b), false);
    }
}

fn lo(lparam: LPARAM) -> i32 {
    (lparam.0 & 0xFFFF) as i16 as i32
}

fn hi(lparam: LPARAM) -> i32 {
    ((lparam.0 >> 16) & 0xFFFF) as i16 as i32
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Editor;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let st = &mut *ptr;

    match msg {
        WM_PAINT => {
            let mut ps = windows::Win32::Graphics::Gdi::PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            // Only the damaged part is repainted, and the image is re-blitted
            // inside that clip, so moving the pointer never flashes the canvas.
            let _ = IntersectClipRect(
                hdc,
                ps.rcPaint.left,
                ps.rcPaint.top,
                ps.rcPaint.right,
                ps.rcPaint.bottom,
            );
            let mut rect = RECT::default();
            let _ = GetClientRect(hwnd, &mut rect);
            let brush = CreateSolidBrush(COLORREF(TEXT_COLORS[0].1));
            FillRect(hdc, &rect, brush);
            let _ = DeleteObject(brush.into());
            let _ = SetStretchBltMode(hdc, HALFTONE);
            let _ = StretchBlt(
                hdc,
                BORDER,
                BORDER,
                st.disp_w,
                st.disp_h,
                Some(st.canvas.dc),
                0,
                0,
                st.base.width,
                st.base.height,
                SRCCOPY,
            );

            if let Some((x0, y0, x1, y1)) = st.drag {
                let (ax, ay) = st.to_disp(x0.min(x1), y0.min(y1));
                let (bx, by) = st.to_disp(x0.max(x1), y0.max(y1));
                let pen = CreatePen(PS_DOT, 2, COLORREF(0x00FFFFFF));
                let old_pen = SelectObject(hdc, pen.into());
                let old_brush = SelectObject(hdc, GetStockObject(NULL_BRUSH));
                let _ = Rectangle(hdc, ax, ay, bx, by);
                SelectObject(hdc, old_brush);
                SelectObject(hdc, old_pen);
                let _ = DeleteObject(pen.into());
            }

            if let Some((cx, cy)) = st.caret() {
                if cx < st.base.width && cy < st.base.height {
                    let (dx, dy) = st.to_disp(cx, cy);
                    draw_caret(hdc, dx, dy, TEXT_SIZE + 6, COLORREF(TEXT_COLORS[st.color].1));
                }
            }
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_LBUTTONDOWN => {
            let (mx, my) = (lo(lparam), hi(lparam));
            if mx >= BORDER && my >= BORDER && mx < BORDER + st.disp_w && my < BORDER + st.disp_h {
                let _ = SetFocus(Some(hwnd));
                let (ix, iy) = st.to_image(mx, my);
                match st.tool {
                    Tool::Mosaic | Tool::Box => {
                        st.drag = Some((ix, iy, ix, iy));
                        SetCapture(hwnd);
                    }
                    Tool::Text => {
                        // Show the insertion point while the input box is up.
                        let mark = caret_box(st, Some((ix, iy)));
                        st.pending = Some((ix, iy));
                        invalidate(hwnd, [mark, None]);
                        let typed = prompt_text(hwnd, mx, my);
                        st.pending = None;
                        if let Some(text) = typed {
                            st.ops.push(Op::Text {
                                x: ix,
                                y: iy,
                                text,
                                color: TEXT_COLORS[st.color].1,
                            });
                            st.compose();
                            let _ = InvalidateRect(Some(hwnd), None, false);
                        } else {
                            // Cancelled: just put the image back under the marker.
                            invalidate(hwnd, [mark, None]);
                        }
                    }
                }
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (mx, my) = (lo(lparam), hi(lparam));
            if !st.tracking_leave {
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                if TrackMouseEvent(&mut tme).is_ok() {
                    st.tracking_leave = true;
                }
            }
            if st.drag.is_some() {
                let before = band_box(st);
                let (ix, iy) = st.to_image(mx, my);
                if let Some(d) = st.drag.as_mut() {
                    d.2 = ix;
                    d.3 = iy;
                }
                invalidate(hwnd, [before, band_box(st)]);
            } else if st.tool == Tool::Text {
                // Preview the insertion point (and the current colour).
                let before = caret_box(st, st.caret());
                let hover = if mx >= BORDER && my >= BORDER && mx < BORDER + st.disp_w && my < BORDER + st.disp_h {
                    Some(st.to_image(mx, my))
                } else {
                    None
                };
                if st.hover != hover {
                    st.hover = hover;
                    invalidate(hwnd, [before, caret_box(st, st.caret())]);
                }
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            st.tracking_leave = false;
            let before = caret_box(st, st.hover);
            if st.hover.take().is_some() {
                invalidate(hwnd, [before, None]);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            if let Some((x0, y0, x1, y1)) = st.drag.take() {
                let _ = ReleaseCapture();
                let (ix, iy) = st.to_image(lo(lparam), hi(lparam));
                let (x, y) = (x0.min(ix).min(x1), y0.min(iy).min(y1));
                let w = x0.max(ix).max(x1) - x;
                let h = y0.max(iy).max(y1) - y;
                if w >= 3 && h >= 3 {
                    let op = match st.tool {
                        Tool::Box => Op::Box { x, y, w, h, color: TEXT_COLORS[st.color].1 },
                        _ => Op::Mosaic { x, y, w, h, block: st.block },
                    };
                    st.ops.push(op);
                    st.compose();
                }
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            match (wparam.0 & 0xFFFF) as u32 {
                BTN_TEXT => st.tool = Tool::Text,
                BTN_MOSAIC => st.tool = Tool::Mosaic,
                BTN_BOX => st.tool = Tool::Box,
                BTN_COLOR => {
                    st.color = (st.color + 1) % TEXT_COLORS.len();
                    let label = wide(&format!("Color {}", TEXT_COLORS[st.color].0));
                    let _ = SetWindowTextW(st.color_btn, PCWSTR(label.as_ptr()));
                }
                BTN_BLOCK => {
                    let i = BLOCK_SIZES.iter().position(|b| *b == st.block).unwrap_or(0);
                    st.block = BLOCK_SIZES[(i + 1) % BLOCK_SIZES.len()];
                    let label = wide(&format!("Block {}", st.block));
                    let _ = SetWindowTextW(st.block_btn, PCWSTR(label.as_ptr()));
                }
                BTN_UNDO => {
                    st.ops.pop();
                    st.compose();
                }
                BTN_SAVE => {
                    st.result = Some(EditResult::Save(st.canvas.snapshot()));
                    let _ = DestroyWindow(hwnd);
                    return LRESULT(0);
                }
                BTN_CANCEL => {
                    let _ = DestroyWindow(hwnd);
                    return LRESULT(0);
                }
                _ => {}
            }
            let _ = InvalidateRect(Some(hwnd), None, false);
            invalidate_tool_buttons(st);
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let control = GetKeyState(VK_CONTROL.0 as i32) < 0;
            match editor_key(wparam.0 as u32, control) {
                Some(EditorKey::Cancel) => { let _ = DestroyWindow(hwnd); return LRESULT(0); }
                Some(EditorKey::Save) => {
                    st.result = Some(EditResult::Save(st.canvas.snapshot()));
                    let _ = DestroyWindow(hwnd);
                    return LRESULT(0);
                }
                Some(EditorKey::Copy) => {
                    let bmp = st.canvas.snapshot();
                    match crate::clipboard::set_bitmap(bmp.width, bmp.height, &bmp.data) {
                        Ok(()) => {
                            st.result = Some(EditResult::Copied);
                            let _ = DestroyWindow(hwnd);
                        }
                        Err(e) => {
                            let message = wide(&format!("Could not copy the screenshot: {e:#}"));
                            let _ = MessageBoxW(Some(hwnd), PCWSTR(message.as_ptr()), w!("Rec-R"), MB_OK | MB_ICONERROR);
                        }
                    }
                    return LRESULT(0);
                }
                Some(EditorKey::Text) => st.tool = Tool::Text,
                Some(EditorKey::Mosaic) => st.tool = Tool::Mosaic,
                Some(EditorKey::Box) => st.tool = Tool::Box,
                Some(EditorKey::Undo) => {
                    st.ops.pop();
                    st.compose();
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                None => {}
            }
            invalidate_tool_buttons(st);
            LRESULT(0)
        }
        WM_SETCURSOR => {
            let cursor = match st.tool {
                Tool::Text => IDC_IBEAM,
                Tool::Mosaic | Tool::Box => IDC_CROSS,
            };
            if let Ok(c) = LoadCursorW(None, cursor) {
                SetCursor(Some(c));
            }
            LRESULT(1)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn invalidate_tool_buttons(st: &Editor) {
    let id = match st.tool {
        Tool::Text => BTN_TEXT,
        Tool::Mosaic => BTN_MOSAIC,
        Tool::Box => BTN_BOX,
    };
    if !st.toolbar.0.is_null() {
        SetWindowLongPtrW(st.toolbar, GWLP_USERDATA, id as isize);
    }
    for &button in &st.tool_buttons {
        if !button.0.is_null() {
            let _ = InvalidateRect(Some(button), None, false);
        }
    }
}

unsafe fn draw_toolbar_button(item: &DRAWITEMSTRUCT, active_tool: u32) {
    let dc = item.hDC;
    let mut rect = item.rcItem;
    let background = CreateSolidBrush(COLORREF(0x00261E1A));
    FillRect(dc, &rect, background);
    let _ = DeleteObject(background.into());
    let pressed = item.itemState.0 & ODS_SELECTED.0 != 0;
    let active = item.CtlID == active_tool;
    let primary = item.CtlID == BTN_SAVE;
    let (fill, border, text) = if primary {
        (if pressed { 0x00006CD2 } else { 0x00008AFF }, 0x00008AFF, 0x001A1A1A)
    } else if active {
        (if pressed { 0x00324454 } else { 0x00304450 }, 0x00008AFF, 0x00FFFFFF)
    } else {
        (if pressed { 0x0050473D } else { 0x003C342C }, 0x005A5148, 0x00F3F1ED)
    };
    let brush = CreateSolidBrush(COLORREF(fill));
    let pen = CreatePen(PS_SOLID, 1, COLORREF(border));
    let old_brush = SelectObject(dc, brush.into());
    let old_pen = SelectObject(dc, pen.into());
    let _ = RoundRect(dc, rect.left, rect.top, rect.right, rect.bottom, 8, 8);
    SelectObject(dc, old_pen);
    SelectObject(dc, old_brush);
    let _ = DeleteObject(pen.into());
    let _ = DeleteObject(brush.into());

    let mut label = [0u16; 128];
    let len = GetWindowTextW(item.hwndItem, &mut label).max(0) as usize;
    let label = String::from_utf16_lossy(&label[..len.min(label.len())]).replace("&&", "&");
    let mut label: Vec<u16> = label.encode_utf16().collect();
    let old_font = SelectObject(dc, GetStockObject(DEFAULT_GUI_FONT));
    SetBkMode(dc, TRANSPARENT);
    let text = if item.itemState.0 & ODS_DISABLED.0 != 0 { 0x008C8884 } else { text };
    SetTextColor(dc, COLORREF(text));
    rect.left += 5;
    rect.right -= 5;
    let _ = DrawTextW(dc, &mut label, &mut rect, DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS);
    SelectObject(dc, old_font);
    if item.itemState.0 & ODS_FOCUS.0 != 0 {
        rect.top += 3;
        rect.bottom -= 3;
        let _ = DrawFocusRect(dc, &rect);
    }
}

unsafe extern "system" fn toolbar_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = windows::Win32::Graphics::Gdi::PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut ps);
            let mut rect = RECT::default();
            let _ = GetClientRect(hwnd, &mut rect);
            let brush: HBRUSH = CreateSolidBrush(COLORREF(0x00261E1A));
            FillRect(dc, &rect, brush);
            let _ = DeleteObject(brush.into());
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_DRAWITEM => {
            if lparam.0 != 0 {
                let item = &*(lparam.0 as *const DRAWITEMSTRUCT);
                draw_toolbar_button(item, GetWindowLongPtrW(hwnd, GWLP_USERDATA) as u32);
                return LRESULT(1);
            }
            LRESULT(0)
        }
        WM_COMMAND | WM_CLOSE => {
            if let Ok(owner) = GetWindow(hwnd, GW_OWNER) {
                SendMessageW(owner, msg, Some(wparam), Some(lparam));
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

// ------------------------------------------------------------ text prompt

struct PromptState {
    edit: HWND,
    value: Option<String>,
}

unsafe extern "system" fn prompt_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut PromptState;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    match msg {
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as u32;
            let code = ((wparam.0 >> 16) & 0xFFFF) as u32;
            // Ignore control notifications (EN_CHANGE/EN_SETFOCUS from the edit
            // box would otherwise close the dialog); only button clicks count.
            if code == 0 {
                if id == PROMPT_OK {
                    let st = &mut *ptr;
                    let mut buf = [0u16; 512];
                    let n = GetWindowTextW(st.edit, &mut buf).max(0) as usize;
                    st.value = Some(String::from_utf16_lossy(&buf[..n.min(buf.len())]));
                    let _ = DestroyWindow(hwnd);
                } else if id == PROMPT_CANCEL {
                    let _ = DestroyWindow(hwnd);
                }
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Use the monitor with the largest intersection, or the nearest one if off-screen.
fn monitor_work_area(area: Option<Rect>) -> Rect {
    unsafe {
        let area = area.unwrap_or(Rect::new(0, 0, 1, 1));
        let bounds = RECT { left: area.x, top: area.y, right: area.right(), bottom: area.bottom() };
        let monitor = MonitorFromRect(&bounds, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            let r = info.rcWork;
            Rect::new(r.left, r.top, r.right - r.left, r.bottom - r.top)
        } else {
            Rect::new(0, 0, GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN))
        }
    }
}

fn window_position(work: Rect, anchor: Option<(i32, i32)>, width: i32, height: i32) -> (i32, i32) {
    let (x, y) = anchor.unwrap_or((work.x + (work.width - width) / 2, work.y + (work.height - height) / 2));
    (
        x.clamp(work.x, (work.right() - width).max(work.x)),
        y.clamp(work.y, (work.bottom() - height).max(work.y)),
    )
}

fn toolbar_position(work: Rect, image: Rect, width: i32) -> (i32, i32) {
    let x = image.x + (image.width - width) / 2;
    (x.clamp(work.x, (work.right() - width).max(work.x)), image.y - TOOLBAR)
}

/// Small modal input box; x and y are the editor's client coordinates.
fn prompt_text(owner: HWND, x: i32, y: i32) -> Option<String> {
    unsafe {
        let hinstance = GetModuleHandleW(None).ok()?;
        let wc = WNDCLASSW {
            lpfnWndProc: Some(prompt_proc),
            hInstance: hinstance.into(),
            lpszClassName: PROMPT_CLASS,
            hCursor: LoadCursorW(None, IDC_IBEAM).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&wc);

        let mut point = POINT { x, y };
        if !ClientToScreen(owner, &mut point).as_bool() {
            return None;
        }
        let work = monitor_work_area(Some(Rect::new(point.x, point.y, 1, 1)));
        let (w, h) = (400, 116);
        let (px, py) = window_position(work, Some((point.x - 40, point.y - 60)), w, h);

        let state = Box::new(PromptState { edit: HWND::default(), value: None });
        let ptr = Box::into_raw(state);

        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            PROMPT_CLASS,
            w!("Add text"),
            WS_POPUP | WS_CAPTION | WS_SYSMENU | WS_VISIBLE,
            px,
            py,
            w,
            h,
            Some(owner),
            None,
            Some(hinstance.into()),
            Some(ptr as *const core::ffi::c_void),
        )
        .ok()?;
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr as isize);

        let edit = CreateWindowExW(
            WS_EX_CLIENTEDGE,
            w!("EDIT"),
            PCWSTR::null(),
            WS_CHILD
                | WS_VISIBLE
                | windows::Win32::UI::WindowsAndMessaging::WS_TABSTOP
                | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
            12,
            10,
            w - 24,
            26,
            Some(hwnd),
            None,
            Some(hinstance.into()),
            None,
        )
        .ok()?;
        (*ptr).edit = edit;

        let _ = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("BUTTON"),
            w!("OK"),
            WS_CHILD | WS_VISIBLE | WINDOW_STYLE(BS_DEFPUSHBUTTON as u32),
            w - 180,
            48,
            76,
            26,
            Some(hwnd),
            Some(HMENU(PROMPT_OK as usize as *mut core::ffi::c_void)),
            Some(hinstance.into()),
            None,
        )
        .ok();
        let _ = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("BUTTON"),
            w!("Cancel"),
            WS_CHILD | WS_VISIBLE | WINDOW_STYLE(BS_PUSHBUTTON as u32),
            w - 96,
            48,
            76,
            26,
            Some(hwnd),
            Some(HMENU(PROMPT_CANCEL as usize as *mut core::ffi::c_void)),
            Some(hinstance.into()),
            None,
        );

        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(edit));

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            // Enter accepts and Esc cancels whatever control has focus; the edit
            // box would otherwise swallow both keys.
            if msg.message == WM_KEYDOWN {
                let chosen = match msg.wParam.0 as u32 {
                    0x0D => Some(PROMPT_OK),
                    0x1B => Some(PROMPT_CANCEL),
                    _ => None,
                };
                if let Some(id) = chosen {
                    let _ = PostMessageW(Some(hwnd), WM_COMMAND, WPARAM(id as usize), LPARAM(0));
                    continue;
                }
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        let boxed = Box::from_raw(ptr);
        boxed
            .value
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }
}

// --------------------------------------------------------------- entry point

/// Let the user annotate `bmp`; `None` when cancelled. `Ctrl+C` copies directly
/// and returns `Copied`; a save returns the bitmap for encoding by the caller.
/// `origin` positions the editor over the captured area. `copy` controls the
/// save-button label; the configured save-time copy happens in `screenshot::finish`.
pub fn edit(bmp: Bitmap, origin: Option<Rect>, copy: bool) -> Result<Option<EditResult>> {
    unsafe {
        let work = monitor_work_area(origin);
        // Fit the capture on the selected monitor, but never scale it up.
        let max_w = (work.width - 2 * BORDER - 80).max(1);
        let max_h = (work.height - TOOLBAR - 2 * BORDER - 120).max(1);
        let scale = (max_w as f64 / bmp.width as f64)
            .min(max_h as f64 / bmp.height as f64)
            .min(1.0);
        let disp_w = ((bmp.width as f64 * scale).round() as i32).max(1);
        let disp_h = ((bmp.height as f64 * scale).round() as i32).max(1);
        let scale = disp_w as f64 / bmp.width as f64;

        let items = toolbar_items(16, copy);
        let widths = button_widths(&items);
        let toolbar_w = (8 + widths.iter().map(|w_| w_ + 6).sum::<i32>()).min(work.width.max(1));
        // Reserve room above the image so the toolbar never covers the capture.
        let image_work = Rect::new(work.x, work.y + TOOLBAR, work.width, (work.height - TOOLBAR).max(1));
        let (frame_w, frame_h) = (disp_w + 2 * BORDER, disp_h + 2 * BORDER);
        let (x, y) = window_position(image_work, origin.map(|r| (r.x - BORDER, r.y - BORDER)), frame_w, frame_h);
        let (toolbar_x, toolbar_y) = toolbar_position(work, Rect::new(x, y, frame_w, frame_h), toolbar_w);

        tracing::info!(
            "annotate: bitmap {}x{}, scale {scale:.3}, display {}x{}",
            bmp.width,
            bmp.height,
            disp_w,
            disp_h
        );
        let canvas = Canvas::new(bmp.width, bmp.height)?;
        register_class()?;
        let hinstance = GetModuleHandleW(None)?;

        let mut state = Box::new(Editor {
            base: bmp,
            canvas,
            ops: Vec::new(),
            tool: Tool::Mosaic,
            toolbar: HWND::default(),
            tool_buttons: [HWND::default(); 3],
            block: 16,
            block_btn: HWND::default(),
            color: 0,
            color_btn: HWND::default(),
            hover: None,
            pending: None,
            tracking_leave: false,
            scale,
            disp_w,
            disp_h,
            drag: None,
            result: None,
        });
        state.compose();
        let ptr = Box::into_raw(state);

        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            CLASS_NAME,
            w!("Rec-R  annotate  (Enter: copy & save, Esc: cancel)"),
            WS_POPUP | WS_VISIBLE,
            x,
            y,
            frame_w,
            frame_h,
            None,
            None,
            Some(hinstance.into()),
            Some(ptr as *const core::ffi::c_void),
        )?;
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr as isize);

        // An owned popup keeps the toolbar separate from the captured image.
        let toolbar = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            TOOLBAR_CLASS,
            w!("Rec-R annotation tools"),
            WS_POPUP | WS_VISIBLE,
            toolbar_x,
            toolbar_y,
            toolbar_w,
            TOOLBAR,
            Some(hwnd),
            None,
            Some(hinstance.into()),
            None,
        )?;
        (*ptr).toolbar = toolbar;
        SetWindowLongPtrW(toolbar, GWLP_USERDATA, BTN_MOSAIC as isize);

        // The block size and colour buttons keep handles so their labels can change.
        let mut bx = 8;
        for ((id, label, _), w_) in items.into_iter().zip(widths) {
            let style = WINDOW_STYLE(BS_OWNERDRAW as u32);
            // `&` in a button label marks an accelerator, so a literal one is doubled.
            let label_w = wide(&label.replace('&', "&&"));
            let btn = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("BUTTON"),
                PCWSTR(label_w.as_ptr()),
                WS_CHILD | WS_VISIBLE | style,
                bx,
                8,
                w_,
                26,
                Some(toolbar),
                Some(HMENU(id as usize as *mut core::ffi::c_void)),
                Some(hinstance.into()),
                None,
            )?;
            // Same font the width was measured with, so labels never clip.
            SendMessageW(
                btn,
                WM_SETFONT,
                Some(WPARAM(GetStockObject(DEFAULT_GUI_FONT).0 as usize)),
                Some(LPARAM(0)),
            );
            match id {
                BTN_TEXT => (*ptr).tool_buttons[0] = btn,
                BTN_MOSAIC => (*ptr).tool_buttons[1] = btn,
                BTN_BOX => (*ptr).tool_buttons[2] = btn,
                BTN_BLOCK => (*ptr).block_btn = btn,
                BTN_COLOR => (*ptr).color_btn = btn,
                _ => {}
            }
            bx += w_ + 6;
        }

        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = BringWindowToTop(hwnd);
        let _ = SetActiveWindow(hwnd);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(hwnd));

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            // Buttons keep keyboard focus after a click; route editor shortcuts to
            // the canvas instead of letting the focused button consume them.
            if msg.message == WM_KEYDOWN
                && (msg.hwnd == toolbar || IsChild(toolbar, msg.hwnd).as_bool())
                && matches!(msg.wParam.0 as u32, 0x1B | 0x0D | 0x54 | 0x4D | 0x42 | 0x5A | 0x43 | 0x53)
            {
                SendMessageW(hwnd, WM_KEYDOWN, Some(msg.wParam), Some(msg.lParam));
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        let mut boxed = Box::from_raw(ptr);
        tracing::info!(
            "annotate: {} op(s), {}",
            boxed.ops.len(),
            match &boxed.result {
                Some(EditResult::Save(b)) => format!("save requested for {}x{}", b.width, b.height),
                Some(EditResult::Copied) => "copied".to_string(),
                None => "cancelled".to_string(),
            }
        );
        boxed.canvas.destroy();
        Ok(boxed.result.take())
    }
}

/// Rectangle outline in `color`, with a thin contrasting inner line so it stays
/// readable on both light and dark backgrounds.
unsafe fn draw_box(dc: HDC, x: i32, y: i32, w: i32, h: i32, color: COLORREF) {
    let (x1, y1) = (x + w, y + h);
    let pen = CreatePen(PS_SOLID, 3, color);
    let old_pen = SelectObject(dc, pen.into());
    let old_brush = SelectObject(dc, GetStockObject(NULL_BRUSH));
    let _ = Rectangle(dc, x, y, x1, y1);
    SelectObject(dc, old_brush);
    SelectObject(dc, old_pen);
    let _ = DeleteObject(pen.into());
}

/// Toolbar layout: `(id, label, alternative labels)`. The alternatives are used
/// only to size the button, so it never clips when its label changes.
fn toolbar_items(block: i32, copy: bool) -> Vec<(u32, String, Vec<String>)> {
    let block_labels: Vec<String> = [8, 16, 28].iter().map(|b| format!("Block {b}")).collect();
    let color_labels: Vec<String> = TEXT_COLORS.iter().map(|(name, _)| format!("Color {name}")).collect();
    let save_labels: Vec<String> = ["Save", "Copy & Save"].iter().map(|s| s.to_string()).collect();
    let hit = |labels: &[String], label: &str| -> Vec<String> {
        let mut out = labels.to_vec();
        if !out.iter().any(|l| l == label) {
            out.push(label.to_string());
        }
        out
    };
    vec![
        (BTN_TEXT, "Text".to_string(), vec!["Text".to_string()]),
        (BTN_MOSAIC, "Mosaic".to_string(), vec!["Mosaic".to_string()]),
        (BTN_BOX, "Box".to_string(), vec!["Box".to_string()]),
        (BTN_BLOCK, format!("Block {block}"), block_labels),
        (
            BTN_COLOR,
            format!("Color {}", TEXT_COLORS[0].0),
            color_labels,
        ),
        (BTN_UNDO, "Undo".to_string(), vec!["Undo".to_string()]),
        (
            BTN_SAVE,
            if copy { "Copy & Save" } else { "Save" }.to_string(),
            hit(&save_labels, if copy { "Copy & Save" } else { "Save" }),
        ),
        (BTN_CANCEL, "Cancel".to_string(), vec!["Cancel".to_string()]),
    ]
}

/// Width each button needs, measured with the dialog font (`BS_PUSHBUTTON` uses
/// the system UI font, which is noticeably wider at 125% DPI than the 96 DPI
/// numbers the labels were first sized for).
fn button_widths(items: &[(u32, String, Vec<String>)]) -> Vec<i32> {
    unsafe {
        let dc = GetDC(None);
        let old = SelectObject(dc, GetStockObject(DEFAULT_GUI_FONT));
        let widths = items
            .iter()
            .map(|(_, _, labels)| {
                let widest = labels
                    .iter()
                    .map(|label| {
                        let text = wide(label);
                        let mut size = SIZE::default();
                        let _ = GetTextExtentPoint32W(dc, &text[..text.len() - 1], &mut size);
                        size.cx
                    })
                    .max()
                    .unwrap_or(0);
                (widest + 30).max(54)
            })
            .collect();
        SelectObject(dc, old);
        ReleaseDC(None, dc);
        widths
    }
}

#[cfg(test)]
mod placement_tests {
    use super::*;

    #[test]
    fn screenshot_shortcuts_require_control() {
        assert_eq!(editor_key(0x43, true), Some(EditorKey::Copy));
        assert_eq!(editor_key(0x43, false), None);
        assert_eq!(editor_key(0x53, true), Some(EditorKey::Save));
        assert_eq!(editor_key(0x53, false), None);
        assert_eq!(editor_key(0x0D, false), Some(EditorKey::Save));
        assert_eq!(editor_key(0x1B, false), Some(EditorKey::Cancel));
        assert_eq!(editor_key(0x5A, true), Some(EditorKey::Undo));
        assert_eq!(editor_key(0x54, false), Some(EditorKey::Text));
        assert_eq!(editor_key(0x54, true), None);
    }

    #[test]
    fn windows_stay_in_the_selected_work_area() {
        let right = Rect::new(1920, 0, 1920, 1040);
        assert_eq!(window_position(right, Some((2000, 100)), 600, 340), (2000, 100));
        assert_eq!(window_position(right, Some((3800, 1000)), 600, 340), (3240, 700));

        let left_above = Rect::new(-1920, -1080, 1920, 1040);
        assert_eq!(window_position(left_above, Some((-1900, -1000)), 600, 340), (-1900, -1000));
        assert_eq!(window_position(left_above, Some((-50, -10)), 600, 340), (-600, -380));
        assert_eq!(window_position(left_above, None, 600, 340), (-1260, -730));
        assert_eq!(window_position(left_above, Some((-50, -10)), 2500, 1500), (-1920, -1080));
        assert_eq!(toolbar_position(right, Rect::new(2400, 200, 400, 300), 600), (2300, 160));
        assert_eq!(toolbar_position(right, Rect::new(1920, 40, 400, 300), 600), (1920, 0));
        assert_eq!(toolbar_position(left_above, Rect::new(-1200, -800, 400, 300), 600), (-1300, -840));
    }

    // Deliver a shortcut to a focused-style toolbar button without synthesizing
    // system-wide keystrokes. Call only from the test's worker thread.
    unsafe fn post_ctrl_shortcut(editor: HWND, key: u32) -> Result<()> {
        use std::{thread, time::Duration};
        use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
        use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyboardState, SetKeyboardState};
        use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, GetDlgItem, GetWindowThreadProcessId, IsWindow, PeekMessageW, PM_NOREMOVE};
        let mut msg = MSG::default();
        let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
        let (from, to) = (GetCurrentThreadId(), GetWindowThreadProcessId(editor, None));
        anyhow::ensure!(AttachThreadInput(from, to, true).as_bool(), "attach keyboard state failed");
        let mut original_keys = [0u8; 256];
        let result = (|| -> Result<()> {
            GetKeyboardState(&mut original_keys)?;
            let mut keys = original_keys;
            keys[VK_CONTROL.0 as usize] |= 0x80;
            SetKeyboardState(&keys)?;
            let toolbar = FindWindowW(TOOLBAR_CLASS, PCWSTR::null())?;
            let button = GetDlgItem(Some(toolbar), BTN_BOX as i32)?;
            PostMessageW(Some(button), WM_KEYDOWN, WPARAM(key as usize), LPARAM(0))?;
            for _ in 0..100 {
                if !IsWindow(Some(editor)).as_bool() { break; }
                thread::sleep(Duration::from_millis(20));
            }
            anyhow::ensure!(!IsWindow(Some(editor)).as_bool(), "shortcut did not close the editor");
            Ok(())
        })();
        let _ = SetKeyboardState(&original_keys);
        let _ = AttachThreadInput(from, to, false);
        result
    }

    // Run explicitly on an unlocked desktop with a secondary display.
    #[test]
    #[ignore]
    fn editor_and_prompt_use_secondary_monitor() -> Result<()> {
        use std::{thread, time::{Duration, Instant}};
        use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, GetDlgItem, GetWindowRect, IsWindow};

        crate::win::init_dpi_awareness();
        let monitor = crate::win::monitor::list_monitors().into_iter()
            .find(|m| !m.is_primary)
            .context("requires a secondary monitor")?;
        let work = monitor.work;
        anyhow::ensure!(work.width >= 800 && work.height >= 600, "secondary work area too small");
        unsafe {
            anyhow::ensure!(FindWindowW(CLASS_NAME, PCWSTR::null()).is_err(), "an editor is already open");
            anyhow::ensure!(FindWindowW(PROMPT_CLASS, PCWSTR::null()).is_err(), "a text prompt is already open");
            anyhow::ensure!(FindWindowW(TOOLBAR_CLASS, PCWSTR::null()).is_err(), "a toolbar is already open");
        }
        let origin = Rect::new(work.x + 100, work.y + 100, 400, 300);
        assert_eq!(monitor_work_area(Some(origin)), work);

        let worker = thread::spawn(move || unsafe {
            fn wait_for(class: PCWSTR) -> Result<HWND> {
                let deadline = Instant::now() + Duration::from_secs(10);
                while Instant::now() < deadline {
                    if let Ok(hwnd) = unsafe { FindWindowW(class, PCWSTR::null()) } {
                        return Ok(hwnd);
                    }
                    thread::sleep(Duration::from_millis(50));
                }
                anyhow::bail!("window did not appear")
            }
            let editor = wait_for(CLASS_NAME)?;
            let result = (|| -> Result<()> {
                thread::sleep(Duration::from_millis(400));
                let mut rect = RECT::default();
                GetWindowRect(editor, &mut rect)?;
                anyhow::ensure!(rect.left + BORDER == origin.x && rect.top + BORDER == origin.y, "editor position: {rect:?}");
                anyhow::ensure!((rect.right - rect.left, rect.bottom - rect.top) == (400 + 2 * BORDER, 300 + 2 * BORDER), "canvas and border size: {rect:?}");
                let toolbar = wait_for(TOOLBAR_CLASS)?;
                let mut bar = RECT::default();
                GetWindowRect(toolbar, &mut bar)?;
                let frame = Rect::new(rect.left, rect.top, rect.right - rect.left, rect.bottom - rect.top);
                let expected = toolbar_position(work, frame, bar.right - bar.left);
                anyhow::ensure!((bar.left, bar.top) == expected && bar.bottom == rect.top, "toolbar position: {bar:?}");
                let save_button = GetDlgItem(Some(toolbar), BTN_SAVE as i32)?;
                let dc = GetDC(Some(save_button));
                let fill = windows::Win32::Graphics::Gdi::GetPixel(dc, 8, 5);
                ReleaseDC(Some(save_button), dc);
                anyhow::ensure!(fill == COLORREF(0x00008AFF), "save button fill: {fill:?}");
                PostMessageW(Some(toolbar), WM_COMMAND, WPARAM(BTN_TEXT as usize), LPARAM(0))?;
                let (x, y) = (BORDER + 120, BORDER + 100);
                PostMessageW(Some(editor), WM_LBUTTONDOWN, WPARAM(0), LPARAM(((y << 16) | x) as isize))?;
                let prompt = wait_for(PROMPT_CLASS)?;
                let mut prompt_rect = RECT::default();
                let check = GetWindowRect(prompt, &mut prompt_rect)
                    .map_err(anyhow::Error::from)
                    .and_then(|_| {
                        let expected = window_position(work, Some((rect.left + x - 40, rect.top + y - 60)), 400, 116);
                        anyhow::ensure!((prompt_rect.left, prompt_rect.top) == expected, "prompt position: {prompt_rect:?}, expected {expected:?}");
                        Ok(())
                    });
                let _ = PostMessageW(Some(prompt), WM_KEYDOWN, WPARAM(0x1B), LPARAM(0));
                check?;
                for _ in 0..100 {
                    if FindWindowW(PROMPT_CLASS, PCWSTR::null()).is_err() { break; }
                    thread::sleep(Duration::from_millis(20));
                }
                anyhow::ensure!(FindWindowW(PROMPT_CLASS, PCWSTR::null()).is_err(), "prompt did not close");
                anyhow::ensure!(IsWindow(Some(editor)).as_bool(), "cancelling text should keep the editor open");
                PostMessageW(Some(toolbar), WM_COMMAND, WPARAM(BTN_BOX as usize), LPARAM(0))?;
                PostMessageW(Some(editor), WM_LBUTTONDOWN, WPARAM(0), LPARAM((60 << 16) | 40))?;
                PostMessageW(Some(editor), WM_MOUSEMOVE, WPARAM(0), LPARAM((180 << 16) | 240))?;
                PostMessageW(Some(editor), WM_LBUTTONUP, WPARAM(0), LPARAM((180 << 16) | 240))?;
                // Exercise the message path when a toolbar button has focus.
                let button = GetDlgItem(Some(toolbar), BTN_BOX as i32)?;
                PostMessageW(Some(button), WM_KEYDOWN, WPARAM(0x1B), LPARAM(0))?;
                for _ in 0..100 {
                    if !IsWindow(Some(editor)).as_bool() { break; }
                    thread::sleep(Duration::from_millis(20));
                }
                anyhow::ensure!(!IsWindow(Some(editor)).as_bool(), "Esc did not close the editor");
                anyhow::ensure!(!IsWindow(Some(toolbar)).as_bool(), "toolbar did not close with the editor");
                Ok(())
            })();
            if let Ok(prompt) = FindWindowW(PROMPT_CLASS, PCWSTR::null()) {
                let _ = PostMessageW(Some(prompt), WM_CLOSE, WPARAM(0), LPARAM(0));
                thread::sleep(Duration::from_millis(100));
            }
            if IsWindow(Some(editor)).as_bool() {
                let _ = PostMessageW(Some(editor), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
            result
        });
        let bmp = Bitmap { width: 400, height: 300, data: vec![0; 400 * 300 * 4] };
        let edit_result = edit(bmp, Some(origin), true);
        let check = worker.join().map_err(|_| anyhow::anyhow!("test worker panicked"))?;
        anyhow::ensure!(edit_result?.is_none(), "cancelled annotation must not produce a bitmap");
        check?;

        // A second editor verifies the visible border is not part of the saved bitmap.
        let worker = thread::spawn(|| unsafe {
            use windows::Win32::Graphics::Gdi::GetPixel;
            let deadline = Instant::now() + Duration::from_secs(10);
            let editor = loop {
                if let Ok(hwnd) = FindWindowW(CLASS_NAME, PCWSTR::null()) { break hwnd; }
                anyhow::ensure!(Instant::now() < deadline, "editor did not reappear");
                thread::sleep(Duration::from_millis(50));
            };
            thread::sleep(Duration::from_millis(400));
            let dc = GetDC(Some(editor));
            let border = GetPixel(dc, 0, 0);
            let image = GetPixel(dc, BORDER + 10, BORDER + 10);
            ReleaseDC(Some(editor), dc);
            let result = (|| -> Result<()> {
                anyhow::ensure!(border == COLORREF(TEXT_COLORS[0].1), "border pixel: {border:?}");
                anyhow::ensure!(image == COLORREF(0x00102030), "image pixel: {image:?}");
                post_ctrl_shortcut(editor, 0x53)
            })();
            if result.is_err() {
                let _ = PostMessageW(Some(editor), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
            result
        });
        let data = [0x10, 0x20, 0x30, 0xff].repeat(400 * 300);
        let output = edit(Bitmap { width: 400, height: 300, data: data.clone() }, Some(origin), true)?;
        worker.join().map_err(|_| anyhow::anyhow!("test worker panicked"))??;
        let Some(EditResult::Save(output)) = output else { anyhow::bail!("save should return a bitmap") };
        anyhow::ensure!((output.width, output.height) == (400, 300) && output.data == data, "UI border changed saved pixels");
        Ok(())
    }

    // This interactive test replaces the current Windows clipboard contents.
    #[test]
    #[ignore]
    fn copy_shortcut_writes_clipboard() -> Result<()> {
        use std::{thread, time::{Duration, Instant}};
        use windows::Win32::Foundation::HGLOBAL;
        use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
        use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
        use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, IsWindow};

        crate::win::init_dpi_awareness();
        unsafe { anyhow::ensure!(FindWindowW(CLASS_NAME, PCWSTR::null()).is_err(), "an editor is already open"); }
        let worker = thread::spawn(|| unsafe {
            let deadline = Instant::now() + Duration::from_secs(10);
            let editor = loop {
                if let Ok(hwnd) = FindWindowW(CLASS_NAME, PCWSTR::null()) { break hwnd; }
                anyhow::ensure!(Instant::now() < deadline, "editor did not appear");
                thread::sleep(Duration::from_millis(50));
            };
            thread::sleep(Duration::from_millis(400));
            let result = post_ctrl_shortcut(editor, 0x43);
            if result.is_err() {
                if let Ok(dialog) = FindWindowW(w!("#32770"), w!("Rec-R")) {
                    if GetWindow(dialog, GW_OWNER).ok() == Some(editor) {
                        let _ = PostMessageW(Some(dialog), WM_CLOSE, WPARAM(0), LPARAM(0));
                    }
                }
                if IsWindow(Some(editor)).as_bool() {
                    let _ = PostMessageW(Some(editor), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
            }
            result
        });
        let data = [0x10, 0x20, 0x30, 0xff].repeat(400 * 300);
        let result = edit(Bitmap { width: 400, height: 300, data }, None, true);
        worker.join().map_err(|_| anyhow::anyhow!("test worker panicked"))??;
        anyhow::ensure!(matches!(result?, Some(EditResult::Copied)), "Ctrl+C must return Copied");
        unsafe {
            OpenClipboard(None)?;
            let check = (|| -> Result<()> {
                let handle = HGLOBAL(GetClipboardData(8)?.0);
                anyhow::ensure!(GlobalSize(handle) >= 43, "clipboard DIB is too short");
                let ptr = GlobalLock(handle) as *const u8;
                anyhow::ensure!(!ptr.is_null(), "GlobalLock failed");
                let dib = std::slice::from_raw_parts(ptr, 43);
                let width = i32::from_le_bytes(dib[4..8].try_into()?);
                let height = i32::from_le_bytes(dib[8..12].try_into()?);
                let valid = width == 400 && height == 300 && dib[40..43] == [0x10, 0x20, 0x30];
                let _ = GlobalUnlock(handle);
                anyhow::ensure!(valid, "clipboard DIB dimensions or pixels are wrong");
                Ok(())
            })();
            let _ = CloseClipboard();
            check?;
        }
        Ok(())
    }
}

fn register_class() -> Result<()> {
    unsafe {
        let hinstance = GetModuleHandleW(None)?;
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance.into(),
            lpszClassName: CLASS_NAME,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&wc);
        let toolbar = WNDCLASSW {
            lpfnWndProc: Some(toolbar_proc),
            lpszClassName: TOOLBAR_CLASS,
            ..wc
        };
        RegisterClassW(&toolbar);
    }
    Ok(())
}
