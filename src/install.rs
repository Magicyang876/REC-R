//! `rec-r install` / `rec-r uninstall`: put the executable somewhere stable,
//! make it reachable as `rec-r`, and add a Start-menu shortcut.
//!
//! Everything is per-user (`%LOCALAPPDATA%\Programs\Rec-R`, `HKCU\Environment`),
//! so no elevation is involved and the install matches the HKCU Run key that
//! `rec-r autostart on` writes.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use windows::core::{Interface, HSTRING};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, LPARAM, WPARAM, WIN32_ERROR};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegGetValueW, RegOpenKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
    KEY_SET_VALUE, REG_EXPAND_SZ, REG_SZ, REG_VALUE_TYPE, RRF_RT_REG_EXPAND_SZ,
    RRF_RT_REG_SZ,
};
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
use windows::Win32::UI::WindowsAndMessaging::{
    SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE,
};

/// Where `rec-r install` puts the exe unless `--dir` overrides it.
pub fn default_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Programs")
        .join("Rec-R")
}

pub fn exe_in(dir: &Path) -> PathBuf {
    dir.join("rec-r.exe")
}

/// `%APPDATA%\Microsoft\Windows\Start Menu\Programs\Rec-R.lnk`
pub fn start_menu_shortcut() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join(r"Microsoft\Windows\Start Menu\Programs\Rec-R.lnk"))
}

#[derive(Debug, Default)]
pub struct InstallReport {
    pub dir: PathBuf,
    pub exe: PathBuf,
    pub copied: bool,
    pub path_updated: bool,
    pub path_value: String,
    pub shortcut: Option<PathBuf>,
    pub autostart: Option<String>,
}

#[derive(Debug, Default)]
pub struct UninstallReport {
    pub dir: PathBuf,
    pub path_updated: bool,
    pub shortcut_removed: Option<PathBuf>,
    pub autostart_removed: bool,
    pub exe_removed: bool,
    pub warnings: Vec<String>,
}

/// Copy the running exe into `dir`, then wire PATH / shortcut / autostart.
pub fn install(
    dir: Option<&Path>,
    with_path: bool,
    with_shortcut: bool,
    with_autostart: bool,
    autostart_http: bool,
) -> Result<InstallReport> {
    let dir = dir.map(Path::to_path_buf).unwrap_or_else(default_dir);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("cannot create {}", dir.display()))?;

    let target = exe_in(&dir);
    let source = std::env::current_exe().context("locating the running rec-r.exe")?;
    let copied = if same_file(&source, &target) {
        tracing::info!("{} is already the installed executable", source.display());
        false
    } else {
        std::fs::copy(&source, &target).with_context(|| {
            format!(
                "cannot copy {} to {} (quit the running rec-r tray app and try again)",
                source.display(),
                target.display()
            )
        })?;
        true
    };

    let path_updated = if with_path { add_to_path(&dir)? } else { false };
    let path_value = read_user_path().map(|(v, _)| v).unwrap_or_default();

    let shortcut = if with_shortcut {
        let lnk = start_menu_shortcut().context("cannot locate the Start menu folder")?;
        create_shortcut(
            &lnk,
            &target,
            "app --http",
            "rec-r tray app (screenshots, recordings, MCP server)",
        )?;
        Some(lnk)
    } else {
        None
    };

    let autostart = if with_autostart {
        Some(crate::autostart::enable_for(&target, autostart_http)?)
    } else {
        None
    };

    Ok(InstallReport {
        dir,
        exe: target,
        copied,
        path_updated,
        path_value,
        shortcut,
        autostart,
    })
}

/// Undo `install`: shortcut, autostart, PATH entry, then the files themselves.
pub fn uninstall(dir: Option<&Path>, keep_exe: bool) -> Result<UninstallReport> {
    let dir = dir.map(Path::to_path_buf).unwrap_or_else(default_dir);
    let mut report = UninstallReport { dir: dir.clone(), ..Default::default() };

    if let Some(lnk) = start_menu_shortcut() {
        if lnk.exists() {
            match std::fs::remove_file(&lnk) {
                Ok(()) => report.shortcut_removed = Some(lnk),
                Err(e) => report.warnings.push(format!("{}: {e}", lnk.display())),
            }
        }
    }

    if crate::autostart::is_enabled() {
        match crate::autostart::disable() {
            Ok(()) => report.autostart_removed = true,
            Err(e) => report.warnings.push(format!("autostart: {e:#}")),
        }
    }

    report.path_updated = remove_from_path(&dir).unwrap_or(false);

    if !keep_exe {
        let exe = exe_in(&dir);
        if exe.exists() {
            match std::fs::remove_file(&exe) {
                Ok(()) => report.exe_removed = true,
                Err(e) => report.warnings.push(format!(
                    "{}: {e} (quit the running rec-r tray app and delete it by hand)",
                    exe.display()
                )),
            }
        }
        // Only succeeds when the directory is empty again.
        let _ = std::fs::remove_dir(&dir);
    }

    Ok(report)
}

// ------------------------------------------------------------------- PATH

/// Add `dir` to the user PATH (HKCU\Environment). Returns true when it changed.
pub fn add_to_path(dir: &Path) -> Result<bool> {
    let want = normalise_path(&dir.to_string_lossy());
    let (current, kind) = read_user_path()?;
    if current.split(';').any(|e| normalise_path(e) == want) {
        return Ok(false);
    }
    let mut next = current.trim().trim_end_matches(';').to_string();
    if !next.is_empty() {
        next.push(';');
    }
    next.push_str(&dir.to_string_lossy());
    write_user_path(&next, kind)?;
    Ok(true)
}

