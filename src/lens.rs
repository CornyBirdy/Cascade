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
//! Where the panel sits against the edge of the screen (the taskbar along
//! the bottom, the lip along the top) there is nothing outside to read, so
//! the ring fades out on that side and leaves the glass under it, instead of
//! smearing whatever it could reach.
//!
//! What's behind can change at any time, so the rings are refreshed a few
//! times a second while one is on screen (`refresh`), and only redrawn when
//! what's behind them did change. Only the strips just outside the panel
//! are copied from the screen (all a ring reads), and a ring whose
//! surroundings keep still is looked at less and less often, until they
//! change or the panel moves.
//!
//! Panels move and change size every frame while they open, close and
//! morph, and reading the screen and bending light for each of those frames
//! is what a ring can't afford. While a panel is on the move its ring is
//! only drawn as the glint along its edge (cheap, and right for any size),
//! and the bent light comes back once the panel has kept still for a moment.
//! Where each pixel of a ring reads from is worked out once per size (see
//! `lensmath`), so redrawing it is only reading and blending. A refresh also
//! stops after a few milliseconds and carries on with the next ring at the
//! next one, so many rings never hold the UI up together.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{COLORREF, HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::lensmath::{self, Geometry, Mesh};

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
    /// Refreshes in a row that found nothing changed, and how many more
    /// ticks to wait before looking again (see `refresh`).
    still: u32,
    wait: u32,
    /// Where each pixel reads its light from, for the size it was made for.
    mesh: Option<Mesh>,
    /// The pane moved or changed size at `moved_at`, and the ring has shown
    /// only its glint since (`glint`): the bent light is drawn once it has
    /// kept still for `MOVING`.
    moved_at: Instant,
    glint: bool,
    /// What the ring's bitmap holds right now is the glint alone.
    glint_shown: bool,
}

/// After this many unchanged looks a ring is looked at only every
/// `SLOW_EVERY` refreshes (a few times a second instead of 20), and after
/// `IDLE_AFTER` more, every `IDLE_EVERY`.
const STILL_AFTER: u32 = 10;
const SLOW_EVERY: u32 = 6;
const IDLE_AFTER: u32 = 40;
const IDLE_EVERY: u32 = 12;
/// How long a pane keeps still before its ring bends light again.
const MOVING: Duration = Duration::from_millis(120);
/// The most time a refresh spends before leaving the other rings to the next.
const BUDGET: Duration = Duration::from_millis(6);

thread_local! {
    static RINGS: RefCell<Vec<Ring>> = const { RefCell::new(Vec::new()) };
    /// Where the next refresh starts, when the last one ran out of time.
    static NEXT: Cell<usize> = const { Cell::new(0) };
}

/// Shows (or moves) the ring of layered window `hwnd`, at `rect` (screen
/// px) with corners of `radius` px.
pub fn show(hwnd: HWND, rect: RECT, radius: i32, lens: Lens) {
    RINGS.with(|rings| {
        let mut rings = rings.borrow_mut();
        let i = match rings.iter().position(|r| r.hwnd == hwnd) {
            Some(i) => i,
            None => {
                // Just shown: most likely on its way in, so the glint first.
                rings.push(Ring {
                    hwnd,
                    rect: RECT::default(),
                    radius,
                    lens,
                    capture: None,
                    out: None,
                    seen: 0,
                    still: 0,
                    wait: 0,
                    mesh: None,
                    moved_at: Instant::now(),
                    glint: true,
                    glint_shown: false,
                });
                rings.len() - 1
            }
        };
        let ring = &mut rings[i];
        let moved = ring.rect != rect || ring.radius != radius || ring.lens != lens;
        ring.rect = rect;
        ring.radius = radius;
        ring.lens = lens;
        if moved {
            // Back to looking every refresh once it's still, and only the
            // glint until then.
            ring.still = 0;
            ring.wait = 0;
            ring.moved_at = Instant::now();
            ring.glint = true;
            draw_glint(ring);
        } else if !ring.glint {
            paint(ring, false);
        }
    });
}

/// Whether any ring is on screen (the refresh timer only runs while one is).
pub fn active() -> bool {
    RINGS.with(|rings| !rings.borrow().is_empty())
}

/// Forgets the ring of `hwnd` (its window was hidden).
pub fn hide(hwnd: HWND) {
    RINGS.with(|rings| rings.borrow_mut().retain(|r| r.hwnd != hwnd));
}

