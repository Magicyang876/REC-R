//! Monitor enumeration (physical pixels, virtual-desktop coordinates).

use serde::{Deserialize, Serialize};
use windows::core::BOOL;
use windows::Win32::Foundation::{LPARAM, RECT, TRUE};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};

use crate::geometry::Rect;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Monitor {
    /// Stable index in the enumeration order used everywhere in this app.
    pub index: usize,
    /// GDI device name, e.g. `\\.\DISPLAY1`.
    pub device: String,
    pub is_primary: bool,
    /// Full monitor bounds in virtual-desktop coordinates.
    pub rect: Rect,
    /// Work area (bounds minus taskbar/appbars).
    pub work: Rect,
    pub dpi: u32,
    pub scale: f32,
}

impl Monitor {
    pub fn label(&self) -> String {
        format!(
            "{} ({}){}",
            self.index,
            self.device,
            if self.is_primary { ", primary" } else { "" }
        )
    }
}

fn rect_from(r: RECT) -> Rect {
    Rect::new(r.left, r.top, r.right - r.left, r.bottom - r.top)
}

unsafe extern "system" fn enum_monitor_cb(
    hmon: HMONITOR,
    _hdc: HDC,
    _clip: *mut RECT,
    data: LPARAM,
) -> BOOL {
    let out = &mut *(data.0 as *mut Vec<(HMONITOR, MONITORINFOEXW)>);
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    if GetMonitorInfoW(hmon, &mut info as *mut _ as *mut MONITORINFO).as_bool() {
        out.push((hmon, info));
    }
    TRUE
}

pub fn list_monitors() -> Vec<Monitor> {
    let mut raw: Vec<(HMONITOR, MONITORINFOEXW)> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(enum_monitor_cb), LPARAM(&mut raw as *mut _ as isize));
    }

    raw.iter()
        .enumerate()
        .map(|(index, (hmon, info))| {
            let mut dpi_x = 96u32;
            let mut dpi_y = 96u32;
            unsafe {
                let _ = GetDpiForMonitor(*hmon, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
            }
            let device = String::from_utf16_lossy(
                &info.szDevice[..info.szDevice.iter().position(|&c| c == 0).unwrap_or(0)],
            );
            Monitor {
                index,
                device,
                is_primary: info.monitorInfo.dwFlags & 1 != 0,
                rect: rect_from(info.monitorInfo.rcMonitor),
                work: rect_from(info.monitorInfo.rcWork),
                dpi: dpi_x,
                scale: dpi_x as f32 / 96.0,
            }
        })
        .collect()
}

pub fn monitor_by_index(index: usize) -> Option<Monitor> {
    list_monitors().into_iter().find(|m| m.index == index)
}

/// The monitor containing `point`, falling back to the primary monitor.
pub fn monitor_at(x: i32, y: i32) -> Option<Monitor> {
    let monitors = list_monitors();
    monitors
        .iter()
        .find(|m| x >= m.rect.x && x < m.rect.right() && y >= m.rect.y && y < m.rect.bottom())
        .cloned()
        .or_else(|| monitors.into_iter().find(|m| m.is_primary))
}

pub fn primary_monitor() -> Option<Monitor> {
    let monitors = list_monitors();
    monitors
        .iter()
        .find(|m| m.is_primary)
        .cloned()
        .or_else(|| monitors.into_iter().next())
}