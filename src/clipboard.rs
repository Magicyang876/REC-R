//! Windows clipboard helpers: text (tray menu "Copy MCP config") and bitmaps
//! (every screenshot is copied by default).

use anyhow::{Context, Result};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};

const CF_DIB: u32 = 8;
const CF_UNICODETEXT: u32 = 13;

/// Open the clipboard, run `fill`, then close it again.
///
/// The clipboard is a shared resource, so opening it is retried briefly: another
/// application holding it for a few milliseconds is normal, not an error.
fn with_clipboard(fill: impl FnOnce() -> Result<()>) -> Result<()> {
    let mut opened = false;
    for _ in 0..10 {
        if unsafe { OpenClipboard(None) }.is_ok() {
            opened = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    anyhow::ensure!(opened, "the clipboard is held by another application");

    let result = (|| -> Result<()> {
        unsafe { EmptyClipboard().context("EmptyClipboard failed")? };
        fill()
    })();

    unsafe {
        let _ = CloseClipboard();
    }
    result
}

/// Hand `bytes` to the clipboard as a `GMEM_MOVEABLE` block.
fn set_bytes(format: u32, bytes: &[u8]) -> Result<()> {
    unsafe {
        let hmem = GlobalAlloc(GMEM_MOVEABLE, bytes.len()).context("GlobalAlloc failed")?;
        let ptr = GlobalLock(hmem) as *mut u8;
        if ptr.is_null() {
            anyhow::bail!("GlobalLock failed");
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
        let _ = GlobalUnlock(hmem);
        // On success the clipboard owns the block; freeing it would corrupt the data.
        SetClipboardData(format, Some(HANDLE(hmem.0))).context("SetClipboardData failed")?;
    }
    Ok(())
}

/// Replace the clipboard contents with `text`.
pub fn set_text(text: &str) -> Result<()> {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes: Vec<u8> = wide.iter().flat_map(|u| u.to_le_bytes()).collect();
    with_clipboard(|| set_bytes(CF_UNICODETEXT, &bytes))
}

/// Replace the clipboard contents with a bitmap.
///
/// `bgra` is the top-down 32-bit buffer the capture engines produce; the
/// clipboard gets a plain `CF_DIB` (24-bit, bottom-up) because that is what
/// every Windows application understands. Screenshots are opaque, so dropping
/// the alpha channel costs nothing.
pub fn set_bitmap(width: i32, height: i32, bgra: &[u8]) -> Result<()> {
    anyhow::ensure!(width > 0 && height > 0, "bitmap has no size");
    let (w, h) = (width as usize, height as usize);
    anyhow::ensure!(bgra.len() >= w * h * 4, "bitmap data is too small");

    let stride = (w * 3 + 3) & !3;
    let mut dib = vec![0u8; 40 + stride * h];
    // BITMAPINFOHEADER: 24 bpp, BI_RGB, bottom-up (positive height).
    dib[0..4].copy_from_slice(&(40u32).to_le_bytes());
    dib[4..8].copy_from_slice(&width.to_le_bytes());
    dib[8..12].copy_from_slice(&height.to_le_bytes());
    dib[12..14].copy_from_slice(&(1u16).to_le_bytes());
    dib[14..16].copy_from_slice(&(24u16).to_le_bytes());
    for row in 0..h {
        let src = &bgra[(h - 1 - row) * w * 4..(h - row) * w * 4];
        let dst = &mut dib[40 + row * stride..40 + row * stride + w * 3];
        for (px, out) in src.chunks_exact(4).zip(dst.chunks_exact_mut(3)) {
            out.copy_from_slice(&px[..3]);
        }
    }
    with_clipboard(|| set_bytes(CF_DIB, &dib))
}