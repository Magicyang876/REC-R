//! Tray icon + context menu (Shell_NotifyIconW).

use anyhow::{Context, Result};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIIF_WARNING, NIM_ADD,
    NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconFromResourceEx, CreatePopupMenu, GetMenuItemCount, GetMenuStringW, DestroyIcon, DestroyMenu,
    GetCursorPos, GetForegroundWindow, GetSystemMetrics, GetWindowThreadProcessId, HMENU, LoadIconW,
    MF_BYPOSITION, MF_CHECKED, MF_POPUP, MF_SEPARATOR, MF_STRING, PostMessageW, SM_CXSMICON,
    SM_CYSMICON,
    SetForegroundWindow, TrackPopupMenu, HICON, IDI_APPLICATION, IMAGE_FLAGS, LR_DEFAULTCOLOR,
    TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_NULL,
};

use crate::actions;
use crate::app::App;

pub const TRAY_UID: u32 = 1;

pub const ID_SHOT_PRIMARY: u32 = 2001;
pub const ID_SHOT_WINDOW: u32 = 2002;
pub const ID_SHOT_REGION: u32 = 2003;
pub const ID_SHOT_DELAYED: u32 = 2004;
pub const ID_SHOT_MONITOR: u32 = 2100;

pub const ID_REC_PRIMARY: u32 = 3001;
pub const ID_REC_WINDOW: u32 = 3002;
pub const ID_REC_REGION: u32 = 3003;
pub const ID_REC_STOP: u32 = 3004;
pub const ID_REC_MONITOR: u32 = 3100;

pub const ID_OPEN_OUTPUT: u32 = 4001;
pub const ID_MCP_INFO: u32 = 4002;
pub const ID_DOCTOR: u32 = 4003;
pub const ID_CONFIG: u32 = 4004;
pub const ID_QUIT: u32 = 4005;
pub const ID_HOTKEYS: u32 = 4006;
pub const ID_AUTOSTART: u32 = 4007;
pub const ID_COPY_MCP: u32 = 4008;
pub const ID_RESTART: u32 = 4009;

/// The .ico we ship, compiled in so a single .exe is self-contained.
const ICON_ICO: &[u8] = include_bytes!("../../assets/rec-r.ico");

/// (width, height, offset, length) for each image inside an .ico file.
fn ico_entries(data: &[u8]) -> Vec<(u32, u32, usize, usize)> {
    let mut out = Vec::new();
    if data.len() < 6 || data[2] != 1 {
        return out;
    }
    let count = u16::from_le_bytes([data[4], data[5]]) as usize;
    for i in 0..count {
        let e = 6 + i * 16;
        if e + 16 > data.len() {
            break;
        }
        let dim = |v: u8| if v == 0 { 256 } else { v as u32 };
        let len = u32::from_le_bytes([data[e + 8], data[e + 9], data[e + 10], data[e + 11]]) as usize;
        let off =
            u32::from_le_bytes([data[e + 12], data[e + 13], data[e + 14], data[e + 15]]) as usize;
        if off + len <= data.len() && len > 0 {
            out.push((dim(data[e]), dim(data[e + 1]), off, len));
        }
    }
    out
}

/// HICON for the requested size, taken from the embedded .ico.
pub fn app_icon(size: i32) -> Option<HICON> {
    let entries = ico_entries(ICON_ICO);
    if entries.is_empty() {
        return None;
    }
    let want = size.max(16) as u32;
    let pick = entries
        .iter()
        .filter(|(w, _, _, _)| *w <= want)
        .max_by_key(|(w, _, _, _)| *w)
        .or_else(|| entries.iter().min_by_key(|(w, _, _, _)| *w))?;
    let bytes = &ICON_ICO[pick.2..pick.2 + pick.3];
    unsafe {
        CreateIconFromResourceEx(
            bytes,
            true,
            0x0003_0000,
            pick.0 as i32,
            pick.1 as i32,
            IMAGE_FLAGS(LR_DEFAULTCOLOR.0),
        )
        .ok()
    }
}

