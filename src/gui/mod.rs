//! Tray-hosted UI thread: menu, hotkeys, region overlay.

pub mod annotate;
pub mod esc;
pub mod frame;
pub mod overlay;
pub mod tray;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, HOT_KEY_MODIFIERS};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, FindWindowW,
    GetWindowLongPtrW,
    MessageBoxW, PeekMessageW, PostQuitMessage, RegisterClassW, SetWindowLongPtrW,
    SetWindowTextW, TranslateMessage, CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, MB_ICONINFORMATION,
    MB_OK, MSG, PM_REMOVE, WM_APP, WM_COMMAND, WM_CONTEXTMENU, WM_DESTROY, WM_HOTKEY, WM_LBUTTONDBLCLK,
    WM_LBUTTONUP, WM_USER,
    WM_RBUTTONUP, WNDCLASSW, WS_OVERLAPPED,
};
use tokio::sync::mpsc::UnboundedReceiver;

/// Is a tray host window already alive (i.e. `rec-r app` is running)?
pub fn running_instance() -> Option<HWND> {
    unsafe { FindWindowW(w!("RecRHost"), PCWSTR::null()).ok() }
}

use crate::actions::{self, RecordRequest, ShotRequest};
use crate::app::{App, UiCommand};
use crate::capture::target::TargetSpec;
use crate::geometry::Rect;

use tray::TrayIcon;

/// Tray callback message.
pub const WM_TRAY: u32 = WM_APP + 1;

const HOTKEY_SHOT: i32 = 1;
const HOTKEY_TOGGLE_REC: i32 = 2;
const HOTKEY_REC_WINDOW: i32 = 3;
const HOTKEY_REGION: i32 = 4;
const HOTKEY_REC_REGION: i32 = 5;

const HOTKEY_STOP: i32 = 6;

struct Host {
    app: Arc<App>,
    hwnd: HWND,
    tray: Option<TrayIcon>,
    /// What was actually registered (`Ctrl+Alt+S` -> shot), after fallbacks.
    hotkey_tip: String,
    /// Window that was active when the tray menu opened: the menu itself owns the
    /// cursor (and the foreground), so "window" items must not re-query it.
    menu_target: std::cell::Cell<Option<i64>>,
    /// Red frame marking the region that is being recorded.
    frame: Option<frame::RecFrame>,
    /// Region waiting for its recording to appear, plus when it was picked.
    pending_frame: Option<(Rect, std::time::Instant)>,
}

impl Host {
    fn status_text(&self) -> String {
        let active = self.app.recorder.any_active();
        let state = if active { "recording" } else { "ready" };
        let cfg = self.app.config();
        let shot = if cfg.hotkeys.shot.is_empty() {
            "tray menu".to_string()
        } else {
            cfg.hotkeys.shot
        };
        let record = if cfg.hotkeys.record.is_empty() {
            "tray menu".to_string()
        } else {
            cfg.hotkeys.record
        };
        format!("Rec-R - {state}  ({shot} shot, {record} record)")
    }

    fn refresh_tip(&self) {
        if let Some(tray) = &self.tray {
            tray.set_tip(&self.status_text());
        }
    }

    fn notify(&self, title: &str, body: &str, warning: bool) {
        match &self.tray {
            Some(tray) => tray.notify(title, body, warning),
            None => tray::notify_raw(self.hwnd, title, body, warning),
        }
    }

