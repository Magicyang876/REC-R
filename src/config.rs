//! Config file + runtime directories.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// FFmpeg executable (path or name on PATH).
    pub ffmpeg: String,
    /// Where screenshots and recordings are written.
    pub output_dir: PathBuf,
    /// Default recording frame rate.
    pub fps: u32,
    /// `auto` | `h264_qsv` | `h264_amf` | `h264_nvenc` | `libx264`
    pub encoder: String,
    /// `balanced` | `high` | `fast` | `lossless`
    pub quality: String,
    /// Screenshot format: png | jpg | webp
    pub screenshot_format: String,
    /// Downscale screenshots wider than this before returning them (0 = never).
    pub screenshot_max_edge: u32,
    /// Streamable HTTP bind address for the MCP server.
    pub http_addr: String,
    /// Max recording duration safety valve (seconds, 0 = unlimited).
    pub max_duration_sec: u64,
    /// Start the tray UI when `rec-r` is launched without arguments.
    pub tray_on_launch: bool,
    /// Global hotkeys; an empty string disables that one.
    pub hotkeys: Hotkeys,
    /// Keep a `Run` registry entry so the tray app starts at login.
    pub autostart: bool,
    /// Put every screenshot on the clipboard as well as on disk.
    pub copy_on_shot: bool,
}

/// Global hotkey specs (`Ctrl+Alt+S`, `Alt+F9`, empty = disabled).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Hotkeys {
    /// Screenshot of the primary display.
    pub shot: String,
    /// Start/stop recording the primary display.
    pub record: String,
    /// Stop whatever is recording (no region, no overlay).
    pub stop: String,
    /// Start/stop recording the window under the cursor.
    pub record_window: String,
    /// Screenshot of a dragged region.
    pub shot_region: String,
    /// Start/stop recording a dragged region.
    pub record_region: String,
}

