//! Wobbly windows (Settings > Personalisation > Wobbly windows), as Linux
//! desktops have them: a window dragged about bends like jelly, the part
//! you hold following the pointer and the rest catching up, and it settles
//! with a wobble when you let go.
//!
//! While a window is dragged its picture (taken once as the drag starts)
//! is drawn over it on a see-through window, cut into a grid whose points
//! are springs: each lags where the window has gone, the farther from where
//! it's held the more, and swings back. The real window is see-through
//! meanwhile (it still moves, under it) and comes back once the wobble has
//! settled. Windows that draw themselves see-through already, maximised
//! ones and very big ones are left as they are.

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::time::{Duration, Instant};

use windows::core::w;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::gfx::{Gfx, Layer};

static ON: AtomicBool = AtomicBool::new(false);
/// The window wobbling now (0: none), and whether it's been let go.
static WINDOW: AtomicIsize = AtomicIsize::new(0);
static RELEASED: AtomicBool = AtomicBool::new(false);

/// How much windows wobble (1 as made) and how quickly they settle (the springs' damping).
static STRENGTH: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x3F80_0000);
static FRICTION: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x3EB3_3333);

pub fn configure(on: bool, strength: f32, friction: f32) {
    ON.store(on, Ordering::Relaxed);
    STRENGTH.store(strength.clamp(0.3, 2.0).to_bits(), Ordering::Relaxed);
    FRICTION.store(friction.clamp(0.1, 1.0).to_bits(), Ordering::Relaxed);
}

/// Grid cells across and down.
const N: usize = 8;
/// Room round the window for it to bend into (px).
const MARGIN: i32 = 90;

/// A window began moving (a drag by its title bar, or super drag).
pub fn start(window: HWND) {
    if !ON.load(Ordering::Relaxed) || WINDOW.load(Ordering::Acquire) != 0 {
        return;
    }
    unsafe {
        if IsZoomed(window).as_bool() || !IsWindowVisible(window).as_bool() {
            return;
        }
        let ex = GetWindowLongW(window, GWL_EXSTYLE) as u32;
        // Already see-through (it draws its own shape), or drawn by DirectComposition: left alone.
        if ex & (WS_EX_LAYERED.0 | WS_EX_NOREDIRECTIONBITMAP.0) != 0 {
            return;
        }
    }
    let r = rect_of(window);
    let (w, h) = (r.right - r.left, r.bottom - r.top);
    if w < 80 || h < 60 || (w as i64 * h as i64) > 3_600_000 {
        return;
    }
    let hv = window.0 as isize;
    WINDOW.store(hv, Ordering::Release);
    RELEASED.store(false, Ordering::Release);
    std::thread::spawn(move || {
        run(hv);
        WINDOW.store(0, Ordering::Release);
    });
}

/// The drag ended: the window settles, then comes back.
pub fn release() {
    RELEASED.store(true, Ordering::Release);
}

fn rect_of(h: HWND) -> RECT {
    let mut r = RECT::default();
    unsafe {
        let _ = GetWindowRect(h, &mut r);
    }
    r
}

