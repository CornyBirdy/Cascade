//! The recorder's own windows, in Cascade's look (as Snip's bar): the
//! countdown before a recording starts, the bar while it runs, and the
//! preview that pops up when it's saved, as the Snipping Tool's.
//!
//! - **Countdown**: 3, 2, 1 in a glass disc over the middle of the area, a
//!   ring running down round it. Esc cancels, a click starts at once.
//! - **Bar**: a pill above the area with a pulsing red dot, the time, and
//!   Stop; drag it by anywhere but Stop. A thin red edge marks the area.
//!   Both are left out of the recording.
//! - **Preview**: a card sliding in at the bottom right with the last frame,
//!   how long and how big, and Play, Folder, Copy and Delete. It goes by
//!   itself after a while (not while the pointer is on it).
//!
//! Each runs a layered window on a thread of its own, drawn with Direct2D.

use std::cell::RefCell;
use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::time::Instant;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::gfx::{Align, Color, Gfx, Layer, Painter};

const WM_MOUSELEAVE: u32 = 0x02A3;

fn instance() -> HINSTANCE {
    unsafe { GetModuleHandleW(None).map(|m| HINSTANCE(m.0)).unwrap_or_default() }
}

pub(crate) fn register(class: PCWSTR, proc: WNDPROC) {
    unsafe {
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: proc,
            hInstance: instance(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassExW(&wc);
    }
}

/// The scale of the screen `r` is on (1.0 at 100%).
pub(crate) fn scale_at(r: &RECT) -> f32 {
    let mut dpi = (96u32, 96u32);
    unsafe {
        let _ =
            GetDpiForMonitor(MonitorFromRect(r, MONITOR_DEFAULTTONEAREST), MDT_EFFECTIVE_DPI, &mut dpi.0, &mut dpi.1);
    }
    dpi.0.max(96) as f32 / 96.0
}

pub(crate) fn work_area(r: &RECT) -> RECT {
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    unsafe {
        let _ = GetMonitorInfoW(MonitorFromRect(r, MONITOR_DEFAULTTONEAREST), &mut mi);
    }
    mi.rcWork
}

/// Leaves a window out of screen captures (the recording's included).
fn exclude(h: HWND) {
    unsafe {
        let _ = SetWindowDisplayAffinity(h, WDA_EXCLUDEFROMCAPTURE);
    }
}

pub(crate) fn ease(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

pub(crate) fn mix(a: Color, b: Color, t: f32) -> Color {
    Color(a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t, a.2 + (b.2 - a.2) * t, a.3 + (b.3 - a.3) * t)
}

/// Snip's glass: a soft shadow, the dark pane, a sheen from the top, a fine edge.
pub(crate) fn glass(p: &Painter, x: f32, y: f32, w: f32, h: f32, r: f32, a: f32) {
    for i in 1..=8 {
        let d = i as f32 * 1.5;
        p.rrect(x - d, y - d + 3.0, w + 2.0 * d, h + 2.0 * d, r + d, Color(0.0, 0.0, 0.0, 0.045 * a));
    }
    p.rrect(x, y, w, h, r, Color(0.11, 0.11, 0.12, 0.96 * a));
    p.gradient_rrect(
        x,
        y,
        w,
        h,
        r,
        &[(0.0, Color::white(0.07 * a)), (0.5, Color::white(0.015 * a)), (1.0, Color::white(0.0))],
    );
    p.stroke_rrect(x + 0.5, y + 0.5, w - 1.0, h - 1.0, (r - 0.5).max(0.0), Color::white(0.12 * a), 1.0);
}

pub(crate) fn glyph(p: &Painter, f: &crate::gfx::Fonts, code: u32, x: f32, y: f32, size: f32, c: Color) {
    if let (Some(ic), Some(ch)) = (&f.icons, char::from_u32(code)) {
        p.text(&ch.to_string(), ic, x, y, size, size, Align::Center, c);
    }
}

/// A layered window's drawing: its Direct2D, its pixels and its scale.
pub(crate) struct Surface {
    pub hwnd: HWND,
    pub gfx: Gfx,
    pub layer: Layer,
    pub k: f32,
}

impl Surface {
    /// Draws `w` x `h` (logical) with `draw` and shows it at `pos` (screen pixels).
    pub fn show(&mut self, pos: POINT, w: f32, h: f32, alpha: f32, draw: impl FnOnce(&Painter, &crate::gfx::Fonts)) {
        let (pw, ph) = ((w * self.k).ceil() as i32, (h * self.k).ceil() as i32);
        if self.gfx.begin(&mut self.layer, pw, ph, self.k).is_err() {
            return;
        }
        {
            let p = self.gfx.painter();
            draw(&p, &self.gfx.fonts);
        }
        let a = (alpha.clamp(0.0, 1.0) * 255.0) as u8;
        let _ = self.gfx.present(&self.layer, self.hwnd, pos, pw, ph, a);
    }

    fn pos(&self) -> POINT {
        let mut r = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut r);
        }
        POINT { x: r.left, y: r.top }
    }
}

