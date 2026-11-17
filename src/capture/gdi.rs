//! GDI screen grabbing: synchronous, DPI-correct, no FFmpeg involvement.
//!
//! Used for screenshots of displays/regions/virtual desktop and as the
//! fallback recorder when a region spans several monitors (WGC captures one
//! monitor at a time).

use anyhow::{Context, Result};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC,
    ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT, DIB_RGB_COLORS,
    HBITMAP, HDC, SRCCOPY,
};

use crate::geometry::Rect;

/// Top-down BGRA bitmap (alpha forced to 255).
pub struct Bitmap {
    pub width: i32,
    pub height: i32,
    pub data: Vec<u8>,
}

impl Bitmap {
}

/// Grab `rect` (virtual-desktop physical pixels) from the desktop.
pub fn capture_screen(rect: Rect) -> Result<Bitmap> {
    if rect.is_empty() {
        anyhow::bail!("capture rect is empty ({rect})");
    }
    unsafe {
        let screen: HDC = GetDC(None);
        if screen.is_invalid() {
            anyhow::bail!("GetDC(NULL) failed");
        }
        let mem = CreateCompatibleDC(Some(screen));
        if mem.is_invalid() {
            ReleaseDC(None, screen);
            anyhow::bail!("CreateCompatibleDC failed");
        }

        let result = (|| -> Result<Bitmap> {
            let bi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: rect.width,
                    biHeight: -rect.height, // top-down
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
            let dib: HBITMAP = CreateDIBSection(Some(mem), &bi, DIB_RGB_COLORS, &mut bits, None, 0)
                .context("CreateDIBSection failed")?;
            if bits.is_null() {
                let _ = DeleteObject(dib.into());
                anyhow::bail!("CreateDIBSection returned no bits");
            }
            let old = SelectObject(mem, dib.into());
            let blt = BitBlt(
                mem,
                0,
                0,
                rect.width,
                rect.height,
                Some(screen),
                rect.x,
                rect.y,
                SRCCOPY | CAPTUREBLT,
            );
            let mut out = None;
            if blt.is_ok() {
                let len = (rect.width as usize) * (rect.height as usize) * 4;
                let mut data = vec![0u8; len];
                std::ptr::copy_nonoverlapping(bits as *const u8, data.as_mut_ptr(), len);
                // GDI leaves alpha undefined; the layered window / FFmpeg both want 255.
                for px in data.chunks_exact_mut(4) {
                    px[3] = 255;
                }
                out = Some(Bitmap { width: rect.width, height: rect.height, data });
            }
            let _ = SelectObject(mem, old);
            let _ = DeleteObject(dib.into());
            out.ok_or_else(|| anyhow::anyhow!("BitBlt failed for {rect}"))
        })();

        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        result
    }
}

/// Grab a window's client-visible area via its window DC.
/// Fast, but captures whatever is drawn on screen (occluded parts are wrong).
pub fn capture_window_dc(hwnd: isize, rect: Rect) -> Result<Bitmap> {
    use windows::Win32::Foundation::HWND;
    if rect.is_empty() {
        anyhow::bail!("window rect is empty");
    }
    unsafe {
        let win_dc = windows::Win32::Graphics::Gdi::GetWindowDC(Some(HWND(hwnd as *mut _)));
        if win_dc.is_invalid() {
            anyhow::bail!("GetWindowDC failed");
        }
        let mem = CreateCompatibleDC(Some(win_dc));
        let bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: rect.width,
                biHeight: -rect.height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let dib = CreateDIBSection(Some(mem), &bi, DIB_RGB_COLORS, &mut bits, None, 0)
            .context("CreateDIBSection failed")?;
        let old = SelectObject(mem, dib.into());
        BitBlt(mem, 0, 0, rect.width, rect.height, Some(win_dc), 0, 0, SRCCOPY)?;
        let len = (rect.width as usize) * (rect.height as usize) * 4;
        let mut data = vec![0u8; len];
        if !bits.is_null() {
            std::ptr::copy_nonoverlapping(bits as *const u8, data.as_mut_ptr(), len);
        }
        for px in data.chunks_exact_mut(4) {
            px[3] = 255;
        }
        let _ = SelectObject(mem, old);
        let _ = DeleteObject(dib.into());
        let _ = DeleteDC(mem);
        windows::Win32::Graphics::Gdi::ReleaseDC(Some(HWND(hwnd as *mut _)), win_dc);
        Ok(Bitmap { width: rect.width, height: rect.height, data })
    }
}

