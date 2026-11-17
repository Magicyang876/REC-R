//! Recording sessions: engine selection, lifecycle, progress reporting.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};

use crate::capture::encoder::RawSpec;
use crate::capture::gdi::{self, Bitmap};
use crate::capture::wgc::{self, RecordFlags, Stats, WgcRecorder};
use crate::capture::{Ffmpeg, ResolvedTarget, TargetKind};
use crate::geometry::Rect;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Engine {
    /// WGC when possible, GDI otherwise.
    Auto,
    /// Windows Graphics Capture (GPU, occluded windows, per-monitor).
    Wgc,
    /// GDI BitBlt loop (works for regions spanning monitors, no cursor).
    Gdi,
}

impl Default for Engine {
    fn default() -> Self {
        Engine::Auto
    }
}

impl std::str::FromStr for Engine {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        Ok(match s.trim().to_ascii_lowercase().as_str() {
            "auto" | "" => Engine::Auto,
            "wgc" | "wgc/graphics_capture" => Engine::Wgc,
            "gdi" | "bitblt" => Engine::Gdi,
            other => anyhow::bail!("unknown engine {other:?} (auto|wgc|gdi)"),
        })
    }
}

#[derive(Debug, Clone)]
pub struct RecordOptions {
    pub fps: u32,
    pub encoder: String,
    pub quality: String,
    pub engine: Engine,
    pub max_duration_sec: u64,
    pub path: PathBuf,
    pub scale: Option<(u32, u32)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingInfo {
    pub id: String,
    /// recording | stopped | failed
    pub status: String,
    pub target: String,
    pub target_kind: String,
    pub path: String,
    pub started_at: String,
    pub duration_sec: f64,
    pub fps: u32,
    pub engine: String,
    pub encoder: String,
    pub width: u32,
    pub height: u32,
    pub frames: u64,
    pub size_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

enum Sink {
    Wgc(WgcRecorder),
    Gdi(GdiLoop),
}

struct GdiLoop {
    halt: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
    frames: Arc<AtomicU64>,
}

/// One PrintWindow frame of `hwnd`, following the window as it moves or resizes.
fn grab_window_frame(hwnd: i64) -> Result<Bitmap> {
    let info = crate::win::window::info_for(hwnd)
        .ok_or_else(|| anyhow!("window {hwnd} is gone"))?;
    let rect = if info.rect.is_empty() {
        crate::win::virtual_desktop_rect()
    } else {
        info.rect
    };
    gdi::capture_window_print(hwnd as isize, rect)
        .or_else(|_| gdi::capture_window_dc(hwnd as isize, rect))
}

/// Keep the raw-video size constant: crop or pad a frame to `w`x`h` (top-left).
fn fit_frame(bmp: Bitmap, w: u32, h: u32) -> Vec<u8> {
    if bmp.width as u32 == w && bmp.height as u32 == h {
        return bmp.data;
    }
    let mut out = vec![0u8; (w * h * 4) as usize];
    let copy_w = (bmp.width as u32).min(w) as usize;
    let copy_h = (bmp.height as u32).min(h) as usize;
    for y in 0..copy_h {
        let src = y * bmp.width as usize * 4;
        let dst = y * w as usize * 4;
        out[dst..dst + copy_w * 4].copy_from_slice(&bmp.data[src..src + copy_w * 4]);
    }
    // Opaque alpha, so padding stays black instead of transparent.
    for px in out.chunks_exact_mut(4) {
        px[3] = 255;
    }
    out
}

/// A frame producer for the GDI loop: screens, windows, anything BitBlt can read.
type FrameSource = Box<dyn FnMut() -> Result<Bitmap> + Send>;

impl GdiLoop {
    fn start_screen(
        ffmpeg: Ffmpeg,
        rect: Rect,
        fps: u32,
        encoder: String,
        quality: String,
        output: PathBuf,
        stats: Arc<Stats>,
    ) -> Result<Self> {
        let source: FrameSource = Box::new(move || gdi::capture_screen(rect));
        Self::start(source, ffmpeg, fps, encoder, quality, output, stats)
    }

    /// Record a window without Windows Graphics Capture (PrintWindow per frame).
    fn start_window(
        ffmpeg: Ffmpeg,
        hwnd: i64,
        fps: u32,
        encoder: String,
        quality: String,
        output: PathBuf,
        stats: Arc<Stats>,
    ) -> Result<Self> {
        let source: FrameSource = Box::new(move || grab_window_frame(hwnd));
        Self::start(source, ffmpeg, fps, encoder, quality, output, stats)
    }

    fn start(
        mut source: FrameSource,
        ffmpeg: Ffmpeg,
        fps: u32,
        encoder: String,
        quality: String,
        output: PathBuf,
        stats: Arc<Stats>,
    ) -> Result<Self> {
        let halt = Arc::new(AtomicBool::new(false));
        let frames = Arc::new(AtomicU64::new(0));
        let (halt_c, frames_c, stats_c) = (halt.clone(), frames.clone(), stats.clone());
        let out = output.clone();
        let join = std::thread::Builder::new()
            .name("gdi-recorder".into())
            .spawn(move || {
                let interval = Duration::from_micros(1_000_000u64 / fps.max(1) as u64);
                let first = match source() {
                    Ok(b) => b,
                    Err(e) => {
                        *stats_c.error.lock().unwrap() = Some(e.to_string());
                        return;
                    }
                };
                let (w, h) = (first.width as u32, first.height as u32);
                stats_c.width.store(w, Ordering::Relaxed);
                stats_c.height.store(h, Ordering::Relaxed);
                let mut enc = match ffmpeg.start_raw(RawSpec {
                    width: w,
                    height: h,
                    fps,
                    pix_fmt: "bgra",
                    encoder,
                    quality,
                    crop: None,
                    scale: None,
                    output: out,
                }) {
                    Ok(e) => e,
                    Err(e) => {
                        *stats_c.error.lock().unwrap() = Some(e.to_string());
                        return;
                    }
                };
                if enc.feed(&first.data).is_err() {
                    return;
                }
                frames_c.fetch_add(1, Ordering::Relaxed);
                let mut next = Instant::now() + interval;
                while !halt_c.load(Ordering::Relaxed) {
                    match source() {
                        Ok(bmp) => {
                            let frame = fit_frame(bmp, w, h);
                            if let Err(e) = enc.feed(&frame) {
                                *stats_c.error.lock().unwrap() = Some(e.to_string());
                                break;
                            }
                            frames_c.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(e) => {
                            *stats_c.error.lock().unwrap() = Some(e.to_string());
                            break;
                        }
                    }
                    let now = Instant::now();
                    if next > now {
                        std::thread::sleep(next - now);
                        next += interval;
                    } else {
                        // fell behind: resync instead of accumulating debt
                        next = now + interval;
                    }
                }
                if let Err(e) = enc.finish() {
                    *stats_c.error.lock().unwrap() = Some(e.to_string());
                }
            })?;
        Ok(Self { halt, join: Some(join), frames })
    }

    fn stop(mut self) -> (u64, Option<String>) {
        self.halt.store(true, Ordering::Relaxed);
        if let Some(h) = self.join.take() {
            let _ = h.join();
        }
        (self.frames.load(Ordering::Relaxed), None)
    }
}

pub struct Recording {
    pub id: String,
    pub target: ResolvedTarget,
    pub path: PathBuf,
    pub started_at: DateTime<Local>,
    started: Instant,
    fps: u32,
    encoder: String,
    engine_used: String,
    halt: Arc<AtomicBool>,
    stats: Arc<Stats>,
    /// Frame size the encoder is configured to produce (the encoder may start after the
    /// first frame arrives, but the encoded size is known up front).
    expected: (u32, u32),
    sink: Mutex<Option<Sink>>,
    stop_lock: Mutex<()>,
    finished: Mutex<Option<RecordingInfo>>,
}

impl Recording {
    pub fn is_active(&self) -> bool {
        self.finished.lock().unwrap().is_none()
    }

    pub fn info(&self) -> RecordingInfo {
        if let Some(done) = self.finished.lock().unwrap().clone() {
            return done;
        }
        let (frames, _, _, err) = self.stats.snapshot();
        // `stats` report the captured surface (the whole monitor when a region is cropped by
        // ffmpeg), so the encoded size is always the one the encoder was configured with.
        let (w, h) = self.expected;
        let _ = (w, h);
        let _ = frames;
        RecordingInfo {
            id: self.id.clone(),
            status: if err.is_some() { "failed".into() } else { "recording".into() },
            target: self.target.label.clone(),
            target_kind: self.target.kind.to_string(),
            path: self.path.to_string_lossy().to_string(),
            started_at: self.started_at.to_rfc3339(),
            duration_sec: round2(self.started.elapsed().as_secs_f64()),
            fps: self.fps,
            engine: self.engine_used.clone(),
            encoder: self.encoder.clone(),
            width: w,
            height: h,
            frames,
            size_bytes: std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0),
            error: err,
        }
    }

    /// Idempotent: stops the capture, finalizes the container, returns the report.
    pub fn stop(&self) -> RecordingInfo {
        // Serialise concurrent stops (CLI watchdog + user) so the second caller
        // sees the final stats instead of an empty snapshot.
        let _guard = self.stop_lock.lock().unwrap();
        if let Some(done) = self.finished.lock().unwrap().clone() {
            return done;
        }
        self.halt.store(true, Ordering::Relaxed);
        let sink = self.sink.lock().unwrap().take();
        let mut frames = 0u64;
        let mut err: Option<String> = None;
        if let Some(sink) = sink {
            match sink {
                Sink::Wgc(rec) => {
                    let (f, _w, _h, e) = rec.stop();
                    frames = f;
                    err = e;
                }
                Sink::Gdi(loop_) => {
                    let (f, e) = loop_.stop();
                    frames = f;
                    err = e;
                }
            }
        }
        if err.is_none() {
            // Give FFmpeg a moment to flush and stat the finished file.
            std::thread::sleep(Duration::from_millis(120));
        }
        let info = RecordingInfo {
            id: self.id.clone(),
            status: if err.is_some() { "failed".into() } else { "stopped".into() },
            target: self.target.label.clone(),
            target_kind: self.target.kind.to_string(),
            path: self.path.to_string_lossy().to_string(),
            started_at: self.started_at.to_rfc3339(),
            duration_sec: round2(self.started.elapsed().as_secs_f64()),
            fps: self.fps,
            engine: self.engine_used.clone(),
            encoder: self.encoder.clone(),
            width: self.expected.0,
            height: self.expected.1,
            frames,
            size_bytes: std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0),
            error: err,
        };
        *self.finished.lock().unwrap() = Some(info.clone());
        info
    }
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

#[derive(Clone)]
pub struct RecordingManager {
    map: Arc<Mutex<HashMap<String, Arc<Recording>>>>,
    seq: Arc<Mutex<u64>>,
}

impl Default for RecordingManager {
    fn default() -> Self {
        Self::new()
    }
}

impl RecordingManager {
    pub fn new() -> Self {
        Self { map: Arc::new(Mutex::new(HashMap::new())), seq: Arc::new(Mutex::new(0)) }
    }

    fn next_id(&self) -> String {
        let mut n = self.seq.lock().unwrap();
        *n += 1;
        format!("{}-{:02}", Local::now().format("%Y%m%d%H%M%S"), *n)
    }

    /// `rec-20261113-164218-display.mp4` is second-granular, so two recordings
    /// started in the same second would write into the same file. Keep the
    /// first name and suffix the others with `-2`, `-3`, ...
    fn unique_output_path(&self, wanted: &std::path::Path) -> PathBuf {
        let live: Vec<PathBuf> = self.map.lock().unwrap().values().map(|r| r.path.clone()).collect();
        let taken = |p: &std::path::Path| p.exists() || live.iter().any(|q| q == p);
        if !taken(wanted) {
            return wanted.to_path_buf();
        }
        let stem = wanted.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let ext = wanted.extension().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let dir = wanted.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        for n in 2..1000 {
            let name = if ext.is_empty() { format!("{stem}-{n}") } else { format!("{stem}-{n}.{ext}") };
            let candidate = dir.join(name);
            if !taken(&candidate) {
                return candidate;
            }
        }
        wanted.to_path_buf()
    }

    pub fn start(
        &self,
        ffmpeg: &Ffmpeg,
        target: ResolvedTarget,
        opts: RecordOptions,
    ) -> Result<Arc<Recording>> {
        if opts.fps == 0 || opts.fps > 240 {
            anyhow::bail!("fps must be between 1 and 240");
        }
        let id = self.next_id();
        let path = self.unique_output_path(&opts.path);
        let halt = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(Stats::default());

        // Decide the engine.
        let mut engine = opts.engine;
        if engine == Engine::Auto {
            engine = match target.kind {
                TargetKind::Window => Engine::Wgc,
                TargetKind::Display => {
                    if wgc::available() {
                        Engine::Wgc
                    } else {
                        Engine::Gdi
                    }
                }
                // WGC can only capture whole items, so a region would be a monitor
                // capture plus a crop - and on Windows 10 the compositor's capture
                // border cannot be disabled, leaving it drawn around the *monitor*
                // instead of the selected area (and inside the crop when the region
                // touches a screen edge). GDI captures exactly the rectangle.
                TargetKind::Region => Engine::Gdi,
                TargetKind::VirtualDesktop => Engine::Gdi,
            };
        }
        if engine == Engine::Wgc && target.kind == TargetKind::Region {
            tracing::warn!(
                "wgc for a region records the whole monitor and crops it, and the Windows                  capture border will outline the monitor instead of the region;                  engine gdi is what you probably want"
            );
        }
        if engine == Engine::Wgc && !wgc::available() {
            tracing::warn!("WGC unavailable, falling back to GDI");
            engine = Engine::Gdi;
        }

        let flags = RecordFlags {
            fps: opts.fps,
            ffmpeg: ffmpeg.clone(),
            encoder: opts.encoder.clone(),
            quality: opts.quality.clone(),
            output: path.clone(),
            crop: None,
            scale: opts.scale,
            halt: halt.clone(),
            stats: stats.clone(),
            shared: None,
        };

        let (sink, engine_used) = match engine {
            Engine::Wgc => {
                let crop_for = |origin: Rect| -> Option<Rect> {
                    if target.kind == TargetKind::Region {
                        Some(Rect::new(
                            target.rect.x - origin.x,
                            target.rect.y - origin.y,
                            target.rect.width,
                            target.rect.height,
                        ))
                    } else {
                        None
                    }
                };
                let gdi_fallback = |reason: &str| -> Result<(Sink, String)> {
                    tracing::warn!("{reason}; falling back to GDI capture");
                    let loop_ = match target.kind {
                        TargetKind::Window => GdiLoop::start_window(
                            ffmpeg.clone(),
                            target.hwnd.ok_or_else(|| anyhow!("window target without hwnd"))?,
                            opts.fps,
                            opts.encoder.clone(),
                            opts.quality.clone(),
                            path.clone(),
                            stats.clone(),
                        )?,
                        _ => GdiLoop::start_screen(
                            ffmpeg.clone(),
                            target.rect,
                            opts.fps,
                            opts.encoder.clone(),
                            opts.quality.clone(),
                            path.clone(),
                            stats.clone(),
                        )?,
                    };
                    Ok((Sink::Gdi(loop_), "gdi/fallback".to_string()))
                };

                match target.kind {
                    TargetKind::Window => {
                        let hwnd = target
                            .hwnd
                            .ok_or_else(|| anyhow!("window target without hwnd"))?;
                        match WgcRecorder::start_window(hwnd, flags.clone()) {
                            Ok(rec) => (Sink::Wgc(rec), "wgc/window".to_string()),
                            Err(e) => gdi_fallback(&format!("WGC cannot capture window {hwnd} ({e:#})"))?,
                        }
                    }
                    _ => {
                        let monitor = target
                            .monitor
                            .clone()
                            .ok_or_else(|| anyhow!("no monitor resolved for target"))?;
                        let mut flags = flags;
                        flags.crop = crop_for(monitor.rect);
                        match WgcRecorder::start_monitor(&monitor, flags) {
                            Ok(rec) => {
                                let used = if target.kind == TargetKind::Region {
                                    "wgc/monitor+crop"
                                } else {
                                    "wgc/monitor"
                                };
                                (Sink::Wgc(rec), used.to_string())
                            }
                            Err(e) => gdi_fallback(&format!("WGC monitor capture failed ({e:#})"))?,
                        }
                    }
                }
            }
            Engine::Gdi | Engine::Auto => {
                let (gdi, used) = match target.kind {
                    TargetKind::Window => (
                        GdiLoop::start_window(
                            ffmpeg.clone(),
                            target.hwnd.ok_or_else(|| anyhow!("window target without hwnd"))?,
                            opts.fps,
                            opts.encoder.clone(),
                            opts.quality.clone(),
                            path.clone(),
                            stats.clone(),
                        )?,
                        "gdi/printwindow",
                    ),
                    _ => (
                        GdiLoop::start_screen(
                            ffmpeg.clone(),
                            target.rect,
                            opts.fps,
                            opts.encoder.clone(),
                            opts.quality.clone(),
                            path.clone(),
                            stats.clone(),
                        )?,
                        "gdi/bitblt",
                    ),
                };
                (Sink::Gdi(gdi), used.to_string())
            }
        };

        let expected_dims = match opts.scale {
            Some((w, h)) => (w & !1, h & !1),
            None => (target.width() as u32, target.height() as u32),
        };

        let rec = Arc::new(Recording {
            id: id.clone(),
            target,
            path,
            started_at: Local::now(),
            started: Instant::now(),
            fps: opts.fps,
            encoder: opts.encoder.clone(),
            engine_used,
            expected: expected_dims,
            halt,
            stats,
            sink: Mutex::new(Some(sink)),
            stop_lock: Mutex::new(()),
            finished: Mutex::new(None),
        });
        self.map.lock().unwrap().insert(id.clone(), rec.clone());

        // Safety valve: never let a forgotten recording run forever.
        if opts.max_duration_sec > 0 {
            let mgr = self.clone();
            let rec_c = rec.clone();
            let id_c = id.clone();
            std::thread::Builder::new().name("rec-timeout".into()).spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(opts.max_duration_sec);
                while Instant::now() < deadline {
                    if !rec_c.is_active() {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
                tracing::info!("recording {id_c} hit max_duration_sec, auto-stopping");
                let _ = mgr.stop(Some(&id_c));
            })?;
        }

        Ok(rec)
    }

    /// Stop a specific recording (or all of them when `id` is `None`).
    pub fn stop(&self, id: Option<&str>) -> Result<Vec<RecordingInfo>> {
        let recs: Vec<Arc<Recording>> = match id {
            Some(id) => {
                let found = self.map.lock().unwrap().get(id).cloned();
                match found {
                    Some(r) => vec![r],
                    None => {
                        if let Some(done) = self.finished_cache(id) {
                            return Ok(vec![done]);
                        }
                        anyhow::bail!("no recording with id {id}")
                    }
                }
            }
            None => {
                let map = self.map.lock().unwrap();
                map.values().cloned().collect()
            }
        };
        let mut out = Vec::new();
        for rec in recs {
            let info = rec.stop();
            self.map.lock().unwrap().remove(&rec.id);
            out.push(info);
        }
        Ok(out)
    }

    fn finished_cache(&self, _id: &str) -> Option<RecordingInfo> {
        // Stopped recordings are returned directly from `stop`; nothing to cache yet.
        None
    }

    pub fn status(&self, id: Option<&str>) -> Vec<RecordingInfo> {
        let map = self.map.lock().unwrap();
        match id {
            Some(id) => map.get(id).map(|r| vec![r.info()]).unwrap_or_default(),
            None => map.values().map(|r| r.info()).collect(),
        }
    }

    pub fn any_active(&self) -> bool {
        self.map.lock().unwrap().values().any(|r| r.is_active())
    }
}

pub fn default_recording_name(kind: TargetKind) -> String {
    let tag = match kind {
        TargetKind::Display => "display",
        TargetKind::Window => "window",
        TargetKind::Region => "region",
        TargetKind::VirtualDesktop => "desktop",
    };
    format!("rec-{}-{tag}.mp4", Local::now().format("%Y%m%d-%H%M%S"))
}