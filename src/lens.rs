//! Refraction for clear glass: light bent round a pane's thick, rounded edge.
//!
//! Real glass with a rounded edge acts as a lens there: what's behind the
//! pane near its edge is squeezed and pulled round the curve, the light
//! from just outside the pane bent into view, its colours split a little
//! as they bend. That bending is what shows where the glass ends, with no
//! outline drawn.
//!
//! Windows can't do this for us, so a panel's backdrop window (see
//! `backdrop`) becomes a layered window holding only a ring along the
//! panel's edge. Its pixels are what's on the screen just around the
//! panel, read with a plain screen copy and folded in over the edge: each
//! pixel in the ring shows the screen from just outside the edge, from
//! further out the nearer it is to the edge, and red, green and blue each
//! a little differently. Since Windows 8 a screen copy includes layered
//! windows too (the panel itself, and the ring), so the ring only ever
//! reads what's outside the panel, never what's under it. The middle of
//! the panel stays truly see-through.
//!
//! What's behind can change at any time, so the rings are refreshed a few
//! times a second while one is on screen (`refresh`), and only redrawn when
//! what's behind them did change.

use std::cell::RefCell;
use std::ffi::c_void;

use windows::Win32::Foundation::{COLORREF, HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::*;

/// How a pane bends light (sizes at 100% scaling).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Lens {
    /// How far in from the edge the glass is curved, in px.
    pub band: f32,
    /// How far the light is bent at the very edge, as a share of `band`.
    pub bend: f32,
    /// How much more red bends than blue (the colour fringes), as a share
    /// of the bend.
    pub split: f32,
}

/// A 32-bit top-down bitmap selected into a memory DC.
struct Dib {
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
    w: i32,
    h: i32,
}

impl Dib {
    fn new(w: i32, h: i32) -> Option<Self> {
        unsafe {
            let dc = CreateCompatibleDC(None);
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut c_void = std::ptr::null_mut();
            let Ok(bitmap) = CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0) else {
                let _ = DeleteDC(dc);
                return None;
            };
            if bits.is_null() {
                let _ = DeleteObject(bitmap.into());
                let _ = DeleteDC(dc);
                return None;
            }
            let old = SelectObject(dc, bitmap.into());
            Some(Self { dc, bitmap, old, bits: bits as *mut u8, w, h })
        }
    }

    fn pixels(&mut self) -> &mut [u8] {
        // SAFETY: the DIB section holds w * h 32-bit pixels for as long as it lives.
        unsafe { std::slice::from_raw_parts_mut(self.bits, (self.w * self.h * 4) as usize) }
    }
}

