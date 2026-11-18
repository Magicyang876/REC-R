//! Business logic shared by the CLI, the tray UI and the MCP server.
//!
//! Everything here is blocking; async callers wrap it in `spawn_blocking`.

use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::app::App;
use crate::capture::encoder::{Ffmpeg, H264_ENCODERS};
use crate::capture::target::{TargetKind, TargetSpec};
use crate::capture::wgc;
use crate::capture::Bitmap;
use crate::recording::{self, Engine, RecordOptions, RecordingInfo};
use crate::screenshot::{self, ShotOptions, ShotResult};
use crate::win::{monitor, window};

const QUALITIES: [&str; 4] = ["lossless", "high", "balanced", "fast"];

#[derive(Debug, Clone, Default)]
pub struct ShotRequest {
    pub target: TargetSpec,
    pub path: Option<String>,
    pub format: Option<String>,
    pub max_edge: Option<u32>,
    pub quality: Option<String>,
    pub inline: bool,
    /// Override `copy_on_shot` for this shot.
    pub copy: Option<bool>,
}

#[derive(Debug, Clone, Default)]
pub struct RecordRequest {
    pub target: TargetSpec,
    pub path: Option<String>,
    pub fps: Option<u32>,
    pub encoder: Option<String>,
    pub quality: Option<String>,
    pub engine: Option<Engine>,
    pub max_duration_sec: Option<u64>,
    pub scale: Option<(u32, u32)>,
}

fn ffmpeg_for(app: &App) -> Ffmpeg {
    Ffmpeg::new(app.config().ffmpeg)
}

pub fn take_screenshot(app: &App, req: ShotRequest) -> Result<ShotResult> {
    let (ffmpeg, opts) = shot_options(app, &req)?;
    let resolved = req.target.resolve()?;
    screenshot::capture(&ffmpeg, &resolved, &opts)
}

/// Encode an already grabbed (and possibly annotated) bitmap using the same
/// format / path / quality rules as [`take_screenshot`].
pub fn save_shot(app: &App, req: ShotRequest, bmp: &Bitmap, label: &str) -> Result<ShotResult> {
    let (ffmpeg, opts) = shot_options(app, &req)?;
    screenshot::finish(&ffmpeg, bmp, "editor", label, &opts)
}

/// Format / quality / destination / clipboard rules shared by every shot path.
fn shot_options(app: &App, req: &ShotRequest) -> Result<(Ffmpeg, ShotOptions)> {
    let cfg = app.config();
    let format = req
        .format
        .clone()
        .unwrap_or_else(|| cfg.screenshot_format.clone())
        .trim_start_matches('.')
        .to_ascii_lowercase();
    if !matches!(format.as_str(), "png" | "jpg" | "jpeg" | "webp") {
        anyhow::bail!("unsupported screenshot format {format:?} (png|jpg|webp)");
    }
    let quality = check_quality(&req.quality.clone().unwrap_or_else(|| cfg.quality.clone()))?;
    let path = cfg.resolve_output(
        req.path.as_deref(),
        &screenshot::default_shot_name(&format),
    )?;
    let opts = ShotOptions {
        format,
        max_edge: req.max_edge.unwrap_or(cfg.screenshot_max_edge),
        quality: quality_pct(&quality),
        path,
        inline: req.inline,
        copy: req.copy.unwrap_or(cfg.copy_on_shot),
    };
    Ok((ffmpeg_for(app), opts))
}

