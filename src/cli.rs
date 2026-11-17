//! Command line interface.

use std::time::Duration;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

use crate::actions;
use crate::app::App;
use crate::capture::encoder::H264_ENCODERS;
use crate::capture::target::{TargetKind, TargetSpec};
use crate::recording::Engine;

#[derive(Debug, Parser)]
#[command(
    name = "rec-r",
    version,
    about = "Screenshot & screen recording for Windows with an MCP server for agents",
    long_about = None,
    disable_help_subcommand = false
)]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Option<Cmd>,
}

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Start the tray app (the default when no subcommand is given).
    App(AppArgs),
    /// Run the MCP server (stdio by default, HTTP with --http).
    Serve(ServeArgs),
    /// Take a screenshot now.
    Shot(ShotArgs),
    /// Start a screen recording.
    Record(RecordArgs),
    /// Stop recording (all sessions, or one by id).
    Stop(StopArgs),
    /// Show recording progress.
    Status(StatusArgs),
    /// List monitors.
    Displays(JsonArg),
    /// List capturable windows.
    Windows(WindowsArgs),
    /// Pick a region interactively and print `x,y,w,h`.
    Region(RegionArgs),
    /// Check the environment (FFmpeg, encoders, WGC, monitors).
    Doctor(JsonArg),
    /// Show or edit the config file.
    Config(ConfigArgs),
    /// Turn start-at-login on or off.
    Autostart(AutostartArgs),
    /// Copy rec-r to a per-user install dir, add it to PATH, add a Start-menu shortcut.
    Install(InstallArgs),
    /// Undo `install`: shortcut, autostart, PATH entry and the installed exe.
    Uninstall(UninstallArgs),
    /// Internal: wait for a process to exit, then start rec-r again (tray "Restart").
    #[command(hide = true)]
    Respawn(RespawnArgs),
    /// Write the app icon (PNG) or the multi-size .ico to a file.
    Icon(IconArgs),
    /// Open the output directory.
    Open,
}

#[derive(Debug, Args)]
pub struct AppArgs {
    /// Also expose the MCP server over HTTP.
    #[arg(long)]
    pub http: bool,
    /// Override the HTTP bind address.
    #[arg(long, value_name = "ADDR")]
    pub addr: Option<String>,
    /// Do not add a tray icon (still runs the message loop / MCP).
    #[arg(long)]
    pub no_tray: bool,
}

#[derive(Debug, Args)]
pub struct ServeArgs {
    /// Use stdio (default unless --http is passed alone).
    #[arg(long)]
    pub stdio: bool,
    /// Serve MCP over streamable HTTP.
    #[arg(long)]
    pub http: bool,
    /// Bind address for --http.
    #[arg(long, value_name = "ADDR")]
    pub addr: Option<String>,
}

#[derive(Debug, Args)]
pub struct ShotArgs {
    #[command(flatten)]
    pub target: TargetArgs,
    /// Output file (absolute, or relative to the output dir).
    #[arg(short, long, value_name = "PATH")]
    pub out: Option<String>,
    /// png | jpg | webp
    #[arg(long, value_name = "FMT")]
    pub format: Option<String>,
    /// Downscale so the longest edge is at most this many pixels.
    #[arg(long, value_name = "PX")]
    pub max_edge: Option<u32>,
    /// lossless | high | balanced | fast
    #[arg(short = 'q', long, value_name = "Q")]
    pub quality: Option<String>,
    /// Wait before capturing.
    #[arg(long, value_name = "SECS", default_value_t = 0.0)]
    pub delay: f32,
    /// Open the file with the default viewer when done.
    #[arg(long)]
    pub open: bool,
    /// Print machine-readable JSON (with base64 image when --inline).
    #[arg(long)]
    pub json: bool,
    /// Include the image as base64 in the JSON output.
    #[arg(long)]
    pub inline: bool,
}

