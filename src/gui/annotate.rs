//! Post-capture annotation editor: text boxes and block mosaic.
//!
//! Shown after a region screenshot (`Screenshot -> Region`, `Ctrl+Alt+A`). The
//! captured bitmap is displayed 1:1 unless it does not fit on screen; mouse
//! coordinates are mapped back into image space, so annotations always land in
//! the pixels that get saved.
//!
//! Keys: `T` text tool, `M` mosaic tool, `Ctrl+Z` undo, `Enter`/`Ctrl+S` copy and
//! save, `Esc` cancel.

use anyhow::{Context, Result};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateCompatibleDC, CreateDIBSection, CreateFontW, CreatePen, CreateSolidBrush,
    DeleteDC, DeleteObject, DrawTextW, EndPaint, FillRect, GetDC, GetStockObject,
    GetTextExtentPoint32W, InvalidateRect, IntersectClipRect, Rectangle, ReleaseDC, SelectObject,
    SetBkMode,
    SetStretchBltMode, SetTextColor, StretchBlt, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    CLEARTYPE_QUALITY, DEFAULT_CHARSET, DEFAULT_GUI_FONT, DIB_RGB_COLORS, DT_LEFT, DT_NOCLIP,
    DT_NOPREFIX, DT_SINGLELINE, FF_DONTCARE, FW_BOLD, HALFTONE, HBRUSH, HDC, HGDIOBJ, NULL_BRUSH,
    OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, PS_DOT, SRCCOPY, TRANSPARENT, VARIABLE_PITCH,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetActiveWindow, SetCapture, SetFocus, TrackMouseEvent, TME_LEAVE,
    TRACKMOUSEEVENT, VK_CONTROL,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GetClientRect, GetMessageW,
    GetSystemMetrics, GetWindowLongPtrW, GetWindowTextW, HMENU, LoadCursorW, PostQuitMessage,
    RegisterClassW, SendMessageW, SetCursor, SetForegroundWindow, SetWindowLongPtrW,
    SetWindowTextW, ShowWindow,
    TranslateMessage, CS_HREDRAW, CS_VREDRAW, ES_AUTOHSCROLL, GWLP_USERDATA, IDC_ARROW, IDC_CROSS,
    IDC_IBEAM, MSG, SM_CXSCREEN, SM_CYSCREEN, SW_SHOW, WM_CLOSE, WM_COMMAND, WM_DESTROY,
    WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WM_SETFONT,
    WM_SETCURSOR,
    WINDOW_EX_STYLE, WINDOW_STYLE, WNDCLASSW, WS_CAPTION, WS_CHILD, WS_EX_CLIENTEDGE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
    WS_SYSMENU, WS_VISIBLE, BS_DEFPUSHBUTTON, BS_PUSHBUTTON,
};

use crate::capture::Bitmap;
use crate::geometry::Rect;

const CLASS_NAME: PCWSTR = w!("RecRAnnotate");
const PROMPT_CLASS: PCWSTR = w!("RecRAnnotateText");

/// Height of the in-window toolbar strip.
const TOOLBAR: i32 = 40;

const BTN_TEXT: u32 = 6101;
const BTN_MOSAIC: u32 = 6102;
const BTN_BLOCK: u32 = 6103;
const BTN_UNDO: u32 = 6104;
const BTN_SAVE: u32 = 6105;
const BTN_CANCEL: u32 = 6106;
const BTN_COLOR: u32 = 6107;

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
}

enum Op {
    Mosaic { x: i32, y: i32, w: i32, h: i32, block: i32 },
    Text { x: i32, y: i32, text: String, color: u32 },
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

struct Editor {
    base: Bitmap,
    canvas: Canvas,
    ops: Vec<Op>,
    tool: Tool,
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
    result: Option<Bitmap>,
}

impl Editor {
    fn to_image(&self, x: i32, y: i32) -> (i32, i32) {
        let ix = (x as f64 / self.scale).floor() as i32;
        let iy = ((y - TOOLBAR) as f64 / self.scale).floor() as i32;
        (
            ix.clamp(0, self.base.width - 1),
            iy.clamp(0, self.base.height - 1),
        )
    }

    fn to_disp(&self, x: i32, y: i32) -> (i32, i32) {
        (
            (x as f64 * self.scale).round() as i32,
            (y as f64 * self.scale).round() as i32 + TOOLBAR,
        )
    }

