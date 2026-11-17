//! rec-r: screenshot / screen recording for Windows with an MCP server.

mod actions;
mod app;
mod autostart;
mod capture;
mod cli;
mod clipboard;
mod config;
mod console;
mod geometry;
mod gui;
mod hotkeys;
mod icon;
mod install;
mod locate;
mod mcp;
mod recording;
mod restart;
mod screenshot;
mod win;

use anyhow::Result;
use clap::Parser;
use std::sync::Arc;

use crate::app::App;

fn main() -> Result<()> {
    // DPI awareness must be set before anything queries the desktop geometry,
    // otherwise Win32 reports scaled (virtualized) coordinates.
    win::init_dpi_awareness();
    let cli = cli::Cli::parse();

    // Started from Explorer (double-click, Start menu, Run key) the process gets
    // its own console window; the tray app should not leave a terminal behind.
    let tray = matches!(&cli.cmd, None | Some(cli::Cmd::App(_)));
    let _ = tray && console::hide_if_owned();
    // Also true for a process the restart helper started detached (it never had
    // a console); printing there would hit a closed handle and panic.
    let log = if tray && !console::attached() {
        Some(start_file_log())
    } else {
        None
    };

    init_tracing(log.is_some());
    if let Some(path) = &log {
        tracing::info!("no console; logging to {}", path.display());
    }
    cli::run(cli)
}

/// Send stdout/stderr (and therefore the tracing output) to
/// `%APPDATA%\Rec-R\rec-r.log`; returns the path.
fn start_file_log() -> std::path::PathBuf {
    let dir = config::config_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("rec-r.log");
    let too_big = std::fs::metadata(&path)
        .map(|m| m.len() > 4 * 1024 * 1024)
        .unwrap_or(false);
    if too_big {
        let _ = std::fs::rename(&path, dir.join("rec-r.log.1"));
    }
    console::redirect_streams_to(&path);
    path
}

fn init_tracing(plain: bool) {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        // A log file should not carry terminal colour codes.
        .with_ansi(!plain)
        .with_writer(std::io::stderr)
        .try_init();
}

/// `rec-r app` — tray UI (optionally with the MCP HTTP server).
pub fn run_tray(args: cli::AppArgs) -> Result<()> {
    win::init_dpi_awareness();
    if let Some(hwnd) = gui::running_instance() {
        let msg = format!(
            "rec-r is already running (tray window 0x{:x}).\n\n\
             Use the tray icon menu, or `rec-r stop` to stop recordings.",
            hwnd.0 as isize
        );
        if console::attached() {
            eprintln!("{msg}");
        } else {
            // Double-clicked with no console around: printing would go nowhere.
            gui::message_box("rec-r", &msg);
        }
        return Ok(());
    }
    let app: Arc<App> = App::load()?;
    tracing::info!("rec-r {} starting", env!("CARGO_PKG_VERSION"));
    warm_encoder_cache(&app);

    if args.http {
        let cfg = app.config();
        let addr = mcp::parse_addr(args.addr.as_deref().unwrap_or(&cfg.http_addr))?;
        // Another instance may already own the port: keep the tray usable.
        match mcp::spawn_http(app.clone(), addr) {
            Ok(_) => println!("MCP (HTTP): http://{addr}/mcp"),
            Err(e) => {
                tracing::warn!("MCP HTTP server not started: {e:#}");
                eprintln!("warning: MCP HTTP server not started on {addr}: {e:#}");
            }
        }
    } else {
        println!("hint: run `rec-r app --http` to expose the MCP server");
    }

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    app.set_ui(tx);
    gui::run(app, rx, !args.no_tray)
}

/// `rec-r serve` — MCP over stdio (default) or HTTP.
pub fn run_serve(args: cli::ServeArgs) -> Result<()> {
    win::init_dpi_awareness();
    let app: Arc<App> = App::load()?;
    warm_encoder_cache(&app);
    let use_http = args.http && !args.stdio;

    if use_http {
        let cfg = app.config();
        let addr = mcp::parse_addr(args.addr.as_deref().unwrap_or(&cfg.http_addr))?;
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
        rt.block_on(mcp::serve_http(app, addr))
    } else {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
        rt.block_on(mcp::serve_stdio(app))
    }
}
/// Probing encoders costs a few seconds, so do it once in the background after
/// startup: recordings then pick the fastest encoder instead of assuming
/// libx264 (which is always available, so the fast path stays correct).
fn warm_encoder_cache(app: &Arc<App>) {
    let app = app.clone();
    let spawned = std::thread::Builder::new()
        .name("rec-bench".into())
        .spawn(move || {
            let cfg = app.config();
            let ffmpeg = crate::capture::Ffmpeg::new(cfg.ffmpeg.clone());
            let speeds = ffmpeg.benchmark_encoders();
            let chosen = ffmpeg.pick_encoder(&cfg.encoder);
            let summary: Vec<String> =
                speeds.iter().map(|(name, fps)| format!("{name} {fps:.0}fps")).collect();
            tracing::info!("encoders: {} -> using {chosen}", summary.join(", "));
        });
    if let Err(e) = spawned {
        tracing::warn!("could not start the encoder benchmark thread: {e}");
    }
}
