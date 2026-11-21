//! Snapping by dragging, in shell mode: drag a window by its title bar to a
//! screen's left or right edge for that half, to a corner for that quarter,
//! or to the top to maximise it; a preview shows where it will go before you
//! let go. Explorer does this when it's the shell. If Windows has snapped the
//! window itself by the time it's let go, it's left as Windows put it.
//!
//! Dragged up to the top of the screen, Windows 11's snap layouts drop down
//! from there: halves, thirds, two thirds and one, a wide middle, a half and
//! two quarters, quarters. Let go over one of a layout's parts and the window
//! takes that part; let go at the very top (not over the layouts) and it's
//! maximised.
//!
//! While a window is being moved, a thread of its own follows the pointer
//! (and stops the moment the button is let go); nothing runs otherwise.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use windows::core::w;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config;
use crate::shellmode::{place, snapped, window_rect};

static ON: AtomicBool = AtomicBool::new(true);
static FOLLOWING: AtomicBool = AtomicBool::new(false);
static PREVIEW: AtomicIsize = AtomicIsize::new(0);

pub fn configure(on: bool) {
    ON.store(on, Ordering::Relaxed);
}

/// Where a window dropped at the pointer goes.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Zone {
    Left,
    Right,
    Top,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    /// A part of one of the snap layouts.
    Layout,
}

/// The snap layouts: each a set of parts (x, y, w, h as shares of the screen).
const LAYOUTS: [&[(f32, f32, f32, f32)]; 6] = [
    &[(0.0, 0.0, 0.5, 1.0), (0.5, 0.0, 0.5, 1.0)],
    &[(0.0, 0.0, 2.0 / 3.0, 1.0), (2.0 / 3.0, 0.0, 1.0 / 3.0, 1.0)],
    &[(0.0, 0.0, 1.0 / 3.0, 1.0), (1.0 / 3.0, 0.0, 1.0 / 3.0, 1.0), (2.0 / 3.0, 0.0, 1.0 / 3.0, 1.0)],
    &[(0.0, 0.0, 0.25, 1.0), (0.25, 0.0, 0.5, 1.0), (0.75, 0.0, 0.25, 1.0)],
    &[(0.0, 0.0, 0.5, 1.0), (0.5, 0.0, 0.5, 0.5), (0.5, 0.5, 0.5, 0.5)],
    &[(0.0, 0.0, 0.5, 0.5), (0.5, 0.0, 0.5, 0.5), (0.0, 0.5, 0.5, 0.5), (0.5, 0.5, 0.5, 0.5)],
];
/// One layout's picture in the strip, the gap round them, and the strip's
/// distance from the top (layout units).
const THUMB: (f32, f32) = (76.0, 48.0);
const PAD: f32 = 12.0;
const STRIP_TOP: f32 = 10.0;

/// The strip of layouts on a screen `m` at scale `k`: (x, y, w, h) in pixels.
fn strip_rect(m: RECT, k: f32) -> RECT {
    let w = ((PAD + LAYOUTS.len() as f32 * (THUMB.0 + PAD)) * k) as i32;
    let h = ((THUMB.1 + 2.0 * PAD) * k) as i32;
    let x = m.left + (m.right - m.left - w) / 2;
    let y = m.top + (STRIP_TOP * k) as i32;
    RECT { left: x, top: y, right: x + w, bottom: y + h }
}

/// Part `cell` of layout `l`'s picture, in the strip (layout units from its corner).
fn cell_rect(l: usize, cell: usize) -> (f32, f32, f32, f32) {
    let (cx, cy, cw, ch) = LAYOUTS[l][cell];
    let (x0, y0) = (PAD + l as f32 * (THUMB.0 + PAD), PAD);
    let g = 2.0;
    (x0 + cx * THUMB.0 + g / 2.0, y0 + cy * THUMB.1 + g / 2.0, cw * THUMB.0 - g, ch * THUMB.1 - g)
}

/// The layout part under the pointer at `p` (screen px) in the strip at `strip`.
fn cell_at(p: POINT, strip: RECT, k: f32) -> Option<(usize, usize)> {
    let (x, y) = ((p.x - strip.left) as f32 / k, (p.y - strip.top) as f32 / k);
    (0..LAYOUTS.len()).find_map(|l| {
        (0..LAYOUTS[l].len()).find_map(|c| {
            let (cx, cy, cw, ch) = cell_rect(l, c);
            (x >= cx - 1.0 && x < cx + cw + 1.0 && y >= cy - 1.0 && y < cy + ch + 1.0).then_some((l, c))
        })
    })
}