    /// Where the caret marker should be drawn, if anywhere.
    fn caret(&self) -> Option<(i32, i32)> {
        match self.tool {
            Tool::Text => self.pending.or(self.hover),
            Tool::Mosaic => None,
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
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);

            let brush: HBRUSH = CreateSolidBrush(COLORREF(0x00262626));
            let bar = RECT { left: 0, top: 0, right: rc.right, bottom: TOOLBAR };
            FillRect(hdc, &bar, brush);
            // The toolbar can be wider than the image, so paint the rest too.
            let body = RECT { left: 0, top: TOOLBAR, right: rc.right, bottom: rc.bottom };
            FillRect(hdc, &body, brush);
            let _ = DeleteObject(brush.into());

            let _ = SetStretchBltMode(hdc, HALFTONE);
            let _ = StretchBlt(
                hdc,
                0,
                TOOLBAR,
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
            if my >= TOOLBAR {
                let (ix, iy) = st.to_image(mx, my);
                match st.tool {
                    Tool::Mosaic => {
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
                let hover = if my >= TOOLBAR { Some(st.to_image(mx, my)) } else { None };
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
                    st.ops.push(Op::Mosaic { x, y, w, h, block: st.block });
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
                    st.result = Some(st.canvas.snapshot());
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
            LRESULT(0)
        }
        WM_KEYDOWN => {
            match wparam.0 as u32 {
                0x1B => {
                    // Esc
                    let _ = DestroyWindow(hwnd);
                }
                0x0D => {
                    // Enter
                    st.result = Some(st.canvas.snapshot());
                    let _ = DestroyWindow(hwnd);
                }
                0x54 => st.tool = Tool::Text,
                0x4D => st.tool = Tool::Mosaic,
                0x5A if GetKeyState(VK_CONTROL.0 as i32) < 0 => {
                    st.ops.pop();
                    st.compose();
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_SETCURSOR => {
            let cursor = match st.tool {
                Tool::Text => IDC_IBEAM,
                Tool::Mosaic => IDC_CROSS,
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

/// Small modal input box; returns the trimmed text or `None`.
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

        let state = Box::new(PromptState { edit: HWND::default(), value: None });
        let ptr = Box::into_raw(state);

        let (w, h) = (400, 116);
        let sx = GetSystemMetrics(SM_CXSCREEN);
        let sy = GetSystemMetrics(SM_CYSCREEN);
        let mut px = x - 40;
        let mut py = y - 60;
        px = px.clamp(0, (sx - w).max(0));
        py = py.clamp(0, (sy - h).max(0));

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

/// Let the user annotate `bmp`; `None` when cancelled.
///
/// `origin` is where the capture was taken from, so the editor appears over the
/// same area instead of jumping to the middle of the screen. `copy` only changes
/// the main button label — the clipboard write itself happens in
/// [`crate::screenshot::finish`], driven by `copy_on_shot`.
pub fn edit(bmp: Bitmap, origin: Option<Rect>, copy: bool) -> Result<Option<Bitmap>> {
    unsafe {
        let (sx, sy) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
        // Fit the capture on screen, but never scale it up.
        let max_w = (sx - 80).max(320);
        let max_h = (sy - TOOLBAR - 120).max(240);
        let scale = (max_w as f64 / bmp.width as f64)
            .min(max_h as f64 / bmp.height as f64)
            .clamp(0.05, 1.0);
        let disp_w = ((bmp.width as f64 * scale).round() as i32).max(1);
        let disp_h = ((bmp.height as f64 * scale).round() as i32).max(1);
        let scale = disp_w as f64 / bmp.width as f64;

        let items = toolbar_items(16, copy);
        let widths = button_widths(&items);
        let toolbar_w = 8 + widths.iter().map(|w_| w_ + 6).sum::<i32>();
        let win_w = disp_w.max(toolbar_w).min(sx.max(320));
        let win_h = TOOLBAR + disp_h;
        let x = origin.map(|r| r.x).unwrap_or((sx - win_w) / 2);
        let y = origin.map(|r| r.y).unwrap_or((sy - win_h) / 2);
        let x = x.clamp(0, (sx - win_w).max(0));
        let y = y.clamp(0, (sy - win_h).max(0));

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
            win_w,
            win_h,
            None,
            None,
            Some(hinstance.into()),
            Some(ptr as *const core::ffi::c_void),
        )?;
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr as isize);

        // Toolbar buttons; the block size and colour buttons keep handles so their
        // labels can change.
        let mut bx = 8;
        for ((id, label, _), w_) in items.into_iter().zip(widths) {
            let style = if id == BTN_SAVE { BS_DEFPUSHBUTTON } else { BS_PUSHBUTTON };
            let style = WINDOW_STYLE(style as u32);
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
                Some(hwnd),
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
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        let mut boxed = Box::from_raw(ptr);
        tracing::info!(
            "annotate: {} op(s), {}",
            boxed.ops.len(),
            match &boxed.result {
                Some(b) => format!("saved {}x{}", b.width, b.height),
                None => "cancelled".to_string(),
            }
        );
        boxed.canvas.destroy();
        Ok(boxed.result.take())
    }
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
    }
    Ok(())
}