/// Drop `dir` from the user PATH. Returns true when it changed.
pub fn remove_from_path(dir: &Path) -> Result<bool> {
    let want = normalise_path(&dir.to_string_lossy());
    let (current, kind) = read_user_path()?;
    let entries: Vec<&str> = current.split(';').collect();
    if !entries.iter().any(|e| normalise_path(e) == want) {
        return Ok(false);
    }
    let kept: Vec<&str> = entries
        .into_iter()
        .filter(|e| !e.trim().is_empty() && normalise_path(e) != want)
        .collect();
    write_user_path(&kept.join(";"), kind)?;
    Ok(true)
}

/// `C:\Foo\Bar\` / `c:/foo/bar` / `"C:\Foo\Bar"` all compare equal.
fn normalise_path(raw: &str) -> String {
    raw.trim()
        .trim_matches('"')
        .trim_end_matches(['\\', '/'])
        .replace('/', "\\")
        .to_ascii_lowercase()
}

fn win32_error(what: &str, code: WIN32_ERROR) -> anyhow::Error {
    let e = windows::core::Error::from(code);
    anyhow::anyhow!("{what} failed: {e}")
}

fn read_user_path() -> Result<(String, REG_VALUE_TYPE)> {
    unsafe {
        let mut kind = REG_VALUE_TYPE::default();
        let mut len: u32 = 0;
        let status = RegGetValueW(
            HKEY_CURRENT_USER,
            windows::core::w!("Environment"),
            windows::core::w!("Path"),
            RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ,
            Some(&mut kind),
            None,
            Some(&mut len),
        );
        if status == ERROR_FILE_NOT_FOUND {
            // No PATH for this user yet: create it as a plain (expandable) string.
            return Ok((String::new(), REG_EXPAND_SZ));
        }
        if status.is_err() {
            return Err(win32_error("reading HKCU\\Environment\\Path", status));
        }

        let mut buf = vec![0u8; len as usize + 2];
        let mut got = buf.len() as u32;
        let status = RegGetValueW(
            HKEY_CURRENT_USER,
            windows::core::w!("Environment"),
            windows::core::w!("Path"),
            RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ,
            Some(&mut kind),
            Some(buf.as_mut_ptr().cast()),
            Some(&mut got),
        );
        if status.is_err() {
            return Err(win32_error("reading HKCU\\Environment\\Path", status));
        }
        let wide: Vec<u16> = buf[..got as usize]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let text = String::from_utf16_lossy(&wide);
        let text = text.trim_end_matches('\0').to_string();
        let kind = if kind == REG_EXPAND_SZ { REG_EXPAND_SZ } else { REG_SZ };
        Ok((text, kind))
    }
}

fn write_user_path(value: &str, kind: REG_VALUE_TYPE) -> Result<()> {
    let key = HKEY::default();
    let mut key = key;
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            windows::core::w!("Environment"),
            None,
            KEY_SET_VALUE,
            &mut key,
        )
    };
    if status.is_err() {
        return Err(win32_error("opening HKCU\\Environment", status));
    }
    let bytes: Vec<u8> = value
        .encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect();
    let status = unsafe {
        RegSetValueExW(key, windows::core::w!("Path"), None, kind, Some(&bytes))
    };
    let _ = unsafe { RegCloseKey(key) };
    if status.is_err() {
        return Err(win32_error("writing HKCU\\Environment\\Path", status));
    }
    broadcast_environment_change();
    Ok(())
}

/// Tell Explorer (and every other running process) that the environment changed,
/// so newly started shells see the new PATH.
fn broadcast_environment_change() {
    let mut name: Vec<u16> = "Environment"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        let _ = SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            WPARAM(0),
            LPARAM(name.as_ptr() as isize),
            SMTO_ABORTIFHUNG,
            2000,
            None,
        );
    }
    name.clear();
}

// --------------------------------------------------------------- shortcut

fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => normalise_path(&a.to_string_lossy()) == normalise_path(&b.to_string_lossy()),
        _ => false,
    }
}

/// Create (or replace) a `.lnk` pointing at `exe` with `args`.
pub fn create_shortcut(lnk: &Path, exe: &Path, args: &str, description: &str) -> Result<()> {
    if let Some(parent) = lnk.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    let exe_text = exe.to_string_lossy().to_string();
    let lnk_text = lnk.to_string_lossy().to_string();
    let workdir = exe
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    unsafe {
        // Shell links need COM; the tray thread may already be initialised.
        let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let initialised = hr.is_ok();
        let result = (|| -> Result<()> {
            let link: IShellLinkW =
                CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)
                    .context("cannot create a shell link")?;
            link.SetPath(&HSTRING::from(exe_text.as_str()))?;
            link.SetArguments(&HSTRING::from(args))?;
            link.SetDescription(&HSTRING::from(description))?;
            if !workdir.is_empty() {
                link.SetWorkingDirectory(&HSTRING::from(workdir.as_str()))?;
            }
            link.SetIconLocation(&HSTRING::from(exe_text.as_str()), 0)?;
            let file: IPersistFile = link.cast().context("shell link is not IPersistFile")?;
            file.Save(&HSTRING::from(lnk_text.as_str()), true)
                .with_context(|| format!("cannot write {}", lnk.display()))?;
            Ok(())
        })();
        if initialised {
            CoUninitialize();
        }
        result
    }
}

/// Dangling-argument guard used by the CLI: an empty install dir is a bug.
pub fn ensure_dir_sane(dir: &Path) -> Result<()> {
    if dir.as_os_str().is_empty() {
        bail!("the install directory is empty");
    }
    Ok(())
}