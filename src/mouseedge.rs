//! Keeps the pointer's height true when it crosses between screens of
//! different sizes or sharpness (what LittleBigMouse does).
//!
//! Windows lines screens up by pixels, so on a 27" screen next to a 24"
//! one the pointer comes out at a different real-world height after the
//! jump. Here, the real size of each screen is read from the screen itself
//! (or typed in as inches), and as the pointer crosses an edge it is moved to
//! where it would be if the screens were lined up the way they really sit.
//!
//! No hook sees every mouse movement (that would wake Cascade thousands of
//! times a second with a fast mouse). Instead, a thin invisible strip sits
//! along each shared edge. It lets every click through, and Windows only asks
//! it a question when the pointer is inside it, so it costs nothing anywhere
//! else.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use windows::core::{w, BOOL};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_RAW_DPI};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::Config;

/// How deep the strip reaches into a screen, in pixels.
const STRIP: i32 = 24;

#[derive(Clone, Copy)]
struct Screen {
    rect: RECT,
    /// Pixels per millimetre.
    ppm_x: f32,
    ppm_y: f32,
}

#[derive(Clone, Copy, PartialEq)]
enum Align {
    Auto,
    Start,
    Middle,
    End,
}

struct Strip {
    hwnd: HWND,
    src: usize,
    dst: usize,
    /// The pointer crosses a vertical edge (screens side by side), so its
    /// height is what gets corrected. Otherwise its x is.
    side_by_side: bool,
    /// The edge it came through is the destination's left / top one.
    near_start: bool,
    /// Where the strip's depth is measured from (the destination's edge).
    edge: i32,
    align: Align,
}

#[derive(Default)]
struct State {
    screens: Vec<Screen>,
    strips: Vec<Strip>,
    last: Option<(isize, Instant)>,
    paused: bool,
    info: String,
}

thread_local! {
    static EDGE: RefCell<State> = RefCell::new(State::default());
}

unsafe extern "system" fn proc(hwnd: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match m {
        WM_NCHITTEST => {
            let x = (lp.0 & 0xFFFF) as i16 as i32;
            let y = ((lp.0 >> 16) & 0xFFFF) as i16 as i32;
            EDGE.with(|e| {
                if let Ok(mut e) = e.try_borrow_mut() {
                    e.hit(hwnd, x, y);
                }
            });
            // HTTRANSPARENT: the click goes to whatever is underneath.
            LRESULT(-1)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        _ => DefWindowProcW(hwnd, m, wp, lp),
    }
}

/// Registers the strips' window class (once, from the UI thread).
pub fn register(instance: HINSTANCE) {
    unsafe {
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(proc),
            hInstance: instance,
            lpszClassName: w!("CascadeEdge"),
            ..Default::default()
        };
        RegisterClassExW(&wc);
    }
}

unsafe extern "system" fn collect(h: HMONITOR, _: HDC, _: *mut RECT, out: LPARAM) -> BOOL {
    (*(out.0 as *mut Vec<HMONITOR>)).push(h);
    true.into()
}

/// Screens left to right, with their real-world density.
fn read_screens(inches: &[f32]) -> Vec<Screen> {
    let mut mons: Vec<HMONITOR> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(collect), LPARAM(&mut mons as *mut _ as isize));
    }
    let mut out: Vec<Screen> = mons
        .into_iter()
        .filter_map(|m| unsafe {
            let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            if !GetMonitorInfoW(m, &mut mi).as_bool() {
                return None;
            }
            let (mut dx, mut dy) = (96u32, 96u32);
            // The raw density comes from the screen's own size information.
            let known = GetDpiForMonitor(m, MDT_RAW_DPI, &mut dx, &mut dy).is_ok();
            let sane = |v: u32| (40..=600).contains(&v);
            let (dx, dy) = if known && sane(dx) && sane(dy) { (dx as f32, dy as f32) } else { (96.0, 96.0) };
            Some(Screen { rect: mi.rcMonitor, ppm_x: dx / 25.4, ppm_y: dy / 25.4 })
        })
        .collect();
    out.sort_by_key(|s| (s.rect.left, s.rect.top));
    // Typed-in sizes win over what the screens report.
    for (s, &d) in out.iter_mut().zip(inches) {
        let (w, h) = ((s.rect.right - s.rect.left) as f32, (s.rect.bottom - s.rect.top) as f32);
        if d > 4.0 && d < 120.0 && w > 0.0 && h > 0.0 {
            let ppm = w.hypot(h) / d / 25.4;
            s.ppm_x = ppm;
            s.ppm_y = ppm;
        }
    }
    out
}