/// Label of the menu's first item, for diagnostics.
unsafe fn first_label(menu: HMENU) -> String {
    let mut buf = [0u16; 128];
    let n = GetMenuStringW(menu, 0, Some(&mut buf), MF_BYPOSITION);
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub struct TrayIcon {
    hwnd: HWND,
    added: bool,
    /// Icon we created ourselves (must be destroyed; the LoadIconW fallback is shared).
    owned_icon: Option<HICON>,
}

impl TrayIcon {
    pub fn add(hwnd: HWND, tip: &str, callback_msg: u32) -> Result<Self> {
        let mut data = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: TRAY_UID,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
            uCallbackMessage: callback_msg,
            ..Default::default()
        };
        let (cx, cy) = unsafe { (GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON)) };
        let owned = app_icon(cx.max(cy).max(16));
        match owned.as_ref().copied() {
            Some(icon) => {
                tracing::info!("tray icon: embedded rec-r icon ({}px)", cx.max(cy).max(16));
                data.hIcon = icon;
            }
            None => {
                tracing::warn!("tray icon: embedded .ico unusable, falling back to the default icon");
                match unsafe { LoadIconW(None, IDI_APPLICATION) } {
                    Ok(fallback) => data.hIcon = fallback,
                    Err(_) => data.uFlags = NIF_MESSAGE | NIF_TIP,
                }
            }
        }
        let text = wide(tip);
        let n = text.len().min(data.szTip.len());
        data.szTip[..n].copy_from_slice(&text[..n]);

        let ok = unsafe { Shell_NotifyIconW(NIM_ADD, &data) };
        anyhow::ensure!(ok.as_bool(), "Shell_NotifyIconW(NIM_ADD) failed");
        Ok(Self { hwnd, added: true, owned_icon: owned })
    }

    pub fn set_tip(&self, tip: &str) {
        if !self.added {
            return;
        }
        let mut data = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: TRAY_UID,
            uFlags: NIF_TIP,
            ..Default::default()
        };
        let text = wide(tip);
        let n = text.len().min(data.szTip.len());
        data.szTip[..n].copy_from_slice(&text[..n]);
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
        }
    }

    /// Balloon notification.
    pub fn notify(&self, title: &str, body: &str, warning: bool) {
        if !self.added {
            return;
        }
        notify_raw(self.hwnd, title, body, warning);
    }

    pub fn remove(&mut self) {
        if !self.added {
            return;
        }
        let data = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: TRAY_UID,
            ..Default::default()
        };
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &data);
            if let Some(icon) = self.owned_icon.take() {
                let _ = DestroyIcon(icon);
            }
        }
        self.added = false;
    }
}

impl Drop for TrayIcon {
    fn drop(&mut self) {
        self.remove();
    }
}

/// Send a balloon notification addressed to the tray icon owned by `hwnd`.
/// Safe to call from any thread.
pub fn notify_raw(hwnd: HWND, title: &str, body: &str, warning: bool) {
    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_UID,
        uFlags: NIF_INFO,
        dwInfoFlags: if warning { NIIF_WARNING } else { NIIF_INFO },
        Anonymous: windows::Win32::UI::Shell::NOTIFYICONDATAW_0 { uTimeout: 5000 },
        ..Default::default()
    };
    let t = wide(&body.replace('\n', "\r\n"));
    let n = t.len().min(data.szInfo.len());
    data.szInfo[..n].copy_from_slice(&t[..n]);
    let ti = wide(title);
    let n = ti.len().min(data.szInfoTitle.len());
    data.szInfoTitle[..n].copy_from_slice(&ti[..n]);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
    }
}