#[derive(Debug, Args)]
pub struct RecordArgs {
    #[command(flatten)]
    pub target: TargetArgs,
    /// Output file (absolute, or relative to the output dir).
    #[arg(short, long, value_name = "PATH")]
    pub out: Option<String>,
    /// Frames per second.
    #[arg(long, value_name = "FPS")]
    pub fps: Option<u32>,
    /// Encoder: auto | h264_qsv | h264_nvenc | h264_amf | libx264
    #[arg(long, value_name = "ENC")]
    pub encoder: Option<String>,
    /// lossless | high | balanced | fast
    #[arg(short = 'q', long, value_name = "Q")]
    pub quality: Option<String>,
    /// Capture engine: auto | wgc | gdi
    #[arg(long, value_name = "ENGINE")]
    pub engine: Option<String>,
    /// Stop automatically after N seconds.
    #[arg(long, value_name = "SECS")]
    pub seconds: Option<u64>,
    /// Hard safety limit when running detached (seconds).
    #[arg(long, value_name = "SECS")]
    pub max_duration: Option<u64>,
    /// Output size, `WxH` (use -2 to keep the aspect ratio, e.g. 1280x-2).
    #[arg(long, value_name = "WxH")]
    pub scale: Option<String>,
    /// Keep recording in the background and return immediately.
    #[arg(long)]
    pub detach: bool,
    /// Open the file when the recording finishes.
    #[arg(long)]
    pub open: bool,
    /// Print JSON instead of a human summary.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct StopArgs {
    /// Recording id from `record`/`status`.
    #[arg(value_name = "ID")]
    pub id: Option<String>,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct StatusArgs {
    #[arg(value_name = "ID")]
    pub id: Option<String>,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct WindowsArgs {
    /// Filter by title, class or process substring.
    #[arg(value_name = "QUERY")]
    pub query: Option<String>,
    /// Max rows (0 = all).
    #[arg(long, default_value_t = 40)]
    pub limit: usize,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct JsonArg {
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct RegionArgs {
    /// JSON instead of `x,y,w,h`.
    #[arg(long)]
    pub json: bool,
    /// Seconds to wait for the selection.
    #[arg(long, default_value_t = 120)]
    pub timeout: u64,
}

#[derive(Debug, Args)]
pub struct ConfigArgs {
    /// Print the config file path.
    #[arg(long)]
    pub path: bool,
    /// Print the effective config as JSON.
    #[arg(long)]
    pub show: bool,
    /// Open the config file in the default editor.
    #[arg(long)]
    pub edit: bool,
    /// Set one value, e.g. `--set fps=60`, `--set hotkeys.shot=Ctrl+Shift+5`.
    #[arg(long, value_name = "KEY=VALUE")]
    pub set: Vec<String>,
}

#[derive(Debug, Args)]
pub struct AutostartArgs {
    /// `on`, `off` or `status` (default).
    #[arg(value_name = "STATE")]
    pub state: Option<String>,
    /// Register the HTTP MCP server too (`rec-r app --http`).
    #[arg(long)]
    pub http: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct InstallArgs {
    /// Install directory (default `%LOCALAPPDATA%\Programs\Rec-R`).
    #[arg(long, value_name = "DIR")]
    pub dir: Option<String>,
    /// Do not add the install directory to the user PATH.
    #[arg(long)]
    pub no_path: bool,
    /// Do not create the Start-menu shortcut.
    #[arg(long)]
    pub no_shortcut: bool,
    /// Also start the tray app at login (HKCU Run key).
    #[arg(long)]
    pub autostart: bool,
    /// With --autostart: start the MCP HTTP server at login too.
    #[arg(long)]
    pub http: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct UninstallArgs {
    /// Install directory (default `%LOCALAPPDATA%\Programs\Rec-R`).
    #[arg(long, value_name = "DIR")]
    pub dir: Option<String>,
    /// Leave rec-r.exe behind.
    #[arg(long)]
    pub keep_exe: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct RespawnArgs {
    /// Process to wait for.
    #[arg(long, value_name = "PID")]
    pub pid: u32,
    /// Arguments for the new process (everything after `--`).
    #[arg(value_name = "ARGS", trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<std::ffi::OsString>,
}

#[derive(Debug, Args)]
pub struct IconArgs {
    /// Output path (default: config output dir).
    #[arg(short = 'o', long, value_name = "PATH")]
    pub out: Option<String>,
    /// Write the multi-size .ico instead of the 256 px PNG.
    #[arg(long)]
    pub ico: bool,
}

#[derive(Debug, Args, Default)]
pub struct TargetArgs {
    /// Monitor index (see `rec-r displays`).
    #[arg(short = 'm', long, value_name = "IDX")]
    pub monitor: Option<usize>,
    /// Window selector: hwnd, title/exe substring, or `cursor`.
    #[arg(short = 'w', long, value_name = "SEL")]
    pub window: Option<String>,
    /// Region as x,y,width,height (physical pixels). Without a value, drag-select on screen.
    #[arg(short = 'r', long, value_name = "RECT", num_args = 0..=1, default_missing_value = "select")]
    pub region: Option<String>,
    #[arg(long)]
    pub x: Option<i32>,
    #[arg(long)]
    pub y: Option<i32>,
    #[arg(long)]
    pub width: Option<i32>,
    #[arg(long)]
    pub height: Option<i32>,
    /// Capture every monitor as one image.
    #[arg(long)]
    pub all: bool,
}

impl TargetArgs {
    /// `-r` with no value (or `region: "select"` over MCP) means "let the human drag a rectangle".
    pub fn wants_selection(&self) -> bool {
        matches!(self.region.as_deref(), Some("select") | Some("pick") | Some("interactive"))
    }

    pub fn to_spec(&self) -> Result<TargetSpec> {
        if self.wants_selection() {
            anyhow::bail!("`-r` needs x,y,width,height here");
        }
        let has_xywh = self.x.is_some() || self.y.is_some() || self.width.is_some() || self.height.is_some();
        if has_xywh && (self.x.is_none() || self.y.is_none() || self.width.is_none() || self.height.is_none()) {
            anyhow::bail!("--x/--y/--width/--height must be given together (or use --region)");
        }
        if self.region.is_some() && has_xywh {
            anyhow::bail!("use either --region or --x/--y/--width/--height, not both");
        }
        if self.all {
            return Ok(TargetSpec { target: Some(TargetKind::VirtualDesktop), ..Default::default() });
        }
        if self.window.is_some() {
            return Ok(TargetSpec { target: Some(TargetKind::Window), window: self.window.clone(), ..Default::default() });
        }
        if self.region.is_some() || has_xywh {
            return Ok(TargetSpec {
                target: Some(TargetKind::Region),
                region: self.region.clone(),
                x: self.x,
                y: self.y,
                width: self.width,
                height: self.height,
                ..Default::default()
            });
        }
        Ok(TargetSpec::display(self.monitor))
    }
}

/// Turns a valueless `-r` into an explicit rectangle by opening the selection overlay.
fn resolve_target(app: &App, target: &TargetArgs, timeout_sec: u64) -> Result<TargetSpec> {
    if !target.wants_selection() {
        return target.to_spec();
    }
    eprintln!("drag a rectangle to choose the region (Esc to cancel)…");
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let Some(rect) = rt.block_on(async { app.select_region(timeout_sec).await })? else {
        anyhow::bail!("region selection cancelled");
    };
    eprintln!("region: {},{},{},{}", rect.x, rect.y, rect.width, rect.height);
    Ok(TargetSpec {
        target: Some(TargetKind::Region),
        x: Some(rect.x),
        y: Some(rect.y),
        width: Some(rect.width),
        height: Some(rect.height),
        ..Default::default()
    })
}

fn parse_scale(text: &str) -> Result<(u32, u32)> {
    let (w, h) = text
        .split_once(['x', 'X', '*', ':'])
        .ok_or_else(|| anyhow::anyhow!("--scale expects WxH, e.g. 1920x1080 or 1280x-2"))?;
    let num = |s: &str| -> Result<u32> {
        let s = s.trim();
        if s == "-1" || s == "-2" {
            return Ok(2);
        }
        Ok(s.parse::<u32>()?)
    };
    Ok((num(w)?, num(h)?))
}

pub fn run(cli: Cli) -> Result<()> {
    let cmd = match cli.cmd {
        Some(c) => c,
        None => Cmd::App(AppArgs { http: false, addr: None, no_tray: false }),
    };
    match cmd {
        Cmd::App(args) => crate::run_tray(args),
        Cmd::Serve(args) => crate::run_serve(args),
        Cmd::Shot(args) => shot(args),
        Cmd::Record(args) => record(args),
        Cmd::Stop(args) => stop(args),
        Cmd::Status(args) => status(args),
        Cmd::Displays(a) => {
            let summary = actions::monitor_summary();
            if a.json {
                println!("{}", serde_json::to_string_pretty(&summary)?);
            } else {
                println!("virtual desktop: {}", summary.virtual_desktop);
                for d in &summary.displays {
                    println!(
                        "  [{idx}] {dev:<14} {w}x{h} at {x},{y}  dpi {dpi} ({scale:.2}x){p}",
                        idx = d.index,
                        dev = d.device,
                        w = d.width,
                        h = d.height,
                        x = d.x,
                        y = d.y,
                        dpi = d.dpi,
                        scale = d.scale,
                        p = if d.primary { "  primary" } else { "" }
                    );
                }
            }
            Ok(())
        }
        Cmd::Windows(args) => {
            let rows = actions::list_windows(args.query.as_deref(), args.limit);
            if args.json {
                println!("{}", serde_json::to_string_pretty(&rows)?);
            } else {
                println!("{:>10}  {:<34} {:<26} {}", "hwnd", "title", "process", "size");
                for w in &rows {
                    println!(
                        "{:>10}  {:<34} {:<26} {}x{} at {},{}",
                        w.hwnd,
                        truncate(&w.title, 34),
                        truncate(&w.process, 26),
                        w.width,
                        w.height,
                        w.x,
                        w.y
                    );
                }
            }
            Ok(())
        }
        Cmd::Region(args) => {
            let app = App::load()?;
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
            let rect = rt.block_on(async { app.select_region(args.timeout).await });
            match rect {
                Ok(Some(r)) => {
                    if args.json {
                        println!("{}", serde_json::json!({"x": r.x, "y": r.y, "width": r.width, "height": r.height}));
                    } else {
                        println!("{},{},{},{}", r.x, r.y, r.width, r.height);
                    }
                    Ok(())
                }
                Ok(None) => {
                    eprintln!("region selection cancelled");
                    std::process::exit(2);
                }
                Err(e) => Err(e),
            }
        }
        Cmd::Doctor(a) => {
            let app = App::load()?;
            let report = actions::doctor(&app);
            if a.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!("rec-r doctor");
                println!("  platform      : {}", report.platform);
                println!("  ffmpeg        : {} ({})", report.ffmpeg, report.ffmpeg_version.as_deref().unwrap_or("MISSING"));
                println!("  chosen encoder: {}", report.chosen_encoder);
                for e in &report.encoders {
                    let state = if e.usable { "ok" } else { "unavailable" };
                    match e.speed_fps {
                        Some(fps) if e.usable => {
                            println!("  encoder {:<12}: {state} ({fps:.1} fps @1080p)", e.name)
                        }
                        _ => println!("  encoder {:<12}: {state}", e.name),
                    }
                }
                println!("  WGC available : {}", report.wgc_available);
                println!("  DPI aware     : {}", report.dpi_aware);
                println!("  virtual desk  : {}", report.monitors.virtual_desktop);
                for d in &report.monitors.displays {
                    println!("  display [{}]   : {} {:.2}x", d.index, d.label, d.scale);
                }
                println!("  output dir    : {}", report.output_dir);
                println!("  config        : {}", report.config_path);
                println!("  MCP (HTTP)    : {}", report.mcp_http_addr);
                println!("  tray UI       : {}", report.tray_ui);
                println!("  recording now : {}", report.active_recordings.len());
                if !report.ok {
                    println!("\nFFmpeg is required for screenshots and recordings: {}",
                        crate::capture::encoder::FFMPEG_MISSING_HINT);
                }
            }
            if report.ok {
                Ok(())
            } else {
                anyhow::bail!("environment is not ready (see above)")
            }
        }
        Cmd::Config(args) => {
            let path = crate::config::config_path();
            if args.path {
                println!("{}", path.display());
                return Ok(());
            }
            let mut cfg = crate::config::Config::load();
            if !args.set.is_empty() {
                for assignment in &args.set {
                    let (key, value) = assignment
                        .split_once('=')
                        .ok_or_else(|| anyhow::anyhow!("expected KEY=VALUE, got {assignment:?}"))?;
                    cfg.apply_setting(key, value)?;
                    println!("set {key} = {value}");
                }
                cfg.save()?;
                if let Some(command) = crate::autostart::sync(cfg.autostart)? {
                    println!("autostart: enabled -> {command}");
                } else if !cfg.autostart {
                    println!("autostart: disabled");
                }
                eprintln!("// {}", path.display());
            }
            if args.edit {
                if !path.exists() {
                    cfg.save()?;
                }
                actions::open_path(&path)?;
                return Ok(());
            }
            println!("{}", serde_json::to_string_pretty(&cfg)?);
            // Path goes to stderr so `rec-r config --show | jq` stays pipeable.
            eprintln!("// {}", path.display());
            Ok(())
        }
        Cmd::Autostart(args) => autostart(args),
        Cmd::Install(args) => install(args),
        Cmd::Uninstall(args) => uninstall(args),
        Cmd::Respawn(args) => crate::restart::run_helper(args.pid, &args.args),
        Cmd::Icon(args) => {
            let cfg = crate::config::Config::load();
            let path = crate::icon::export(&cfg, args.out.as_deref(), args.ico)?;
            println!("{}", path.display());
            Ok(())
        }
        Cmd::Open => {
            let app = App::load()?;
            let dir = app.config().ensure_output_dir()?;
            actions::open_path(&dir)
        }
    }
}

fn autostart(args: AutostartArgs) -> Result<()> {
    let state = args.state.as_deref().unwrap_or("status").to_ascii_lowercase();
    let want = match state.as_str() {
        "status" | "show" => None,
        "on" | "enable" | "enabled" | "install" => Some(true),
        "off" | "disable" | "disabled" | "uninstall" => Some(false),
        other => anyhow::bail!("unknown autostart state {other:?} (use on, off or status)"),
    };

    if let Some(on) = want {
        crate::autostart::set_enabled(on, args.http)?;
        let mut cfg = crate::config::Config::load();
        cfg.autostart = on;
        cfg.save()?;
    }

    let info = crate::autostart::summary();
    if args.json {
        println!("{}", serde_json::to_string_pretty(&info)?);
        return Ok(());
    }
    let enabled = info["enabled"].as_bool().unwrap_or(false);
    println!("autostart: {}", if enabled { "enabled" } else { "disabled" });
    if let Some(cmd) = info["command"].as_str() {
        println!("  command: {cmd}");
    }
    println!(
        "  registry: {} (value {:?})",
        info["key"].as_str().unwrap_or(""),
        info["value"].as_str().unwrap_or("")
    );
    if !enabled {
        println!("\nenable with: rec-r autostart on   (add --http to start the MCP server at login)");
    }
    Ok(())
}

fn shot(args: ShotArgs) -> Result<()> {
    let app = App::load()?;
    if args.delay > 0.0 {
        std::thread::sleep(Duration::from_secs_f32(args.delay));
    }
    let req = actions::ShotRequest {
        target: resolve_target(&app, &args.target, 120)?,
        path: args.out.clone(),
        format: args.format.clone(),
        max_edge: args.max_edge,
        quality: args.quality.clone(),
        inline: args.inline,
        copy: None,
    };
    let res = actions::take_screenshot(&app, req)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&res)?);
    } else {
        println!(
            "saved {} ({}x{}, {:.1} KiB) via {}\n  target: {}",
            res.path,
            res.width,
            res.height,
            res.bytes as f64 / 1024.0,
            res.engine,
            res.target
        );
    }
    if args.open {
        actions::open_path(std::path::Path::new(&res.path))?;
    }
    Ok(())
}

fn record(args: RecordArgs) -> Result<()> {
    let app = App::load()?;
    if let Some(enc) = &args.encoder {
        if !enc.eq_ignore_ascii_case("auto") && !H264_ENCODERS.contains(&enc.as_str()) {
            eprintln!(
                "warning: {enc:?} is not in {:?}; FFmpeg will probably fail",
                H264_ENCODERS
            );
        }
    }
    let engine = match args.engine.as_deref() {
        None | Some("auto") => None,
        Some(other) => Some(other.parse::<Engine>()?),
    };
    let req = actions::RecordRequest {
        target: resolve_target(&app, &args.target, 120)?,
        path: args.out.clone(),
        fps: args.fps,
        encoder: args.encoder.clone(),
        quality: args.quality.clone(),
        engine,
        max_duration_sec: args.max_duration.or(args.seconds),
        scale: args.scale.as_deref().map(parse_scale).transpose()?,
    };
    let info = actions::start_recording(&app, req)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&info)?);
    } else {
        println!(
            "recording {} -> {}\n  target: {}\n  {} @ {} fps, engine {}",
            info.id, info.path, info.target, info.encoder, info.fps, info.engine
        );
    }
    let Some(seconds) = args.seconds else {
        if args.detach {
            println!("detached; stop it with `rec-r stop {}`", info.id);
            return Ok(());
        }
        println!("press Enter to stop…");
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        let stopped = actions::stop_recording(&app, Some(&info.id))?;
        print_stopped(&stopped, args.json)?;
        if args.open {
            if let Some(s) = stopped.first() {
                actions::open_path(std::path::Path::new(&s.path))?;
            }
        }
        return Ok(());
    };