/// Where part `cell` of layout `l` is on a screen whose free area is `work`.
fn cell_target(l: usize, cell: usize, work: RECT) -> RECT {
    let (x, y, w, h) = LAYOUTS[l][cell];
    let (ww, wh) = ((work.right - work.left) as f32, (work.bottom - work.top) as f32);
    let r = |v: f32| v.round() as i32;
    RECT {
        left: work.left + r(x * ww),
        top: work.top + r(y * wh),
        right: work.left + r((x + w) * ww),
        bottom: work.top + r((y + h) * wh),
    }
}

/// The strip of layouts, while a window is dragged near the top: a
/// click-through layered window on the dragging thread.
struct Strip {
    hwnd: HWND,
    gfx: crate::gfx::Gfx,
    layer: crate::gfx::Layer,
    rect: RECT,
    k: f32,
    lit: Option<(usize, usize)>,
    shown: bool,
}

unsafe extern "system" fn strip_proc(hwnd: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if m == WM_NCHITTEST {
        return LRESULT(-1); // HTTRANSPARENT
    }
    DefWindowProcW(hwnd, m, wp, lp)
}

impl Strip {
    fn new() -> Option<Strip> {
        unsafe {
            let instance: HINSTANCE = windows::Win32::System::LibraryLoader::GetModuleHandleW(None).ok()?.into();
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(strip_proc),
                hInstance: instance,
                lpszClassName: w!("CascadeSnapLayouts"),
                ..Default::default()
            };
            RegisterClassExW(&wc);
            let ex = WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
            let hwnd = CreateWindowExW(
                ex,
                w!("CascadeSnapLayouts"),
                w!(""),
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                Some(instance),
                None,
            )
            .ok()?;
            let gfx = crate::gfx::Gfx::new().ok()?;
            Some(Strip { hwnd, gfx, layer: Default::default(), rect: RECT::default(), k: 1.0, lit: None, shown: false })
        }
    }

    /// Shows it on screen `m` with part `lit` lit (or hides it: `None`).
    fn show(&mut self, on: Option<(RECT, f32)>, lit: Option<(usize, usize)>) {
        let Some((m, k)) = on else {
            if self.shown {
                unsafe {
                    let _ = ShowWindow(self.hwnd, SW_HIDE);
                }
                self.shown = false;
            }
            return;
        };
        let rect = strip_rect(m, k);
        if self.shown && rect == self.rect && lit == self.lit {
            return;
        }
        (self.rect, self.k, self.lit) = (rect, k, lit);
        let (pw, ph) = (rect.right - rect.left, rect.bottom - rect.top);
        let (w, h) = (pw as f32 / k, ph as f32 / k);
        let accent = crate::theme::accent();
        if self.gfx.begin(&mut self.layer, pw, ph, k).is_ok() {
            {
                let p = self.gfx.painter();
                p.rrect(0.0, 0.0, w, h, 12.0, crate::gfx::Color(0.13, 0.13, 0.14, 0.94));
                p.stroke_rrect(0.5, 0.5, w - 1.0, h - 1.0, 12.0, crate::gfx::Color::white(0.10), 1.0);
                for (l, cells) in LAYOUTS.iter().enumerate() {
                    for c in 0..cells.len() {
                        let (x, y, cw, ch) = cell_rect(l, c);
                        let on = lit == Some((l, c));
                        let fill = if on { accent } else { crate::gfx::Color::white(0.16) };
                        p.rrect(x, y, cw, ch, 4.0, fill);
                    }
                }
            }
            let _ = self.gfx.present(&self.layer, self.hwnd, POINT { x: rect.left, y: rect.top }, pw, ph, 255);
        }
        if !self.shown {
            unsafe {
                let _ = SetWindowPos(
                    self.hwnd,
                    Some(HWND_TOPMOST),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
            }
            self.shown = true;
        }
    }
}

