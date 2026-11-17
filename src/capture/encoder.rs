//! FFmpeg integration: encoder probing, raw-frame recording pipes and image
//! encoding. All screen content is captured in-process (physical pixels) and
//! piped into FFmpeg, which only does encoding/muxing.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::capture::gdi::Bitmap;
use crate::geometry::Rect;

pub const H264_ENCODERS: [&str; 4] = ["h264_qsv", "h264_nvenc", "h264_amf", "libx264"];

/// Throughput (frames/second) per `"<ffmpeg exe>::<encoder>"`, filled by
/// `benchmark_encoders`. Probing costs seconds, so recordings read it as-is.
static SPEED_CACHE: OnceLock<Mutex<std::collections::HashMap<String, f64>>> = OnceLock::new();

#[derive(Clone, Debug)]
pub struct Ffmpeg {
    pub exe: String,
}

impl Ffmpeg {
    pub fn new(exe: impl Into<String>) -> Self {
        Self { exe: exe.into() }
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(&self.exe);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        cmd
    }

    pub fn version(&self) -> Result<String> {
        let out = self
            .command()
            .args(["-hide_banner", "-version"])
            .output()
            .with_context(|| format!("failed to run {} — is FFmpeg installed / on PATH?", self.exe))?;
        if !out.status.success() {
            anyhow::bail!("{} -version failed: {}", self.exe, String::from_utf8_lossy(&out.stderr));
        }
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .unwrap_or_default()
            .to_string())
    }

    /// Real functional probe: an encoder can be compiled in but still unusable
    /// (no GPU / unsupported driver). Results are cached per executable.
    pub fn encoder_works(&self, encoder: &str) -> bool {
        static CACHE: OnceLock<Mutex<std::collections::HashMap<String, bool>>> = OnceLock::new();
        let cache = CACHE.get_or_init(Default::default);
        let key = format!("{}::{}", self.exe, encoder);
        if let Some(v) = cache.lock().unwrap().get(&key) {
            return *v;
        }
        let ok = self
            .command()
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=256x144:rate=30:duration=0.2",
                "-c:v",
                encoder,
                "-f",
                "null",
                "-",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        cache.lock().unwrap().insert(key, ok);
        ok
    }

    /// Resolve `auto` (or an explicit preference) to a working H.264 encoder.
    /// Cached throughput of an encoder, if it has already been probed.
    pub fn cached_speed(&self, encoder: &str) -> Option<f64> {
        let cache = SPEED_CACHE.get_or_init(Default::default);
        let key = format!("{}::{}", self.exe, encoder);
        cache.lock().unwrap().get(&key).copied()
    }

    /// Probe every candidate encoder once and remember the throughput.
    /// Called from `doctor` and from a background thread at startup, never
    /// from a recording start (the probe takes a few seconds).
    pub fn benchmark_encoders(&self) -> Vec<(String, f64)> {
        // libx264 first: it is the reference the hardware encoders are compared against.
        let mut order = vec!["libx264"];
        order.extend(H264_ENCODERS.iter().copied().filter(|e| *e != "libx264"));
        order
            .into_iter()
            .map(|e| (e.to_string(), self.encoder_speed(e)))
            .filter(|(_, speed)| *speed > 0.0)
            .collect()
    }

    /// Rough throughput probe (frames/second) at 1920x1080. Hardware encoders
    /// are not always faster than libx264 — some drivers fall back to software —
    /// so ranking is measured rather than assumed. Cached per (exe, encoder).
    pub fn encoder_speed(&self, encoder: &str) -> f64 {
        if let Some(v) = self.cached_speed(encoder) {
            return v;
        }
        let cache = SPEED_CACHE.get_or_init(Default::default);
        let key = format!("{}::{}", self.exe, encoder);
        let frames = 24u32;
        let start = Instant::now();
        let ok = self
            .command()
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=1920x1080:rate=30",
                "-frames:v",
                &frames.to_string(),
                "-c:v",
                encoder,
                "-f",
                "null",
                "-",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        let secs = start.elapsed().as_secs_f64().max(0.001);
        let speed = if ok { frames as f64 / secs } else { 0.0 };
        tracing::debug!("encoder {encoder}: {speed:.1} fps");
        cache.lock().unwrap().insert(key, speed);
        speed
    }

    pub fn pick_encoder(&self, preference: &str) -> String {
        self.pick_encoder_for(preference).0
    }

    /// Returns the chosen encoder plus its known throughput (0.0 = not probed).
    /// This never probes: a recording must start immediately. Unknown encoders
    /// fall back to libx264 (present in every GPL build); run `benchmark_encoders`
    /// once to let hardware win.
    pub fn pick_encoder_for(&self, preference: &str) -> (String, f64) {
        let pref = preference.trim();
        if !pref.is_empty() && !pref.eq_ignore_ascii_case("auto") {
            if self.encoder_works(pref) {
                return (pref.to_string(), self.cached_speed(pref).unwrap_or(0.0));
            }
            tracing::warn!("encoder {pref} is unavailable, falling back to auto");
        }
        let mut best: Option<(String, f64)> = None;
        for candidate in H264_ENCODERS {
            let Some(speed) = self.cached_speed(candidate).filter(|s| *s > 0.0) else {
                continue;
            };
            if best.as_ref().map(|(_, s)| speed > *s).unwrap_or(true) {
                best = Some((candidate.to_string(), speed));
            }
        }
        match best {
            // Only take a hardware encoder when it clearly beats the software one.
            Some((enc, speed)) if enc != "libx264" => {
                match self.cached_speed("libx264").filter(|s| *s > 0.0) {
                    Some(sw) if speed <= sw * 1.2 => ("libx264".to_string(), sw),
                    Some(_) => (enc, speed),
                    // libx264 has not been measured yet (a recording started while the warm-up
                    // benchmark was still running): trust the software encoder instead of a
                    // lone hardware number, which can look good on synthetic input only.
                    None => ("libx264".to_string(), 0.0),
                }
            }
            Some(found) => found,
            // Nothing probed yet: libx264 is always in GPL builds; if it is
            // missing after all, the caller surfaces the FFmpeg stderr.
            None => ("libx264".to_string(), 0.0),
        }
    }

    /// Encode a captured bitmap to `path` (png / jpg / webp).
    /// Returns the written pixel size.
    pub fn write_image(
        &self,
        bmp: &Bitmap,
        path: &Path,
        format: &str,
        max_edge: u32,
        quality: u8,
    ) -> Result<(u32, u32)> {
        let mut w = bmp.width.max(1) as u32;
        let mut h = bmp.height.max(1) as u32;
        if max_edge > 0 && w.max(h) > max_edge {
            let f = max_edge as f64 / w.max(h) as f64;
            w = ((w as f64 * f).round() as u32).max(1);
            h = ((h as f64 * f).round() as u32).max(1);
        }
        let format = format.to_ascii_lowercase();
        let mut filters: Vec<String> = Vec::new();
        if (w, h) != (bmp.width as u32, bmp.height as u32) {
            // even dimensions keep the swscale conversions happy
            let ew = w - w % 2;
            let eh = h - h % 2;
            filters.push(format!("scale={}:{}:flags=lanczos", ew.max(2), eh.max(2)));
            w = ew.max(2);
            h = eh.max(2);
        }
        let (ext_args, pix): (Vec<String>, &str) = match format.as_str() {
            "jpg" | "jpeg" => (
                vec!["-q:v".into(), (2 + (100 - quality.min(100)) / 20).to_string()],
                "yuvj420p",
            ),
            "webp" => (vec!["-quality".into(), quality.to_string()], "bgra"),
            _ => (vec!["-compression_level".into(), "4".into()], "rgba"),
        };
        filters.push(format!("format={pix}"));

        let mut cmd = self.command();
        cmd.args(["-hide_banner", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", "bgra"])
            .args(["-s", &format!("{}x{}", bmp.width, bmp.height)])
            .args(["-i", "pipe:0", "-frames:v", "1", "-vf", &filters.join(",")])
            .args(&ext_args)
            .args(["-y"])
            .arg(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().with_context(|| format!("failed to spawn {}", self.exe))?;
        {
            let stdin = child.stdin.as_mut().expect("stdin piped");
            stdin.write_all(&bmp.data)?;
            stdin.flush()?;
        }
        drop(child.stdin.take());
        let out = child.wait_with_output()?;
        if !out.status.success() {
            anyhow::bail!(
                "ffmpeg image encode failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok((w, h))
    }

    /// Spawn a raw-frame H.264 encoder writing to `spec.output`.
    pub fn start_raw(&self, spec: RawSpec) -> Result<RawEncoder> {
        let mut filters: Vec<String> = Vec::new();
        if let Some(crop) = spec.crop {
            let c = crop.even();
            filters.push(format!("crop={}:{}:{}:{}", c.width, c.height, c.x, c.y));
        }
        if let Some((w, h)) = spec.scale {
            filters.push(format!("scale={}:{}:flags=bicubic", w - w % 2, h - h % 2));
        } else {
            // yuv420p needs an even frame size, and a window or a hand-dragged
            // region can easily be odd (401x301). Cropping one pixel line beats
            // libx264 refusing the whole stream with "width not divisible by 2".
            let (out_w, out_h) = match spec.crop {
                Some(c) => {
                    let c = c.even();
                    (c.width as u32, c.height as u32)
                }
                None => (spec.width, spec.height),
            };
            if out_w % 2 != 0 || out_h % 2 != 0 {
                filters.push("crop=trunc(iw/2)*2:trunc(ih/2)*2:0:0".to_string());
            }
        }
        filters.push("format=yuv420p".to_string());

        let mut cmd = self.command();
        cmd.args(["-hide_banner", "-loglevel", "error"])
            .args(["-f", "rawvideo", "-pix_fmt", spec.pix_fmt])
            .args(["-s", &format!("{}x{}", spec.width, spec.height)])
            .args(["-r", &spec.fps.to_string()])
            .args(["-i", "pipe:0", "-vf", &filters.join(",")])
            .args(["-c:v", &spec.encoder])
            .args(quality_args(&spec.encoder, &spec.quality, spec.fps))
            .args(["-r", &spec.fps.to_string()])
            .args(["-movflags", "+faststart", "-y"])
            .arg(&spec.output)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());

        tracing::debug!("spawn: {} {:?}", self.exe, cmd.get_args().collect::<Vec<_>>());
        let mut child = cmd
            .spawn()
            .with_context(|| format!("failed to spawn {} ({} args)", self.exe, "rawvideo"))?;
        let stdin = child.stdin.take().expect("stdin piped");
        let stderr = Arc::new(Mutex::new(Vec::<u8>::new()));
        if let Some(mut pipe) = child.stderr.take() {
            let buf = stderr.clone();
            std::thread::Builder::new().name("ffmpeg-stderr".into()).spawn(move || {
                let mut chunk = [0u8; 4096];
                loop {
                    match pipe.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let mut b = buf.lock().unwrap();
                            b.extend_from_slice(&chunk[..n]);
                            if b.len() > 16 * 1024 {
                                let cut = b.len() - 16 * 1024;
                                b.drain(..cut);
                            }
                        }
                    }
                }
            })?;
        }
        Ok(RawEncoder {
            child: Some(child),
            stdin: Some(stdin),
            stderr,
            frames: 0,
            bytes: 0,
            output: spec.output,
            finished: false,
        })
    }
}

pub struct RawSpec {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub pix_fmt: &'static str,
    pub encoder: String,
    pub quality: String,
    pub crop: Option<Rect>,
    pub scale: Option<(u32, u32)>,
    pub output: PathBuf,
}

pub struct RawEncoder {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    stderr: Arc<Mutex<Vec<u8>>>,
    frames: u64,
    bytes: u64,
    output: PathBuf,
    finished: bool,
}

impl RawEncoder {
    pub fn feed(&mut self, frame: &[u8]) -> Result<()> {
        let Some(stdin) = self.stdin.as_mut() else {
            anyhow::bail!("encoder stdin already closed");
        };
        stdin
            .write_all(frame)
            .map_err(|e| anyhow::anyhow!("writing frame to ffmpeg: {e} {}", self.stderr_tail()))?;
        self.frames += 1;
        self.bytes += frame.len() as u64;
        Ok(())
    }

    pub fn stderr_tail(&self) -> String {
        let b = self.stderr.lock().unwrap();
        String::from_utf8_lossy(&b).trim().to_string()
    }

    /// Close stdin so FFmpeg finalizes the container, then reap it.
    pub fn finish(mut self) -> Result<PathBuf> {
        self.finished = true;
        drop(self.stdin.take());
        let Some(mut child) = self.child.take() else {
            return Ok(self.output.clone());
        };
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            match child.try_wait()? {
                Some(status) => {
                    let err = self.stderr_tail();
                    if self.frames == 0 {
                        // A zero-byte mp4 cannot be played at all; drop it so the
                        // user is left with an error instead of a broken file.
                        let _ = std::fs::remove_file(&self.output);
                        anyhow::bail!(
                            "nothing was recorded (the encoder got no frames): {err}"
                        );
                    }
                    tracing::info!(
                        "ffmpeg finished {status} after {} frames: {err}",
                        self.frames
                    );
                    break;
                }
                None if Instant::now() > deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    tracing::warn!("ffmpeg did not exit in time, killed");
                    break;
                }
                None => std::thread::sleep(Duration::from_millis(30)),
            }
        }
        Ok(self.output.clone())
    }
}