fn describe(screens: &[Screen]) -> String {
    if screens.len() < 2 {
        return "Only one screen found.".to_string();
    }
    screens
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let (w, h) = ((s.rect.right - s.rect.left) as f32, (s.rect.bottom - s.rect.top) as f32);
            let inches = (w / s.ppm_x).hypot(h / s.ppm_y) / 25.4;
            format!("{}: {}x{}, {:.1} in", i + 1, w as i32, h as i32, inches)
        })
        .collect::<Vec<_>>()
        .join("   ")
}

fn parse_inches(text: &str) -> Vec<f32> {
    text.split(',').map(|t| t.trim().parse::<f32>().unwrap_or(0.0)).collect()
}

impl State {
    fn clear_strips(&mut self) {
        for s in self.strips.drain(..) {
            unsafe {
                let _ = DestroyWindow(s.hwnd);
            }
        }
        self.last = None;
    }

    fn make_strip(&mut self, rect: RECT, src: usize, dst: usize, side_by_side: bool, near_start: bool, edge: i32, align: Align) {
        unsafe {
            let Ok(instance) = GetModuleHandleW(None) else { return };
            let ex = WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
            let Ok(hwnd) = CreateWindowExW(
                ex,
                w!("CascadeEdge"),
                w!(""),
                WS_POPUP,
                rect.left,
                rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top,
                None,
                None,
                Some(instance.into()),
                None,
            ) else {
                return;
            };
            // Almost see-through, but not fully: a fully transparent window
            // isn't asked about the pointer at all.
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 1, LWA_ALPHA);
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                rect.left,
                rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            self.strips.push(Strip { hwnd, src, dst, side_by_side, near_start, edge, align });
        }
    }

    fn rebuild(&mut self, cfg: &Config) {
        self.clear_strips();
        self.screens = read_screens(&parse_inches(&cfg.screen_inches));
        self.info = describe(&self.screens);
        if !cfg.mouse_match || self.screens.len() < 2 {
            return;
        }
        let forced = match cfg.mouse_align.as_str() {
            "top" => Align::Start,
            "middle" => Align::Middle,
            "bottom" => Align::End,
            _ => Align::Auto,
        };
        let screens = self.screens.clone();
        for (s, a) in screens.iter().enumerate() {
            for (d, b) in screens.iter().enumerate() {
                if s == d {
                    continue;
                }
                let (ar, br) = (a.rect, b.rect);
                let near = |p: i32, q: i32| (p - q).abs() <= 1;
                // Side by side: the pointer leaves `a` through its right edge
                // into `b`'s left one (or the other way round).
                let (top, bottom) = (ar.top.max(br.top), ar.bottom.min(br.bottom));
                if bottom - top > 8 {
                    if near(ar.right, br.left) {
                        let rect = RECT { left: br.left, top, right: br.left + STRIP, bottom };
                        self.make_strip(rect, s, d, true, true, br.left, forced);
                    } else if near(ar.left, br.right) {
                        let rect = RECT { left: br.right - STRIP, top, right: br.right, bottom };
                        self.make_strip(rect, s, d, true, false, br.right - 1, forced);
                    }
                }
                // Stacked.
                let (left, right) = (ar.left.max(br.left), ar.right.min(br.right));
                if right - left > 8 {
                    if near(ar.bottom, br.top) {
                        let rect = RECT { left, top: br.top, right, bottom: br.top + STRIP };
                        self.make_strip(rect, s, d, false, true, br.top, forced);
                    } else if near(ar.top, br.bottom) {
                        let rect = RECT { left, top: br.bottom - STRIP, right, bottom: br.bottom };
                        self.make_strip(rect, s, d, false, false, br.bottom - 1, forced);
                    }
                }
            }
        }
        if self.paused {
            self.show_strips(false);
        }
    }

    fn show_strips(&self, show: bool) {
        for s in &self.strips {
            unsafe {
                let _ = ShowWindow(s.hwnd, if show { SW_SHOWNOACTIVATE } else { SW_HIDE });
            }
        }
    }

    /// The pointer is over one of the strips at (x, y).
    fn hit(&mut self, hwnd: HWND, x: i32, y: i32) {
        let Some(strip) = self.strips.iter().find(|s| s.hwnd == hwnd) else { return };
        let now = Instant::now();
        // The first look after a gap is the pointer arriving from the other
        // screen; later ones are it moving about inside the strip.
        let fresh = self.last.is_none_or(|(h, t)| h != hwnd.0 as isize || now.duration_since(t) > Duration::from_millis(120));
        self.last = Some((hwnd.0 as isize, now));
        if !fresh || self.paused {
            return;
        }
        let along = if strip.side_by_side { x } else { y };
        let depth = if strip.near_start { along - strip.edge } else { strip.edge - along };
        // Only a pointer that has just come through the edge, not one that
        // wandered in from the inside of this screen.
        if depth > STRIP / 2 {
            return;
        }
        let (a, b) = (self.screens[strip.src], self.screens[strip.dst]);
        let current = if strip.side_by_side { y } else { x };
        let target = remap(&a, &b, current, strip.side_by_side, strip.align);
        if (target - current).abs() < 2 {
            return;
        }
        unsafe {
            let _ = if strip.side_by_side { SetCursorPos(x, target) } else { SetCursorPos(target, y) };
        }
    }
}

