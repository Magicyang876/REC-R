//! Finding ffmpeg when it is not on the PATH of the process that started us.
//!
//! The MSYS/PowerShell split on Windows bites here: an ffmpeg that only exists
//! in a shell's private PATH is invisible to a tray app started from Explorer.
//! On startup we resolve `ffmpeg` to a concrete absolute path, remember it in
//! the config file, and print an actionable hint when nothing is found.

use std::path::{Path, PathBuf};

use crate::config::Config;

pub const MISSING_HINT: &str = "install it (`winget install Gyan.FFmpeg`, `scoop install ffmpeg`) \
or put the full path in %APPDATA%\\Rec-R\\config.json -> \"ffmpeg\", \
e.g. `rec-r config --set ffmpeg=C:\\Tools\\ffmpeg\\bin\\ffmpeg.exe`";

/// Resolve `cfg.ffmpeg` to an existing executable and persist the result.
pub fn resolve_ffmpeg(cfg: &mut Config) {
    let configured = cfg.ffmpeg.clone();
    let Some(found) = find(&configured) else {
        tracing::warn!("ffmpeg not found ({configured}): {MISSING_HINT}");
        return;
    };
    let found_str = found.to_string_lossy().to_string();
    if found_str != configured {
        tracing::info!("ffmpeg: {found_str}");
        cfg.ffmpeg = found_str;
        if let Err(e) = cfg.save() {
            tracing::warn!("could not save the resolved ffmpeg path: {e:#}");
        }
    }
}

pub fn find(configured: &str) -> Option<PathBuf> {
    let configured = configured.trim();
    if !configured.is_empty() {
        let p = Path::new(configured);
        if p.components().count() > 1 || p.is_absolute() {
            if is_executable(p) {
                return Some(p.to_path_buf());
            }
        } else if let Some(hit) = search_path(&[configured.to_string()]) {
            return Some(hit);
        }
    }
    search_path(&["ffmpeg.exe".to_string()]).or_else(search_known_dirs)
}

fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// Walk the process PATH looking for `names[0]`.
fn search_path(names: &[String]) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for name in names {
            let candidate = dir.join(name);
            if is_executable(&candidate) {
                return Some(candidate);
            }
            if !name.contains('.') {
                let exe = dir.join(format!("{name}.exe"));
                if is_executable(&exe) {
                    return Some(exe);
                }
            }
        }
    }
    None
}

fn search_known_dirs() -> Option<PathBuf> {
    let mut shallow: Vec<PathBuf> = Vec::new();
    let mut deep: Vec<PathBuf> = Vec::new();
    for var in ["USERPROFILE", "LOCALAPPDATA", "PROGRAMFILES", "PROGRAMDATA"] {
        if let Some(v) = std::env::var_os(var) {
            let base = PathBuf::from(v);
            shallow.push(base.join("scoop").join("shims"));
            shallow.push(
                base.join("scoop")
                    .join("apps")
                    .join("ffmpeg")
                    .join("current")
                    .join("bin"),
            );
            shallow.push(base.join("chocolatey").join("bin"));
            shallow.push(base.join("Microsoft").join("WinGet").join("Links"));
            deep.push(base.join("Tools"));
            deep.push(base.join("Downloads"));
            deep.push(base.join("Programs"));
            deep.push(base.join("scoop").join("apps"));
            deep.push(base.join("Microsoft").join("WinGet").join("Packages"));
        }
    }
    shallow.push(PathBuf::from(r"C:\ffmpeg\bin"));
    shallow.push(PathBuf::from(r"C:\msys64\mingw64\bin"));
    shallow.push(PathBuf::from(r"C:\Program Files\ffmpeg\bin"));
    shallow.push(PathBuf::from(r"C:\Program Files (x86)\ffmpeg\bin"));

    for dir in &shallow {
        let direct = dir.join("ffmpeg.exe");
        if is_executable(&direct) {
            return Some(direct);
        }
    }
    // Portable/zip layouts live a couple of levels down
    // (`Tools\ffmpeg-7.1-full_build\bin\ffmpeg.exe`).
    for root in &deep {
        if let Some(hit) = scan_deep(root, 3) {
            return Some(hit);
        }
    }
    None
}

/// Bounded search below `root`, matching portable layouts such as
/// `Downloads\ffmpeg-7.1-full_build\bin\ffmpeg.exe` and
/// `WinGet\Packages\Gyan.FFmpeg_xxx\ffmpeg-7.1-full_build\bin\ffmpeg.exe`.
fn scan_deep(root: &Path, depth: usize) -> Option<PathBuf> {
    if depth == 0 {
        return None;
    }
    for dir in child_dirs(root) {
        let direct = dir.join("ffmpeg.exe");
        if is_executable(&direct) {
            return Some(direct);
        }
        if let Some(hit) = scan_deep(&dir, depth - 1) {
            return Some(hit);
        }
    }
    None
}

fn child_dirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect()
}