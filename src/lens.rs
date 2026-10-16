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
//! panel, read from the compositor's own frames (see `screen`) and folded
//! in over the edge: each pixel in the ring shows the screen from just
//! outside the edge, from further out the nearer it is to the edge, and
//! red, green and blue each a little differently. What's on screen
//! includes the panel itself and the ring, so the ring only ever reads
//! what's outside the panel, never what's under it. The middle of the
//! panel stays truly see-through.
//!
//! Where each ring pixel's light comes from only depends on the ring's
//! shape, so it's worked out once per shape (`build`); a redraw only looks
//! the pixels up (`render`). What's behind can change at any time, so the
//! rings are refreshed 20 times a second while one is on screen
//! (`refresh`): a refresh only reads the screen back when the compositor
//! drew a new frame, and only redraws when what's round the ring changed.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;

use windows::Win32::Foundation::{COLORREF, HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::screen;

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
    /// The screen around it (only the strips just outside its edge are read).
    capture: Option<Dib>,
    /// The ring itself.
    out: Option<Dib>,
    /// Where each of its pixels gets its light from, for its current shape.
    map: Option<Map>,
    /// The duplicated screen's frame last read (see `screen`).
    frame: Option<u64>,
    /// What the capture held last time, to skip redrawing an unchanged ring.
    seen: u64,
    /// Refreshes in a row that found nothing new.
    idle: u32,
    /// Counts skipped refreshes while idle on a screen copied with GDI.
    skip: u32,
}

/// What a ring's map is worked out for.
#[derive(Clone, Copy, PartialEq)]
struct Shape {
    w: i32,
    h: i32,
    radius: i32,
    lens: Lens,
    dpi: u32,
    /// The part of the capture that is on a screen (capture px).
    valid: RECT,
}

/// One of the capture's pixels, blended with the next one along and the one
/// below by `fx` and `fy` (out of 256), as far as those are on screen.
#[derive(Clone, Copy)]
struct Tap {
    /// Byte offset in the capture.
    at: u32,
    fx: u16,
    fy: u16,
    /// Bit 0: blend with the next pixel along; bit 1: with the one below.
    step: u8,
}

/// A pixel of the ring.
struct Px {
    /// Byte offset in the ring.
    at: u32,
    alpha: u8,
    /// Light caught in the curve, added to each colour.
    glint: u8,
    /// Where its red, green and blue come from.
    taps: [Tap; 3],
}

struct Map {
    shape: Shape,
    px: Vec<Px>,
}

/// Refreshes that find nothing new before a ring copied with GDI is looked
/// at less often (a GDI screen copy is slow; see `screen`).
const IDLE: u32 = 20;

thread_local! {
    static RINGS: RefCell<Vec<Ring>> = const { RefCell::new(Vec::new()) };
    /// Refreshes in a row with no ring on screen.
    static EMPTY: Cell<u32> = const { Cell::new(0) };
}