/// `PrintWindow` with `PW_RENDERFULLCONTENT` — captures DWM content of a window
/// even when it is partially covered (works for most modern apps).
pub fn capture_window_print(hwnd: isize, rect: Rect) -> Result<Bitmap> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Gdi::{CreateCompatibleBitmap, GetDIBits};
    const PW_RENDERFULLCONTENT: u32 = 0x0000_0002;
    unsafe {
        let win_dc = windows::Win32::Graphics::Gdi::GetWindowDC(Some(HWND(hwnd as *mut _)));
        if win_dc.is_invalid() {
            anyhow::bail!("GetWindowDC failed");
        }
        let mem = CreateCompatibleDC(Some(win_dc));
        let bmp = CreateCompatibleBitmap(win_dc, rect.width, rect.height);
        let old = SelectObject(mem, bmp.into());
        let ok = windows::Win32::Storage::Xps::PrintWindow(
            HWND(hwnd as *mut _),
            mem,
            windows::Win32::Storage::Xps::PRINT_WINDOW_FLAGS(PW_RENDERFULLCONTENT),
        );
        let mut data = vec![0u8; (rect.width as usize) * (rect.height as usize) * 4];
        let mut bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: rect.width,
                biHeight: -rect.height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let lines = if ok.as_bool() {
            GetDIBits(
                mem,
                bmp,
                0,
                rect.height as u32,
                Some(data.as_mut_ptr() as *mut _),
                &mut bi,
                DIB_RGB_COLORS,
            )
        } else {
            0
        };
        let _ = SelectObject(mem, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mem);
        windows::Win32::Graphics::Gdi::ReleaseDC(Some(HWND(hwnd as *mut _)), win_dc);
        if lines == 0 {
            anyhow::bail!("PrintWindow returned no bitmap data");
        }
        for px in data.chunks_exact_mut(4) {
            px[3] = 255;
        }
        Ok(Bitmap { width: rect.width, height: rect.height, data })
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    /// `capture_screen(rect)` must return exactly the pixels that the same rect
    /// occupies in a full-desktop capture - this is what makes region recording
    /// and region screenshots agree with each other.
    ///
    /// Needs a real desktop session: `cargo test -- --ignored`
    #[test]
    #[ignore]
    fn capture_rect_matches_full_desktop_capture() {
        crate::win::init_dpi_awareness();
        let desktop = crate::win::virtual_desktop_rect();
        let full = capture_screen(desktop).expect("full desktop capture");

        let mut worst = 0.0f64;
        for rect in [
            Rect::new(desktop.x, desktop.y, 320, 240),
            Rect::new(desktop.x + 600, desktop.y + 400, 400, 300),
            Rect::new(desktop.right() - 360, desktop.bottom() - 260, 320, 240),
        ] {
            let sub = capture_screen(rect).expect("region capture");
            assert_eq!((sub.width, sub.height), (rect.width, rect.height));
            let mut diff = 0.0f64;
            for y in 0..rect.height {
                for x in 0..rect.width {
                    let si = ((y * sub.width + x) * 4) as usize;
                    let fi = ((((rect.y - desktop.y + y) * full.width) + (rect.x - desktop.x + x)) * 4) as usize;
                    for c in 0..3 {
                        diff += (sub.data[si + c] as f64 - full.data[fi + c] as f64).abs();
                    }
                }
            }
            let mad = diff / (rect.width * rect.height * 3) as f64;
            println!("{rect}: mean abs diff {mad:.2}");
            worst = worst.max(mad);
        }
        // Small differences are expected (the screen may change between the two
        // captures); a wrong offset or stride shows up as a huge value.
        assert!(worst < 8.0, "region capture does not line up with the desktop (mad {worst:.2})");
    }
}