pub fn start_recording(app: &App, req: RecordRequest) -> Result<RecordingInfo> {
    let cfg = app.config();
    let resolved = req.target.resolve()?;
    let fps = req.fps.unwrap_or(cfg.fps);
    let quality = check_quality(&req.quality.unwrap_or_else(|| cfg.quality.clone()))?;
    let ffmpeg = ffmpeg_for(app);
    let encoder = match req.encoder.unwrap_or_else(|| cfg.encoder.clone()) {
        e if e.eq_ignore_ascii_case("auto") || e.trim().is_empty() => ffmpeg.pick_encoder(""),
        e => e,
    };
    if !H264_ENCODERS.contains(&encoder.as_str()) {
        tracing::warn!("encoder {encoder:?} is not a known H.264 encoder; FFmpeg may fail");
    }
    let path = cfg.resolve_output(
        req.path.as_deref(),
        &recording::default_recording_name(resolved.kind),
    )?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let opts = RecordOptions {
        fps,
        encoder,
        quality,
        engine: req.engine.unwrap_or_default(),
        max_duration_sec: req.max_duration_sec.unwrap_or(cfg.max_duration_sec),
        path,
        scale: req.scale.map(|(w, h)| (even(w), even(h))),
    };
    let rec = app.recorder.start(&ffmpeg, resolved, opts)?;
    Ok(rec.info())
}

pub fn stop_recording(app: &App, id: Option<&str>) -> Result<Vec<RecordingInfo>> {
    app.recorder.stop(id)
}

