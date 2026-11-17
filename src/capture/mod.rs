//! Capture engines: in-process frame grabbing (WGC / GDI) + the FFmpeg pipe.

pub mod encoder;
pub mod gdi;
pub mod target;
pub mod wgc;

pub use encoder::Ffmpeg;
pub use gdi::Bitmap;
pub use target::{ResolvedTarget, TargetKind};