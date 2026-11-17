//! Windows Graphics Capture (WGC) based frame source.
//!
//! WGC gives DPI-correct physical pixels, captures occluded windows, and is
//! GPU-backed, which is what we want as the primary capture path. Frames are
//! piped into FFmpeg, which does the encoding.

use std::error::Error as StdError;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context as _};
use anyhow::Result;
use windows_capture::capture::{CaptureControl, Context as CaptureContext, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};
use windows_capture::window::Window as WgcWindow;

use crate::capture::encoder::{Ffmpeg, RawEncoder, RawSpec};
use crate::capture::gdi::Bitmap;
use crate::geometry::Rect;
use crate::win::monitor::Monitor;

type HandlerResult<T> = std::result::Result<T, Box<dyn StdError + Send + Sync>>;

#[derive(Default)]
pub struct Stats {
    pub frames: AtomicU64,
    pub width: AtomicU32,
    pub height: AtomicU32,
    pub error: Mutex<Option<String>>,
}

impl Stats {
    pub fn snapshot(&self) -> (u64, u32, u32, Option<String>) {
        (
            self.frames.load(Ordering::Relaxed),
            self.width.load(Ordering::Relaxed),
            self.height.load(Ordering::Relaxed),
            self.error.lock().unwrap().clone(),
        )
    }
}

// ---------------------------------------------------------------- recording

/// Latest frame shared between the WGC callback and the pacer thread.
#[derive(Default)]
pub struct Shared {
    latest: Mutex<Option<(u32, u32, Arc<Vec<u8>>)>>,
    ticks: Arc<AtomicU64>,
}

#[derive(Clone)]
pub struct RecordFlags {
    pub fps: u32,
    pub ffmpeg: Ffmpeg,
    pub encoder: String,
    pub quality: String,
    pub output: PathBuf,
    /// Crop relative to the captured item's frame origin.
    pub crop: Option<Rect>,
    pub scale: Option<(u32, u32)>,
    pub halt: Arc<AtomicBool>,
    pub stats: Arc<Stats>,
    /// Set by `WgcRecorder::start`; the callback publishes frames into it.
    pub shared: Option<Arc<Shared>>,
}

struct RecordHandler {
    flags: RecordFlags,
    shared: Arc<Shared>,
    scratch: Vec<u8>,
}

impl GraphicsCaptureApiHandler for RecordHandler {
    type Flags = RecordFlags;
    type Error = Box<dyn StdError + Send + Sync>;