    // Fixed duration: poll status so we can print progress.
    let started = std::time::Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(250));
        let status = actions::recording_status(&app, Some(&info.id));
        let Some(st) = status.first() else {
            break;
        };
        if st.status != "recording" {
            print_stopped(&status, args.json)?;
            return Ok(());
        }
        let elapsed = started.elapsed().as_secs_f64();
        if !args.json && elapsed.ceil() as u64 != (elapsed - 0.25).ceil().max(0.0) as u64 {
            eprint!(
                "\r  {elapsed:>6.1}s / {seconds}s  {} frames",
                st.frames
            );
        }
        if elapsed >= seconds as f64 {
            break;
        }
    }
    let stopped = actions::stop_recording(&app, Some(&info.id))?;
    if !args.json {
        eprintln!();
    }
    print_stopped(&stopped, args.json)?;
    if args.open {
        if let Some(s) = stopped.first() {
            actions::open_path(std::path::Path::new(&s.path))?;
        }
    }
    Ok(())
}

fn print_stopped(list: &[crate::recording::RecordingInfo], json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(list)?);
        return Ok(());
    }
    for info in list {
        println!(
            "stopped {} ({:.1}s, {} frames) -> {}",
            info.status, info.duration_sec, info.frames, info.path
        );
        if let Some(err) = &info.error {
            eprintln!("  error: {err}");
        }
    }
    Ok(())
}

