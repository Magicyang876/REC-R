//! Top-level window enumeration and lookup.

use serde::{Deserialize, Serialize};
use windows::core::{BOOL, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT, TRUE};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetAncestor, GetClassNameW, GetForegroundWindow, GetTopWindow, GetWindow,
    GetWindowLongPtrW, GetWindowRect, GetWindowTextW, GetWindowThreadProcessId, IsIconic,
    IsWindow, IsWindowVisible, GA_ROOT, GWL_EXSTYLE, GWL_STYLE, GW_HWNDNEXT, WS_CAPTION,
    WS_EX_TOOLWINDOW, WS_THICKFRAME,
};

use crate::geometry::Rect;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowInfo {
    /// `HWND` as an integer — accepted back as a selector.
    pub hwnd: i64,
    pub title: String,
    pub class: String,
    /// Executable file name, e.g. `Code.exe`.
    pub process: String,
    pub pid: u32,
    pub rect: Rect,
    pub minimized: bool,
    pub visible: bool,
}

impl WindowInfo {
}

fn rect_from(r: RECT) -> Rect {
    Rect::new(r.left, r.top, r.right - r.left, r.bottom - r.top)
}

fn window_text(hwnd: HWND) -> String {
    let mut buf = [0u16; 512];
    unsafe {
        let len = GetWindowTextW(hwnd, &mut buf);
        String::from_utf16_lossy(&buf[..len.max(0) as usize])
    }
}

fn window_class(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    unsafe {
        let len = GetClassNameW(hwnd, &mut buf);
        String::from_utf16_lossy(&buf[..len.max(0) as usize])
    }
}

fn process_name(pid: u32) -> String {
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return String::new();
        };
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let mut name = String::new();
        if QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len)
            .is_ok()
        {
            let full = String::from_utf16_lossy(&buf[..len as usize]);
            name = full
                .rsplit(['\\', '/'])
                .next()
                .unwrap_or(&full)
                .to_string();
        }
        let _ = CloseHandle(handle);
        name
    }
}

fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked: u32 = 0;
    unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut _ as *mut _,
            std::mem::size_of::<u32>() as u32,
        )
        .is_ok()
            && cloaked != 0
    }
}

/// Client-visible frame bounds (excludes the invisible resize border of
/// DWM-composited windows), falling back to `GetWindowRect`.
fn frame_rect(hwnd: HWND) -> Rect {
    let mut r = RECT::default();
    unsafe {
        if DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut r as *mut _ as *mut _,
            std::mem::size_of::<RECT>() as u32,
        )
        .is_ok()
        {
            return rect_from(r);
        }
        if GetWindowRect(hwnd, &mut r).is_ok() {
            return rect_from(r);
        }
    }
    Rect::default()
}

fn collect(hwnd: HWND, out: &mut Vec<WindowInfo>) {
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() || is_cloaked(hwnd) {
            return;
        }
        let title = window_text(hwnd);
        if title.trim().is_empty() {
            return;
        }
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
        let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        // Frameless windows (Qt / Electron / custom chrome) have no WS_CAPTION but
        // are still normal top-level windows; fixed-size tool windows are not.
        let has_frame = style & WS_CAPTION.0 != 0 || style & WS_THICKFRAME.0 != 0;
        if !has_frame || ex_style & WS_EX_TOOLWINDOW.0 != 0 {
            return;
        }
        // Skip minimized windows: they report the iconic position (-32000).
        if IsIconic(hwnd).as_bool() {
            return;
        }
        let rect = frame_rect(hwnd);
        if rect.width < 40 || rect.height < 30 {
            return;
        }
        if rect.x <= -32000 || rect.y <= -32000 {
            return;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        out.push(WindowInfo {
            hwnd: hwnd.0 as i64,
            title,
            class: window_class(hwnd),
            process: process_name(pid),
            pid,
            rect,
            minimized: IsIconic(hwnd).as_bool(),
            visible: true,
        });
    }
}

unsafe extern "system" fn enum_window_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let out = &mut *(lparam.0 as *mut Vec<WindowInfo>);
    collect(hwnd, out);
    TRUE
}

/// Visible, non-tool, non-cloaked top-level windows in Z-order (topmost first).
pub fn list_windows() -> Vec<WindowInfo> {
    let mut out: Vec<WindowInfo> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(enum_window_cb), LPARAM(&mut out as *mut _ as isize));
    }
    out
}

/// Resolve `hwnd` (decimal or `0x` hex), exact title, or case-insensitive title substring.
pub fn resolve_window(selector: &str) -> Option<WindowInfo> {
    let sel = selector.trim();
    if sel.is_empty() {
        return None;
    }
    if let Some(hwnd) = parse_hwnd(sel) {
        if let Some(w) = list_windows().into_iter().find(|w| w.hwnd == hwnd) {
            return Some(w);
        }
        // Hidden-but-real window: still let callers capture by explicit hwnd.
        let h = HWND(hwnd as *mut _);
        unsafe {
            if IsWindowVisible(h).as_bool() {
                let title = window_text(h);
                let mut pid = 0u32;
                GetWindowThreadProcessId(h, Some(&mut pid));
                return Some(WindowInfo {
                    hwnd,
                    title,
                    class: window_class(h),
                    process: process_name(pid),
                    pid,
                    rect: frame_rect(h),
                    minimized: IsIconic(h).as_bool(),
                    visible: true,
                });
            }
        }
        return None;
    }
    let windows = list_windows();
    let lower = sel.to_lowercase();
    windows
        .iter()
        .find(|w| w.title == sel)
        .or_else(|| windows.iter().find(|w| w.title.to_lowercase() == lower))
        .or_else(|| {
            windows
                .iter()
                .find(|w| w.title.to_lowercase().contains(&lower))
        })
        .or_else(|| {
            windows
                .iter()
                .find(|w| w.process.to_lowercase().contains(&lower))
        })
        .cloned()
}

