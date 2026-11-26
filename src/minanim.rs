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
use std::sync::Mutex;
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

/// Windows about to be restored from the taskbar, already parked off the
/// screen: the window, its placement before, and where it belongs (screen
/// pixels).
static PARKED: Mutex<Vec<(isize, WINDOWPLACEMENT, RECT)>> = Mutex::new(Vec::new());

/// Before a minimised `window` is restored: change where it will come back to,
/// off the screen, so Windows restores it out of sight. Without this the
/// restored window shows in its place for a frame before the animation takes
/// it off the screen to grow it out of its button (the flicker). The animation
/// (`play`) puts it back where it belongs when it lands, and if nothing plays
/// one within a moment the window is put back anyway. A window that comes back
/// maximised is parked as a normal window and maximised again when it's put
/// back (it would otherwise show for a moment before it was taken away).
pub fn park_for_restore(window: HWND) {
    if !ON.load(Ordering::Relaxed) {
        return;
    }
    unsafe {
        if !IsIconic(window).as_bool() {
            return;
        }
        let mut wp = WINDOWPLACEMENT { length: std::mem::size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
        if GetWindowPlacement(window, &mut wp).is_err() {
            return;
        }
        let Some(place) = normal_rect(window) else { return };
        let original = wp;
        // Not to come back maximised: it's maximised again once it's put back.
        wp.flags = WINDOWPLACEMENT_FLAGS(wp.flags.0 & !WPF_RESTORETOMAXIMIZED.0);
        wp.rcNormalPosition.right -= wp.rcNormalPosition.left + 32000;
        wp.rcNormalPosition.bottom -= wp.rcNormalPosition.top + 32000;
        wp.rcNormalPosition.left = -32000;
        wp.rcNormalPosition.top = -32000;
        if SetWindowPlacement(window, &wp).is_err() {
            return;
        }
        if let Ok(mut parked) = PARKED.lock() {
            parked.retain(|p| p.0 != window.0 as isize);
            parked.push((window.0 as isize, original, place));
        }
        let w = window.0 as isize;
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(900));
            unpark(HWND(w as *mut _));
        });
    }
}

/// Puts a window `park_for_restore` moved back where it belongs, if it's still
/// parked (the restore failed, or no animation came to do it).
pub fn unpark(window: HWND) {
    let Some((_, original, place)) = take_parked(window) else { return };
    unsafe {
        if IsIconic(window).as_bool() {
            let _ = SetWindowPlacement(window, &original);
        } else {
            land(window, &original, place);
        }
    }
}

/// Whether the window was to come back maximised.
fn comes_back_maximised(original: &WINDOWPLACEMENT) -> bool {
    original.flags.0 & WPF_RESTORETOMAXIMIZED.0 != 0
}

/// A parked window, restored, to where it belongs: moved back, or (it came
/// back maximised) maximised again with its normal place as it had it.
unsafe fn land(window: HWND, original: &WINDOWPLACEMENT, place: RECT) {
    if comes_back_maximised(original) {
        let mut wp = *original;
        wp.showCmd = SW_SHOWMAXIMIZED.0 as u32;
        wp.flags = WINDOWPLACEMENT_FLAGS(wp.flags.0 & !WPF_RESTORETOMAXIMIZED.0);
        let _ = SetWindowPlacement(window, &wp);
    } else {
        let flags = SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER;
        let _ = SetWindowPos(window, None, place.left, place.top, 0, 0, flags);
    }
}

fn take_parked(window: HWND) -> Option<(isize, WINDOWPLACEMENT, RECT)> {
    let mut parked = PARKED.lock().ok()?;
    let at = parked.iter().position(|p| p.0 == window.0 as isize)?;
    Some(parked.remove(at))
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
        // Coming out: wait for the window to be restored, then keep it off the
        // screen while its picture grows out of the button (a DWM thumbnail
        // still shows a window off the screen), and put it back at the end:
        // it's there the moment the picture lands, not before.
        let mut place = place;
        let mut hidden_at: Option<RECT> = None;
        // How a parked window is put back (it may have to be maximised again).
        let mut parked_from: Option<WINDOWPLACEMENT> = None;
        if !into {
            for _ in 0..25 {
                if !IsIconic(window).as_bool() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(4));
            }
            let mut r = RECT::default();
            if let Some((_, original, parked)) = take_parked(window) {
                // Restored out of sight already (`park_for_restore`): nothing
                // to move away, only to put back at the end.
                place = parked;
                hidden_at = Some(parked);
                parked_from = Some(original);
            } else if !IsIconic(window).as_bool() && GetWindowRect(window, &mut r).is_ok() && r.left > -10000 {
                place = r;
                let flags = SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER;
                if SetWindowPos(window, None, -32000, -32000, 0, 0, flags).is_ok() {
                    hidden_at = Some(r);
                }
            }
        }
        let put_back = || {
            if let Some(r) = hidden_at {
                match &parked_from {
                    Some(original) => land(window, original, r),
                    None => {
                        let flags = SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER;
                        let _ = SetWindowPos(window, None, r.left, r.top, 0, 0, flags);
                    }
                }
            }
        };
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
        let made = CreateWindowExW(
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
        );
        let Ok(over) = made else {
            put_back();
            return;
        };
        // Its own black is see-through: only the window's picture shows.
        let _ = SetLayeredWindowAttributes(over, COLORREF(0), 0, LWA_COLORKEY);
        let Ok(thumb) = DwmRegisterThumbnail(over, window) else {
            put_back();
            let _ = DestroyWindow(over);
            return;
        };
        // Nothing drawn until the first frame is placed: a thumbnail not told
        // where to go shows the window at its own size in the overlay's corner.
        let first = DWM_THUMBNAIL_PROPERTIES {
            dwFlags: DWM_TNP_RECTDESTINATION | DWM_TNP_OPACITY | DWM_TNP_VISIBLE,
            rcDestination: RECT { left: 0, top: 0, right: 1, bottom: 1 },
            opacity: 0,
            fVisible: BOOL(0),
            ..Default::default()
        };
        let _ = DwmUpdateThumbnailProperties(thumb, &first);
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
            // In: fading as it goes. Out: solid, the window itself under it at the end.
            let opacity = if into { 255.0 * (1.0 - 0.75 * t) } else { 140.0 + 115.0 * t.min(0.6) / 0.6 };
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
        // The window back in its place, drawn, before its picture goes.
        put_back();
        if hidden_at.is_some() {
            let _ = DwmFlush();
            let _ = DwmFlush();
        }
        let _ = DwmUnregisterThumbnail(thumb);
        let _ = DestroyWindow(over);
    });
}