/// Redraws every ring on screen whose background changed, and brings back
/// the bent light of rings whose pane has stopped moving. Rings that have
/// kept still for a while are only looked at every few calls, and it stops
/// early (carrying on there next time) rather than take long. False if
/// there's no ring on screen.
pub fn refresh() -> bool {
    RINGS.with(|rings| {
        let mut rings = rings.borrow_mut();
        let n = rings.len();
        let start = if n == 0 { 0 } else { NEXT.get() % n };
        let began = Instant::now();
        let mut next = start;
        for k in 0..n {
            let i = (start + k) % n;
            if k > 0 && began.elapsed() > BUDGET {
                next = i;
                break;
            }
            let ring = &mut rings[i];
            if ring.glint {
                if ring.moved_at.elapsed() >= MOVING {
                    ring.glint = false;
                    paint(ring, true);
                    ring.still = 0;
                    ring.wait = 0;
                }
                continue;
            }
            if ring.wait > 0 {
                ring.wait -= 1;
                continue;
            }
            if paint(ring, false) {
                ring.still = 0;
            } else {
                ring.still = ring.still.saturating_add(1);
                // Each ring waits a little differently, so they don't all
                // look at the screen on the same tick.
                let skew = (i % 3) as u32;
                if ring.still >= STILL_AFTER + IDLE_AFTER {
                    ring.wait = IDLE_EVERY - 1 + skew;
                } else if ring.still >= STILL_AFTER {
                    ring.wait = SLOW_EVERY - 1 + skew;
                }
            }
        }
        NEXT.set(next);
        n > 0
    })
}

/// Draws the ring as only the glint along its edge: nothing is read from the
/// screen and nothing is bent, so it's cheap enough for every frame of a pane
/// on the move, whatever size it is.
fn draw_glint(ring: &mut Ring) {
    let (w, h) = (ring.rect.right - ring.rect.left, ring.rect.bottom - ring.rect.top);
    if w < 4 || h < 4 {
        return;
    }
    if ring.out.as_ref().is_none_or(|o| o.w != w || o.h != h) {
        ring.out = Dib::new(w, h);
    }
    let Some(out) = ring.out.as_mut() else { return };
    lensmath::glint_only(w, h, ring.radius as f32, out.pixels());
    ring.glint_shown = true;
    present(ring);
}

/// Puts the ring's bitmap on its window.
fn present(ring: &Ring) {
    let Some(out) = ring.out.as_ref() else { return };
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
            Some(&SIZE { cx: out.w, cy: out.h }),
            Some(out.dc),
            Some(&POINT { x: 0, y: 0 }),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        );
    }
}

/// Reads what's around the ring and redraws it if that changed (or
/// `force`). True if it was redrawn.
fn paint(ring: &mut Ring, force: bool) -> bool {
    let (w, h) = (ring.rect.right - ring.rect.left, ring.rect.bottom - ring.rect.top);
    if w < 4 || h < 4 {
        return false;
    }
    let dpi = unsafe { GetDpiForWindow(ring.hwnd) };
    let scale = if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 };
    let band = (ring.lens.band * scale).min(w.min(h) as f32 / 2.0).max(1.0);
    let bend = band * ring.lens.bend;
    let split = ring.lens.split;
    let margin = (bend * (1.0 + split)).ceil() as i32 + 3;
    let (cw, ch) = (w + 2 * margin, h + 2 * margin);
    let (cx, cy) = (ring.rect.left - margin, ring.rect.top - margin);

    // What's just around it: the strips above, below and to either side.
    // Every pixel the ring shows comes from there; the middle is never read,
    // so it isn't copied (a screen copy costs the compositor a read back).
    if ring.capture.as_ref().is_none_or(|c| c.w != cw || c.h != ch) {
        ring.capture = Dib::new(cw, ch);
    }
    let Some(capture) = ring.capture.as_mut() else { return false };
    unsafe {
        let screen = GetDC(None);
        let strips = [
            (0, 0, cw, margin),
            (0, ch - margin, cw, margin),
            (0, margin, margin, ch - 2 * margin),
            (cw - margin, margin, margin, ch - 2 * margin),
        ];
        for (x, y, sw, sh) in strips {
            if sw > 0 && sh > 0 {
                let _ = BitBlt(capture.dc, x, y, sw, sh, Some(screen), cx + x, cy + y, SRCCOPY);
            }
        }
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
        return false;
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
    let geometry =
        Geometry { w, h, radius: ring.radius as f32, band, bend: ring.lens.bend, split, margin, cw, ch, valid };
    if ring.mesh.as_ref().is_none_or(|m| m.geometry != geometry) {
        ring.mesh = Some(lensmath::mesh(&geometry));
    }
    let Some(mesh) = ring.mesh.as_ref() else { return false };

    let fresh = ring.out.as_ref().is_none_or(|o| o.w != w || o.h != h);
    if fresh {
        ring.out = Dib::new(w, h);
    }
    let Some(out) = ring.out.as_mut() else { return false };
    if !fresh && ring.glint_shown {
        // Clear of the glint it was showing while the pane moved.
        out.pixels().fill(0);
    }
    lensmath::compose(mesh, src, out.pixels());
    ring.glint_shown = false;
    present(ring);
    true
}
