//! Shell mode: going to another of Cascade's desktops slides, as Windows'
//! own desktops do. The desktop you leave moves off to one side of each
//! screen while the one you go to comes in from the other (the next desktop
//! from the right, the one before from the left); the taskbar and the notch
//! stay where they are, over it.
//!
//! How: the screens are copied just before the switch and held up in a
//! window over everything but Cascade's own surfaces. The switch happens
//! under it; a moment later (once the windows that came back have drawn) the
//! screens are copied again, through that window (it's left out of screen
//! copies), and the two copies slide past each other. Then the window goes.
//! Two screen copies and a few hundred milliseconds of blits: nothing runs
//! between switches. Where Windows can't leave a window out of screen copies
//! (before Windows 10 2004) there's no slide.

use std::sync::Mutex;
use std::time::Instant;

use windows::core::{w, BOOL};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

/// How long the slide takes, and how long the new desktop gets to draw first.
const SLIDE_MS: f32 = 300.0;
const SETTLE_MS: u128 = 70;
const TIMER: usize = 1;

struct Slide {
    hwnd: HWND,
    /// The virtual screen's top left, and its size.
    origin: (i32, i32),
    w: i32,
    h: i32,
    /// Each screen, in window pixels.
    screens: Vec<RECT>,
    old: HBITMAP,
    new: Option<HBITMAP>,
    /// Going to the next desktop (it comes in from the right).
    forward: bool,
    opened: Instant,
    /// When the second copy was taken and the slide began.
    sliding: Option<Instant>,
}

unsafe impl Send for Slide {}

static SLIDE: Mutex<Option<Slide>> = Mutex::new(None);

fn slide() -> std::sync::MutexGuard<'static, Option<Slide>> {
    SLIDE.lock().unwrap_or_else(|e| e.into_inner())
}

/// A copy of the virtual screen as it is now.
unsafe fn copy_screen(origin: (i32, i32), w: i32, h: i32) -> Option<HBITMAP> {
    let screen = GetDC(None);
    let bitmap = CreateCompatibleBitmap(screen, w, h);
    if bitmap.is_invalid() {
        ReleaseDC(None, screen);
        return None;
    }
    let mem = CreateCompatibleDC(Some(screen));
    let old = SelectObject(mem, bitmap.into());
    let _ = BitBlt(mem, 0, 0, w, h, Some(screen), origin.0, origin.1, SRCCOPY | CAPTUREBLT);
    SelectObject(mem, old);
    let _ = DeleteDC(mem);
    ReleaseDC(None, screen);
    Some(bitmap)
}

unsafe extern "system" fn add_screen(_: HMONITOR, _: HDC, r: *mut RECT, out: LPARAM) -> BOOL {
    (*(out.0 as *mut Vec<RECT>)).push(*r);
    true.into()
}

unsafe extern "system" fn collect(h: HWND, out: LPARAM) -> BOOL {
    (*(out.0 as *mut Vec<HWND>)).push(h);
    true.into()
}

/// Puts Cascade's own always-on-top windows (the taskbar, the notch and
/// their glass) back over the slide, keeping their order.
unsafe fn raise_own(except: HWND) {
    let mut all: Vec<HWND> = Vec::new();
    let _ = EnumWindows(Some(collect), LPARAM(&mut all as *mut _ as isize));
    let own = windows::Win32::System::Threading::GetCurrentProcessId();
    // Bottom first, so each lands over the one before and the order holds.
    for &h in all.iter().rev() {
        if h == except || !IsWindowVisible(h).as_bool() {
            continue;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, Some(&mut pid));
        if pid != own || GetWindowLongW(h, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST.0 == 0 {
            continue;
        }
        let _ = SetWindowPos(h, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
    }
}

/// Holds the screens up as they are, before a switch to the next desktop
/// (`forward`) or the one before. Call it right before switching; the slide
/// plays by itself after.
pub fn begin(forward: bool) {
    finish();
    unsafe {
        let (x, y) = (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN));
        let (w, h) = (GetSystemMetrics(SM_CXVIRTUALSCREEN), GetSystemMetrics(SM_CYVIRTUALSCREEN));
        if w <= 0 || h <= 0 {
            return;
        }
        let Some(old) = copy_screen((x, y), w, h) else { return };
        let Ok(instance) = GetModuleHandleW(None) else {
            let _ = DeleteObject(old.into());
            return;
        };
        let class = w!("CascadeDeskSlide");
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(proc),
            hInstance: instance.into(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let ex = WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT;
        let Ok(hwnd) = CreateWindowExW(ex, class, w!(""), WS_POPUP, x, y, w, h, None, None, Some(instance.into()), None)
        else {
            let _ = DeleteObject(old.into());
            return;
        };
        // Left out of screen copies, so the second copy sees what's under it.
        if SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE).is_err() {
            let _ = DestroyWindow(hwnd);
            let _ = DeleteObject(old.into());
            return;
        }
        let mut screens: Vec<RECT> = Vec::new();
        let _ = EnumDisplayMonitors(None, None, Some(add_screen), LPARAM(&mut screens as *mut _ as isize));
        for r in &mut screens {
            *r = RECT { left: r.left - x, top: r.top - y, right: r.right - x, bottom: r.bottom - y };
        }
        *slide() = Some(Slide {
            hwnd,
            origin: (x, y),
            w,
            h,
            screens,
            old,
            new: None,
            forward,
            opened: Instant::now(),
            sliding: None,
        });
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let _ = UpdateWindow(hwnd);
        raise_own(hwnd);
        SetTimer(Some(hwnd), TIMER, 10, None);
    }
}

/// Ends a slide now (another switch came, or it's done).
fn finish() {
    let Some(s) = slide().take() else { return };
    unsafe {
        let _ = KillTimer(Some(s.hwnd), TIMER);
        let _ = DestroyWindow(s.hwnd);
        let _ = DeleteObject(s.old.into());
        if let Some(n) = s.new {
            let _ = DeleteObject(n.into());
        }
    }
}

/// Eases in and out (cubic), as Windows' own desktop slide does.
fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (2.0 - 2.0 * t).powi(3) / 2.0
    }
}