    fn spawn<F>(&self, title: &'static str, job: F)
    where
        F: FnOnce() -> Result<String> + Send + 'static,
    {
        let raw = self.hwnd.0 as isize;
        std::thread::spawn(move || {
            let hwnd = raw_hwnd(raw);
            match job() {
                Ok(msg) => {
                    tracing::info!("{title}: {msg}");
                    tray::notify_raw(hwnd, title, &msg, false);
                }
                Err(e) => {
                    tracing::error!("{title} failed: {e:#}");
                    tray::notify_raw(hwnd, title, &first_line(&format!("{e:#}")), true);
                }
            }
        });
    }

    /// Notification body for a finished screenshot.
    fn shot_done(res: &crate::screenshot::ShotResult) -> String {
        match res.copied {
            true => format!("copied & saved {}", res.path),
            false => format!("saved {}", res.path),
        }
    }

    fn shot_display(&self, monitor: Option<usize>) {
        let app = self.app.clone();
        self.spawn("Screenshot", move || {
            let spec = TargetSpec::display(monitor);
            let res = actions::take_screenshot(
                &app,
                ShotRequest {
                    target: spec,
                    inline: false,
                    ..Default::default()
                },
            )?;
            Ok(Self::shot_done(&res))
        });
    }

    /// Target for "window" actions: the window that was active before our menu
    /// opened, else whatever is under the pointer (hotkey path).
    fn window_target(&self) -> TargetSpec {
        match self.menu_target.get() {
            Some(hwnd) => TargetSpec::window(hwnd.to_string()),
            None => TargetSpec::window("cursor"),
        }
    }

    fn shot_window(&self) {
        let app = self.app.clone();
        let spec = self.window_target();
        self.spawn("Screenshot", move || {
            let res = actions::take_screenshot(
                &app,
                ShotRequest {
                    target: spec,
                    inline: false,
                    ..Default::default()
                },
            )?;
            Ok(Self::shot_done(&res))
        });
    }

    fn shot_region(&self) {
        // The overlay must run on this (UI) thread.
        let rect = match overlay::select_region_blocking() {
            Ok(Some(r)) => r,
            Ok(None) => return,
            Err(e) => {
                self.notify("Screenshot", &first_line(&format!("{e:#}")), true);
                return;
            }
        };
        let spec = rect_spec(rect);
        // Grab here, right after the overlay closes: the editor needs the pixels
        // and a worker thread must not re-capture a screen that has moved on.
        let bmp = match spec.resolve().and_then(|t| crate::screenshot::grab(&t)) {
            Ok((bmp, _engine)) => bmp,
            Err(e) => {
                self.notify("Screenshot", &first_line(&format!("{e:#}")), true);
                return;
            }
        };
        // Text boxes / mosaic happen on the UI thread (it owns the window).
        let copy = self.app.config().copy_on_shot;
        let edited = match annotate::edit(bmp, Some(rect), copy) {
            Ok(Some(b)) => b,
            Ok(None) => return,
            Err(e) => {
                self.notify("Screenshot", &first_line(&format!("{e:#}")), true);
                return;
            }
        };
        let app = self.app.clone();
        self.spawn("Screenshot", move || {
            let res = actions::save_shot(
                &app,
                ShotRequest {
                    target: spec,
                    inline: false,
                    ..Default::default()
                },
                &edited,
                "region",
            )?;
            Ok(Self::shot_done(&res))
        });
    }

    fn shot_delayed(&self) {
        let app = self.app.clone();
        let raw = self.hwnd.0 as isize;
        std::thread::spawn(move || {
            let hwnd = raw_hwnd(raw);
            std::thread::sleep(Duration::from_secs(5));
            match actions::take_screenshot(
                &app,
                ShotRequest {
                    target: TargetSpec::default(),
                    inline: false,
                    ..Default::default()
                },
            ) {
                Ok(res) => tray::notify_raw(hwnd, "Screenshot", &Self::shot_done(&res), false),
                Err(e) => tray::notify_raw(hwnd, "Screenshot", &first_line(&format!("{e:#}")), true),
            }
        });
    }

    fn record_start(&self, spec: TargetSpec) {
        let app = self.app.clone();
        self.spawn("Recording", move || {
            let info = actions::start_recording(
                &app,
                RecordRequest {
                    target: spec,
                    ..Default::default()
                },
            )?;
            Ok(format!("recording {} -> {}", info.target, info.path))
        });
        self.refresh_tip();
    }

    fn record_stop(&self) {
        let app = self.app.clone();
        self.spawn("Recording", move || {
            let stopped = actions::stop_recording(&app, None)?;
            if stopped.is_empty() {
                return Ok("nothing was recording".to_string());
            }
            let first = &stopped[0];
            Ok(format!("saved {}", first.path))
        });
        self.refresh_tip();
    }

    fn record_region(&mut self) {
        let rect = match overlay::select_region_blocking() {
            Ok(Some(r)) => r,
            Ok(None) => return,
            Err(e) => {
                self.notify("Recording", &first_line(&format!("{e:#}")), true);
                return;
            }
        };
        self.record_start(rect_spec(rect));
        // Armed now, drawn once the recording is actually live (see `sync_frame`).
        self.pending_frame = Some((rect, std::time::Instant::now()));
    }

    /// Stop the running recording and throw the file away (Esc).
    fn cancel_recording(&mut self) {
        if !self.app.recorder.any_active() {
            return;
        }
        tracing::info!("recording cancelled");
        self.pending_frame = None;
        let app = self.app.clone();
        self.spawn("Recording", move || {
            let stopped = actions::cancel_recording(&app, None)?;
            if stopped.is_empty() {
                return Ok("nothing was recording".to_string());
            }
            Ok(format!("cancelled, removed {}", stopped[0].path))
        });
        self.refresh_tip();
    }

    /// Stop whatever is recording; shared by the stop hotkey and the menu.
    fn stop_recording(&self) {
        if !self.app.recorder.any_active() {
            self.notify("Recording", "nothing is recording", false);
            return;
        }
        self.record_stop();
    }

    fn show_frame(&mut self, area: Rect) {
        match &self.frame {
            Some(f) => f.move_to(area),
            None => match frame::RecFrame::show(area) {
                Ok(f) => self.frame = Some(f),
                Err(e) => tracing::warn!("recording frame unavailable: {e:#}"),
            },
        }
    }

    fn hide_frame(&mut self) {
        if self.frame.is_some() {
            frame::hide(&mut self.frame);
        }
    }

    /// Show the frame once the region recording is live, drop it when it ends.
    fn sync_frame(&mut self) {
        let active = self.app.recorder.any_active();
        // Esc cancels a running recording, so the watcher lives exactly as long
        // as something records (this also covers recordings started over MCP).
        match (active, esc::installed()) {
            (true, false) => {
                esc::install(self.hwnd);
            }
            (false, true) => esc::uninstall(),
            _ => {}
        }
        if let Some((area, picked)) = self.pending_frame {
            if active {
                self.pending_frame = None;
                self.show_frame(area);
            } else if picked.elapsed() > Duration::from_secs(3) {
                // Starting the recording failed; never flash a stale frame later.
                self.pending_frame = None;
            }
        }
        if !active {
            self.hide_frame();
        }
    }

    fn toggle_record_display(&self) {
        if self.app.recorder.any_active() {
            self.record_stop();
        } else {
            self.record_start(TargetSpec::default());
        }
    }

    fn toggle_record_window(&self) {
        if self.app.recorder.any_active() {
            self.record_stop();
        } else {
            self.record_start(self.window_target());
        }
    }

    fn toggle_record_region(&mut self) {
        if self.app.recorder.any_active() {
            self.record_stop();
        } else {
            self.record_region();
        }
    }

    fn show_mcp_info(&self) {
        let cfg = self.app.config();
        let dir = dirs::home_dir().unwrap_or_default().join("rec-r");
        let addr = &cfg.http_addr;
        let text = format!(
            "Rec-R MCP server\r\n\r\n\
             HTTP endpoint (streamable):\r\n  http://{addr}/mcp\r\n\r\n\
             Start the HTTP listener:\r\n  rec-r app --http\r\n\r\n\
             stdio for MCP clients that launch a process:\r\n  rec-r serve --stdio\r\n\r\n\
             Example client config:\r\n\
             {{\r\n  \"mcpServers\": {{\r\n    \"rec-r\": {{\r\n      \"command\": \"rec-r\",\r\n      \
             \"args\": [\"serve\", \"--stdio\"]\r\n    }}\r\n  }}\r\n}}\r\n\r\n\
             Tools: list_displays, list_windows, screenshot, record_start, record_stop, \
             record_status, select_region, doctor.\r\n\r\nOutput folder:\r\n  {}\r\n{}\r\n\r\n\
             Tip: the tray menu's \"Copy MCP config\" item puts a paste-ready \
             mcpServers block (with the absolute executable path) on the clipboard.",
            cfg.output_dir.display(),
            dir.display()
        );
        unsafe {
            let _ = MessageBoxW(
                Some(self.hwnd),
                PCWSTR(wide(&text).as_ptr()),
                w!("Rec-R  MCP"),
                MB_OK | MB_ICONINFORMATION,
            );
        }
    }

    /// Put a paste-ready MCP client config for *this* executable on the clipboard.
    fn copy_mcp_config(&mut self) {
        let text = crate::mcp::client_config();
        match crate::clipboard::set_text(&text) {
            Ok(()) => {
                tracing::info!("copied MCP config: {text}");
                self.notify(
                    "MCP config",
                    "mcpServers block copied to the clipboard",
                    false,
                );
            }
            Err(e) => self.notify("MCP config", &first_line(&format!("{e:#}")), true),
        }
    }

    /// Restart with the same arguments (a helper waits for this process to exit
    /// so the tray icon, hotkeys and MCP port are released before the new one starts).
    fn restart(&mut self) {
        let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
        match crate::restart::relaunch_self(&args) {
            Ok(()) => {
                tracing::info!("restarting with {args:?}");
                unsafe {
                    let _ = DestroyWindow(self.hwnd);
                }
            }
            Err(e) => self.notify("Restart", &first_line(&format!("{e:#}")), true),
        }
    }

    fn run_doctor(&self) {
        let app = self.app.clone();
        self.spawn("Diagnostics", move || {
            let report = actions::doctor(&app);
            let dir = app.config().ensure_output_dir()?;
            let path = dir.join("doctor.json");
            let text = serde_json::to_string_pretty(&report).unwrap_or_default();
            std::fs::write(&path, text)?;
            let _ = actions::open_path(&path);
            Ok(format!("written to {}", path.display()))
        });
    }

    fn handle_menu(&mut self, id: u32) {
        match id {
            tray::ID_SHOT_PRIMARY => self.shot_display(None),
            tray::ID_SHOT_WINDOW => self.shot_window(),
            tray::ID_SHOT_REGION => self.shot_region(),
            tray::ID_SHOT_DELAYED => self.shot_delayed(),
            tray::ID_REC_PRIMARY => self.record_start(TargetSpec::default()),
            tray::ID_REC_WINDOW => {
                let spec = self.window_target();
                self.record_start(spec);
            }
            tray::ID_REC_REGION => self.record_region(),
            tray::ID_REC_STOP => self.record_stop(),
            tray::ID_OPEN_OUTPUT => {
                if let Ok(dir) = self.app.config().ensure_output_dir() {
                    let _ = actions::open_path(&dir);
                }
            }
            tray::ID_MCP_INFO => self.show_mcp_info(),
            tray::ID_COPY_MCP => self.copy_mcp_config(),
            tray::ID_RESTART => self.restart(),
            tray::ID_DOCTOR => self.run_doctor(),
            tray::ID_CONFIG => {
                let path = crate::config::config_path();
                if !path.exists() {
                    let _ = self.app.config().save();
                }
                let _ = actions::open_path(&path);
            }
            tray::ID_HOTKEYS => self.show_hotkeys(),
            tray::ID_AUTOSTART => self.toggle_autostart(),
            id if id >= tray::ID_SHOT_MONITOR && id < tray::ID_SHOT_MONITOR + 64 => {
                self.shot_display(Some((id - tray::ID_SHOT_MONITOR) as usize))
            }
            id if id >= tray::ID_REC_MONITOR && id < tray::ID_REC_MONITOR + 64 => {
                self.record_start(TargetSpec::display(Some((id - tray::ID_REC_MONITOR) as usize)))
            }
            tray::ID_QUIT => unsafe {
                let _ = DestroyWindow(self.hwnd);
            },
            other => tracing::debug!("unhandled menu command {other}"),
        }
    }

    fn show_hotkeys(&self) {
        let cfg = self.app.config();
        let tip = if self.hotkey_tip.is_empty() {
            "no hotkeys registered".to_string()
        } else {
            self.hotkey_tip.clone()
        };
        let text = format!(
            "Registered:\n  {tip}\n\nConfigured in {}:\n  shot = {}\n  record = {}\n  stop = {}\n  record_window = {}\n  shot_region = {}\n  record_region = {}\n\nChange them with:\n  rec-r config --set hotkeys.shot=Ctrl+Shift+5\nor the MCP `config` tool, then restart the tray app.",
            crate::config::config_path().display(),
            cfg.hotkeys.shot,
            cfg.hotkeys.record,
            cfg.hotkeys.stop,
            cfg.hotkeys.record_window,
            cfg.hotkeys.shot_region,
            cfg.hotkeys.record_region,
        );
        unsafe {
            let _ = MessageBoxW(
                Some(self.hwnd),
                PCWSTR(wide(&text).as_ptr()),
                w!("Rec-R  Hotkeys"),
                MB_OK | MB_ICONINFORMATION,
            );
        }
    }

    /// Flip `autostart` in the config and mirror it into the Run registry key.
    fn toggle_autostart(&self) {
        let cfg = self.app.config();
        let want = !cfg.autostart;
        match crate::autostart::set_enabled(want, false) {
            Ok(on) => {
                let saved = self.app.update_config(|c| c.autostart = on);
                let msg = if on {
                    format!("starts with Windows: {}", crate::autostart::current().unwrap_or_default())
                } else {
                    "autostart disabled".to_string()
                };
                match saved {
                    Ok(_) => self.notify("Autostart", &msg, false),
                    Err(e) => self.notify("Autostart", &format!("{msg} (config not saved: {e:#})"), true),
                }
            }
            Err(e) => self.notify("Autostart", &first_line(&format!("{e:#}")), true),
        }
    }

    fn open_menu(&mut self) {
        // TrackPopupMenu activates our host window, so remember what the user was
        // working in *before* the menu steals the foreground.
        let active = crate::win::window::foreground_window()
            .filter(crate::win::window::is_capturable)
            .map(|w| w.hwnd);
        self.menu_target.set(active);
        match active.and_then(crate::win::window::info_for) {
            Some(w) => tracing::debug!("menu window target: [{}] {:?}", w.hwnd, w.title),
            None => tracing::debug!("menu window target: none (will use the pointer)"),
        }
        let cmd = tray::show_menu(self.hwnd, &self.app);
        if let Some(cmd) = cmd {
            self.handle_menu(cmd);
        }
        self.menu_target.set(None);
    }
}

/// Simple information box, for paths that have no console to print into.
pub fn message_box(title: &str, text: &str) {
    let text = wide(text);
    let title = wide(title);
    unsafe {
        let _ = MessageBoxW(
            None,
            PCWSTR(text.as_ptr()),
            PCWSTR(title.as_ptr()),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}

/// Rebuild an `HWND` from a raw value inside a worker thread (`HWND` is not `Send`).
fn raw_hwnd(raw: isize) -> HWND {
    HWND(raw as *mut core::ffi::c_void)
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or_default().to_string()
}

fn rect_spec(rect: Rect) -> TargetSpec {
    TargetSpec {
        target: Some(crate::capture::target::TargetKind::Region),
        x: Some(rect.x),
        y: Some(rect.y),
        width: Some(rect.width),
        height: Some(rect.height),
        ..Default::default()
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Host;
    match msg {
        WM_TRAY => {
            // The shell sends the mouse message in lParam. Right click arrives as
            // WM_RBUTTONUP on legacy (v0) icons, but Explorer also sends
            // WM_CONTEXTMENU (keyboard/context path) and NIN_SELECT/NIN_KEYSELECT,
            // so accept every "open the menu" flavour.
            let event = lparam.0 as u32;
            const NIN_SELECT: u32 = WM_USER;
            const NIN_KEYSELECT: u32 = WM_USER + 1;
            let open = matches!(
                event,
                WM_LBUTTONUP
                    | WM_RBUTTONUP
                    | WM_CONTEXTMENU
                    | WM_LBUTTONDBLCLK
                    | NIN_SELECT
                    | NIN_KEYSELECT
            );
            tracing::debug!("tray event 0x{event:04x}");
            if !ptr.is_null() && open {
                (*ptr).open_menu();
            }
            LRESULT(0)
        }
        WM_HOTKEY => {
            if !ptr.is_null() {
                let host = &mut *ptr;
                match wparam.0 as i32 {
                    HOTKEY_SHOT => host.shot_display(None),
                    HOTKEY_TOGGLE_REC => host.toggle_record_display(),
                    HOTKEY_REC_WINDOW => host.toggle_record_window(),
                    HOTKEY_REGION => host.shot_region(),
                    HOTKEY_REC_REGION => host.toggle_record_region(),
                    HOTKEY_STOP => host.stop_recording(),
                    _ => {}
                }
            }
            LRESULT(0)
        }
        esc::WM_CANCEL_RECORDING => {
            if !ptr.is_null() {
                (*ptr).cancel_recording();
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            if !ptr.is_null() {
                let id = (wparam.0 & 0xFFFF) as u32;
                (*ptr).handle_menu(id);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            if !ptr.is_null() {
                let mut host = Box::from_raw(ptr);
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                if let Some(mut t) = host.tray.take() {
                    t.remove();
                }
            }
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Create the hidden host window, tray icon and hotkeys, then pump messages.
pub fn run(app: Arc<App>, mut rx: UnboundedReceiver<UiCommand>, show_tray: bool) -> Result<()> {
    unsafe {
        let hinstance = GetModuleHandleW(None)?;
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance.into(),
            lpszClassName: w!("RecRHost"),
            ..Default::default()
        };
        anyhow::ensure!(RegisterClassW(&wc) != 0, "RegisterClassW failed");

        let hwnd = CreateWindowExW(
            Default::default(),
            w!("RecRHost"),
            w!("Rec-R"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinstance.into()),
            None,
        )
        .context("CreateWindowExW failed")?;

        let ptr = Box::into_raw(Box::new(Host {
            app: app.clone(),
            hwnd,
            tray: None,
            hotkey_tip: String::new(),
            menu_target: std::cell::Cell::new(None),
            frame: None,
            pending_frame: None,
        }));
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr as isize);
        let host = &mut *ptr;

        if show_tray {
            match TrayIcon::add(hwnd, &host.status_text(), WM_TRAY) {
                Ok(t) => host.tray = Some(t),
                Err(e) => tracing::warn!("tray icon unavailable: {e:#}"),
            }
        } else {
            tracing::info!("tray icon disabled");
        }

        let hotkey_tip = register_hotkeys(hwnd, &app.config());
        (*ptr).hotkey_tip = hotkey_tip.clone();
        if !hotkey_tip.is_empty() {
            tracing::info!("hotkeys: {hotkey_tip}");
        }
        (*ptr).refresh_tip();

        if show_tray {
            let msg = if hotkey_tip.is_empty() {
                "Use the tray menu to capture".to_string()
            } else {
                hotkey_tip.clone()
            };
            (*ptr).notify("Rec-R ready", &msg, false);
        }
        let _ = SetWindowTextW(hwnd, PCWSTR(wide(&(*ptr).status_text()).as_ptr()));

        pump(ptr, &mut rx);
        let _ = Box::from_raw(ptr);
        Ok(())
    }
}

/// Registers the configured hotkeys. When another application already owns a
/// combination, the same key with Shift toggled is tried before giving up.
/// Returns a human readable summary of what was actually registered.
unsafe fn register_hotkeys(hwnd: HWND, cfg: &crate::config::Config) -> String {
    let specs = [
        (HOTKEY_STOP, "stop", cfg.hotkeys.stop.as_str()),
        (HOTKEY_SHOT, "shot", cfg.hotkeys.shot.as_str()),
        (HOTKEY_TOGGLE_REC, "record", cfg.hotkeys.record.as_str()),
        (HOTKEY_REC_WINDOW, "record window", cfg.hotkeys.record_window.as_str()),
        (HOTKEY_REGION, "shot region", cfg.hotkeys.shot_region.as_str()),
        (HOTKEY_REC_REGION, "record region", cfg.hotkeys.record_region.as_str()),
    ];
    let mut parts = Vec::new();
    for (id, action, spec) in specs {
        let parsed = match crate::hotkeys::parse(spec) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("hotkey for {action} ignored: {e:#}");
                continue;
            }
        };
        let Some(configured) = parsed else {
            tracing::info!("hotkey for {action}: disabled");
            continue;
        };
        let fallback = configured.shifted_fallback();
        let mut attempts = vec![configured];
        if fallback != configured {
            attempts.push(fallback);
        }
        let mut done = false;
        for candidate in attempts {
            let mods = HOT_KEY_MODIFIERS(candidate.register_mods());
            match RegisterHotKey(Some(hwnd), id, mods, candidate.vk) {
                Ok(()) => {
                    parts.push(format!("{} {action}", candidate.label()));
                    done = true;
                    break;
                }
                Err(e) => tracing::debug!("hotkey {} unavailable: {e}", candidate.label()),
            }
        }
        if !done {
            tracing::warn!("no hotkey could be registered for {action}");
        }
    }
    parts.join(" · ")
}

fn pump(host: *mut Host, rx: &mut UnboundedReceiver<UiCommand>) {
    loop {
        // Work requested by the MCP server / other threads.
        loop {
            match rx.try_recv() {
                Ok(UiCommand::SelectRegion { reply }) => {
                    let result = match overlay::select_region_blocking() {
                        Ok(r) => r,
                        Err(e) => {
                            tracing::error!("region overlay failed: {e:#}");
                            None
                        }
                    };
                    let _ = reply.send(result);
                }
                Err(_) => break,
            }
        }

        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == windows::Win32::UI::WindowsAndMessaging::WM_QUIT {
                    return;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }

        if !host.is_null() {
            unsafe { (*host).sync_frame() };
        }

        std::thread::sleep(Duration::from_millis(8));
    }
}