impl Drop for RawEncoder {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        drop(self.stdin.take());
        drop(self.child.take());
        if self.frames == 0 {
            let _ = std::fs::remove_file(&self.output);
        }
    }
}

pub fn quality_args(encoder: &str, quality: &str, fps: u32) -> Vec<String> {
    let q = quality.to_ascii_lowercase();
    match encoder {
        "libx264" | "libx265" => {
            let (preset, crf) = match q.as_str() {
                "lossless" => ("ultrafast", "0"),
                "high" => ("medium", "18"),
                "fast" => ("ultrafast", "28"),
                _ => ("veryfast", "23"),
            };
            vec![
                "-preset".into(), preset.into(),
                "-crf".into(), crf.into(),
                "-tune".into(), "zerolatency".into(),
                "-g".into(), (fps * 2).to_string(),
            ]
        }
        "h264_qsv" => {
            let preset = match q.as_str() {
                "lossless" => "veryslow",
                "high" => "medium",
                "fast" => "veryfast",
                _ => "faster",
            };
            let gq = match q.as_str() {
                "lossless" => "1",
                "high" => "20",
                "fast" => "30",
                _ => "25",
            };
            vec![
                "-preset".into(), preset.into(),
                "-global_quality".into(), gq.into(),
                "-look_ahead".into(), "0".into(),
                "-g".into(), (fps * 2).to_string(),
            ]
        }
        "h264_amf" => {
            let amf_quality = match q.as_str() {
                "lossless" | "high" => "quality",
                "fast" => "speed",
                _ => "balanced",
            };
            let br = match amf_quality {
                "quality" => "12000k",
                "speed" => "4000k",
                _ => "8000k",
            };
            vec!["-quality".into(), amf_quality.into(), "-b:v".into(), br.into()]
        }
        "h264_nvenc" => {
            let (preset, cq) = match q.as_str() {
                "lossless" => ("p7", "1"),
                "high" => ("p6", "20"),
                "fast" => ("p2", "30"),
                _ => ("p4", "25"),
            };
            vec!["-preset".into(), preset.into(), "-rc".into(), "vbr".into(), "-cq".into(), cq.into()]
        }
        _ => vec!["-b:v".into(), "6000k".into()],
    }
}

pub static FFMPEG_MISSING_HINT: &str =
    "FFmpeg is required. Install it (`winget install Gyan.FFmpeg`, `scoop install ffmpeg`), or \
     point the `ffmpeg` setting at ffmpeg.exe: `rec-r config --set ffmpeg=C:\\Tools\\ffmpeg\\bin\\ffmpeg.exe`";

