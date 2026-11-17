//! Rectangular geometry in *physical* (DPI-unaware-scaled) pixels,
//! always expressed in virtual-desktop coordinates.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self { x, y, width, height }
    }

    pub fn right(&self) -> i32 {
        self.x + self.width
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.height
    }

    pub fn is_empty(&self) -> bool {
        self.width <= 0 || self.height <= 0
    }

    /// Clamp to a bounding box, returning `None` when the result is empty.
    pub fn intersect(&self, other: Rect) -> Option<Rect> {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        if right <= x || bottom <= y {
            None
        } else {
            Some(Rect::new(x, y, right - x, bottom - y))
        }
    }

    /// Force even dimensions (FFmpeg yuv420p encoders require it) while
    /// staying inside the original rect.
    pub fn even(&self) -> Rect {
        Rect::new(self.x, self.y, self.width - (self.width % 2), self.height - (self.height % 2))
    }

}

impl std::fmt::Display for Rect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{},{} {}x{}", self.x, self.y, self.width, self.height)
    }
}

/// Parse `"x,y,w,h"`, `"x,y,w,h@monitor-index"` is *not* supported — keep it dumb.
pub fn parse_region(s: &str) -> anyhow::Result<Rect> {
    let parts: Vec<&str> = s
        .split(|c| c == ',' || c == 'x' || c == 'X' || c == ' ' || c == ':' || c == '/')
        .filter(|p| !p.trim().is_empty())
        .collect();
    if parts.len() != 4 {
        anyhow::bail!("region must be \"x,y,width,height\" (got {s:?})");
    }
    let n: Vec<i32> = parts
        .iter()
        .map(|p| p.trim().parse::<i32>())
        .collect::<Result<_, _>>()
        .map_err(|e| anyhow::anyhow!("region parse error in {s:?}: {e}"))?;
    let rect = Rect::new(n[0], n[1], n[2], n[3]);
    if rect.is_empty() {
        anyhow::bail!("region {s:?} has non-positive size");
    }
    Ok(rect)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_regions() {
        assert_eq!(parse_region("10,20,300,400").unwrap(), Rect::new(10, 20, 300, 400));
        assert_eq!(parse_region("10x20x300x400").unwrap(), Rect::new(10, 20, 300, 400));
        assert!(parse_region("1,2,3").is_err());
        assert!(parse_region("1,2,3,0").is_err());
    }

    #[test]
    fn even_and_intersect() {
        assert_eq!(Rect::new(0, 0, 101, 99).even(), Rect::new(0, 0, 100, 98));
        assert_eq!(
            Rect::new(0, 0, 100, 100).intersect(Rect::new(-10, 50, 60, 60)),
            Some(Rect::new(0, 50, 50, 50))
        );
    }
}