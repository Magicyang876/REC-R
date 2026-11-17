//! MCP server exposing screenshot/recording capabilities to agents.

use std::time::Duration;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig,
};
use rmcp::schemars;
use rmcp::schemars::JsonSchema;
use rmcp::{tool, tool_handler, tool_router, ServerHandler};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::actions::{self, RecordRequest, ShotRequest};
use crate::app::App;
use crate::capture::target::{TargetKind, TargetSpec};
use crate::recording::Engine;

const INSTRUCTIONS: &str = r#"rec-r captures screenshots and records video of Windows displays, windows and screen regions.

Typical flow:
1. `list_displays` to find a monitor index, and/or `list_windows` to find a window (by title, process or hwnd).
2. `screenshot` with `target` = display/window/region. By default the image is returned inline as an
   image content block so you can look at it; pass `inline: false` when you only need the file path.
3. `record_start` to begin a video recording, `record_status` to poll progress, `record_stop` to finish.
   Always stop a recording when you are done - a forgotten recording keeps encoding frames.

Targets
- display: a whole monitor (`monitor` index) or the primary monitor by default.
- window:  one window by hwnd (`"131234"`), title/exe substring (`"chrome"`), or `"cursor"` for the
  window currently under the mouse pointer. Window capture prefers Windows Graphics Capture, so the
  window may be occluded or partially off-screen.
- region: `"x,y,width,height"` in virtual-desktop physical pixels, or explicit x/y/width/height.
  Coordinates are physical pixels; use `list_displays` / `screenshot` results to reason about layout.
- desktop: every monitor as one image (`target: "desktop"`).

Notes
- Screenshots and videos are written under the output dir (see `doctor`), and the absolute path is
  returned in every result.
- `select_region` lets the human drag a rectangle on screen and returns its coordinates. Pass
  `region: "select"` to `screenshot`/`record_start` to select and capture in one call; selection works
  with or without the tray app, but uses the app's overlay when `rec-r app` is running.
- Use `doctor` first if something fails: it reports FFmpeg availability, usable H.264 encoders and
  whether Windows Graphics Capture is supported."#;

#[derive(Clone)]
pub struct RecServer {
    // Only read by the generated `#[tool_handler]` glue.
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
    app: std::sync::Arc<App>,
}

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(default, rename_all = "snake_case")]
pub struct ScreenshotParams {
    /// "display" (default) | "window" | "region" | "desktop"
    pub target: Option<String>,
    /// Monitor index from `list_displays`.
    pub monitor: Option<usize>,
    /// Window selector: hwnd, title/process substring, or "cursor".
    pub window: Option<String>,
    /// "x,y,width,height" in physical pixels, or "select" to let the human drag a rectangle.
    pub region: Option<String>,
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    /// Output file path (absolute, or relative to the output dir).
    pub path: Option<String>,
    /// png (default) | jpg | webp
    pub format: Option<String>,
    /// Downscale so the longest edge is at most this many pixels (0 = full size).
    pub max_edge: Option<u32>,
    /// lossless | high | balanced | fast
    pub quality: Option<String>,
    /// Return the image bytes inline as an image content block (default true).
    pub inline: Option<bool>,
    /// Wait this many seconds before capturing (useful to let a menu open).
    pub delay_sec: Option<f32>,
}

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(default, rename_all = "snake_case")]
pub struct RecordParams {
    /// "display" (default) | "window" | "region" | "desktop"
    pub target: Option<String>,
    pub monitor: Option<usize>,
    pub window: Option<String>,
    /// "x,y,width,height" in physical pixels, or "select" to let the human drag a rectangle first.
    pub region: Option<String>,
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    /// Output .mp4 path (absolute, or relative to the output dir).
    pub path: Option<String>,
    /// Frames per second (default 30).
    pub fps: Option<u32>,
    /// auto | h264_qsv | h264_nvenc | h264_amf | libx264
    pub encoder: Option<String>,
    /// lossless | high | balanced | fast
    pub quality: Option<String>,
    /// auto | wgc | gdi
    pub engine: Option<String>,
    /// Safety limit; the recording stops itself after this many seconds.
    pub max_duration_sec: Option<u64>,
    /// Output size like "1280x720" or "1280x-2" (keep aspect ratio).
    pub scale: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(default, rename_all = "snake_case")]
pub struct StopParams {
    /// Recording id; omit to stop every active recording.
    pub id: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(default, rename_all = "snake_case")]