pub(crate) fn layered(class: PCWSTR, ex: WINDOW_EX_STYLE, x: i32, y: i32) -> Option<HWND> {
    unsafe {
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | ex,
            class,
            w!(""),
            WS_POPUP,
            x,
            y,
            1,
            1,
            None,
            None,
            Some(instance()),
            None,
        )
        .ok()
    }
}

pub(crate) fn track_leave(hwnd: HWND) {
    let mut t = TRACKMOUSEEVENT {
        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
        dwFlags: TME_LEAVE,
        hwndTrack: hwnd,
        dwHoverTime: 0,
    };
    unsafe {
        let _ = TrackMouseEvent(&mut t);
    }
}

/// A mouse message's position in logical units.
pub(crate) fn at(lp: LPARAM, k: f32) -> (f32, f32) {
    ((lp.0 & 0xFFFF) as i16 as f32 / k, ((lp.0 >> 16) & 0xFFFF) as i16 as f32 / k)
}

// ------------------------------------------------------------ countdown --

const DISC: f32 = 300.0;

struct Count {
    s: Surface,
    began: Instant,
    secs: u32,
    pos: POINT,
    done: Option<bool>,
}

thread_local! {
    static COUNT: RefCell<Option<Count>> = const { RefCell::new(None) };
}

fn draw_count(c: &mut Count) {
    let t = c.began.elapsed().as_secs_f32();
    let total = c.secs as f32;
    if t >= total {
        c.done = Some(true);
        return;
    }
    let left = total - t;
    let n = left.ceil() as u32;
    let phase = t.fract();
    let pop = 1.0 + 0.35 * (1.0 - ease(phase * 4.0));
    let fade_in = ease(t / 0.25);
    let accent = crate::theme::accent();
    let pos = c.pos;
    c.s.show(pos, DISC, DISC, fade_in, |p, f| {
        let (cx, cy) = (DISC / 2.0, DISC / 2.0 - 16.0);
        let r = 92.0;
        glass(p, cx - r, cy - r, 2.0 * r, 2.0 * r, r, 1.0);
        p.stroke_circle(cx, cy, r - 10.0, Color::white(0.08), 4.0);
        let sweep = 360.0 * left / total;
        p.arc(cx, cy, r - 10.0, 0.0, sweep, accent, 4.0);
        let a = ease(phase * 5.0);
        p.scaled(2.8 * pop, cx, cy, || {
            p.text(&n.to_string(), &f.big, cx - 40.0, cy - 22.0, 80.0, 44.0, Align::Center, Color::white(a));
        });
        let hint = "Esc cancels \u{b7} click to start now";
        let hw = p.measure(hint, &f.small) + 28.0;
        let (hx, hy) = (cx - hw / 2.0, cy + r + 18.0);
        p.rrect(hx, hy, hw, 26.0, 13.0, Color(0.08, 0.08, 0.09, 0.85));
        p.text(hint, &f.small, hx, hy, hw, 26.0, Align::Center, Color::white(0.8));
    });
}

unsafe extern "system" fn count_proc(hwnd: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let set = |v: bool| {
        COUNT.with(|c| {
            if let Some(c) = c.borrow_mut().as_mut() {
                c.done.get_or_insert(v);
            }
        })
    };
    match m {
        WM_TIMER => {
            COUNT.with(|c| {
                if let Ok(mut c) = c.try_borrow_mut() {
                    if let Some(c) = c.as_mut() {
                        draw_count(c);
                    }
                }
            });
            LRESULT(0)
        }
        WM_KEYDOWN if wp.0 == VK_ESCAPE.0 as usize => {
            set(false);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            set(true);
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            set(false);
            LRESULT(0)
        }
        WM_SETCURSOR => {
            SetCursor(LoadCursorW(None, IDC_HAND).ok());
            LRESULT(1)
        }
        _ => DefWindowProcW(hwnd, m, wp, lp),
    }
}