/// Stop every active recording and delete what it wrote (Esc = throw it away).
pub fn cancel_recording(app: &App, id: Option<&str>) -> Result<Vec<RecordingInfo>> {
    let stopped = app.recorder.stop(id)?;
    for info in &stopped {
        // The muxer may still be closing the file, so retry briefly.
        let mut last: Option<std::io::Error> = None;
        for _ in 0..40 {
            match std::fs::remove_file(&info.path) {
                Ok(()) => {
                    last = None;
                    break;
                }
                Err(e) => {
                    last = Some(e);
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
        }
        match last {
            None => tracing::info!("cancelled recording removed {}", info.path),
            Some(e) => tracing::warn!("could not remove {}: {e}", info.path),
        }
    }
    Ok(stopped)
}

pub fn recording_status(app: &App, id: Option<&str>) -> Vec<RecordingInfo> {
    app.recorder.status(id)
}

fn even(v: u32) -> u32 {
    if v % 2 == 1 {
        v + 1
    } else {
        v
    }
}

/// Map a named quality preset to an FFmpeg image-quality percentage.
fn quality_pct(q: &str) -> u8 {
    match q {
        "lossless" => 100,
        "high" => 92,
        "fast" => 60,
        _ => 80,
    }
}

pub fn check_quality(q: &str) -> Result<String> {
    let q = q.trim().to_ascii_lowercase();
    if QUALITIES.contains(&q.as_str()) {
        Ok(q)
    } else {
        anyhow::bail!("unknown quality {q:?} (lossless|high|balanced|fast)")
    }
}

// ---------------------------------------------------------------- discovery

#[derive(Debug, Clone, Serialize)]
pub struct DisplayView {
    pub index: usize,
    pub device: String,
    pub label: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub dpi: u32,
    pub scale: f32,
    pub primary: bool,
}

pub fn list_displays() -> Vec<DisplayView> {
    monitor::list_monitors()
        .into_iter()
        .map(|m| DisplayView {
            index: m.index,
            device: m.device.clone(),
            label: m.label(),
            x: m.rect.x,
            y: m.rect.y,
            width: m.rect.width,
            height: m.rect.height,
            dpi: m.dpi,
            scale: m.scale,
            primary: m.is_primary,
        })
        .collect()
}

#[derive(Debug, Clone, Serialize)]
pub struct MonitorSummary {
    pub count: usize,
    pub virtual_desktop: String,
    pub displays: Vec<DisplayView>,
}

pub fn monitor_summary() -> MonitorSummary {
    let displays = list_displays();
    MonitorSummary {
        count: displays.len(),
        virtual_desktop: crate::win::virtual_desktop_rect().to_string(),
        displays,
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct WindowView {
    pub hwnd: String,
    pub title: String,
    pub class: String,
    pub process: String,
    pub pid: u32,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub minimized: bool,
}

pub fn list_windows(query: Option<&str>, limit: usize) -> Vec<WindowView> {
    let q = query.map(|q| q.to_ascii_lowercase());
    window::list_windows()
        .into_iter()
        .filter(|w| match &q {
            None => true,
            Some(q) => {
                w.title.to_ascii_lowercase().contains(q)
                    || w.process.to_ascii_lowercase().contains(q)
                    || w.class.to_ascii_lowercase().contains(q)
            }
        })
        .take(if limit == 0 { usize::MAX } else { limit })
        .map(|w| WindowView {
            hwnd: format!("{}", w.hwnd),
            title: w.title,
            class: w.class,
            process: w.process,
            pid: w.pid,
            x: w.rect.x,
            y: w.rect.y,
            width: w.rect.width,
            height: w.rect.height,
            minimized: w.minimized,
        })
        .collect()
}

// ------------------------------------------------------------------- doctor

#[derive(Debug, Clone, Serialize)]
pub struct EncoderCheck {
    pub name: String,
    pub usable: bool,
    /// Measured throughput in frames/second (None when the probe failed).
    pub speed_fps: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub ok: bool,
    pub platform: String,
    pub ffmpeg: String,
    pub ffmpeg_available: bool,
    pub ffmpeg_version: Option<String>,
    pub encoders: Vec<EncoderCheck>,
    pub chosen_encoder: String,
    pub wgc_available: bool,
    pub dpi_aware: bool,
    pub monitors: MonitorSummary,
    pub output_dir: String,
    pub config_path: String,
    pub mcp_http_addr: String,
    pub tray_ui: bool,
    pub active_recordings: Vec<RecordingInfo>,
}

pub fn doctor(app: &App) -> DoctorReport {
    let cfg = app.config();
    let ffmpeg = Ffmpeg::new(cfg.ffmpeg.clone());
    let version = ffmpeg.version().ok();
    // `doctor` is the one place allowed to spend seconds probing encoders.
    let speeds = ffmpeg.benchmark_encoders();
    let mut encoders: Vec<EncoderCheck> = H264_ENCODERS
        .iter()
        .map(|e| EncoderCheck {
            name: (*e).to_string(),
            usable: speeds.iter().any(|(n, s)| n == e && *s > 0.0),
            speed_fps: speeds.iter().find(|(n, _)| n == e).map(|(_, s)| *s),
        })
        .collect();
    // FFmpeg can be present without any H.264 encoder; keep a software one visible.
    if !encoders.iter().any(|e| e.usable) {
        let usable = ffmpeg.encoder_works("libx264");
        encoders.push(EncoderCheck { name: "libx264".into(), usable, speed_fps: None });
    }
    let chosen = ffmpeg.pick_encoder(&cfg.encoder);
    DoctorReport {
        ok: version.is_some() && encoders.iter().any(|e| e.usable),
        platform: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        ffmpeg: cfg.ffmpeg.clone(),
        ffmpeg_available: version.is_some(),
        ffmpeg_version: version,
        encoders,
        chosen_encoder: chosen,
        wgc_available: wgc::available(),
        dpi_aware: crate::win::is_dpi_aware(),
        monitors: monitor_summary(),
        output_dir: cfg.output_dir.to_string_lossy().to_string(),
        config_path: crate::config::config_path().to_string_lossy().to_string(),
        mcp_http_addr: format!("http://{}/mcp", cfg.http_addr),
        tray_ui: app.has_ui(),
        active_recordings: app.recorder.status(None),
    }
}

// -------------------------------------------------------------------- misc

pub fn open_path(path: &Path) -> Result<()> {
    std::process::Command::new("cmd")
        .args(["/C", "start", "", &path.to_string_lossy()])
        .current_dir(path.parent().unwrap_or(Path::new(".")))
        .spawn()
        .with_context(|| format!("failed to open {}", path.display()))?;
    Ok(())
}

pub fn target_from_parts(
    target: Option<TargetKind>,
    monitor: Option<usize>,
    window: Option<String>,
    region: Option<String>,
    xywh: Option<(i32, i32, i32, i32)>,
) -> TargetSpec {
    let mut spec = TargetSpec { target, monitor, window, region, ..Default::default() };
    if let Some((x, y, w, h)) = xywh {
        spec.x = Some(x);
        spec.y = Some(y);
        spec.width = Some(w);
        spec.height = Some(h);
    }
    spec
}