impl Default for Hotkeys {
    fn default() -> Self {
        Self {
            shot: "Ctrl+Alt+S".to_string(),
            record: "Ctrl+Alt+R".to_string(),
            stop: "Ctrl+Alt+X".to_string(),
            // Plain Ctrl+Alt+W is claimed by a lot of software, so the default
            // already includes Shift.
            record_window: "Ctrl+Alt+Shift+W".to_string(),
            shot_region: "Ctrl+Alt+A".to_string(),
            record_region: "Ctrl+Alt+Shift+A".to_string(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            ffmpeg: "ffmpeg".to_string(),
            output_dir: default_output_dir(),
            fps: 30,
            encoder: "auto".to_string(),
            quality: "balanced".to_string(),
            screenshot_format: "png".to_string(),
            screenshot_max_edge: 0,
            http_addr: "127.0.0.1:7391".to_string(),
            max_duration_sec: 3600,
            tray_on_launch: true,
            hotkeys: Hotkeys::default(),
            autostart: false,
            copy_on_shot: true,
        }
    }
}

pub fn default_output_dir() -> PathBuf {
    dirs::video_dir()
        .or_else(dirs::picture_dir)
        .or_else(|| dirs::home_dir().map(|h| h.join("Videos")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Rec-R")
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Rec-R")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

impl Config {
    pub fn load() -> Self {
        let path = config_path();
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                tracing::warn!("config {} is invalid ({e}); using defaults", path.display());
                Config::default()
            }),
            Err(_) => Config::default(),
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn ensure_output_dir(&self) -> Result<PathBuf> {
        std::fs::create_dir_all(&self.output_dir)?;
        Ok(self.output_dir.clone())
    }

    /// Clamp/validate a user-supplied output path.
    pub fn resolve_output(&self, requested: Option<&str>, default_name: &str) -> Result<PathBuf> {        let base = self.ensure_output_dir()?;
        let path = match requested {
            Some(p) if !p.trim().is_empty() => {
                let p = Path::new(p);
                if p.is_absolute() {
                    p.to_path_buf()
                } else {
                    base.join(p)
                }
            }
            _ => base.join(default_name),
        };
        if path.is_dir() {
            anyhow::bail!("output path {} is a directory", path.display());
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(path)
    }
}
/// Keys accepted by `rec-r config --set key=value` and the MCP `config` tool.
pub const SETTABLE_KEYS: &[&str] = &[
    "ffmpeg",
    "output_dir",
    "fps",
    "encoder",
    "quality",
    "screenshot_format",
    "screenshot_max_edge",
    "http_addr",
    "max_duration_sec",
    "tray_on_launch",
    "autostart",
    "copy_on_shot",
    "hotkeys.shot",
    "hotkeys.record",
    "hotkeys.stop",
    "hotkeys.record_window",
    "hotkeys.shot_region",
    "hotkeys.record_region",
];

fn parse_bool(value: &str) -> Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "on" | "yes" | "y" | "enable" | "enabled" => Ok(true),
        "0" | "false" | "off" | "no" | "n" | "disable" | "disabled" => Ok(false),
        other => bail!("{other:?} is not a boolean (use on/off)"),
    }
}

impl Config {
    /// Apply one `key=value` override, validating it the same way the CLI and
    /// MCP do. Hotkey specs are canonicalised (`ctrl+alt+s` -> `Ctrl+Alt+S`).
    pub fn apply_setting(&mut self, key: &str, value: &str) -> Result<()> {
        let key = key.trim();
        let normalised = key.to_ascii_lowercase().replace('-', "_");
        // `hotkey.shot`, `shot_hotkey` and `hotkeys.shot` all mean the same.
        let normalised = match normalised.as_str() {
            "hotkey.shot" | "shot_hotkey" => "hotkeys.shot".to_string(),
            "hotkey.record" | "record_hotkey" => "hotkeys.record".to_string(),
            "hotkey.stop" | "stop_hotkey" | "hotkey.record_stop" => "hotkeys.stop".to_string(),
            "hotkey.record_window" | "record_window_hotkey" | "hotkey.window" => {
                "hotkeys.record_window".to_string()
            }
            "hotkey.shot_region" | "shot_region_hotkey" | "hotkey.region" => {
                "hotkeys.shot_region".to_string()
            }
            "hotkey.record_region" | "record_region_hotkey" | "hotkey.region_record" => {
                "hotkeys.record_region".to_string()
            }
            other => other.to_string(),
        };
        let value = value.trim();
        match normalised.as_str() {
            "ffmpeg" => self.ffmpeg = value.to_string(),
            "output_dir" => self.output_dir = PathBuf::from(value),
            "fps" => {
                let fps: u32 = value.parse().context("fps must be a number")?;
                if !(1..=240).contains(&fps) {
                    bail!("fps must be between 1 and 240");
                }
                self.fps = fps;
            }
            "encoder" => {
                let enc = value.to_ascii_lowercase();
                let allowed = crate::capture::encoder::H264_ENCODERS;
                if enc != "auto" && !allowed.iter().any(|e| *e == enc) {
                    bail!("encoder must be auto or one of {allowed:?}");
                }
                self.encoder = enc;
            }
            "quality" => {
                crate::actions::check_quality(value)?;
                self.quality = value.to_ascii_lowercase();
            }
            "screenshot_format" => {
                let fmt = value.to_ascii_lowercase();
                if !["png", "jpg", "jpeg", "webp"].contains(&fmt.as_str()) {
                    bail!("screenshot_format must be png, jpg or webp");
                }
                self.screenshot_format = if fmt == "jpeg" { "jpg".into() } else { fmt };
            }
            "screenshot_max_edge" => {
                self.screenshot_max_edge = value.parse().context("screenshot_max_edge must be a number")?
            }
            "http_addr" => {
                crate::mcp::parse_addr(value)?;
                self.http_addr = value.to_string();
            }
            "max_duration_sec" => {
                self.max_duration_sec = value.parse().context("max_duration_sec must be a number")?
            }
            "tray_on_launch" => self.tray_on_launch = parse_bool(value)?,
            "autostart" => self.autostart = parse_bool(value)?,
            "copy_on_shot" => self.copy_on_shot = parse_bool(value)?,
            "hotkeys.shot"
            | "hotkeys.record"
            | "hotkeys.stop"
            | "hotkeys.record_window"
            | "hotkeys.shot_region"
            | "hotkeys.record_region" => {
                let parsed = crate::hotkeys::parse(value)?;
                let canonical = match parsed {
                    Some(h) => h.label(),
                    None => String::new(),
                };
                match normalised.as_str() {
                    "hotkeys.shot" => self.hotkeys.shot = canonical,
                    "hotkeys.record" => self.hotkeys.record = canonical,
                    "hotkeys.stop" => self.hotkeys.stop = canonical,
                    "hotkeys.record_window" => self.hotkeys.record_window = canonical,
                    "hotkeys.shot_region" => self.hotkeys.shot_region = canonical,
                    _ => self.hotkeys.record_region = canonical,
                }
            }
            other => bail!(
                "unknown config key {other:?}; known keys: {}",
                SETTABLE_KEYS.join(", ")
            ),
        }
        Ok(())
    }
}
