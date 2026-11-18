//! Snapping by dragging, in shell mode: drag a window by its title bar to a
//! screen's left or right edge for that half, to a corner for that quarter,
//! or to the top to maximise it; a preview shows where it will go before you
//! let go. Explorer does this when it's the shell. If Windows has snapped the
//! window itself by the time it's let go, it's left as Windows put it.
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
            let now = screen_at(p).and_then(|(m, work)| zone_at(p, m).map(|z| (z, target(z, work))));
            if now != zone {
                zone = now;
                show_preview(zone.map(|z| z.1));
            }
            std::thread::sleep(std::time::Duration::from_millis(16));
        }
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
    }
}