unsafe fn paint(dc: HDC, s: &Slide) {
    let src = CreateCompatibleDC(Some(dc));
    let keep = SelectObject(src, s.old.into());
    let t = s.sliding.map(|at| ease(at.elapsed().as_secs_f32() * 1000.0 / SLIDE_MS)).unwrap_or(0.0);
    match s.new {
        None => {
            let _ = BitBlt(dc, 0, 0, s.w, s.h, Some(src), 0, 0, SRCCOPY);
        }
        Some(new) => {
            let next = CreateCompatibleDC(Some(dc));
            let keep_next = SelectObject(next, new.into());
            for m in &s.screens {
                let (mw, mh) = (m.right - m.left, m.bottom - m.top);
                let off = ((mw as f32) * t).round() as i32;
                if s.forward {
                    // Out to the left, in from the right.
                    let _ = BitBlt(dc, m.left, m.top, mw - off, mh, Some(src), m.left + off, m.top, SRCCOPY);
                    let _ = BitBlt(dc, m.right - off, m.top, off, mh, Some(next), m.left, m.top, SRCCOPY);
                } else {
                    let _ = BitBlt(dc, m.left + off, m.top, mw - off, mh, Some(src), m.left, m.top, SRCCOPY);
                    let _ = BitBlt(dc, m.left, m.top, off, mh, Some(next), m.right - off, m.top, SRCCOPY);
                }
            }
            SelectObject(next, keep_next);
            let _ = DeleteDC(next);
        }
    }
    SelectObject(src, keep);
    let _ = DeleteDC(src);
}

unsafe extern "system" fn proc(hwnd: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match m {
        WM_ERASEBKGND => LRESULT(1),
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut ps);
            if let Some(s) = slide().as_ref().filter(|s| s.hwnd == hwnd) {
                paint(dc, s);
            }
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER => {
            let done = {
                let mut guard = slide();
                let Some(s) = guard.as_mut().filter(|s| s.hwnd == hwnd) else {
                    let _ = KillTimer(Some(hwnd), TIMER);
                    return LRESULT(0);
                };
                if s.new.is_none() && s.opened.elapsed().as_millis() >= SETTLE_MS {
                    // The desktop arrived at, as it has drawn by now.
                    s.new = copy_screen(s.origin, s.w, s.h);
                    if s.new.is_none() {
                        true
                    } else {
                        s.sliding = Some(Instant::now());
                        false
                    }
                } else {
                    s.sliding.is_some_and(|at| at.elapsed().as_secs_f32() * 1000.0 >= SLIDE_MS)
                }
            };
            if done {
                finish();
            } else {
                let _ = InvalidateRect(Some(hwnd), None, false);
                let _ = UpdateWindow(hwnd);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, m, wp, lp),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eases_from_start_to_end() {
        assert_eq!(ease(0.0), 0.0);
        assert!((ease(1.0) - 1.0).abs() < 1e-6);
        let mut last = 0.0;
        for i in 1..=20 {
            let v = ease(i as f32 / 20.0);
            assert!(v >= last, "not monotonic at {i}");
            last = v;
        }
    }
}
