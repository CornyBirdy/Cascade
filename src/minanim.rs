//! Windows going into their taskbar button when minimised, and coming back
//! out of it when restored, where Windows won't animate it itself (its
//! "Animate windows when minimizing and maximizing" off, or not in shell
//! mode, where Windows animates towards Explorer's hidden taskbar instead).
//!
//! The window's own live picture (a DWM thumbnail, which Windows keeps even
//! for a minimised window) flies between its place and its button on a
//! see-through window over everything, shrinking and fading on the way in,
//! growing and fading out over the window on the way out. Each runs on a
//! thread of its own for a quarter of a second, and costs only DWM's
//! composition while it does.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use windows::core::{w, BOOL};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, GetMonitorInfoW, MonitorFromWindow, UnionRect, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

static ON: AtomicBool = AtomicBool::new(true);

/// Whether Cascade animates minimising and restoring (`min_animation`).
pub fn configure(on: bool) {
    ON.store(on, Ordering::Relaxed);
}

/// Whether Windows animates minimising itself (its animation setting on,
/// and Cascade the shell, so it asks Cascade where the button is).
pub fn windows_animates() -> bool {
    let mut info = ANIMATIONINFO { cbSize: std::mem::size_of::<ANIMATIONINFO>() as u32, iMinAnimate: 0 };
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETANIMATION,
            info.cbSize,
            Some(&mut info as *mut _ as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .is_ok()
    };
    ok && info.iMinAnimate != 0 && crate::shellmode::active()
}

/// Where `window` sits when not minimised (screen pixels).
pub fn normal_rect(window: HWND) -> Option<RECT> {
    unsafe {
        let mut wp = WINDOWPLACEMENT { length: std::mem::size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
        GetWindowPlacement(window, &mut wp).ok()?;
        let mut r = wp.rcNormalPosition;
        // Workspace coordinates: from the main screen's work area.
        let mut work = RECT::default();
        let _ = SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut work as *mut _ as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
        let (dx, dy) = (work.left, work.top);
        r.left += dx;
        r.right += dx;
        r.top += dy;
        r.bottom += dy;
        if wp.showCmd == SW_SHOWMAXIMIZED.0 as u32 || IsZoomed(window).as_bool() {
            // Maximised: the screen's work area it's on.
            let mon = MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST);
            let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            if GetMonitorInfoW(mon, &mut mi).as_bool() {
                r = mi.rcWork;
            }
        }
        (r.right - r.left > 20 && r.bottom - r.top > 20).then_some(r)
    }
}

unsafe extern "system" fn proc(hwnd: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    DefWindowProcW(hwnd, m, wp, lp)
}

fn ease_in(t: f32) -> f32 {
    t * t
}

fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t) * (1.0 - t)
}

/// Plays it: `window` from `place` into `button` (`into`), or out of it.
pub fn play(window: HWND, place: RECT, button: RECT, into: bool) {
    if !ON.load(Ordering::Relaxed) {
        return;
    }
    let w = window.0 as isize;
    std::thread::spawn(move || unsafe {
        let window = HWND(w as *mut _);
        let Ok(module) = GetModuleHandleW(None) else { return };
        let instance: HINSTANCE = module.into();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(proc),
            hInstance: instance,
            hbrBackground: CreateSolidBrush(COLORREF(0)),
            lpszClassName: w!("CascadeMinAnim"),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let mut area = RECT::default();
        let _ = UnionRect(&mut area, &place, &button);
        let ex = WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
        let Ok(over) = CreateWindowExW(
            ex,
            w!("CascadeMinAnim"),
            w!(""),
            WS_POPUP,
            area.left,
            area.top,
            area.right - area.left,
            area.bottom - area.top,
            None,
            None,
            Some(instance),
            None,
        ) else {
            return;
        };
        // Its own black is see-through: only the window's picture shows.
        let _ = SetLayeredWindowAttributes(over, COLORREF(0), 0, LWA_COLORKEY);
        let Ok(thumb) = DwmRegisterThumbnail(over, window) else {
            let _ = DestroyWindow(over);
            return;
        };
        let _ = ShowWindow(over, SW_SHOWNOACTIVATE);
        let dur = Duration::from_millis(if into { 240 } else { 220 });
        let began = Instant::now();
        loop {
            let t = (began.elapsed().as_secs_f32() / dur.as_secs_f32()).min(1.0);
            // Into the button: it speeds up as it goes, the sides closing in
            // a little faster than the top and bottom; out: the other way round.
            let (tp, ts) = if into { (ease_in(t), ease_in(t).sqrt().min(1.0)) } else { (ease_out(t), ease_out(t)) };
            let (a, b) = if into { (place, button) } else { (button, place) };
            let lerp = |x: i32, y: i32, k: f32| x as f32 + (y - x) as f32 * k;
            let (acx, acy) = ((a.left + a.right) as f32 / 2.0, (a.top + a.bottom) as f32 / 2.0);
            let (bcx, bcy) = ((b.left + b.right) as f32 / 2.0, (b.top + b.bottom) as f32 / 2.0);
            let cx = acx + (bcx - acx) * tp;
            let cy = acy + (bcy - acy) * tp;
            let w = lerp(a.right - a.left, b.right - b.left, ts);
            let h = lerp(a.bottom - a.top, b.bottom - b.top, tp);
            let rect = RECT {
                left: (cx - w / 2.0) as i32 - area.left,
                top: (cy - h / 2.0) as i32 - area.top,
                right: (cx + w / 2.0) as i32 - area.left,
                bottom: (cy + h / 2.0) as i32 - area.top,
            };
            let opacity = if into { 255.0 * (1.0 - 0.75 * t) } else { 220.0 * (1.0 - t * t) };
            let props = DWM_THUMBNAIL_PROPERTIES {
                dwFlags: DWM_TNP_RECTDESTINATION | DWM_TNP_OPACITY | DWM_TNP_VISIBLE | DWM_TNP_SOURCECLIENTAREAONLY,
                rcDestination: rect,
                opacity: opacity.clamp(0.0, 255.0) as u8,
                fVisible: BOOL(1),
                fSourceClientAreaOnly: BOOL(0),
                ..Default::default()
            };
            let _ = DwmUpdateThumbnailProperties(thumb, &props);
            let _ = DwmFlush();
            if t >= 1.0 {
                break;
            }
            // Messages for the overlay (there are none to speak of).
            let mut m = MSG::default();
            while PeekMessageW(&mut m, None, 0, 0, PM_REMOVE).as_bool() {
                DispatchMessageW(&m);
            }
        }
        let _ = DwmUnregisterThumbnail(thumb);
        let _ = DestroyWindow(over);
    });
}
