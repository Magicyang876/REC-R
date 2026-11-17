//! Screenshots: grab one frame in-process, encode it with FFmpeg.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::capture::{gdi, wgc, Bitmap, Ffmpeg, ResolvedTarget, TargetKind};

#[derive(Debug, Clone)]
pub struct ShotOptions {
    pub format: String,
    pub max_edge: u32,
    pub quality: u8,
    pub path: PathBuf,
    pub inline: bool,
    /// Also put the captured bitmap on the clipboard.
    pub copy: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShotResult {
    pub path: String,
    pub width: u32,
    pub height: u32,
    pub bytes: u64,
    pub format: String,
    pub target: String,
    pub engine: String,
    /// True when the bitmap was also put on the Windows clipboard
    pub copied: bool,
    /// base64 payload, only when `inline` was requested
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base64: Option<String>,
}

/// Grab a bitmap for the resolved target, preferring the most reliable engine.
pub fn grab(target: &ResolvedTarget) -> Result<(Bitmap, &'static str)> {
    match target.kind {
        TargetKind::Window => {
            let hwnd = target
                .hwnd
                .ok_or_else(|| anyhow::anyhow!("window target without hwnd"))?;
            match wgc::grab_window(hwnd) {
                Ok(b) => Ok((b, "wgc")),
                Err(wgc_err) => {
                    tracing::warn!("WGC window grab failed ({wgc_err}); trying PrintWindow");
                    match gdi::capture_window_print(hwnd as isize, target.rect) {
                        Ok(b) => Ok((b, "printwindow")),
                        Err(pw_err) => {
                            tracing::warn!("PrintWindow failed ({pw_err}); trying window DC");
                            let b = gdi::capture_window_dc(hwnd as isize, target.rect)
                                .or_else(|_| gdi::capture_screen(target.rect))?;
                            Ok((b, "gdi"))
                        }
                    }
                }
            }
        }
        _ => match gdi::capture_screen(target.rect) {
            Ok(b) => Ok((b, "gdi")),
            Err(gdi_err) => {
                let mon = target.monitor.clone().ok_or(gdi_err)?;
                let b = wgc::grab_monitor(&mon)?;
                Ok((b, "wgc"))
            }
        },
    }
}

pub fn capture(ffmpeg: &Ffmpeg, target: &ResolvedTarget, opts: &ShotOptions) -> Result<ShotResult> {
    let (bmp, engine) = grab(target)?;
    finish(ffmpeg, &bmp, engine, &target.label, opts)
}

/// Copy (optionally) and encode an already captured bitmap.
///
/// Split out from [`capture`] so the annotation editor can hand back an edited
/// bitmap without grabbing the screen a second time.
pub fn finish(
    ffmpeg: &Ffmpeg,
    bmp: &Bitmap,
    engine: &'static str,
    label: &str,
    opts: &ShotOptions,
) -> Result<ShotResult> {
    let mut copied = false;
    if opts.copy {
        // A busy clipboard must not lose the screenshot.
        match crate::clipboard::set_bitmap(bmp.width, bmp.height, &bmp.data) {
            Ok(()) => copied = true,
            Err(e) => tracing::warn!("copying the screenshot to the clipboard failed: {e:#}"),
        }
    }
    let (w, h) = ffmpeg.write_image(bmp, &opts.path, &opts.format, opts.max_edge, opts.quality)?;
    let bytes = std::fs::metadata(&opts.path).map(|m| m.len()).unwrap_or(0);
    let base64 = if opts.inline {
        use base64::Engine as _;
        Some(base64::engine::general_purpose::STANDARD.encode(std::fs::read(&opts.path)?))
    } else {
        None
    };
    Ok(ShotResult {
        path: path_string(&opts.path),
        width: w,
        height: h,
        bytes,
        format: opts.format.clone(),
        target: label.to_string(),
        engine: engine.to_string(),
        copied,
        base64,
    })
}

pub fn path_string(p: &Path) -> String {
    p.to_string_lossy().to_string()
}

pub fn default_shot_name(format: &str) -> String {
    let ext = match format.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => "jpg",
        "webp" => "webp",
        _ => "png",
    };
    format!("shot-{}.{ext}", chrono::Local::now().format("%Y%m%d-%H%M%S"))
}