/// Build the context menu reflecting the current state.
pub fn build_menu(app: &App) -> Result<HMENU> {
    let recording = app.recorder.any_active();
    let hk = app.config().hotkeys;
    unsafe {
        let menu = CreatePopupMenu().context("CreatePopupMenu failed")?;
        let shot = CreatePopupMenu().context("CreatePopupMenu failed")?;
        let rec = CreatePopupMenu().context("CreatePopupMenu failed")?;

        // Menu items render a right-aligned accelerator after a TAB.
        let item = |menu: HMENU, id: u32, label: &str, hotkey: &str| {
            let text = if hotkey.is_empty() {
                label.to_string()
            } else {
                format!("{label}\t{hotkey}")
            };
            let text = wide(&text);
            let _ = AppendMenuW(menu, MF_STRING, id as usize, PCWSTR(text.as_ptr()));
        };

        item(shot, ID_SHOT_PRIMARY, "Primary display", &hk.shot);
        for d in actions::list_displays() {
            let label = format!("Screenshot {}   {}", d.index, d.label);
            let wl = wide(&label);
            let _ = AppendMenuW(
                shot,
                MF_STRING,
                (ID_SHOT_MONITOR + d.index as u32) as usize,
                PCWSTR(wl.as_ptr()),
            );
        }
        let _ = AppendMenuW(shot, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(shot, MF_STRING, ID_SHOT_WINDOW as usize, w!("Window (active window)"));
        item(shot, ID_SHOT_REGION, "Select region...", &hk.shot_region);
        let _ = AppendMenuW(shot, MF_STRING, ID_SHOT_DELAYED as usize, w!("Primary display after 5s"));

        if recording {
            item(rec, ID_REC_STOP, "Stop recording", &hk.stop);
        } else {
            item(rec, ID_REC_PRIMARY, "Record primary display", &hk.record);
            for d in actions::list_displays() {
                let label = format!("Record monitor {}   {}", d.index, d.label);
                let wl = wide(&label);
                let _ = AppendMenuW(
                    rec,
                    MF_STRING,
                    (ID_REC_MONITOR + d.index as u32) as usize,
                    PCWSTR(wl.as_ptr()),
                );
            }
            let _ = AppendMenuW(rec, MF_SEPARATOR, 0, PCWSTR::null());
            let _ = AppendMenuW(rec, MF_STRING, ID_REC_WINDOW as usize, w!("Record window (active window)"));
            item(rec, ID_REC_REGION, "Record region...", &hk.record_region);
        }

        let _ = AppendMenuW(menu, MF_POPUP, shot.0 as usize, w!("Screenshot"));
        let _ = AppendMenuW(menu, MF_POPUP, rec.0 as usize, w!("Record"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, ID_OPEN_OUTPUT as usize, w!("Open output folder"));
        let _ = AppendMenuW(menu, MF_STRING, ID_MCP_INFO as usize, w!("MCP / agent connection..."));
        let _ = AppendMenuW(menu, MF_STRING, ID_COPY_MCP as usize, w!("Copy MCP config"));
        let _ = AppendMenuW(menu, MF_STRING, ID_DOCTOR as usize, w!("Run diagnostics"));
        let _ = AppendMenuW(menu, MF_STRING, ID_CONFIG as usize, w!("Open config file"));
        let _ = AppendMenuW(menu, MF_STRING, ID_HOTKEYS as usize, w!("Hotkeys..."));
        let autostart_flags = if app.config().autostart {
            MF_STRING | MF_CHECKED
        } else {
            MF_STRING
        };
        let _ = AppendMenuW(
            menu,
            autostart_flags,
            ID_AUTOSTART as usize,
            w!("Start with Windows"),
        );
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, ID_RESTART as usize, w!("Restart"));
        let _ = AppendMenuW(menu, MF_STRING, ID_QUIT as usize, w!("Exit"));
        tracing::debug!(
            "tray menu: {} items, first label {:?}",
            GetMenuItemCount(Some(menu)),
            first_label(menu)
        );
        Ok(menu)
    }
}

/// Make our window the foreground window so TrackPopupMenu gets input.
///
/// `SetForegroundWindow` is refused while another app owns the foreground, which
/// is exactly the tray case; attaching to the foreground thread lifts the refusal.
fn activate(hwnd: HWND) {
    unsafe {
        if SetForegroundWindow(hwnd).as_bool() {
            return;
        }
        let foreground = GetForegroundWindow();
        let target_thread = GetWindowThreadProcessId(foreground, None);
        let this_thread = GetCurrentThreadId();
        if foreground.is_invalid()
            || target_thread == 0
            || target_thread == this_thread
        {
            return;
        }
        if AttachThreadInput(target_thread, this_thread, true).as_bool() {
            let _ = SetForegroundWindow(hwnd);
            let _ = AttachThreadInput(target_thread, this_thread, false);
        }
    }
}

/// Show the menu at the cursor and return the chosen command id.
pub fn show_menu(hwnd: HWND, app: &App) -> Option<u32> {
    let menu = match build_menu(app) {
        Ok(m) => m,
        Err(e) => {
            tracing::error!("cannot build tray menu: {e:#}");
            return None;
        }
    };
    let mut pt = windows::Win32::Foundation::POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut pt);
        activate(hwnd);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_LEFTALIGN | TPM_BOTTOMALIGN,
            pt.x,
            pt.y,
            None,
            hwnd,
            None,
        );
        let _ = DestroyMenu(menu);
        // Documented requirement: without this the menu can stay on screen or the
        // next click anywhere gets eaten instead of dismissing it.
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let id = cmd.0 as u32;
        tracing::debug!("tray menu -> command {id}");
        (id != 0).then_some(id)
    }
}