impl Drop for Dib {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

/// A ring on screen.
struct Ring {
    hwnd: HWND,
    rect: RECT,
    radius: i32,
    lens: Lens,
    /// The screen behind and around it.
    capture: Option<Dib>,
    /// The ring itself.
    out: Option<Dib>,
    /// What the capture held last time, to skip redrawing an unchanged ring.
    seen: u64,
}

thread_local! {
    static RINGS: RefCell<Vec<Ring>> = const { RefCell::new(Vec::new()) };
}

/// Shows (or moves) the ring of layered window `hwnd`, at `rect` (screen
/// px) with corners of `radius` px.
pub fn show(hwnd: HWND, rect: RECT, radius: i32, lens: Lens) {
    RINGS.with(|rings| {
        let mut rings = rings.borrow_mut();
        let i = match rings.iter().position(|r| r.hwnd == hwnd) {
            Some(i) => i,
            None => {
                rings.push(Ring { hwnd, rect, radius, lens, capture: None, out: None, seen: 0 });
                rings.len() - 1
            }
        };
        let ring = &mut rings[i];
        let moved = ring.rect != rect || ring.radius != radius || ring.lens != lens;
        ring.rect = rect;
        ring.radius = radius;
        ring.lens = lens;
        paint(ring, moved);
    });
}

/// Forgets the ring of `hwnd` (its window was hidden).
pub fn hide(hwnd: HWND) {
    RINGS.with(|rings| rings.borrow_mut().retain(|r| r.hwnd != hwnd));
}

/// Redraws every ring on screen whose background changed.
pub fn refresh() {
    RINGS.with(|rings| {
        for ring in rings.borrow_mut().iter_mut() {
            paint(ring, false);
        }
    });
}

/// Signed distance from (x, y) to the edge of a w x h rounded rect with
/// corners of radius r (negative inside), and the outward normal there.
fn edge(x: f32, y: f32, w: f32, h: f32, r: f32) -> (f32, f32, f32) {
    let (px, py) = (x - w / 2.0, y - h / 2.0);
    let (qx, qy) = (px.abs() - (w / 2.0 - r), py.abs() - (h / 2.0 - r));
    let (sx, sy) = (px.signum(), py.signum());
    if qx > 0.0 && qy > 0.0 {
        let len = qx.hypot(qy).max(1e-4);
        (len - r, sx * qx / len, sy * qy / len)
    } else if qx > qy {
        (qx - r, sx, 0.0)
    } else {
        (qy - r, 0.0, sy)
    }
}

/// One channel of the capture at (x, y), blended between its four nearest
/// pixels; the point is kept inside `valid` (the part of the capture that
/// is on a screen).
fn sample(src: &[u8], stride: usize, valid: (f32, f32, f32, f32), x: f32, y: f32, channel: usize) -> f32 {
    let x = x.clamp(valid.0, valid.2) - 0.5;
    let y = y.clamp(valid.1, valid.3) - 0.5;
    let (x0, y0) = (x.floor().max(0.0), y.floor().max(0.0));
    let (fx, fy) = ((x - x0).clamp(0.0, 1.0), (y - y0).clamp(0.0, 1.0));
    let (ix, iy) = (x0 as usize, y0 as usize);
    let max_x = (valid.2 - 1.0).max(0.0) as usize;
    let max_y = (valid.3 - 1.0).max(0.0) as usize;
    let at = |x: usize, y: usize| src[y.min(max_y) * stride + x.min(max_x) * 4 + channel] as f32;
    let top = at(ix, iy) + (at(ix + 1, iy) - at(ix, iy)) * fx;
    let bottom = at(ix, iy + 1) + (at(ix + 1, iy + 1) - at(ix, iy + 1)) * fx;
    top + (bottom - top) * fy
}

fn paint(ring: &mut Ring, force: bool) {
    let (w, h) = (ring.rect.right - ring.rect.left, ring.rect.bottom - ring.rect.top);
    if w < 4 || h < 4 {
        return;
    }
    let dpi = unsafe { GetDpiForWindow(ring.hwnd) };
    let scale = if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 };
    let band = (ring.lens.band * scale).min(w.min(h) as f32 / 2.0).max(1.0);
    let bend = band * ring.lens.bend;
    let split = ring.lens.split;
    let margin = (bend * (1.0 + split)).ceil() as i32 + 3;
    let (cw, ch) = (w + 2 * margin, h + 2 * margin);
    let (cx, cy) = (ring.rect.left - margin, ring.rect.top - margin);

    // What's behind, and a little around it.
    if ring.capture.as_ref().is_none_or(|c| c.w != cw || c.h != ch) {
        ring.capture = Dib::new(cw, ch);
    }
    let Some(capture) = ring.capture.as_mut() else { return };
    unsafe {
        let screen = GetDC(None);
        let _ = BitBlt(capture.dc, 0, 0, cw, ch, Some(screen), cx, cy, SRCCOPY);
        ReleaseDC(None, screen);
    }
    let src = capture.pixels();
    // FNV-1a over the pixels around the panel (all the ring reads): nothing
    // to do if they didn't change. What's inside is the panel and the ring
    // themselves, which change with every frame drawn.
    let mut hash = 0xcbf2_9ce4_8422_2325u64 ^ ((cw as u64) << 32) ^ (ch as u64);
    let row = |y: i32| &src[(y * cw * 4) as usize..((y + 1) * cw * 4) as usize];
    for y in 0..ch {
        let r = row(y);
        let outer = y < margin || y >= ch - margin;
        let m = (margin * 4) as usize;
        let parts: [&[u8]; 2] = if outer { [r, &[]] } else { [&r[..m], &r[r.len() - m..]] };
        for part in parts {
            for px in part.as_chunks::<4>().0 {
                hash = (hash ^ u32::from_le_bytes([px[0], px[1], px[2], 0]) as u64).wrapping_mul(0x0100_0000_01b3);
            }
        }
    }
    if !force && hash == ring.seen {
        return;
    }
    ring.seen = hash;