/// Counts down `secs` over the middle of `area` (screen pixels). False if
/// it was cancelled (Esc or a right-click); a click ends it early. Runs on
/// the calling thread; the window is gone when it returns.
pub fn countdown(area: RECT, secs: u32) -> bool {
    if secs == 0 {
        return true;
    }
    register(w!("CascadeRecordCount"), Some(count_proc));
    let k = scale_at(&area);
    let size = (DISC * k) as i32;
    let pos = POINT { x: (area.left + area.right) / 2 - size / 2, y: (area.top + area.bottom) / 2 - size / 2 };
    let Some(hwnd) = layered(w!("CascadeRecordCount"), WINDOW_EX_STYLE(0), pos.x, pos.y) else { return true };
    exclude(hwnd);
    let Ok(gfx) = Gfx::new() else {
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
        return true;
    };
    COUNT.with(|c| {
        *c.borrow_mut() = Some(Count {
            s: Surface { hwnd, gfx, layer: Layer::default(), k },
            began: Instant::now(),
            secs,
            pos,
            done: None,
        })
    });
    unsafe {
        COUNT.with(|c| {
            if let Some(c) = c.borrow_mut().as_mut() {
                draw_count(c);
            }
        });
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(hwnd));
        SetTimer(Some(hwnd), 1, 16, None);
        let mut m = MSG::default();
        loop {
            if let Some(done) = COUNT.with(|c| c.borrow().as_ref().and_then(|c| c.done)) {
                let _ = DestroyWindow(hwnd);
                COUNT.with(|c| c.borrow_mut().take());
                return done;
            }
            if GetMessageW(&mut m, None, 0, 0).0 <= 0 {
                break;
            }
            let _ = TranslateMessage(&m);
            DispatchMessageW(&m);
        }
        let _ = DestroyWindow(hwnd);
    }
    COUNT.with(|c| c.borrow_mut().take());
    true
}

// ------------------------------------------------------------------ bar --

const BAR_W: f32 = 228.0;
const BAR_H: f32 = 48.0;
pub(crate) const SHADOW: f32 = 14.0;

static BAR: AtomicIsize = AtomicIsize::new(0);

struct Bar {
    s: Surface,
    began: Instant,
    hot: bool,
    lit: f32,
    last: Instant,
}

thread_local! {
    static BAR_STATE: RefCell<Option<Bar>> = const { RefCell::new(None) };
}

/// Stop's place on the bar (logical, in the window).
fn stop_rect() -> (f32, f32, f32, f32) {
    (SHADOW + BAR_W - 104.0, SHADOW + 8.0, 96.0, BAR_H - 16.0)
}

fn draw_bar(b: &mut Bar) {
    let dt = b.last.elapsed().as_secs_f32().min(0.05);
    b.last = Instant::now();
    b.lit += ((if b.hot { 1.0 } else { 0.0 }) - b.lit) * (1.0 - (-18.0 * dt).exp());
    let secs = crate::recorder::elapsed() as u32;
    let pulse = 0.55 + 0.45 * (b.began.elapsed().as_secs_f32() * 3.0).sin().abs();
    let lit = b.lit;
    let pos = b.s.pos();
    let (ww, wh) = (BAR_W + 2.0 * SHADOW, BAR_H + 2.0 * SHADOW);
    let fade = ease(b.began.elapsed().as_secs_f32() / 0.25);
    b.s.show(pos, ww, wh, fade, |p, f| {
        let (x, y) = (SHADOW, SHADOW);
        glass(p, x, y, BAR_W, BAR_H, BAR_H / 2.0, 1.0);
        let red = Color(0.93, 0.25, 0.22, 1.0);
        let (dx, dy) = (x + 24.0, y + BAR_H / 2.0);
        p.circle(dx, dy, 11.0, red.alpha(0.18 * pulse));
        p.circle(dx, dy, 6.0, red.alpha(0.6 + 0.4 * pulse));
        let clock = format!("{:02}:{:02}", secs / 60, secs % 60);
        p.text(&clock, &f.title, x + 42.0, y, 70.0, BAR_H, Align::Left, Color::white(0.95));
        let (sx, sy, sw, sh) = stop_rect();
        p.line(sx - 10.0, sy + 6.0, sx - 10.0, sy + sh - 6.0, Color::white(0.14), 1.0);
        let fill = mix(Color::white(0.09), Color(0.85, 0.2, 0.16, 0.95), lit);
        p.rrect(sx, sy, sw, sh, sh / 2.0, fill);
        p.rrect(sx + 18.0, sy + sh / 2.0 - 5.0, 10.0, 10.0, 2.0, Color::white(0.95));
        p.text("Stop", &f.body, sx + 36.0, sy, sw - 40.0, sh, Align::Left, Color::white(0.95));
    });
}