fn stop(args: StopArgs) -> Result<()> {
    let app = App::load()?;
    let list = actions::stop_recording(&app, args.id.as_deref())?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&list)?);
    } else if list.is_empty() {
        println!("no active recordings");
    } else {
        print_stopped(&list, false)?;
    }
    Ok(())
}

fn status(args: StatusArgs) -> Result<()> {
    let app = App::load()?;
    let list = actions::recording_status(&app, args.id.as_deref());
    if args.json {
        println!("{}", serde_json::to_string_pretty(&list)?);
    } else if list.is_empty() {
        println!("idle (no recordings)");
    } else {
        for info in &list {
            println!(
                "{} {:<9} {:>6.1}s {:>6} frames  {}  [{}]  {}",
                info.id,
                info.status,
                info.duration_sec,
                info.frames,
                info.encoder,
                info.engine,
                info.path
            );
        }
    }
    Ok(())
}

fn install(args: InstallArgs) -> Result<()> {
    let dir = args.dir.as_deref().map(std::path::Path::new);
    if let Some(dir) = dir {
        crate::install::ensure_dir_sane(dir)?;
    }
    // --http only makes sense together with autostart, so it implies it.
    let with_autostart = args.autostart || args.http;
    let report = crate::install::install(
        dir,
        !args.no_path,
        !args.no_shortcut,
        with_autostart,
        args.http,
    )?;

    if args.json {
        let json = serde_json::json!({
            "dir": report.dir.display().to_string(),
            "exe": report.exe.display().to_string(),
            "copied": report.copied,
            "path_updated": report.path_updated,
            "path": report.path_value,
            "shortcut": report.shortcut.as_ref().map(|p| p.display().to_string()),
            "autostart": report.autostart,
        });
        println!("{}", serde_json::to_string_pretty(&json)?);
        return Ok(());
    }

    println!("installed {}", report.exe.display());
    println!(
        "  copy       : {}",
        if report.copied { "written" } else { "already in place" }
    );
    if args.no_path {
        println!("  PATH       : skipped (--no-path)");
    } else if report.path_updated {
        println!(
            "  PATH       : added {} — open a new shell to use `rec-r`",
            report.dir.display()
        );
    } else {
        println!("  PATH       : already contains {}", report.dir.display());
    }
    match &report.shortcut {
        Some(p) => println!("  shortcut   : {}", p.display()),
        None => println!("  shortcut   : skipped (--no-shortcut)"),
    }
    match &report.autostart {
        Some(c) => println!("  autostart  : {c}"),
        None => println!("  autostart  : not registered (use --autostart [--http])"),
    }
    println!();
    println!("next: open a new terminal and run `rec-r doctor`, then `rec-r app --http`");
    Ok(())
}

fn uninstall(args: UninstallArgs) -> Result<()> {
    let dir = args.dir.as_deref().map(std::path::Path::new);
    let report = crate::install::uninstall(dir, args.keep_exe)?;

    if args.json {
        let json = serde_json::json!({
            "dir": report.dir.display().to_string(),
            "path_updated": report.path_updated,
            "shortcut_removed": report.shortcut_removed.as_ref().map(|p| p.display().to_string()),
            "autostart_removed": report.autostart_removed,
            "exe_removed": report.exe_removed,
            "warnings": report.warnings,
        });
        println!("{}", serde_json::to_string_pretty(&json)?);
    } else {
        println!("uninstalled from {}", report.dir.display());
        if report.path_updated {
            println!("  PATH       : entry removed");
        }
        if let Some(p) = &report.shortcut_removed {
            println!("  shortcut   : removed {}", p.display());
        }
        if report.autostart_removed {
            println!("  autostart  : disabled");
        }
        if report.exe_removed {
            println!("  files      : removed");
        }
        for w in &report.warnings {
            println!("  warning    : {w}");
        }
    }
    Ok(())
}

fn truncate(text: &str, max: usize) -> String {
    let mut out: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        out.pop();
        out.push('…');
    }
    out
}