impl Drop for Strip {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

/// A screen's scaling.
fn scale_of(m: RECT) -> f32 {
    let mut dpi = (96u32, 96u32);
    unsafe {
        let _ = windows::Win32::UI::HiDpi::GetDpiForMonitor(
            MonitorFromRect(&m, MONITOR_DEFAULTTONEAREST),
            windows::Win32::UI::HiDpi::MDT_EFFECTIVE_DPI,
            &mut dpi.0,
            &mut dpi.1,
        );
    }
    dpi.0.max(96) as f32 / 96.0
}

/// How near an edge (px) counts as at it, and how much of an edge's length at
/// either end counts as its corner.
const EDGE: i32 = 2;
const CORNER: f32 = 0.18;

/// The zone for the pointer at `p` on a screen `m` (all of it).
fn zone_at(p: POINT, m: RECT) -> Option<Zone> {
    let (w, h) = ((m.right - m.left) as f32, (m.bottom - m.top) as f32);
    let fy = (p.y - m.top) as f32 / h.max(1.0);
    let fx = (p.x - m.left) as f32 / w.max(1.0);
    let left = p.x <= m.left + EDGE;
    let right = p.x >= m.right - 1 - EDGE;
    let top = p.y <= m.top + EDGE;
    match (left, right, top) {
        (true, _, _) if fy < CORNER => Some(Zone::TopLeft),
        (true, _, _) if fy > 1.0 - CORNER => Some(Zone::BottomLeft),
        (true, _, _) => Some(Zone::Left),
        (_, true, _) if fy < CORNER => Some(Zone::TopRight),
        (_, true, _) if fy > 1.0 - CORNER => Some(Zone::BottomRight),
        (_, true, _) => Some(Zone::Right),
        (_, _, true) if fx < CORNER => Some(Zone::TopLeft),
        (_, _, true) if fx > 1.0 - CORNER => Some(Zone::TopRight),
        (_, _, true) => Some(Zone::Top),
        _ => None,
    }
}

/// Where a window in `zone` goes on a screen whose free area is `work`.
fn target(zone: Zone, work: RECT) -> RECT {
    let mx = work.left + (work.right - work.left) / 2;
    let my = work.top + (work.bottom - work.top) / 2;
    match zone {
        Zone::Top => work,
        Zone::Left => RECT { right: mx, ..work },
        Zone::Right => RECT { left: mx, ..work },
        Zone::TopLeft => RECT { right: mx, bottom: my, ..work },
        Zone::TopRight => RECT { left: mx, bottom: my, ..work },
        Zone::BottomLeft => RECT { right: mx, top: my, ..work },
        Zone::BottomRight => RECT { left: mx, top: my, ..work },
        Zone::Layout => work,
    }
}

fn screen_at(p: POINT) -> Option<(RECT, RECT)> {
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    unsafe { GetMonitorInfoW(MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST), &mut mi).as_bool() }
        .then_some((mi.rcMonitor, mi.rcWork))
}

unsafe extern "system" fn preview_proc(hwnd: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if m == WM_NCHITTEST {
        return LRESULT(-1); // HTTRANSPARENT
    }
    DefWindowProcW(hwnd, m, wp, lp)
}

/// Makes the preview (a translucent, click-through pane in the accent colour). On the UI thread.
pub fn create(instance: HINSTANCE) {
    if PREVIEW.load(Ordering::Acquire) != 0 {
        return;
    }
    unsafe {
        let c = crate::theme::accent();
        let rgb = ((c.2 * 255.0) as u32) << 16 | ((c.1 * 255.0) as u32) << 8 | (c.0 * 255.0) as u32;
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(preview_proc),
            hInstance: instance,
            hbrBackground: CreateSolidBrush(COLORREF(rgb)),
            lpszClassName: w!("CascadeSnapPreview"),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let ex = WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
        if let Ok(h) = CreateWindowExW(
            ex,
            w!("CascadeSnapPreview"),
            w!(""),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance),
            None,
        ) {
            let _ = SetLayeredWindowAttributes(h, COLORREF(0), 70, LWA_ALPHA);
            PREVIEW.store(h.0 as isize, Ordering::Release);
        }
    }
}

fn show_preview(r: Option<RECT>) {
    let h = PREVIEW.load(Ordering::Acquire);
    if h == 0 {
        return;
    }
    let h = HWND(h as *mut c_void);
    unsafe {
        match r {
            Some(r) => {
                let inset = 6;
                let flags = SWP_NOACTIVATE | SWP_SHOWWINDOW;
                let _ = SetWindowPos(
                    h,
                    Some(HWND_TOPMOST),
                    r.left + inset,
                    r.top + inset,
                    r.right - r.left - 2 * inset,
                    r.bottom - r.top - 2 * inset,
                    flags,
                );
            }
            None => {
                let _ = ShowWindow(h, SW_HIDE);
            }
        }
    }
}