unsafe extern "system" fn bar_proc(hwnd: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let k = BAR_STATE.with(|b| b.borrow().as_ref().map_or(1.0, |b| b.s.k));
    let over_stop = |x: f32, y: f32| {
        let (sx, sy, sw, sh) = stop_rect();
        x >= sx && x < sx + sw && y >= sy && y < sy + sh
    };
    match m {
        WM_TIMER => {
            BAR_STATE.with(|b| {
                if let Ok(mut b) = b.try_borrow_mut() {
                    if let Some(b) = b.as_mut() {
                        draw_bar(b);
                    }
                }
            });
            LRESULT(0)
        }
        WM_NCHITTEST => {
            // Dragged by anywhere but Stop.
            let mut pt = POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 };
            let _ = ScreenToClient(hwnd, &mut pt);
            if over_stop(pt.x as f32 / k, pt.y as f32 / k) {
                LRESULT(HTCLIENT as isize)
            } else {
                LRESULT(HTCAPTION as isize)
            }
        }
        WM_MOUSEMOVE => {
            let (x, y) = at(lp, k);
            BAR_STATE.with(|b| {
                if let Some(b) = b.borrow_mut().as_mut() {
                    b.hot = over_stop(x, y);
                }
            });
            track_leave(hwnd);
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            BAR_STATE.with(|b| {
                if let Some(b) = b.borrow_mut().as_mut() {
                    b.hot = false;
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = at(lp, k);
            if over_stop(x, y) {
                crate::recorder::stop();
            }
            LRESULT(0)
        }
        WM_SETCURSOR if (lp.0 & 0xFFFF) as u32 == HTCLIENT => {
            SetCursor(LoadCursorW(None, IDC_HAND).ok());
            LRESULT(1)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, m, wp, lp),
    }
}

unsafe extern "system" fn edge_proc(hwnd: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    DefWindowProcW(hwnd, m, wp, lp)
}

/// Closes the bar (from any thread).
pub fn close_bar() {
    let bar = BAR.swap(0, Ordering::AcqRel);
    if bar != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(bar as *mut c_void)), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

/// The bar above `area` (inside its top if there's no room) and a thin red
/// edge round it; runs until `close_bar`.
pub fn bar(area: RECT) {
    register(w!("CascadeRecordBar"), Some(bar_proc));
    unsafe {
        let edge = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(edge_proc),
            hInstance: instance(),
            hbrBackground: CreateSolidBrush(COLORREF(0x003C3CE8)),
            lpszClassName: w!("CascadeRecordEdge"),
            ..Default::default()
        };
        RegisterClassExW(&edge);
    }
    let k = scale_at(&area);
    let (pw, ph) = (((BAR_W + 2.0 * SHADOW) * k) as i32, ((BAR_H + 2.0 * SHADOW) * k) as i32);
    let x = (area.left + area.right) / 2 - pw / 2;
    let above = area.top - ph;
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    unsafe {
        let _ = GetMonitorInfoW(MonitorFromRect(&area, MONITOR_DEFAULTTONEAREST), &mut mi);
    }
    let y = if above >= mi.rcMonitor.top { above } else { area.top + 4 };
    let Some(hwnd) = layered(w!("CascadeRecordBar"), WS_EX_NOACTIVATE, x, y) else { return };
    exclude(hwnd);
    let Ok(gfx) = Gfx::new() else { return };
    BAR.store(hwnd.0 as isize, Ordering::Release);
    BAR_STATE.with(|b| {
        *b.borrow_mut() = Some(Bar {
            s: Surface { hwnd, gfx, layer: Layer::default(), k },
            began: Instant::now(),
            hot: false,
            lit: 0.0,
            last: Instant::now(),
        })
    });
    unsafe {
        BAR_STATE.with(|b| {
            if let Some(b) = b.borrow_mut().as_mut() {
                draw_bar(b);
            }
        });
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetTimer(Some(hwnd), 1, 33, None);
        // The edge: four thin strips just outside the area, clicks going through them.
        let t = 2;
        let strips = [
            (area.left - t, area.top - t, area.right - area.left + 2 * t, t),
            (area.left - t, area.bottom, area.right - area.left + 2 * t, t),
            (area.left - t, area.top, t, area.bottom - area.top),
            (area.right, area.top, t, area.bottom - area.top),
        ];
        for (sx, sy, sw, sh) in strips {
            let ex = WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT | WS_EX_LAYERED;
            if let Ok(e) = CreateWindowExW(
                ex,
                w!("CascadeRecordEdge"),
                w!(""),
                WS_POPUP | WS_VISIBLE,
                sx,
                sy,
                sw,
                sh,
                Some(hwnd),
                None,
                Some(instance()),
                None,
            ) {
                let _ = SetLayeredWindowAttributes(e, COLORREF(0), 200, LWA_ALPHA);
                exclude(e);
            }
        }
        let mut m = MSG::default();
        while GetMessageW(&mut m, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&m);
            DispatchMessageW(&m);
        }
    }
    BAR_STATE.with(|b| b.borrow_mut().take());
}