/// Where `pos` (on screen `a`, along the edge) lands on screen `b`.
fn remap(a: &Screen, b: &Screen, pos: i32, side_by_side: bool, align: Align) -> i32 {
    // Along the edge: heights for side-by-side screens, widths for stacked ones.
    let span = |s: &Screen| if side_by_side { (s.rect.top, s.rect.bottom, s.ppm_y) } else { (s.rect.left, s.rect.right, s.ppm_x) };
    let (a_lo, a_hi, a_ppm) = span(a);
    let (b_lo, b_hi, b_ppm) = span(b);
    let align = match align {
        Align::Auto => {
            // Read it from how the screens are lined up in Windows' arrangement.
            let tol = 3;
            if (a_lo - b_lo).abs() <= tol {
                Align::Start
            } else if (a_hi - b_hi).abs() <= tol {
                Align::End
            } else {
                Align::Middle
            }
        }
        other => other,
    };
    // Millimetres from the line the screens are lined up on.
    let reference = |lo: i32, hi: i32, ppm: f32| match align {
        Align::Start | Align::Auto => 0.0,
        Align::Middle => (hi - lo) as f32 / ppm / 2.0,
        Align::End => (hi - lo) as f32 / ppm,
    };
    let mm = (pos - a_lo) as f32 / a_ppm - reference(a_lo, a_hi, a_ppm);
    let out = b_lo as f32 + (mm + reference(b_lo, b_hi, b_ppm)) * b_ppm;
    (out.round() as i32).clamp(b_lo, (b_hi - 1).max(b_lo))
}

/// Re-reads the screens and rebuilds the edge strips for the settings.
pub fn configure(cfg: &Config) {
    EDGE.with(|e| e.borrow_mut().rebuild(cfg));
}

/// Hides the strips (and ignores the pointer) while something is fullscreen.
pub fn pause(on: bool) {
    EDGE.with(|e| {
        let mut e = e.borrow_mut();
        if e.paused != on {
            e.paused = on;
            e.show_strips(!on);
        }
    });
}

/// "1: 3440x1440, 34.0 in   2: 1920x1080, 24.0 in".
pub fn info() -> String {
    EDGE.with(|e| e.borrow().info.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(left: i32, top: i32, w: i32, h: i32, ppm: f32) -> Screen {
        Screen { rect: RECT { left, top, right: left + w, bottom: top + h }, ppm_x: ppm, ppm_y: ppm }
    }

    #[test]
    fn same_screens_change_nothing() {
        let a = screen(0, 0, 1920, 1080, 4.0);
        let b = screen(1920, 0, 1920, 1080, 4.0);
        assert_eq!(remap(&a, &b, 500, true, Align::Auto), 500);
    }

    #[test]
    fn denser_screen_scales_the_height() {
        // Both lined up at the top; the second is twice as sharp, so a point
        // 400 px down the first is 800 px down the second.
        let a = screen(0, 0, 1920, 1080, 4.0);
        let b = screen(1920, 0, 2560, 1440, 8.0);
        assert_eq!(remap(&a, &b, 400, true, Align::Start), 800);
    }

    #[test]
    fn middle_alignment_uses_the_centre() {
        // Centres line up: the middle of one is the middle of the other.
        let a = screen(0, 0, 1920, 1080, 4.0);
        let b = screen(1920, -180, 2560, 1440, 8.0);
        assert_eq!(remap(&a, &b, 540, true, Align::Middle), -180 + 720);
    }

    #[test]
    fn stays_on_the_screen() {
        let a = screen(0, 0, 1920, 1080, 4.0);
        let b = screen(1920, 0, 1280, 720, 4.0);
        assert_eq!(remap(&a, &b, 1000, true, Align::Start), 719);
    }

    #[test]
    fn parses_inches() {
        assert_eq!(parse_inches("27, 24.5"), vec![27.0, 24.5]);
        assert_eq!(parse_inches(""), vec![0.0]);
    }
}