pub struct StatusParams {
    /// Recording id; omit to list every active recording.
    pub id: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(default, rename_all = "snake_case")]
pub struct ListWindowsParams {
    /// Filter by title, class or process substring.
    pub query: Option<String>,
    /// Max rows (default 40, 0 = all).
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(default, rename_all = "snake_case")]
pub struct ConfigParams {
    /// Config key to change (`fps`, `quality`, `hotkeys.shot`, `autostart`, ...).
    pub key: Option<String>,
    /// New value for `key`.
    pub value: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(default, rename_all = "snake_case")]
pub struct SelectRegionParams {
    /// Give up after this many seconds (default 120).
    pub timeout_sec: Option<u64>,
}

fn parse_target(value: Option<&str>) -> Option<TargetKind> {
    match value?.trim().to_ascii_lowercase().as_str() {
        "" => None,
        "display" | "monitor" | "screen" => Some(TargetKind::Display),
        "window" | "win" => Some(TargetKind::Window),
        "region" | "rect" | "area" => Some(TargetKind::Region),
        "desktop" | "all" | "virtual" | "virtual_desktop" => Some(TargetKind::VirtualDesktop),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn spec_from(
    target: Option<&str>,
    monitor: Option<usize>,
    window: Option<&str>,
    region: Option<&str>,
    x: Option<i32>,
    y: Option<i32>,
    w: Option<i32>,
    h: Option<i32>,
) -> TargetSpec {
    let xywh = match (x, y, w, h) {
        (Some(x), Some(y), Some(w), Some(h)) => Some((x, y, w, h)),
        _ => None,
    };
    let target = parse_target(target).or_else(|| {
        if window.is_some() {
            Some(TargetKind::Window)
        } else if region.is_some() || xywh.is_some() {
            Some(TargetKind::Region)
        } else {
            None
        }
    });
    actions::target_from_parts(
        target,
        monitor,
        window.map(str::to_string),
        region.map(str::to_string),
        xywh,
    )
}

fn parse_scale(text: &str) -> Result<(u32, u32), String> {
    let (w, h) = text
        .split_once(['x', 'X', '*', ':'])
        .ok_or_else(|| format!("scale expects WxH, got {text:?}"))?;
    let num = |s: &str| -> Result<u32, String> {
        let s = s.trim();
        if s == "-1" || s == "-2" {
            return Ok(2);
        }
        s.parse::<u32>().map_err(|e| format!("bad scale component {s:?}: {e}"))
    };
    Ok((num(w)?, num(h)?))
}

fn structured(value: Value) -> CallToolResult {
    CallToolResult::structured(value)
}

fn failed(context: &str, e: impl std::fmt::Display) -> CallToolResult {
    CallToolResult::structured_error(json!({ "error": format!("{context}: {e}") }))
}

impl RecServer {
    pub fn new(app: std::sync::Arc<App>) -> Self {
        Self { tool_router: Self::tool_router(), app }
    }

    fn app(&self) -> std::sync::Arc<App> {
        self.app.clone()
    }

    /// `region: "select"` asks the human to drag a rectangle first, so an agent can
    /// record "whatever the user picks right now" in a single call.
    async fn pick_region<'a>(
        &self,
        region: Option<&'a str>,
    ) -> Result<(Option<&'a str>, Option<i32>, Option<i32>, Option<i32>, Option<i32>), CallToolResult>
    {
        match region {
            Some(spec) if is_select(spec) => match self.app().select_region(120).await {
                Ok(Some(r)) => Ok((None, Some(r.x), Some(r.y), Some(r.width), Some(r.height))),
                Ok(None) => Err(CallToolResult::error(vec![ContentBlock::text(
                    "region selection cancelled - the overlay was dismissed or timed out",
                )])),
                Err(e) => Err(failed("region selection failed", e)),
            },
            _ => Ok((region, None, None, None, None)),
        }
    }
}

/// `region: "select"` / `"pick"` / `"interactive"` means "let the human choose".
fn is_select(value: &str) -> bool {
    matches!(value.trim().to_ascii_lowercase().as_str(), "select" | "pick" | "interactive")
}

#[tool_router]
impl RecServer {
    #[tool(
        name = "list_displays",
        description = "List monitors with their index, device name, physical bounds (x, y, width, height) and DPI scale, plus the virtual desktop bounds."
    )]
    async fn list_displays(&self) -> CallToolResult {
        structured(serde_json::to_value(actions::monitor_summary()).unwrap_or(Value::Null))
    }

    #[tool(
        name = "list_windows",
        description = "List capturable top-level windows: hwnd, title, class, process, pid and physical bounds. Use `query` to filter by title/class/process substring. Pass the hwnd or a title substring as the `window` selector of screenshot/record_start."
    )]
    async fn list_windows(&self, Parameters(p): Parameters<ListWindowsParams>) -> CallToolResult {
        let limit = p.limit.unwrap_or(40);
        let rows = actions::list_windows(p.query.as_deref(), limit);
        structured(json!({ "count": rows.len(), "windows": rows }))
    }

    #[tool(
        name = "screenshot",
        description = "Capture a screenshot of a display, a window, a region or the whole desktop. Returns the saved file path and, unless inline=false, the image itself so you can inspect it."
    )]
    async fn screenshot(&self, Parameters(p): Parameters<ScreenshotParams>) -> CallToolResult {
        let (target, x, y, w, h) = match self.pick_region(p.region.as_deref()).await {
            Ok(v) => v,
            Err(e) => return e,
        };
        let target = spec_from(
            p.target.as_deref().filter(|t| !is_select(*t)),
            p.monitor,
            p.window.as_deref(),
            target,
            x.or(p.x),
            y.or(p.y),
            w.or(p.width),
            h.or(p.height),
        );
        let inline = p.inline.unwrap_or(true);
        let req = ShotRequest {
            target,
            path: p.path,
            format: p.format,
            max_edge: p.max_edge,
            quality: p.quality,
            inline,
            copy: None,
        };
        let delay = p.delay_sec.unwrap_or(0.0).clamp(0.0, 60.0);
        let app = self.app();
        let job = tokio::task::spawn_blocking(move || {
            if delay > 0.0 {
                std::thread::sleep(Duration::from_secs_f32(delay));
            }
            actions::take_screenshot(&app, req)
        });
        match job.await {
            Ok(Ok(res)) => {
                let mime = match res.format.as_str() {
                    "jpg" | "jpeg" => "image/jpeg",
                    "webp" => "image/webp",
                    _ => "image/png",
                };
                let mut payload = serde_json::to_value(&res).unwrap_or(Value::Null);
                if let Some(obj) = payload.as_object_mut() {
                    obj.remove("base64");
                }
                let mut result = CallToolResult::structured(payload);
                if let Some(b64) = res.base64 {
                    result.content.push(ContentBlock::image(b64, mime.to_string()));
                }
                result
            }
            Ok(Err(e)) => failed("screenshot failed", e),
            Err(e) => failed("screenshot task panicked", e),
        }
    }

    #[tool(
        name = "record_start",
        description = "Start recording video (H.264 MP4) of a display, window, region or the whole desktop. Returns a recording id; finish with record_stop."
    )]
    async fn record_start(&self, Parameters(p): Parameters<RecordParams>) -> CallToolResult {
        let (target, x, y, w, h) = match self.pick_region(p.region.as_deref()).await {
            Ok(v) => v,
            Err(e) => return e,
        };
        let target = spec_from(
            p.target.as_deref().filter(|t| !is_select(*t)),
            p.monitor,
            p.window.as_deref(),
            target,
            x.or(p.x),
            y.or(p.y),
            w.or(p.width),
            h.or(p.height),
        );
        let engine = match p.engine.as_deref() {
            None | Some("") | Some("auto") => None,
            Some(other) => match other.parse::<Engine>() {
                Ok(e) => Some(e),
                Err(e) => return failed("bad engine", e),
            },
        };
        let scale = match p.scale.as_deref() {
            None | Some("") => None,
            Some(text) => match parse_scale(text) {
                Ok(s) => Some(s),
                Err(e) => return failed("bad scale", e),
            },
        };
        let req = RecordRequest {
            target,
            path: p.path,
            fps: p.fps,
            encoder: p.encoder,
            quality: p.quality,
            engine,
            max_duration_sec: p.max_duration_sec,
            scale,
        };
        let app = self.app();
        match tokio::task::spawn_blocking(move || actions::start_recording(&app, req)).await {
            Ok(Ok(info)) => structured(serde_json::to_value(info).unwrap_or(Value::Null)),
            Ok(Err(e)) => failed("record_start failed", e),
            Err(e) => failed("record_start task panicked", e),
        }
    }

    #[tool(
        name = "record_stop",
        description = "Stop one recording by id (or every active recording when id is omitted) and finalize the MP4. Returns duration, frame count, size and path per recording."
    )]
    async fn record_stop(&self, Parameters(p): Parameters<StopParams>) -> CallToolResult {
        let app = self.app();
        match tokio::task::spawn_blocking(move || {
            actions::stop_recording(&app, p.id.as_deref())
        })
        .await
        {
            Ok(Ok(list)) => structured(json!({ "stopped": list, "count": list.len() })),
            Ok(Err(e)) => failed("record_stop failed", e),
            Err(e) => failed("record_stop task panicked", e),
        }
    }

    #[tool(
        name = "record_status",
        description = "Report active recordings: id, target, elapsed seconds, frame count, current file size and health."
    )]
    async fn record_status(&self, Parameters(p): Parameters<StatusParams>) -> CallToolResult {
        let list = actions::recording_status(&self.app, p.id.as_deref());
        let active = list.iter().filter(|r| r.status == "recording").count();
        structured(json!({ "count": list.len(), "active": active, "recordings": list }))
    }

    #[tool(
        name = "select_region",
        description = "Ask the human to drag a rectangle on screen; returns {x, y, width, height} in physical pixels (or cancelled=true). Requires the tray app to be running."
    )]
    async fn select_region(&self, Parameters(p): Parameters<SelectRegionParams>) -> CallToolResult {
        let timeout = p.timeout_sec.unwrap_or(120);
        match self.app.select_region(timeout).await {
            Ok(Some(r)) => structured(json!({
                "cancelled": false,
                "x": r.x, "y": r.y, "width": r.width, "height": r.height,
                "region": r.to_string(),
            })),
            Ok(None) => structured(json!({ "cancelled": true })),
            Err(e) => failed("select_region failed", e),
        }
    }

    #[tool(
        name = "config",
        description = "Read or change settings (config file + Run registry). Without `key` it returns the whole config plus autostart state. With `key` and `value` it persists one change, e.g. key=\"fps\" value=\"60\", key=\"hotkeys.shot\" value=\"Ctrl+Shift+5\", key=\"autostart\" value=\"on\", key=\"ffmpeg\" value=\"C:\\\\tools\\\\ffmpeg\\\\bin\\\\ffmpeg.exe\". Hotkey changes take effect after the tray app restarts."
    )]
    async fn config(&self, Parameters(p): Parameters<ConfigParams>) -> CallToolResult {
        if let Some(key) = p.key.as_deref() {
            let Some(value) = p.value.as_deref() else {
                return CallToolResult::error(vec![ContentBlock::text(
                    "`value` is required when `key` is given".to_string(),
                )]);
            };
            if let Err(e) = self.app.set_config(key, value) {
                return failed(&format!("cannot set {key}"), e);
            }
        }
        let cfg = self.app.config();
        structured(json!({
            "config": cfg,
            "config_path": crate::config::config_path().to_string_lossy(),
            "autostart": crate::autostart::summary(),
            "settable_keys": crate::config::SETTABLE_KEYS,
            "hotkeys_active": "hotkey changes apply after `rec-r app` restarts",
        }))
    }

    #[tool(
        name = "doctor",
        description = "Diagnose the environment: FFmpeg availability and version, usable H.264 encoders, chosen encoder, Windows Graphics Capture support, DPI awareness, monitors, output dir and active recordings."
    )]
    async fn doctor(&self) -> CallToolResult {
        structured(serde_json::to_value(actions::doctor(&self.app)).unwrap_or(Value::Null))
    }

    #[tool(
        name = "open_output",
        description = "Open the output directory (where screenshots and recordings are stored) in Explorer."
    )]
    async fn open_output(&self) -> CallToolResult {
        let dir = match self.app.config().ensure_output_dir() {
            Ok(d) => d,
            Err(e) => return failed("cannot create output dir", e),
        };
        match actions::open_path(&dir) {
            Ok(()) => structured(json!({ "opened": dir.to_string_lossy() })),
            Err(e) => failed("cannot open output dir", e),
        }
    }
}

#[tool_handler]
impl ServerHandler for RecServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                Implementation::new("rec-r", env!("CARGO_PKG_VERSION"))
                    .with_title("Rec-R screen capture")
                    .with_description(
                        "Screenshot and screen recording for Windows displays, windows and regions",
                    ),
            )
            .with_instructions(INSTRUCTIONS)
    }
}