// -------------------------------------------------------------- preview --

/// The recording's last frame, small: BGRA, opaque.
pub struct Thumb {
    pub w: u32,
    pub h: u32,
    pub px: Vec<u8>,
}

impl Thumb {
    /// A copy of a BGRx frame (`w` x `h`, top row first) at most `max_w` wide.
    pub fn from_frame(bits: &[u8], w: u32, h: u32, max_w: u32) -> Option<Thumb> {
        if w == 0 || h == 0 || bits.len() < (w * h * 4) as usize {
            return None;
        }
        let tw = w.min(max_w);
        let th = ((h as u64 * tw as u64) / w as u64).max(1) as u32;
        let mut px = vec![0u8; (tw * th * 4) as usize];
        for y in 0..th {
            let sy = (y as u64 * h as u64 / th as u64) as u32;
            for x in 0..tw {
                let sx = (x as u64 * w as u64 / tw as u64) as u32;
                let s = ((sy * w + sx) * 4) as usize;
                let d = ((y * tw + x) * 4) as usize;
                px[d..d + 3].copy_from_slice(&bits[s..s + 3]);
                px[d + 3] = 255;
            }
        }
        Some(Thumb { w: tw, h: th, px })
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Pv {
    Thumb,
    Close,
    Play,
    Folder,
    Copy,
    Delete,
}

const CARD_W: f32 = 344.0;
/// How long the preview stays (not counting while the pointer's on it).
const STAY: f32 = 10.0;

struct Preview {
    s: Surface,
    path: PathBuf,
    /// A screenshot's picture (BGRA, its size): Edit opens it in the editor.
    /// None for a recording, which plays.
    still: Option<(Vec<u32>, i32, i32)>,
    thumb: Option<Thumb>,
    bmp: Option<(u32, ID2D1Bitmap)>,
    detail: String,
    shown: Instant,
    last: Instant,
    /// Seconds left before it goes by itself.
    left: f32,
    hot: Option<Pv>,
    inside: bool,
    lit: [f32; 6],
    copied: Option<Instant>,
    closing: Option<Instant>,
    corner: POINT,
    hits: Vec<(f32, f32, f32, f32, Pv)>,
}

thread_local! {
    static PREVIEW: RefCell<Option<Preview>> = const { RefCell::new(None) };
}

fn thumb_h(p: &Preview) -> f32 {
    let w = CARD_W - 24.0;
    p.thumb.as_ref().map_or(150.0, |t| (w * t.h as f32 / t.w as f32).clamp(110.0, 220.0))
}

fn card_h(p: &Preview) -> f32 {
    12.0 + thumb_h(p) + 12.0 + 44.0 + 40.0 + 14.0
}

fn draw_preview(pv: &mut Preview) -> bool {
    let dt = pv.last.elapsed().as_secs_f32().min(0.05);
    pv.last = Instant::now();
    if !pv.inside && pv.closing.is_none() {
        pv.left -= dt;
        if pv.left <= 0.0 {
            pv.closing = Some(Instant::now());
        }
    }
    let out = pv.closing.map_or(0.0, |t| ease(t.elapsed().as_secs_f32() / 0.2));
    if out >= 1.0 {
        return false;
    }
    let order = [Pv::Thumb, Pv::Close, Pv::Play, Pv::Folder, Pv::Copy, Pv::Delete];
    for (i, b) in order.iter().enumerate() {
        let to = if pv.hot == Some(*b) { 1.0 } else { 0.0 };
        pv.lit[i] += (to - pv.lit[i]) * (1.0 - (-18.0 * dt).exp());
    }
    let ent = ease(pv.shown.elapsed().as_secs_f32() / 0.3);
    let k = pv.s.k;
    let (cw, ch) = (CARD_W, card_h(pv));
    let (ww, wh) = (cw + 2.0 * SHADOW, ch + 2.0 * SHADOW);
    let slide = ((1.0 - ent) * 48.0 + out * 24.0) * k;
    let pos = POINT { x: pv.corner.x - (ww * k) as i32 + slide as i32, y: pv.corner.y - (wh * k) as i32 };
    let th = thumb_h(pv);
    let gen = pv.s.gfx.generation;
    let need_bmp = pv.thumb.is_some() && pv.bmp.as_ref().is_none_or(|(g, _)| *g != gen);
    let lit = pv.lit;
    let left = (pv.left / STAY).clamp(0.0, 1.0);
    let copied = pv.copied.is_some_and(|t| t.elapsed().as_secs_f32() < 1.6);
    let detail = pv.detail.clone();
    let still = pv.still.is_some();
    let mut hits = Vec::new();
    let thumb = pv.thumb.as_ref();
    let mut made: Option<ID2D1Bitmap> = None;
    let existing = pv.bmp.as_ref().map(|(_, b)| b.clone());
    let accent = crate::theme::accent();
    let (pw, ph) = ((ww * k).ceil() as i32, (wh * k).ceil() as i32);
    if pv.s.gfx.begin(&mut pv.s.layer, pw, ph, k).is_ok() {
        if need_bmp {
            if let Some(t) = thumb {
                made = pv.s.gfx.bitmap(t.w, t.h, &t.px).ok();
            }
        }
        {
            let p = pv.s.gfx.painter();
            let f = &pv.s.gfx.fonts;
            let (x, y) = (SHADOW, SHADOW);
            glass(&p, x, y, cw, ch, 12.0, 1.0);
            // The last frame, with a play button on it.
            let (tx, ty, tw) = (x + 12.0, y + 12.0, cw - 24.0);
            p.rrect(tx, ty, tw, th, 8.0, Color(0.0, 0.0, 0.0, 0.6));
            if let Some(b) = made.as_ref().or(existing.as_ref()) {
                p.clip(tx, ty, tw, th);
                if let Some(t) = thumb {
                    let iw = tw.max(th * t.w as f32 / t.h as f32);
                    let ih = iw * t.h as f32 / t.w as f32;
                    p.image(b, tx + (tw - iw) / 2.0, ty + (th - ih) / 2.0, iw, ih, 1.0);
                }
                p.unclip();
            }
            p.stroke_rrect(tx + 0.5, ty + 0.5, tw - 1.0, th - 1.0, 8.0, Color::white(0.1), 1.0);
            let (pcx, pcy) = (tx + tw / 2.0, ty + th / 2.0);
            if still {
                // A screenshot: lit under the pointer, a click opens the editor.
                p.rrect(tx, ty, tw, th, 8.0, Color::white(0.08 * lit[0]));
            } else {
                let pr = 26.0 + 3.0 * lit[0];
                p.circle(pcx, pcy, pr, Color(0.0, 0.0, 0.0, 0.45 + 0.15 * lit[0]));
                p.stroke_circle(pcx, pcy, pr, Color::white(0.35 + 0.4 * lit[0]), 1.5);
                p.polygon(&[(pcx - 7.0, pcy - 11.0), (pcx - 7.0, pcy + 11.0), (pcx + 12.0, pcy)], Color::white(0.95));
            }
            hits.push((tx, ty, tw, th, Pv::Thumb));
            // Close, over the frame's corner.
            let (qx, qy) = (tx + tw - 34.0, ty + 6.0);
            p.circle(qx + 14.0, qy + 14.0, 14.0, Color(0.0, 0.0, 0.0, 0.5 + 0.3 * lit[1]));
            glyph(&p, f, 0xE711, qx + 7.0, qy + 7.0, 14.0, Color::white(0.8 + 0.2 * lit[1]));
            hits.push((qx, qy, 28.0, 28.0, Pv::Close));
            // What it is.
            let iy = ty + th + 12.0;
            let title = if still { "Screenshot saved" } else { "Screen recording saved" };
            p.text(title, &f.title, x + 16.0, iy, cw - 32.0, 22.0, Align::Left, Color::white(0.95));
            p.text(&detail, &f.small, x + 16.0, iy + 22.0, cw - 32.0, 18.0, Align::Left, Color::white(0.55));
            // What to do with it.
            let by = iy + 48.0;
            let gap = 6.0;
            let bw = (cw - 24.0 - 3.0 * gap) / 4.0;
            let buttons = [
                if still { (Pv::Play, 0xE70F, "Edit") } else { (Pv::Play, 0xE768, "Play") },
                (Pv::Folder, 0xE8B7, "Folder"),
                (Pv::Copy, if copied { 0xE73E } else { 0xE8C8 }, if copied { "Copied" } else { "Copy" }),
                (Pv::Delete, 0xE74D, "Delete"),
            ];
            for (i, (b, g, label)) in buttons.iter().enumerate() {
                let bx = x + 12.0 + i as f32 * (bw + gap);
                let l = lit[i + 2];
                let fill = if *b == Pv::Delete {
                    mix(Color::white(0.07), Color(0.77, 0.17, 0.11, 0.9), l)
                } else {
                    Color::white(0.07 + 0.07 * l)
                };
                p.rrect(bx, by, bw, 34.0, 8.0, fill);
                p.stroke_rrect(bx + 0.5, by + 0.5, bw - 1.0, 33.0, 8.0, Color::white(0.05 + 0.08 * l), 1.0);
                let tw = p.measure(label, &f.small) + 22.0;
                let lx = bx + (bw - tw) / 2.0;
                glyph(&p, f, *g, lx, by + 10.0, 14.0, Color::white(0.92));
                p.text(label, &f.small, lx + 20.0, by, tw, 34.0, Align::Left, Color::white(0.92));
                hits.push((bx, by, bw, 34.0, *b));
            }
            // How long it stays, running down along the bottom.
            let (lx, ly) = (x + 12.0, y + ch - 5.0);
            p.rrect(lx, ly, (cw - 24.0) * left, 2.0, 1.0, accent.alpha(0.7));
        }
        let a = ((ent * (1.0 - out)).clamp(0.0, 1.0) * 255.0) as u8;
        let _ = pv.s.gfx.present(&pv.s.layer, pv.s.hwnd, pos, pw, ph, a);
    }
    if let Some(b) = made {
        pv.bmp = Some((gen, b));
    }
    pv.hits = hits;
    true
}

/// Puts `path` on the clipboard as a file (as Explorer's Copy does).
fn copy_file(owner: HWND, path: &std::path::Path) -> bool {
    use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    const CF_HDROP: u32 = 15;
    let wide: Vec<u16> = path.as_os_str().to_string_lossy().encode_utf16().chain([0, 0]).collect();
    // DROPFILES: the offset of the names, a point, two flags (the names are wide).
    let head: [u32; 5] = [20, 0, 0, 0, 1];
    let size = 20 + wide.len() * 2;
    unsafe {
        let Ok(mem) = GlobalAlloc(GMEM_MOVEABLE, size) else { return false };
        let ptr = GlobalLock(mem) as *mut u8;
        if ptr.is_null() {
            return false;
        }
        std::ptr::copy_nonoverlapping(head.as_ptr() as *const u8, ptr, 20);
        std::ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, ptr.add(20), wide.len() * 2);
        let _ = GlobalUnlock(mem);
        if OpenClipboard(Some(owner)).is_err() {
            return false;
        }
        let _ = EmptyClipboard();
        let ok = SetClipboardData(CF_HDROP, Some(HANDLE(mem.0))).is_ok();
        let _ = CloseClipboard();
        ok
    }
}

fn press(pv: &mut Preview, b: Pv) {
    let path = pv.path.clone();
    match b {
        Pv::Thumb | Pv::Play => {
            match &pv.still {
                Some((px, w, h)) => crate::snipedit::open(px.clone(), *w, *h, Some(path)),
                None => crate::shell::open(&path.to_string_lossy()),
            }
            pv.closing = Some(Instant::now());
        }
        Pv::Folder => {
            if let Some(dir) = path.parent() {
                crate::appwin::launch(&["--files", &dir.to_string_lossy()]);
            }
            pv.closing = Some(Instant::now());
        }
        Pv::Copy => {
            if copy_file(pv.s.hwnd, &path) {
                pv.copied = Some(Instant::now());
            }
        }
        Pv::Delete => {
            crate::deskicons::delete(pv.s.hwnd, &[path.to_string_lossy().into_owned()], false);
            pv.closing = Some(Instant::now());
        }
        Pv::Close => pv.closing = Some(Instant::now()),
    }
}

unsafe extern "system" fn preview_proc(hwnd: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let k = PREVIEW.with(|p| p.borrow().as_ref().map_or(1.0, |p| p.s.k));
    let hit = |x: f32, y: f32| {
        PREVIEW.with(|p| {
            p.borrow().as_ref().and_then(|p| {
                p.hits.iter().rev().find(|h| x >= h.0 && x < h.0 + h.2 && y >= h.1 && y < h.1 + h.3).map(|h| h.4)
            })
        })
    };
    match m {
        WM_TIMER => {
            let alive = PREVIEW.with(|p| match p.try_borrow_mut() {
                Ok(mut p) => p.as_mut().is_none_or(draw_preview),
                Err(_) => true,
            });
            if !alive {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = at(lp, k);
            let h = hit(x, y);
            PREVIEW.with(|p| {
                if let Some(p) = p.borrow_mut().as_mut() {
                    p.hot = h;
                    p.inside = true;
                }
            });
            track_leave(hwnd);
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            PREVIEW.with(|p| {
                if let Some(p) = p.borrow_mut().as_mut() {
                    p.hot = None;
                    p.inside = false;
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = at(lp, k);
            if let Some(b) = hit(x, y) {
                PREVIEW.with(|p| {
                    if let Some(p) = p.borrow_mut().as_mut() {
                        press(p, b);
                    }
                });
            }
            LRESULT(0)
        }
        WM_SETCURSOR if (lp.0 & 0xFFFF) as u32 == HTCLIENT => {
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let _ = ScreenToClient(hwnd, &mut pt);
            let over = hit(pt.x as f32 / k, pt.y as f32 / k).is_some();
            SetCursor(LoadCursorW(None, if over { IDC_HAND } else { IDC_ARROW }).ok());
            LRESULT(1)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, m, wp, lp),
    }
}

/// Pops up the preview of a saved recording at the bottom right of the
/// screen `near` is on; `secs` is how long it runs. Returns at once.
pub fn preview(path: PathBuf, thumb: Option<Thumb>, secs: f32, near: RECT) {
    show_preview(path, thumb, None, secs, near);
}

/// Pops up the preview of a screenshot just taken (`pixels`, `w`×`h`,
/// saved at `path`), as a recording's: Edit opens it in the editor.
pub fn screenshot_preview(path: PathBuf, pixels: Vec<u32>, w: i32, h: i32, near: RECT) {
    let bytes: Vec<u8> = pixels.iter().flat_map(|p| (p | 0xFF00_0000).to_le_bytes()).collect();
    let thumb = Thumb::from_frame(&bytes, w as u32, h as u32, 640);
    show_preview(path, thumb, Some((pixels, w, h)), 0.0, near);
}

fn show_preview(path: PathBuf, thumb: Option<Thumb>, still: Option<(Vec<u32>, i32, i32)>, secs: f32, near: RECT) {
    std::thread::spawn(move || unsafe {
        let _ =
            windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
        register(w!("CascadeRecordPreview"), Some(preview_proc));
        let k = scale_at(&near);
        let work = work_area(&near);
        let margin = (16.0 * k) as i32;
        let corner =
            POINT { x: work.right - margin + (SHADOW * k) as i32, y: work.bottom - margin + (SHADOW * k) as i32 };
        let Some(hwnd) = layered(w!("CascadeRecordPreview"), WS_EX_NOACTIVATE, corner.x, corner.y) else { return };
        let Ok(gfx) = Gfx::new() else { return };
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let s = secs.round() as u32;
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let detail = match &still {
            Some((_, w, h)) => format!("{w}\u{d7}{h} \u{b7} {} \u{b7} {name}", crate::appwin::size_text(size)),
            None => format!("{}:{:02} \u{b7} {} \u{b7} {name}", s / 60, s % 60, crate::appwin::size_text(size)),
        };
        PREVIEW.with(|p| {
            *p.borrow_mut() = Some(Preview {
                s: Surface { hwnd, gfx, layer: Layer::default(), k },
                path,
                still,
                thumb,
                bmp: None,
                detail,
                shown: Instant::now(),
                last: Instant::now(),
                left: STAY,
                hot: None,
                inside: false,
                lit: [0.0; 6],
                copied: None,
                closing: None,
                corner,
                hits: Vec::new(),
            })
        });
        PREVIEW.with(|p| {
            if let Some(p) = p.borrow_mut().as_mut() {
                draw_preview(p);
            }
        });
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetTimer(Some(hwnd), 1, 16, None);
        let mut m = MSG::default();
        while GetMessageW(&mut m, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&m);
            DispatchMessageW(&m);
        }
        PREVIEW.with(|p| p.borrow_mut().take());
    });
}
