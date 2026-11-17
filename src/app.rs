//! Runtime state shared by the CLI, the tray UI and the MCP server.

use std::sync::{Arc, Mutex};

use anyhow::Result;
use tokio::sync::{mpsc, oneshot};

use crate::config::Config;
use crate::geometry::Rect;
use crate::recording::RecordingManager;

/// Work that must run on the UI thread because it needs a Win32 message loop.
pub enum UiCommand {
    /// Let the user draw a rectangle on screen; `None` means cancelled.
    SelectRegion { reply: oneshot::Sender<Option<Rect>> },
}

pub struct App {
    config: Mutex<Config>,
    pub recorder: RecordingManager,
    ui: Mutex<Option<mpsc::UnboundedSender<UiCommand>>>,
}

impl App {
    pub fn new(config: Config) -> Arc<Self> {
        Arc::new(Self {
            config: Mutex::new(config),
            recorder: RecordingManager::new(),
            ui: Mutex::new(None),
        })
    }

    pub fn load() -> Result<Arc<Self>> {
        let mut config = Config::load();
        // A tray app started from Explorer sees a different PATH than the shell
        // that installed ffmpeg, so pin the absolute path here.
        crate::locate::resolve_ffmpeg(&mut config);
        // `autostart: true` should mean the login entry actually works. A stale
        // pointer (old `target\release` path, uninstalled copy) fails silently at
        // every login, so repair it here. A missing entry is left alone: only an
        // explicit `autostart` change creates one.
        if config.autostart && crate::autostart::points_at_missing_exe() {
            match crate::autostart::sync(true) {
                Ok(Some(command)) => tracing::info!("autostart repointed at {command}"),
                Ok(None) => {}
                Err(e) => tracing::warn!("cannot repair the autostart entry: {e:#}"),
            }
        }
        Ok(Self::new(config))
    }

    pub fn config(&self) -> Config {
        self.config.lock().unwrap().clone()
    }

    /// Mutate + persist the config (`rec-r config --set`, MCP `config`).
    pub fn update_config<F>(&self, f: F) -> Result<Config>
    where
        F: FnOnce(&mut Config),
    {
        let mut cfg = self.config.lock().unwrap();
        f(&mut cfg);
        cfg.save()?;
        Ok(cfg.clone())
    }

    /// Persist one `key=value` override, applying the same side effects the CLI
    /// does (e.g. `autostart` writes the Run registry entry).
    pub fn set_config(&self, key: &str, value: &str) -> Result<Config> {
        let mut cfg = self.config.lock().unwrap();
        cfg.apply_setting(key, value)?;
        cfg.save()?;
        let snapshot = cfg.clone();
        drop(cfg);
        // `autostart` is mirrored into the Run registry. `sync` registers the
        // *installed* exe (never the build directory) and repoints a stale entry.
        crate::autostart::sync(snapshot.autostart)?;
        Ok(snapshot)
    }

    pub fn set_ui(&self, tx: mpsc::UnboundedSender<UiCommand>) {
        *self.ui.lock().unwrap() = Some(tx);
    }

    pub fn has_ui(&self) -> bool {
        self.ui.lock().unwrap().is_some()
    }

    fn ui_sender(&self) -> Result<mpsc::UnboundedSender<UiCommand>> {
        self.ui
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| anyhow::anyhow!("the tray UI is not running (start `rec-r app` first)"))
    }

    /// Ask the UI to run an interactive region selection. Without a tray UI the
    /// overlay runs on its own thread, so a headless `rec-r serve` can still ask
    /// the human for a rectangle.
    pub async fn select_region(&self, timeout_sec: u64) -> Result<Option<Rect>> {
        let timeout = std::time::Duration::from_secs(timeout_sec.max(5));
        if self.has_ui() {
            let (reply, rx) = oneshot::channel::<Option<Rect>>();
            self.ui_sender()?
                .send(UiCommand::SelectRegion { reply })
                .map_err(|_| anyhow::anyhow!("tray UI stopped"))?;
            match tokio::time::timeout(timeout, rx).await {
                Ok(Ok(rect)) => Ok(rect),
                Ok(Err(_)) => anyhow::bail!("region selection was aborted"),
                Err(_) => anyhow::bail!("region selection timed out after {timeout_sec}s"),
            }
        } else {
            let (reply, rx) = oneshot::channel::<Result<Option<Rect>>>();
            std::thread::Builder::new()
                .name("rec-region".into())
                .spawn(move || {
                    let _ = reply.send(crate::gui::overlay::select_region_blocking());
                })?;
            match tokio::time::timeout(timeout, rx).await {
                Ok(Ok(result)) => result,
                Ok(Err(_)) => anyhow::bail!("region selection was aborted"),
                Err(_) => anyhow::bail!("region selection timed out after {timeout_sec}s"),
            }
        }
    }

}