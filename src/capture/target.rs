//! Target specification: display / window / region / whole virtual desktop.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::geometry::{parse_region, Rect};
use crate::win::monitor::{self, Monitor};
use crate::win::window;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    Display,
    Window,
    Region,
    VirtualDesktop,
}

impl Default for TargetKind {
    fn default() -> Self {
        Self::Display
    }
}

impl std::fmt::Display for TargetKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            TargetKind::Display => "display",
            TargetKind::Window => "window",
            TargetKind::Region => "region",
            TargetKind::VirtualDesktop => "virtual_desktop",
        };
        f.write_str(s)
    }
}

/// Raw, user/agent-provided target. Kept separate from [`ResolvedTarget`] so
/// callers never have to resolve coordinates themselves.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TargetSpec {
    pub target: Option<TargetKind>,
    /// Monitor index from `list_displays`.
    pub monitor: Option<usize>,
    /// Window selector: HWND (`1234`, `0x4d2`), exact title, or title/process substring.
    pub window: Option<String>,
    /// `"x,y,width,height"` in virtual-desktop physical pixels.
    pub region: Option<String>,
    /// Explicit x/y/width/height (alternative to `region`).
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub width: Option<i32>,
    pub height: Option<i32>,
}

#[derive(Debug, Clone)]
pub struct ResolvedTarget {
    pub kind: TargetKind,
    pub label: String,
    /// Bounds in virtual-desktop physical pixels.
    pub rect: Rect,
    pub hwnd: Option<i64>,
    pub monitor: Option<Monitor>,
}

impl ResolvedTarget {
    pub fn width(&self) -> u32 {
        self.rect.width.max(1) as u32
    }
    pub fn height(&self) -> u32 {
        self.rect.height.max(1) as u32
    }
}

impl TargetSpec {
    pub fn display(monitor: Option<usize>) -> Self {
        Self { target: Some(TargetKind::Display), monitor, ..Default::default() }
    }

    pub fn window(selector: impl Into<String>) -> Self {
        Self { target: Some(TargetKind::Window), window: Some(selector.into()), ..Default::default() }
    }

    fn explicit_rect(&self) -> Option<Rect> {
        if let Some(text) = self.region.as_deref().filter(|s| !s.trim().is_empty()) {
            return parse_region(text).ok();
        }
        match (self.x, self.y, self.width, self.height) {
            (Some(x), Some(y), Some(w), Some(h)) => Some(Rect::new(x, y, w, h)),
            _ => None,
        }
    }

    /// Turn the loose spec into concrete monitor/window/geometry.
    pub fn resolve(&self) -> Result<ResolvedTarget> {
        if let Some(rect) = self.explicit_rect() {
            if rect.is_empty() {
                bail!("region {rect} has a non-positive size");
            }
            let desktop = crate::win::virtual_desktop_rect();
            let clipped = rect
                .intersect(desktop)
                .ok_or_else(|| anyhow::anyhow!("region {rect} is outside the virtual desktop {desktop}"))?;
            if clipped != rect {
                tracing::warn!("region {rect} clipped to {clipped} (virtual desktop {desktop})");
            }
            // yuv420p (and therefore every H.264 encoder) needs even dimensions;
            // rounding here keeps the reported size in sync with the encoded one.
            let even = clipped.even();
            if even != clipped {
                tracing::debug!("region {clipped} rounded to {even} for yuv420p");
            }
            let mon = monitor::monitor_at(even.x + even.width / 2, even.y + even.height / 2);
            return Ok(ResolvedTarget {
                kind: TargetKind::Region,
                label: format!("region {even}"),
                rect: even,
                hwnd: None,
                monitor: mon,
            });
        }

        if let Some(selector) = self.window.as_deref().filter(|s| !s.trim().is_empty()) {
            let selector = selector.trim();
            let info = if selector.eq_ignore_ascii_case("cursor") || selector == "@cursor" {
                window::window_under_cursor(&[std::process::id() as i64]).ok_or_else(|| {
                    anyhow::anyhow!(
                        "no capturable window under the cursor - hover the window you want (not the tray/menu) \
                         or pass a window title, process name or hwnd (`rec-r windows` to list them)"
                    )
                })?
            } else {
                let info = window::resolve_window(selector).ok_or_else(|| {
                    anyhow::anyhow!("no visible window matches {selector:?}; call list_windows")
                })?;
                if !window::is_capturable(&info) {
                    if info.minimized {
                        bail!(
                            "window [{}] {:?} is minimized - restore it before capturing",
                            info.hwnd,
                            info.title
                        );
                    }
                    bail!(
                        "window [{}] {:?} cannot be captured (hidden, cloaked, or a shell popup) - \
                         try another window or a region",
                        info.hwnd,
                        info.title
                    );
                }
                info
            };
            if info.rect.is_empty() {
                bail!("window {:?} has an empty frame rect (minimized?)", info.title);
            }
            let mon = monitor::monitor_at(info.rect.x + info.rect.width / 2, info.rect.y + info.rect.height / 2);
            return Ok(ResolvedTarget {
                kind: TargetKind::Window,
                label: format!("window [{}] {} ({})", info.hwnd, info.title, info.process),
                rect: info.rect,
                hwnd: Some(info.hwnd),
                monitor: mon,
            });
        }

        let kind = self.target.unwrap_or(TargetKind::Display);
        match kind {
            TargetKind::VirtualDesktop => {
                let rect = crate::win::virtual_desktop_rect();
                if rect.is_empty() {
                    bail!("virtual desktop bounds are empty");
                }
                Ok(ResolvedTarget {
                    kind,
                    label: format!("virtual desktop {rect}"),
                    rect,
                    hwnd: None,
                    monitor: None,
                })
            }
            TargetKind::Window => bail!("target=window requires a `window` selector"),
            TargetKind::Region => bail!("target=region requires `region` or x/y/width/height"),
            TargetKind::Display => {
                let mon = match self.monitor {
                    Some(idx) => monitor::monitor_by_index(idx)
                        .ok_or_else(|| anyhow::anyhow!("monitor index {idx} not found"))?,
                    None => monitor::primary_monitor()
                        .ok_or_else(|| anyhow::anyhow!("no monitors detected"))?,
                };
                Ok(ResolvedTarget {
                    kind,
                    label: format!("display {} [{}] {}{}", mon.index, mon.device, mon.rect, if mon.is_primary { " primary" } else { "" }),
                    rect: mon.rect,
                    hwnd: None,
                    monitor: Some(mon),
                })
            }
        }
    }
}