/// The window's picture now (BGRA, opaque), as big as its window rect.
fn snapshot(h: HWND, w: i32, ht: i32) -> Option<Vec<u8>> {
    unsafe {
        let screen = GetDC(None);
        let dc = CreateCompatibleDC(Some(screen));
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -ht,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let bmp = CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
        let old = SelectObject(dc, bmp.into());
        // PW_RENDERFULLCONTENT: what DWM shows, for windows drawn with the GPU too.
        let ok = PrintWindow(h, dc, PRINT_WINDOW_FLAGS(2)).as_bool();
        let _ = GdiFlush();
        let mut px = std::slice::from_raw_parts(bits as *const u8, (w * ht * 4) as usize).to_vec();
        SelectObject(dc, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(dc);
        ReleaseDC(None, screen);
        if !ok {
            return None;
        }
        for a in px.iter_mut().skip(3).step_by(4) {
            *a = 255;
        }
        Some(px)
    }
}

unsafe extern "system" fn proc(hwnd: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    DefWindowProcW(hwnd, m, wp, lp)
}

/// Makes the real window see-through (or shows it again).
fn hide(h: HWND, hidden: bool) {
    unsafe {
        let ex = GetWindowLongW(h, GWL_EXSTYLE) as u32;
        if hidden {
            SetWindowLongW(h, GWL_EXSTYLE, (ex | WS_EX_LAYERED.0) as i32);
            let _ = SetLayeredWindowAttributes(h, COLORREF(0), 0, LWA_ALPHA);
        } else {
            let _ = SetLayeredWindowAttributes(h, COLORREF(0), 255, LWA_ALPHA);
            SetWindowLongW(h, GWL_EXSTYLE, (ex & !WS_EX_LAYERED.0) as i32);
            let _ = RedrawWindow(Some(h), None, None, RDW_INVALIDATE | RDW_ALLCHILDREN | RDW_FRAME);
        }
    }
}

fn run(hv: isize) {
    let window = HWND(hv as *mut _);
    let start = rect_of(window);
    let (w, h) = (start.right - start.left, start.bottom - start.top);
    let Some(px) = snapshot(window, w, h) else { return };
    let Ok(mut gfx) = Gfx::new() else { return };
    unsafe {
        let Ok(module) = GetModuleHandleW(None) else { return };
        let instance: HINSTANCE = module.into();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(proc),
            hInstance: instance,
            lpszClassName: w!("CascadeWobble"),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let ex = WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
        let Ok(over) = CreateWindowExW(
            ex,
            w!("CascadeWobble"),
            w!(""),
            WS_POPUP,
            start.left - MARGIN,
            start.top - MARGIN,
            1,
            1,
            None,
            None,
            Some(instance),
            None,
        ) else {
            return;
        };
        // The grid: each point's offset from where it rests, and its speed.
        let n = (N + 1) * (N + 1);
        let mut off = vec![(0.0f32, 0.0f32); n];
        let mut vel = vec![(0.0f32, 0.0f32); n];
        let mut cur = POINT::default();
        let _ = GetCursorPos(&mut cur);
        let grab =
            (((cur.x - start.left) as f32).clamp(0.0, w as f32), ((cur.y - start.top) as f32).clamp(0.0, h as f32));
        let rest = |i: usize| ((i % (N + 1)) as f32 * w as f32 / N as f32, (i / (N + 1)) as f32 * h as f32 / N as f32);
        let strength = f32::from_bits(STRENGTH.load(Ordering::Relaxed));
        let friction = f32::from_bits(FRICTION.load(Ordering::Relaxed));
        let far = (w as f32).hypot(h as f32);
        // Points near the hand follow quickly; far ones lag and swing.
        let stiff: Vec<f32> = (0..n)
            .map(|i| {
                let (x, y) = rest(i);
                let d = ((x - grab.0).hypot(y - grab.1) / far).clamp(0.0, 1.0);
                (180.0 + 900.0 * (1.0 - d).powi(3)) / strength
            })
            .collect();
        let mut layer = Layer::default();
        let mut last = start;
        let mut shown = false;
        let mut quiet_since: Option<Instant> = None;
        let mut bmp = None;
        let mut gen = u32::MAX;
        let mut t_prev = Instant::now();
        let began = Instant::now();
        loop {
            let now_r = rect_of(window);
            // Resized rather than moved: not a wobble.
            if now_r.right - now_r.left != w || now_r.bottom - now_r.top != h || !IsWindow(Some(window)).as_bool() {
                break;
            }
            let dt = t_prev.elapsed().as_secs_f32().min(1.0 / 30.0);
            t_prev = Instant::now();
            let (dx, dy) = ((now_r.left - last.left) as f32, (now_r.top - last.top) as f32);
            last = now_r;
            for o in off.iter_mut() {
                o.0 -= dx;
                o.1 -= dy;
            }
            // Springs back to rest, a little evened out with the neighbours.
            let mut moving = 0.0f32;
            for i in 0..n {
                let k = stiff[i];
                let c = 2.0 * k.sqrt() * friction;
                vel[i].0 += (-k * off[i].0 - c * vel[i].0) * dt;
                vel[i].1 += (-k * off[i].1 - c * vel[i].1) * dt;
            }
            for i in 0..n {
                off[i].0 += vel[i].0 * dt;
                off[i].1 += vel[i].1 * dt;
                moving = moving.max(off[i].0.abs() + off[i].1.abs() + 0.05 * (vel[i].0.abs() + vel[i].1.abs()));
            }
            let smooth = off.clone();
            for y in 1..N {
                for x in 1..N {
                    let i = y * (N + 1) + x;
                    let avg = |f: fn(&(f32, f32)) -> f32| {
                        (f(&smooth[i - 1]) + f(&smooth[i + 1]) + f(&smooth[i - N - 1]) + f(&smooth[i + N + 1])) / 4.0
                    };
                    off[i].0 += (avg(|p| p.0) - off[i].0) * 0.2;
                    off[i].1 += (avg(|p| p.1) - off[i].1) * 0.2;
                }
            }
            for o in off.iter_mut() {
                o.0 = o.0.clamp(-(MARGIN as f32) + 4.0, MARGIN as f32 - 4.0);
                o.1 = o.1.clamp(-(MARGIN as f32) + 4.0, MARGIN as f32 - 4.0);
            }
            // Drawn: each cell of the picture warped onto its moved corners.
            let (ow, oh) = (w + 2 * MARGIN, h + 2 * MARGIN);
            if gfx.begin(&mut layer, ow, oh, 1.0).is_ok() {
                if gfx.generation != gen || bmp.is_none() {
                    bmp = gfx.bitmap(w as u32, h as u32, &px).ok();
                    gen = gfx.generation;
                }
                {
                    let p = gfx.painter();
                    if let Some(b) = &bmp {
                        let m = MARGIN as f32;
                        let at = |i: usize| {
                            let (x, y) = rest(i);
                            (x + off[i].0 + m, y + off[i].1 + m)
                        };
                        // Each cell as two triangles, each stretched exactly onto
                        // its moved corners, so neighbours share their edges.
                        if let Some(brush) = p.bitmap_brush(b) {
                            p.aliased(|| {
                                for cy in 0..N {
                                    for cx in 0..N {
                                        let i = cy * (N + 1) + cx;
                                        let (i10, i01, i11) = (i + 1, i + N + 1, i + N + 2);
                                        p.textured_triangle(
                                            &brush,
                                            [rest(i), rest(i10), rest(i01)],
                                            [at(i), at(i10), at(i01)],
                                        );
                                        p.textured_triangle(
                                            &brush,
                                            [rest(i11), rest(i01), rest(i10)],
                                            [at(i11), at(i01), at(i10)],
                                        );
                                    }
                                }
                            });
                        }
                    }
                }
                let pos = POINT { x: now_r.left - MARGIN, y: now_r.top - MARGIN };
                let _ = gfx.present(&layer, over, pos, ow, oh, 255);
            }
            if !shown {
                let _ = ShowWindow(over, SW_SHOWNOACTIVATE);
                hide(window, true);
                shown = true;
            }
            // Let go and still (or too long at it): done.
            // Let go: the drag's end said so, or the button's up (super drag has no end event).
            let released = RELEASED.load(Ordering::Acquire)
                || windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(0x01) >= 0;
            if released && moving < 0.6 {
                if quiet_since.get_or_insert_with(Instant::now).elapsed() > Duration::from_millis(60) {
                    break;
                }
            } else {
                quiet_since = None;
            }
            if began.elapsed() > Duration::from_secs(120) || (released && began.elapsed() > Duration::from_secs(150)) {
                break;
            }
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                DispatchMessageW(&msg);
            }
            std::thread::sleep(Duration::from_millis(8));
        }
        if shown {
            hide(window, false);
        }
        let _ = DestroyWindow(over);
    }
}