    fn new(ctx: CaptureContext<Self::Flags>) -> HandlerResult<Self> {
        let shared = ctx
            .flags
            .shared
            .clone()
            .unwrap_or_else(|| Arc::new(Shared::default()));
        Ok(Self {
            flags: ctx.flags,
            shared,
            scratch: Vec::new(),
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        control: InternalCaptureControl,
    ) -> HandlerResult<()> {
        if self.flags.halt.load(Ordering::Relaxed) {
            control.stop();
            return Ok(());
        }

        let result = (|| -> Result<()> {
            let fb = frame.buffer().map_err(|e| anyhow!("frame buffer: {e}"))?;
            let (w, h) = (fb.width(), fb.height());
            // `as_nopadding_buffer` only fills its argument when the frame has
            // row padding; otherwise it returns its own buffer, so always copy
            // out of the returned slice.
            let owned = {
                let bytes = fb.as_nopadding_buffer(&mut self.scratch);
                bytes.to_vec()
            };
            if owned.len() != (w as usize) * (h as usize) * 4 {
                return Err(anyhow!(
                    "unexpected frame size {} for {w}x{h}",
                    owned.len()
                ));
            }
            *self.shared.latest.lock().unwrap() = Some((w, h, Arc::new(owned)));
            Ok(())
        })();

        if let Err(e) = result {
            *self.flags.stats.error.lock().unwrap() = Some(e.to_string());
            self.flags.halt.store(true, Ordering::Relaxed);
            control.stop();
        }
        Ok(())
    }
}

pub struct WgcRecorder {
    control: Option<CaptureControl<RecordHandler, Box<dyn StdError + Send + Sync>>>,
    stop_flag: Arc<AtomicBool>,
    pacer: Option<Pacer>,
    pub stats: Arc<Stats>,
}

impl WgcRecorder {
    pub fn start_monitor(monitor: &Monitor, flags: RecordFlags) -> Result<Self> {
        let item = wgc_monitor(monitor)?;
        Self::start(item, flags)
    }

    pub fn start_window(hwnd: i64, flags: RecordFlags) -> Result<Self> {
        let item = WgcWindow::from_raw_hwnd(hwnd as *mut std::ffi::c_void);
        if !item.is_valid() {
            return Err(anyhow!("invalid window handle {hwnd}"));
        }
        Self::start(item, flags)
    }

    fn start<T>(item: T, flags: RecordFlags) -> Result<Self>
    where
        T: TryInto<windows_capture::settings::GraphicsCaptureItemType> + Send + 'static,
    {
        let stop_flag = flags.halt.clone();
        let stats = flags.stats.clone();

        let shared = Arc::new(Shared::default());
        let mut capture_flags = flags.clone();
        capture_flags.shared = Some(shared.clone());
        let pacer = spawn_pacer(flags, shared);

        // `MinUpdateInterval` is unsupported on Windows 10 builds; the pacer
        // thread keeps a constant frame rate instead.
        let settings = Settings::new(
            item,
            CursorCaptureSettings::Default,
            DrawBorderSettings::Default,
            SecondaryWindowSettings::Default,
            MinimumUpdateIntervalSettings::Default,
            DirtyRegionSettings::Default,
            ColorFormat::Rgba8,
            capture_flags,
        );
        let control = <RecordHandler as GraphicsCaptureApiHandler>::start_free_threaded(settings)
            .map_err(|e| anyhow!("WGC start failed: {e}"))?;
        Ok(Self {
            control: Some(control),
            stop_flag,
            pacer: Some(pacer),
            stats,
        })
    }

    pub fn stop(mut self) -> (u64, u32, u32, Option<String>) {
        // debug removed
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(control) = self.control.take() {
            let _ = control.stop();
        }
        if let Some(p) = self.pacer.take() {
            p.join();
        }
        self.stats.snapshot()
    }
}

impl Drop for WgcRecorder {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(control) = self.control.take() {
            let _ = control.stop();
        }
        if let Some(p) = self.pacer.take() {
            p.join();
        }
    }
}

/// Feeds `ffmpeg` at a constant rate, repeating the newest WGC frame.
struct Pacer {
    ticker: std::thread::JoinHandle<()>,
    writer: std::thread::JoinHandle<()>,
}

impl Pacer {
    fn join(self) {
        let _ = self.ticker.join();
        let _ = self.writer.join();
    }
}

/// Split "when frames should happen" from "pushing bytes into FFmpeg".
///
/// A ticker advances the timeline at the requested frame rate, while the writer
/// feeds FFmpeg until it has caught up with the ticker. Static screens reuse the
/// last frame; when the encoder is slower than realtime the ticker never drags
/// the timeline behind, so the recording keeps real duration.
fn spawn_pacer(flags: RecordFlags, shared: Arc<Shared>) -> Pacer {
    let ticker = {
        let halt = flags.halt.clone();
        let ticks = shared.ticks.clone();
        let fps = flags.fps.max(1);
        let interval = Duration::from_secs_f64(1.0 / fps as f64);
        std::thread::Builder::new()
            .name("rec-ticker".into())
            .spawn(move || {
                let mut next = Instant::now() + interval;
                while !halt.load(Ordering::Relaxed) {
                    let now = Instant::now();
                    if next > now {
                        std::thread::sleep(next - now);
                    } else if now.duration_since(next) > Duration::from_secs(1) {
                        next = now;
                    }
                    next += interval;
                    if halt.load(Ordering::Relaxed) {
                        break;
                    }
                    let t = ticks.fetch_add(1, Ordering::Relaxed) + 1;
                    if t % 30 == 0 {
                        // debug removed
                    }
                }
            })
            .expect("spawn rec-ticker")
    };

    let writer = {
        let halt = flags.halt.clone();
        let ticks = shared.ticks.clone();
        std::thread::Builder::new()
            .name("rec-writer".into())
            .spawn(move || {
                let mut encoder: Option<RawEncoder> = None;
                let mut last: Option<(u32, u32, Arc<Vec<u8>>)> = None;
                let mut written: u64 = 0;
                let mut failed = false;

                let mut it: u64 = 0;
                loop {
                    it += 1;
                    if it % 400 == 0 {
                        // debug removed
                    }
                    let stopping = halt.load(Ordering::Relaxed);
                    let target = ticks.load(Ordering::Relaxed);
                    if let Some(frame) = { shared.latest.lock().unwrap().take() } {
                        last = Some(frame);
                    }
                    if target == written {
                        if stopping {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    let Some((w, h, data)) = last.clone() else {
                        // Nothing captured yet: those ticks are simply lost.
                        written = target;
                        if stopping {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    };

                    if encoder.is_none() {
                        let spec = RawSpec {
                            width: w,
                            height: h,
                            fps: flags.fps.max(1),
                            pix_fmt: "rgba",
                            encoder: flags.encoder.clone(),
                            quality: flags.quality.clone(),
                            crop: flags.crop,
                            scale: flags.scale,
                            output: flags.output.clone(),
                        };
                        match flags.ffmpeg.start_raw(spec) {
                            Ok(e) => {
                                flags.stats.width.store(w, Ordering::Relaxed);
                                flags.stats.height.store(h, Ordering::Relaxed);
                                encoder = Some(e);
                            }
                            Err(e) => {
                                *flags.stats.error.lock().unwrap() = Some(e.to_string());
                                failed = true;
                                break;
                            }
                        }
                    }

                    if let Some(enc) = encoder.as_mut() {
                        let mut ok = true;
                        for _ in 0..(target - written) {
                            if let Err(e) = enc.feed(data.as_slice()) {
                                *flags.stats.error.lock().unwrap() = Some(format!("feed: {e}"));
                                failed = true;
                                ok = false;
                                break;
                            }
                            let f = flags.stats.frames.fetch_add(1, Ordering::Relaxed) + 1;
                            if f % 30 == 0 {
                                // debug removed
                            }
                        }
                        written = target;
                        if !ok {
                            break;
                        }
                    }
                }

                // debug removed
                tracing::debug!(
                    "writer exiting, failed={failed}, frames={}",
                    flags.stats.frames.load(Ordering::Relaxed)
                );
                // debug removed
                match encoder {
                    Some(enc) => {
                        if let Err(e) = enc.finish() {
                            tracing::warn!("writer finish: {e:#}");
                            if !failed {
                                *flags.stats.error.lock().unwrap() = Some(e.to_string());
                            }
                        }
                    }
                    None => {
                        if !failed {
                            let mut err = flags.stats.error.lock().unwrap();
                            if err.is_none() {
                                *err = Some("no frames were captured".to_string());
                            }
                        }
                    }
                }
            })
            .expect("spawn rec-writer")
    };

    Pacer { ticker, writer }
}

fn wgc_monitor(monitor: &Monitor) -> Result<windows_capture::monitor::Monitor> {
    let all = windows_capture::monitor::Monitor::enumerate()
        .map_err(|e| anyhow!("enumerating WGC monitors failed: {e}"))?;
    // `from_index` is 1-based; `position`/our index are 0-based.
    let idx0 = all
        .iter()
        .position(|m| {
            m.device_name()
                .map(|n| n == monitor.device)
                .unwrap_or(false)
        })
        .unwrap_or(monitor.index);
    let idx = idx0 + 1;
    windows_capture::monitor::Monitor::from_index(idx)
        .map_err(|e| anyhow!("WGC monitor {idx0} (1-based {idx}) unavailable: {e}"))
}

// --------------------------------------------------------------- screenshots

struct OneShotFlags {
    tx: mpsc::SyncSender<(u32, u32, Vec<u8>)>,
}

struct OneShotHandler {
    flags: OneShotFlags,
    buf: Vec<u8>,
}

impl GraphicsCaptureApiHandler for OneShotHandler {
    type Flags = OneShotFlags;
    type Error = Box<dyn StdError + Send + Sync>;

    fn new(ctx: CaptureContext<Self::Flags>) -> HandlerResult<Self> {
        Ok(Self { flags: ctx.flags, buf: Vec::new() })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        control: InternalCaptureControl,
    ) -> HandlerResult<()> {
        match frame.buffer() {
            Ok(fb) => {
                let (w, h) = (fb.width(), fb.height());
                let mut data = fb.as_nopadding_buffer(&mut self.buf).to_vec();
                // RGBA -> BGRA for our Bitmap type
                for px in data.chunks_exact_mut(4) {
                    px.swap(0, 2);
                }
                let _ = self.flags.tx.try_send((w, h, data));
            }
            Err(e) => {
                let _ = self.flags.tx.try_send((0, 0, e.to_string().into_bytes()));
            }
        }
        control.stop();
        Ok(())
    }
}

fn one_shot<T>(item: T, timeout: Duration) -> Result<Bitmap>
where
    T: TryInto<windows_capture::settings::GraphicsCaptureItemType> + Send + 'static,
{
    let (tx, rx) = mpsc::sync_channel(1);
    let settings = Settings::new(
        item,
        CursorCaptureSettings::Default,
        DrawBorderSettings::Default,
        SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Default,
        DirtyRegionSettings::Default,
        ColorFormat::Rgba8,
        OneShotFlags { tx },
    );
    let control = <OneShotHandler as GraphicsCaptureApiHandler>::start_free_threaded(settings)
        .map_err(|e| anyhow!("WGC start failed: {e}"))?;
    let msg = rx.recv_timeout(timeout);
    let _ = control.stop();
    match msg {
        Ok((w, h, data)) if w > 0 && h > 0 => {
            Ok(Bitmap { width: w as i32, height: h as i32, data })
        }
        Ok((_, _, err)) => Err(anyhow!("WGC capture failed: {}", String::from_utf8_lossy(&err))),
        Err(_) => Err(anyhow!("WGC capture timed out (window closed or protected content?)")),
    }
}

pub fn grab_window(hwnd: i64) -> Result<Bitmap> {
    let item = WgcWindow::from_raw_hwnd(hwnd as *mut std::ffi::c_void);
    if !item.is_valid() {
        return Err(anyhow!("invalid window handle {hwnd}"));
    }
    one_shot(item, Duration::from_secs(5))
}

pub fn grab_monitor(monitor: &Monitor) -> Result<Bitmap> {
    one_shot(wgc_monitor(monitor)?, Duration::from_secs(5))
}

/// WGC is available on Windows 10 1903+ and only when capture is not blocked by policy.
pub fn available() -> bool {
    windows_capture::monitor::Monitor::enumerate().map(|v| !v.is_empty()).unwrap_or(false)
}

pub fn _unused_context() -> Result<()> {
    Ffmpeg::new("ffmpeg").version().context("ffmpeg").map(|_| ())
}