    // Only the part of the capture that is on a screen is worth reading.
    let (vx, vy, vw, vh) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    let valid = (
        ((vx - cx) as f32).clamp(0.0, cw as f32 - 1.0) + 0.5,
        ((vy - cy) as f32).clamp(0.0, ch as f32 - 1.0) + 0.5,
        ((vx + vw - cx) as f32).clamp(1.0, cw as f32) - 0.5,
        ((vy + vh - cy) as f32).clamp(1.0, ch as f32) - 0.5,
    );

    if ring.out.as_ref().is_none_or(|o| o.w != w || o.h != h) {
        ring.out = Dib::new(w, h);
    }
    let Some(out) = ring.out.as_mut() else { return };
    let dst = out.pixels();
    let stride = cw as usize * 4;
    let (fw, fh) = (w as f32, h as f32);
    let r = (ring.radius as f32).min(fw.min(fh) / 2.0);
    // Rows clear of the curved corners and the top and bottom bands only
    // have a band at each end.
    let reach = (band + r).ceil() as i32 + 1;
    for y in 0..h {
        let mid = y >= reach && y < h - reach;
        let spans: [(i32, i32); 2] = if mid { [(0, reach.min(w)), ((w - reach).max(0), w)] } else { [(0, w), (w, w)] };
        for (x0, x1) in spans {
            for x in x0..x1 {
                let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                let (d, nx, ny) = edge(fx, fy, fw, fh, r);
                let inside = -d;
                let o = (y * w + x) as usize * 4;
                if inside < -0.5 || inside >= band {
                    dst[o..o + 4].copy_from_slice(&[0, 0, 0, 0]);
                    continue;
                }
                // 1 at the edge, 0 where the glass is flat again.
                let t = (1.0 - inside.max(0.0) / band).clamp(0.0, 1.0);
                // How far outside the edge this pixel's light comes from:
                // furthest at the edge, right at it where the glass is flat.
                let out = bend * t * t;
                // Out to the edge first, then a pixel clear of it.
                let to_edge = inside.max(0.0) + 1.0;
                // Each colour from a little further along the bend than the next.
                let (sx, sy) = (fx + margin as f32, fy + margin as f32);
                let at = |k: f32, channel: usize| {
                    let d = to_edge + out * k;
                    sample(src, stride, valid, sx + nx * d, sy + ny * d, channel)
                };
                let (red, green, blue) = (at(1.0 + split, 2), at(1.0, 1), at(1.0 - split, 0));
                // Light caught in the curve, most along its lit (upper) part:
                // what shows where the edge is over a plain background.
                let glint = 255.0 * 0.16 * t * t * t * (0.55 - 0.45 * ny);
                let cover = (inside + 0.5).clamp(0.0, 1.0);
                // Fade into the flat glass, where the ring shows what's behind as it is.
                let fade = ((band - inside) / (band * 0.25)).clamp(0.0, 1.0);
                let a = cover * fade;
                let px = |c: f32| ((c + glint).min(255.0) * a).round() as u8;
                dst[o..o + 4].copy_from_slice(&[px(blue), px(green), px(red), (a * 255.0).round() as u8]);
            }
        }
    }

    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    unsafe {
        let _ = UpdateLayeredWindow(
            ring.hwnd,
            None,
            Some(&POINT { x: ring.rect.left, y: ring.rect.top }),
            Some(&SIZE { cx: w, cy: h }),
            Some(out.dc),
            Some(&POINT { x: 0, y: 0 }),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_distance_and_normal() {
        // The middle of the top edge of a 100 x 40 rect: on the edge, normal up.
        let (d, nx, ny) = edge(50.0, 0.0, 100.0, 40.0, 8.0);
        assert!(d.abs() < 1e-4 && nx == 0.0 && ny == -1.0);
        // 5 px in from the left edge.
        let (d, nx, _) = edge(5.0, 20.0, 100.0, 40.0, 8.0);
        assert!((d + 5.0).abs() < 1e-4 && nx == -1.0);
        // The very corner of the box is outside the rounded corner.
        let (d, nx, ny) = edge(0.0, 0.0, 100.0, 40.0, 8.0);
        assert!(d > 0.0 && nx < 0.0 && ny < 0.0);
    }
}