/// Shows (or moves) the ring of layered window `hwnd`, at `rect` (screen
/// px) with corners of `radius` px.
pub fn show(hwnd: HWND, rect: RECT, radius: i32, lens: Lens) {
    screen::poll();
    RINGS.with(|rings| {
        let mut rings = rings.borrow_mut();
        let i = match rings.iter().position(|r| r.hwnd == hwnd) {
            Some(i) => i,
            None => {
                rings.push(Ring {
                    hwnd,
                    rect,
                    radius,
                    lens,
                    capture: None,
                    out: None,
                    map: None,
                    frame: None,
                    seen: 0,
                    idle: 0,
                    skip: 0,
                });
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

/// Redraws every ring on screen whose background changed. With no ring on
/// screen for a few seconds (say, while a game is fullscreen), lets go of
/// the screens.
pub fn refresh() {
    let empty = RINGS.with(|rings| rings.borrow().is_empty());
    if empty {
        let n = EMPTY.get().saturating_add(1);
        EMPTY.set(n);
        if n == 60 {
            screen::release();
        }
        return;
    }
    EMPTY.set(0);
    screen::poll();
    RINGS.with(|rings| {
        for ring in rings.borrow_mut().iter_mut() {
            paint(ring, false);
        }
    });
}

/// Stops reading the screen (refraction was turned off).
pub fn stop() {
    screen::release();
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

/// Works out where each pixel of a `shape` ring gets its light from, in a
/// capture `cw` px wide with the ring `margin` px in from its corner. Done
/// once per shape, so a refresh only has to look the pixels up.
fn build(shape: &Shape, band: f32, bend: f32, split: f32, margin: i32, cw: i32) -> Vec<Px> {
    let Shape { w, h, valid, .. } = *shape;
    let (fw, fh) = (w as f32, h as f32);
    let r = (shape.radius as f32).min(fw.min(fh) / 2.0);
    let stride = cw as usize * 4;
    // Pixel centres of the part of the capture that is on a screen.
    let (vl, vt) = (valid.left as f32 + 0.5, valid.top as f32 + 0.5);
    let (vr, vb) = (valid.right as f32 - 0.5, valid.bottom as f32 - 0.5);
    // The capture at (x, y), kept to the part on a screen.
    let tap = |x: f32, y: f32| {
        let x = x.clamp(vl, vr) - 0.5;
        let y = y.clamp(vt, vb) - 0.5;
        let (x0, y0) = (x.floor().max(0.0), y.floor().max(0.0));
        let (ix, iy) = (x0 as i32, y0 as i32);
        let weight = |f: f32| (f.clamp(0.0, 1.0) * 256.0).round() as u16;
        let step = (ix + 1 < valid.right) as u8 | ((iy + 1 < valid.bottom) as u8) << 1;
        Tap { at: (iy as usize * stride + ix as usize * 4) as u32, fx: weight(x - x0), fy: weight(y - y0), step }
    };
    let mut px = Vec::new();
    // Rows clear of the curved corners and the top and bottom bands only
    // have a band at each end.
    let reach = (band + r).ceil() as i32 + 1;
    for y in 0..h {
        let mid = y >= reach && y < h - reach;
        let left = reach.min(w);
        let spans: [(i32, i32); 2] = if mid { [(0, left), ((w - reach).max(left), w)] } else { [(0, w), (w, w)] };
        for (x0, x1) in spans {
            for x in x0..x1 {
                let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                let (d, nx, ny) = edge(fx, fy, fw, fh, r);
                let inside = -d;
                if inside < -0.5 || inside >= band {
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
                let at = |k: f32| {
                    let d = to_edge + out * k;
                    tap(sx + nx * d, sy + ny * d)
                };
                // Light caught in the curve, most along its lit (upper) part:
                // what shows where the edge is over a plain background.
                let glint = 255.0 * 0.16 * t * t * t * (0.55 - 0.45 * ny);
                let cover = (inside + 0.5).clamp(0.0, 1.0);
                // Fade into the flat glass, where the ring shows what's behind as it is.
                let fade = ((band - inside) / (band * 0.25)).clamp(0.0, 1.0);
                let alpha = (cover * fade * 255.0).round() as u8;
                if alpha == 0 {
                    continue;
                }
                px.push(Px {
                    at: ((y * w + x) * 4) as u32,
                    alpha,
                    glint: glint.round().clamp(0.0, 255.0) as u8,
                    taps: [at(1.0 + split), at(1.0), at(1.0 - split)],
                });
            }
        }
    }
    px
}

/// Draws the ring's pixels `px` from the capture `src` (`stride` bytes a
/// row) into `dst`, premultiplied BGRA.
fn render(px: &[Px], src: &[u8], stride: usize, dst: &mut [u8]) {
    let sample = |t: &Tap, channel: usize| {
        let i = t.at as usize + channel;
        let right = if t.step & 1 != 0 { 4 } else { 0 };
        let down = if t.step & 2 != 0 { stride } else { 0 };
        let (fx, fy) = (t.fx as u32, t.fy as u32);
        let top = src[i] as u32 * (256 - fx) + src[i + right] as u32 * fx;
        let bottom = src[i + down] as u32 * (256 - fx) + src[i + down + right] as u32 * fx;
        (top * (256 - fy) + bottom * fy + (1 << 15)) >> 16
    };
    for p in px {
        let (red, green, blue) = (sample(&p.taps[0], 2), sample(&p.taps[1], 1), sample(&p.taps[2], 0));
        let a = p.alpha as u32;
        let c = |v: u32| (((v + p.glint as u32).min(255) * a + 127) / 255) as u8;
        let o = p.at as usize;
        dst[o..o + 4].copy_from_slice(&[c(blue), c(green), c(red), p.alpha]);
    }
}

fn paint(ring: &mut Ring, force: bool) {
    let (w, h) = (ring.rect.right - ring.rect.left, ring.rect.bottom - ring.rect.top);
    if w < 4 || h < 4 {
        return;
    }
    let dpi = match unsafe { GetDpiForWindow(ring.hwnd) } {
        0 => 96,
        d => d,
    };
    let scale = dpi as f32 / 96.0;
    let band = (ring.lens.band * scale).min(w.min(h) as f32 / 2.0).max(1.0);
    let bend = band * ring.lens.bend;
    let split = ring.lens.split;
    let margin = (bend * (1.0 + split)).ceil() as i32 + 3;
    let (cw, ch) = (w + 2 * margin, h + 2 * margin);
    let (cx, cy) = (ring.rect.left - margin, ring.rect.top - margin);
    let area = RECT { left: cx, top: cy, right: cx + cw, bottom: cy + ch };

    // Nothing to do if the screen round it can't have changed.
    let frame = screen::frame(area);
    if !force {
        match frame {
            Some(f) if ring.frame == Some(f) => return,
            Some(_) => {}
            None if ring.idle >= IDLE => {
                ring.skip = (ring.skip + 1) % 4;
                if ring.skip != 0 {
                    return;
                }
            }
            None => {}
        }
    }

    // What's around it: the strips just outside its edge, all the ring reads.
    if ring.capture.as_ref().is_none_or(|c| c.w != cw || c.h != ch) {
        ring.capture = Dib::new(cw, ch);
    }
    let Some(capture) = ring.capture.as_mut() else { return };
    let strips = [
        RECT { left: cx, top: cy, right: cx + cw, bottom: cy + margin },
        RECT { left: cx, top: cy + ch - margin, right: cx + cw, bottom: cy + ch },
        RECT { left: cx, top: cy + margin, right: cx + margin, bottom: cy + ch - margin },
        RECT { left: cx + cw - margin, top: cy + margin, right: cx + cw, bottom: cy + ch - margin },
    ];
    let read = frame.and_then(|_| screen::read(area, &strips, capture.pixels()));
    let valid = match read {
        Some((valid, f)) => {
            ring.frame = Some(f);
            valid
        }
        None => {
            // The slow way, for a screen that isn't duplicated.
            ring.frame = None;
            unsafe {
                let screen = GetDC(None);
                let _ = BitBlt(capture.dc, 0, 0, cw, ch, Some(screen), cx, cy, SRCCOPY);
                ReleaseDC(None, screen);
                let (vx, vy) = (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN));
                let (vw, vh) = (GetSystemMetrics(SM_CXVIRTUALSCREEN), GetSystemMetrics(SM_CYVIRTUALSCREEN));
                RECT {
                    left: vx.max(area.left),
                    top: vy.max(area.top),
                    right: (vx + vw).min(area.right),
                    bottom: (vy + vh).min(area.bottom),
                }
            }
        }
    };
    if valid.left >= valid.right || valid.top >= valid.bottom {
        return;
    }
    let src: &[u8] = capture.pixels();
    // FNV-1a over the strips: nothing to do if they didn't change. What's
    // inside is the panel and the ring themselves, which change with every
    // frame drawn.
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
        ring.idle = ring.idle.saturating_add(1);
        return;
    }
    ring.seen = hash;
    ring.idle = 0;

    if ring.out.as_ref().is_none_or(|o| o.w != w || o.h != h) {
        ring.out = Dib::new(w, h);
        ring.map = None;
    }
    let Some(out) = ring.out.as_mut() else { return };
    let local = RECT { left: valid.left - cx, top: valid.top - cy, right: valid.right - cx, bottom: valid.bottom - cy };
    let shape = Shape { w, h, radius: ring.radius, lens: ring.lens, dpi, valid: local };
    if ring.map.as_ref().is_none_or(|m| m.shape != shape) {
        // Pixels outside the ring stay clear from here on.
        out.pixels().fill(0);
        ring.map = Some(Map { shape, px: build(&shape, band, bend, split, margin, cw) });
    }
    let Some(map) = ring.map.as_ref() else { return };
    render(&map.px, src, cw as usize * 4, out.pixels());

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

    #[test]
    fn ring_over_a_plain_background() {
        // Over plain grey, every ring pixel is that grey plus its glint, at its alpha.
        let (w, h, margin) = (100, 40, 12);
        let (cw, ch) = (w + 2 * margin, h + 2 * margin);
        let lens = Lens { band: 10.0, bend: 0.75, split: 0.12 };
        let valid = RECT { left: 0, top: 0, right: cw, bottom: ch };
        let shape = Shape { w, h, radius: 8, lens, dpi: 96, valid };
        let px = build(&shape, 10.0, 7.5, 0.12, margin, cw);
        assert!(!px.is_empty());
        let src = vec![100u8; (cw * ch * 4) as usize];
        let mut dst = vec![0u8; (w * h * 4) as usize];
        render(&px, &src, cw as usize * 4, &mut dst);
        for p in &px {
            let o = p.at as usize;
            let want = (((100 + p.glint as u32).min(255) * p.alpha as u32 + 127) / 255) as u8;
            assert_eq!(&dst[o..o + 4], &[want, want, want, p.alpha]);
        }
        // The middle stays clear.
        let mid = ((h / 2 * w + w / 2) * 4) as usize;
        assert_eq!(&dst[mid..mid + 4], &[0, 0, 0, 0]);
    }
}
