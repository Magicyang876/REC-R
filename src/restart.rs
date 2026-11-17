//! Restarting the tray app from its own menu.
//!
//! The new process cannot simply be spawned: the running instance still owns the
//! `RecRHost` window class, the hotkeys and the MCP port, so `rec-r app` would
//! print "already running" and exit. Instead the old process spawns a tiny helper
//! (`rec-r respawn --pid <own pid> -- <original args>`) that waits for the old
//! process to exit and only then starts the new one.

use std::ffi::OsString;
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Threading::{
    OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const DETACHED_PROCESS: u32 = 0x0000_0008;

/// Start the helper for the current process. Does not wait for it.
pub fn relaunch_self(args: &[OsString]) -> Result<()> {
    let exe = std::env::current_exe().context("locating the rec-r executable")?;
    let mut cmd = Command::new(&exe);
    cmd.arg("respawn")
        .arg("--pid")
        .arg(std::process::id().to_string())
        .arg("--")
        .args(args);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS);
    cmd.spawn()
        .with_context(|| format!("cannot start the restart helper {}", exe.display()))?;
    Ok(())
}

/// Helper body: wait for `pid` to exit, then start `rec-r <args>` detached.
pub fn run_helper(pid: u32, args: &[OsString]) -> Result<()> {
    wait_for_exit(pid, Duration::from_secs(20));
    let exe = std::env::current_exe().context("locating the rec-r executable")?;
    let mut cmd = Command::new(&exe);
    cmd.args(args);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS);
    cmd.spawn()
        .with_context(|| format!("cannot restart {}", exe.display()))?;
    Ok(())
}

/// Block until the process is gone (or the timeout expires).
fn wait_for_exit(pid: u32, timeout: Duration) {
    // Poll instead of one long wait so a stale pid can never hang the helper.
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let alive = unsafe {
            match OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
                Ok(handle) => {
                    let state = WaitForSingleObject(handle, 250);
                    let _ = CloseHandle(handle);
                    // WAIT_TIMEOUT == the process is still running.
                    state.0 == 258
                }
                Err(_) => false,
            }
        };
        if !alive {
            return;
        }
    }
    tracing::warn!("process {pid} did not exit within {timeout:?}; restarting anyway");
}