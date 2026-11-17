//! Start-at-login support via `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.

use anyhow::{bail, Context, Result};
use windows::core::w;
use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
use windows::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ, REG_VALUE_TYPE,
};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "Rec-R";

fn win32_error(what: &str, code: windows::Win32::Foundation::WIN32_ERROR) -> anyhow::Error {
    let e = windows::core::Error::from(code);
    anyhow::anyhow!("{what} failed: {e}")
}

/// `"C:\path\rec-r.exe" app [--http]`
pub fn command_line(with_http: bool) -> Result<String> {
    let exe = std::env::current_exe().context("locating the rec-r executable")?;
    Ok(command_line_at(&exe, with_http))
}

/// Same as [`command_line`], for an explicit executable (used by `rec-r install`,
/// which registers the *installed* copy rather than the one currently running).
pub fn command_line_at(exe: &std::path::Path, with_http: bool) -> String {
    let exe = exe.display().to_string();
    if with_http {
        format!("\"{exe}\" app --http")
    } else {
        format!("\"{exe}\" app")
    }
}

fn open_run_key(access: windows::Win32::System::Registry::REG_SAM_FLAGS) -> Result<HKEY> {
    let mut key = HKEY::default();
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Run"),
            None,
            access,
            &mut key,
        )
    };
    if status.is_err() {
        return Err(win32_error("opening the Run registry key", status));
    }
    Ok(key)
}

/// The command line currently registered, if any.
pub fn current() -> Option<String> {
    let key = open_run_key(KEY_QUERY_VALUE).ok()?;
    let mut kind = REG_VALUE_TYPE::default();
    let mut len: u32 = 0;
    let status = unsafe {
        RegQueryValueExW(key, w!("Rec-R"), None, Some(&mut kind), None, Some(&mut len))
    };
    if status.is_err() || len == 0 {
        let _ = unsafe { RegCloseKey(key) };
        return None;
    }
    let mut buf = vec![0u8; len as usize + 2];
    let mut got = buf.len() as u32;
    let status = unsafe {
        RegQueryValueExW(
            key,
            w!("Rec-R"),
            None,
            Some(&mut kind),
            Some(buf.as_mut_ptr()),
            Some(&mut got),
        )
    };
    let _ = unsafe { RegCloseKey(key) };
    if status.is_err() {
        return None;
    }
    let mut wide: Vec<u16> = buf[..got as usize]
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    while wide.last() == Some(&0) {
        wide.pop();
    }
    Some(String::from_utf16_lossy(&wide))
}

pub fn is_enabled() -> bool {
    current().is_some()
}

pub fn enable(with_http: bool) -> Result<String> {
    let command = command_line(with_http)?;
    apply(&command)?;
    Ok(command)
}

/// Register `exe` (and keep `command_line`/`current` in sync).
pub fn enable_for(exe: &std::path::Path, with_http: bool) -> Result<String> {
    let command = command_line_at(exe, with_http);
    apply(&command)?;
    Ok(command)
}

fn apply(command: &str) -> Result<()> {
    let key = open_run_key(KEY_SET_VALUE)?;
    let mut bytes: Vec<u8> = command
        .encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect();
    let status = unsafe {
        RegSetValueExW(key, w!("Rec-R"), None, REG_SZ, Some(bytes.as_slice()))
    };
    let _ = unsafe { RegCloseKey(key) };
    bytes.clear();
    if status.is_err() {
        return Err(win32_error("writing the Run registry value", status));
    }
    Ok(())
}

pub fn disable() -> Result<()> {
    let key = open_run_key(KEY_SET_VALUE)?;
    let status = unsafe { RegDeleteValueW(key, w!("Rec-R")) };
    let _ = unsafe { RegCloseKey(key) };
    if status.is_err() && status != ERROR_FILE_NOT_FOUND {
        return Err(win32_error("deleting the Run registry value", status));
    }
    Ok(())
}

/// `on` / `off` / `toggle`; returns the new state.
pub fn set_enabled(on: bool, with_http: bool) -> Result<bool> {
    if on {
        enable(with_http)?;
    } else {
        disable()?;
    }
    if is_enabled() != on {
        bail!("registry write did not stick (still {})", is_enabled());
    }
    Ok(on)
}

pub fn summary() -> serde_json::Value {
    serde_json::json!({
        "enabled": is_enabled(),
        "command": current(),
        "key": format!(r"HKEY_CURRENT_USER\{RUN_KEY}"),
        "value": VALUE_NAME,
    })
}