pub fn parse_hwnd(s: &str) -> Option<i64> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        i64::from_str_radix(hex, 16).ok()
    } else {
        s.parse::<i64>().ok()
    }
}

/// Shell / menu / host classes that look like windows but are never worth capturing.
const SKIP_CLASSES: &[&str] = &[
    "#32768",                                  // popup menu (tray menu, context menu)
    "tooltips_class32",
    "SysShadow",
    "ForegroundStaging",
    "Windows.UI.Core.CoreWindow",
    "ApplicationManager_DesktopShellWindow",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "NotifyIconOverflowWindow",
    "Progman",
    "WorkerW",
    "TaskListThumbnailWnd",
    "MultitaskingViewFrame",
    "XamlExplorerHostIslandWindow",
    "RecRHost",         // our tray host window
    "RecRRegionOverlay" // our selection overlay
];

/// Snapshot of one hwnd, without the filtering `list_windows` applies.
pub fn info_for(hwnd: i64) -> Option<WindowInfo> {
    let hwnd = HWND(hwnd as *mut core::ffi::c_void);
    unsafe {
        if hwnd.0.is_null() || !IsWindow(Some(hwnd)).as_bool() {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        Some(WindowInfo {
            hwnd: hwnd.0 as i64,
            title: window_text(hwnd),
            class: window_class(hwnd),
            process: process_name(pid),
            pid,
            rect: frame_rect(hwnd),
            minimized: IsIconic(hwnd).as_bool(),
            visible: IsWindowVisible(hwnd).as_bool(),
        })
    }
}

/// Whether this window is something Windows Graphics Capture can actually grab.
///
/// Tray menus, tooltips, the shell and our own windows are rejected: capturing
/// them is either impossible (they vanish as soon as the menu closes) or useless.
pub fn is_capturable(info: &WindowInfo) -> bool {
    if !info.visible || info.minimized || info.title.trim().is_empty() {
        return false;
    }
    if info.pid == std::process::id() {
        return false;
    }
    if SKIP_CLASSES
        .iter()
        .any(|c| c.eq_ignore_ascii_case(&info.class))
    {
        return false;
    }
    if info.rect.width < 40 || info.rect.height < 30 {
        return false;
    }
    if info.rect.x <= -32000 || info.rect.y <= -32000 {
        return false;
    }
    let hwnd = HWND(info.hwnd as *mut core::ffi::c_void);
    // Graphics Capture does not care about window styles, so a frameless Qt /
    // Electron window (no WS_CAPTION) is perfectly capturable.
    !is_cloaked(hwnd)
}

/// The window that was active before our menu/hotkey stole focus.
pub fn foreground_window() -> Option<WindowInfo> {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return None;
        }
        info_for(root_of(hwnd)?.0 as i64)
    }
}

fn root_of(hwnd: HWND) -> Option<HWND> {
    if hwnd.0.is_null() {
        return None;
    }
    let mut root = hwnd;
    for _ in 0..8 {
        let parent = unsafe { GetAncestor(root, GA_ROOT) };
        if parent.0.is_null() || parent == root {
            break;
        }
        root = parent;
    }
    Some(root)
}

fn point_in(r: Rect, x: i32, y: i32) -> bool {
    x >= r.x && x < r.right() && y >= r.y && y < r.bottom()
}

/// Topmost capturable window covering the given point (z-order scan).
fn top_window_at(x: i32, y: i32) -> Option<WindowInfo> {
    unsafe {
        let mut hwnd = GetTopWindow(None).ok()?;
        for _ in 0..2000 {
            if hwnd.0.is_null() {
                return None;
            }
            if let Some(info) = info_for(hwnd.0 as i64) {
                if is_capturable(&info) && point_in(info.rect, x, y) {
                    return Some(info);
                }
            }
            match GetWindow(hwnd, GW_HWNDNEXT) {
                Ok(next) if !next.0.is_null() => hwnd = next,
                _ => return None,
            }
        }
        None
    }
}

/// Window under the mouse cursor, ignoring our own windows and shell popups.
///
/// While a tray menu is open the cursor sits on the menu (`#32768`), so the
/// active window and a z-order scan are tried before giving up.
pub fn window_under_cursor(ignore: &[i64]) -> Option<WindowInfo> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, WindowFromPoint};

    let mut pt = POINT::default();
    unsafe {
        GetCursorPos(&mut pt).ok()?;
        let at = WindowFromPoint(pt);
        let mut candidates = Vec::new();
        if let Some(root) = root_of(at) {
            if let Some(info) = info_for(root.0 as i64) {
                candidates.push(info);
            }
        }
        if let Some(fg) = foreground_window() {
            candidates.push(fg);
        }
        if let Some(top) = top_window_at(pt.x, pt.y) {
            candidates.push(top);
        }
        candidates
            .into_iter()
            .find(|w| !ignore.contains(&w.hwnd) && is_capturable(w))
    }
}
