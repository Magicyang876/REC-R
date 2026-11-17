//! Embedded app icon, used by the tray and exported by `rec-r icon`.

use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::config::Config;

/// Multi-size ICO (16…256 px). Compiled into the exe both as a Win32 resource
/// (see `build.rs`) and as this byte blob (for `CreateIconFromResourceEx`).
pub const ICO: &[u8] = include_bytes!("../assets/rec-r.ico");
/// 256 px PNG for READMEs, docs and agent outputs.
pub const PNG: &[u8] = include_bytes!("../assets/icon-256.png");

pub fn export(cfg: &Config, out: Option<&str>, ico: bool) -> Result<PathBuf> {
    let (bytes, default_name) = if ico {
        (ICO, "rec-r.ico")
    } else {
        (PNG, "rec-r-icon.png")
    };
    let path = cfg.resolve_output(out, default_name)?;
    std::fs::write(&path, bytes).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}