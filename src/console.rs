//! Console window handling.
//!
//! `rec-r.exe` is a console-subsystem binary so that `rec-r shot` etc. can write
//! to a terminal. That has one ugly side effect: double-clicking the exe (or the
//! Start-menu shortcut, or starting it from the Run key) makes Windows open a
//! console window that stays around for as long as the tray app runs.
//!
//! Windows tells us whether the console belongs to us alone: a console started
//! by Explorer has exactly one attached process. When rec-r is launched from a
//! shell, the shell is attached too, and the console is left alone.

use std::os::windows::io::AsRawHandle;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Console::{
    FreeConsole, GetConsoleProcessList, GetConsoleWindow, SetStdHandle, STD_ERROR_HANDLE,
    STD_OUTPUT_HANDLE,
};
use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE};

/// Is this process still connected to a console?
pub fn attached() -> bool {
    unsafe { !GetConsoleWindow().0.is_null() }
}

/// Hide and release the console window if Windows created it just for us.
///
/// Returns `true` when the console was dropped (i.e. the app now runs windowless
/// and has no stdout/stderr anymore).
pub fn hide_if_owned() -> bool {
    unsafe {
        let hwnd = GetConsoleWindow();
        if hwnd.0.is_null() {
            return false;
        }
        // 1 attached process == nobody else is using this console, so it was
        // created for this process (double-click / shortcut / Run key).
        let mut pids = [0u32; 2];
        if GetConsoleProcessList(&mut pids) != 1 {
            return false;
        }
        // Hide first: FreeConsole would otherwise flash a closing window.
        let _ = ShowWindow(hwnd, SW_HIDE);
        FreeConsole().is_ok()
    }
}

/// Point stdout/stderr at a file so a windowless process does not print into a
/// closed handle (Rust's `println!` panics when the write fails).
pub fn redirect_streams_to(path: &std::path::Path) {
    let file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        Ok(file) => file,
        Err(e) => {
            // Nothing sensible left to report to; keep the tray running.
            let _ = e;
            return;
        }
    };
    let handle = HANDLE(file.as_raw_handle());
    unsafe {
        let _ = SetStdHandle(STD_OUTPUT_HANDLE, handle);
        let _ = SetStdHandle(STD_ERROR_HANDLE, handle);
    }
    // `SetStdHandle` stores the raw handle process-wide, so the file must stay
    // open for the lifetime of the process: leak it on purpose.
    std::mem::forget(file);
}