/// A window began to move or be resized (EVENT_SYSTEM_MOVESIZESTART).
pub fn on_start(window: HWND) {
    if !ON.load(Ordering::Relaxed) || !crate::shellmode::active() || FOLLOWING.swap(true, Ordering::AcqRel) {
        return;
    }
    let resizable = unsafe { GetWindowLongW(window, GWL_STYLE) as u32 & WS_THICKFRAME.0 != 0 };
    if !resizable || unsafe { IsZoomed(window).as_bool() } {
        FOLLOWING.store(false, Ordering::Release);
        return;
    }
    let start = window_rect(window);
    let window = window.0 as isize;
    std::thread::spawn(move || {
        let w = HWND(window as *mut c_void);
        let size = |r: RECT| (r.right - r.left, r.bottom - r.top);
        let mut zone: Option<(Zone, RECT)> = None;
        let mut strip: Option<Strip> = None;
        let mut strip_on = false;
        // Until the button is let go (moving or resizing ends with it).
        while unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) } as u16 & 0x8000 != 0 {
            // Its size changing means it's being resized, not moved: nothing to snap.
            if size(window_rect(w)) != size(start) {
                break;
            }
            let mut p = POINT::default();
            unsafe {
                let _ = GetCursorPos(&mut p);
            }
            let mut now = screen_at(p).and_then(|(m, work)| zone_at(p, m).map(|z| (z, target(z, work))));
            // Near the top, away from the corners: the layouts drop down, and
            // stay while the pointer's over them.
            if let Some((m, work)) = screen_at(p) {
                let k = scale_of(m);
                let sr = strip_rect(m, k);
                let fx = (p.x - m.left) as f32 / (m.right - m.left).max(1) as f32;
                let near_top = p.y <= m.top + (24.0 * k) as i32 && (0.2..0.8).contains(&fx);
                let over = p.x >= sr.left - 20 && p.x < sr.right + 20 && p.y < sr.bottom + (24.0 * k) as i32;
                strip_on = near_top || (strip_on && over);
                if strip_on {
                    let lit = cell_at(p, sr, k);
                    if let Some((l, c)) = lit {
                        now = Some((Zone::Layout, cell_target(l, c, work)));
                    }
                    if strip.is_none() {
                        strip = Strip::new();
                    }
                    if let Some(s) = strip.as_mut() {
                        s.show(Some((m, k)), lit);
                    }
                } else if let Some(s) = strip.as_mut() {
                    s.show(None, None);
                }
            }
            if now != zone {
                zone = now;
                show_preview(zone.map(|z| z.1));
            }
            std::thread::sleep(std::time::Duration::from_millis(16));
        }
        drop(strip);
        show_preview(None);
        if let Some((z, to)) = zone {
            // Give Windows a moment: if it snapped the window itself, leave it so.
            std::thread::sleep(std::time::Duration::from_millis(60));
            let now = window_rect(w);
            let windows_did = unsafe { IsZoomed(w).as_bool() } || size(now) != size(start);
            if !windows_did {
                if z == Zone::Top {
                    unsafe {
                        let _ = ShowWindow(w, SW_MAXIMIZE);
                    }
                } else {
                    let mut list = snapped();
                    list.retain(|e| e.0 != window);
                    list.push((window, start, to));
                    drop(list);
                    place(w, to);
                }
                config::log(&format!("snap: dragged to {z:?}"));
            }
        }
        FOLLOWING.store(false, Ordering::Release);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zones_by_edge() {
        let m = RECT { left: 0, top: 0, right: 1920, bottom: 1080 };
        let at = |x, y| zone_at(POINT { x, y }, m);
        assert_eq!(at(0, 540), Some(Zone::Left));
        assert_eq!(at(1919, 540), Some(Zone::Right));
        assert_eq!(at(960, 0), Some(Zone::Top));
        assert_eq!(at(0, 50), Some(Zone::TopLeft));
        assert_eq!(at(1919, 1050), Some(Zone::BottomRight));
        assert_eq!(at(100, 0), Some(Zone::TopLeft));
        assert_eq!(at(960, 540), None);
        let work = RECT { left: 0, top: 0, right: 1920, bottom: 1032 };
        assert_eq!(target(Zone::Left, work), RECT { left: 0, top: 0, right: 960, bottom: 1032 });
        assert_eq!(target(Zone::BottomRight, work), RECT { left: 960, top: 516, right: 1920, bottom: 1032 });
        // The layouts: a third, and the strip's parts found under the pointer.
        assert_eq!(cell_target(2, 1, work), RECT { left: 640, top: 0, right: 1280, bottom: 1032 });
        let strip = strip_rect(m, 1.0);
        let (x, y, cw, ch) = cell_rect(5, 3);
        let p = POINT { x: strip.left + (x + cw / 2.0) as i32, y: strip.top + (y + ch / 2.0) as i32 };
        assert_eq!(cell_at(p, strip, 1.0), Some((5, 3)));
        assert_eq!(cell_at(POINT { x: strip.left + 2, y: strip.top + 2 }, strip, 1.0), None);
    }
}
