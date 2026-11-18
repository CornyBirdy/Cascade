//! Snipping, as the Snipping Tool: Win+Shift+S (or Print Screen) freezes
//! and dims the screens, with a bar at the top to pick how: a rectangle you
//! drag, a window you click, a whole screen, or a freeform shape you draw.
//! The bar also sets a delay (3, 5 or 10 seconds, to set up a menu first)
//! and switches to recording (Win+Shift+R starts there): drag an area or
//! click a screen, with or without the PC's sound (see `recorder`).
//!
//! It always opens on Rectangle. A snip goes on the clipboard, is saved in
//! Pictures\Screenshots, and
//! opens in the editor to mark up, crop, copy text out of, or save
//! elsewhere (see `snipedit`). Esc or a right-click leaves without one.

use std::cell::RefCell;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use windows::core::{w, HSTRING};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_INPROC_SERVER};
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, SetFocus, VK_ESCAPE, VK_RETURN};
use windows::Win32::UI::Shell::{FOLDERID_Screenshots, SHGetKnownFolderPath, KF_FLAG_DEFAULT};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::gfx::{Align, Color, Gfx};
use crate::{config, msg};

const CF_DIB: u32 = 8;

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Rect,
    Window,
    Screen,
    Free,
}

/// The mode picked on the bar (each snip starts on Rectangle; a delayed one
/// keeps what was picked).
static MODE: AtomicU32 = AtomicU32::new(0);
static SOUND: AtomicBool = AtomicBool::new(true);

fn mode() -> Mode {
    match MODE.load(Ordering::Relaxed) {
        1 => Mode::Window,
        2 => Mode::Screen,
        3 => Mode::Free,
        _ => Mode::Rect,
    }
}

fn set_mode(m: Mode) {
    MODE.store(
        match m {
            Mode::Rect => 0,
            Mode::Window => 1,
            Mode::Screen => 2,
            Mode::Free => 3,
        },
        Ordering::Relaxed,
    );
}

/// The bar's buttons.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Button {
    Mode(Mode),
    Delay,
    Record,
    Sound,
    Close,
}

impl std::fmt::Debug for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Mode::Rect => "Rect",
            Mode::Window => "Window",
            Mode::Screen => "Screen",
            Mode::Free => "Free",
        })
    }
}

/// The capture of every screen, and the snip being made.
struct Snip {
    window: HWND,
    instance: HINSTANCE,
    /// Where the capture starts on the screen (the virtual screen's top left).
    origin: (i32, i32),
    w: i32,
    h: i32,
    bright: HBITMAP,
    dim: HBITMAP,
    pixels: &'static mut [u32],
    record: bool,
    delay: u32,
    /// The bar's buttons: where each is (window pixels).
    buttons: Vec<(Button, RECT)>,
    hot: Option<Button>,
    /// The windows on screen as it opened (front first), for Window mode.
    windows: Vec<RECT>,
    hover_window: Option<RECT>,
    from: Option<POINT>,
    to: POINT,
    /// The freeform outline being drawn.
    path: Vec<POINT>,
    /// Pixels per layout unit on the screen the bar is on.
    scale: f32,
    /// When it opened: the bar drops in from the top edge.
    opened: Instant,
}

unsafe impl Send for Snip {}

static SNIP: Mutex<Option<Snip>> = Mutex::new(None);

fn snip() -> std::sync::MutexGuard<'static, Option<Snip>> {
    SNIP.lock().unwrap_or_else(|e| e.into_inner())
}

thread_local! {
    /// Direct2D for the bar and the size label, made on the first snip.
    static GFX: RefCell<Option<Gfx>> = const { RefCell::new(None) };
}

fn with_gfx<R>(f: impl FnOnce(&mut Gfx) -> R) -> Option<R> {
    GFX.with(|g| {
        let mut g = g.borrow_mut();
        if g.is_none() {
            *g = Gfx::new().ok();
        }
        g.as_mut().map(f)
    })
}

/// How long the bar takes to drop in.
const OPEN_MS: f32 = 220.0;
const TIMER_OPEN: usize = 1;

/// A top-down 32-bit bitmap and its pixels.
pub(crate) unsafe fn dib(w: i32, h: i32) -> Option<(HBITMAP, &'static mut [u32])> {
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
    let bitmap = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
    if bits.is_null() {
        let _ = DeleteObject(bitmap.into());
        return None;
    }
    Some((bitmap, std::slice::from_raw_parts_mut(bits as *mut u32, (w * h) as usize)))
}

fn rect_of(a: POINT, b: POINT, w: i32, h: i32) -> RECT {
    RECT {
        left: a.x.min(b.x).max(0),
        top: a.y.min(b.y).max(0),
        right: a.x.max(b.x).min(w),
        bottom: a.y.max(b.y).min(h),
    }
}

fn big_enough(r: &RECT) -> bool {
    r.right - r.left > 3 && r.bottom - r.top > 3
}

/// The selection being dragged, in window pixels.
fn selection(s: &Snip) -> Option<RECT> {
    let r = rect_of(s.from?, s.to, s.w, s.h);
    big_enough(&r).then_some(r)
}

/// The whole screen under `p` (window pixels).
fn screen_at(s: &Snip, p: POINT) -> RECT {
    unsafe {
        let at = POINT { x: p.x + s.origin.0, y: p.y + s.origin.1 };
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(MonitorFromPoint(at, MONITOR_DEFAULTTONEAREST), &mut mi);
        let m = mi.rcMonitor;
        RECT {
            left: m.left - s.origin.0,
            top: m.top - s.origin.1,
            right: m.right - s.origin.0,
            bottom: m.bottom - s.origin.1,
        }
    }
}

unsafe extern "system" fn collect(h: HWND, out: LPARAM) -> windows::core::BOOL {
    (*(out.0 as *mut Vec<HWND>)).push(h);
    true.into()
}

/// The windows on screen, front first, as what you see of each (window pixels).
unsafe fn window_rects(origin: (i32, i32)) -> Vec<RECT> {
    let mut all: Vec<HWND> = Vec::new();
    let _ = EnumWindows(Some(collect), LPARAM(&mut all as *mut _ as isize));
    let own = windows::Win32::System::Threading::GetCurrentProcessId();
    let mut out = Vec::new();
    for h in all {
        if !IsWindowVisible(h).as_bool() || IsIconic(h).as_bool() || crate::shell::is_desktop(h) {
            continue;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, Some(&mut pid));
        let mut cloaked = 0u32;
        let _ = DwmGetWindowAttribute(h, DWMWA_CLOAKED, &mut cloaked as *mut u32 as *mut c_void, 4);
        if cloaked != 0 {
            continue;
        }
        let mut r = RECT::default();
        if DwmGetWindowAttribute(h, DWMWA_EXTENDED_FRAME_BOUNDS, &mut r as *mut RECT as *mut c_void, 16).is_err() {
            let _ = GetWindowRect(h, &mut r);
        }
        // Cascade's own windows: its taskbar can be snipped as a window, nothing else of it.
        let class = {
            let mut c = [0u16; 32];
            let n = GetClassNameW(h, &mut c) as usize;
            String::from_utf16_lossy(&c[..n])
        };
        if pid == own && class != "CascadeWindow" {
            continue;
        }
        if r.right - r.left < 8 || r.bottom - r.top < 8 {
            continue;
        }
        out.push(RECT {
            left: r.left - origin.0,
            top: r.top - origin.1,
            right: r.right - origin.0,
            bottom: r.bottom - origin.1,
        });
    }
    out
}

/// Starts a snip (or, with `record`, picking what to record).
pub fn start(instance: HINSTANCE, record: bool) {
    open(instance, record, Mode::Rect);
}

fn open(instance: HINSTANCE, record: bool, start_mode: Mode) {
    if snip().is_some() || crate::recorder::recording() {
        return;
    }
    unsafe {
        let (x, y) = (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN));
        let (w, h) = (GetSystemMetrics(SM_CXVIRTUALSCREEN), GetSystemMetrics(SM_CYVIRTUALSCREEN));
        let windows = window_rects((x, y));
        let (Some((bright, pixels)), Some((dim, dim_px))) = (dib(w, h), dib(w, h)) else { return };
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let old = SelectObject(mem, bright.into());
        let _ = BitBlt(mem, 0, 0, w, h, Some(screen), x, y, SRCCOPY | CAPTUREBLT);
        SelectObject(mem, old);
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        let _ = GdiFlush();
        for (d, b) in dim_px.iter_mut().zip(pixels.iter_mut()) {
            *b |= 0xFF00_0000;
            *d = 0xFF00_0000 | ((*b >> 1) & 0x007F_7F7F);
        }
        let class = w!("CascadeSnip");
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(proc),
            hInstance: instance,
            hCursor: LoadCursorW(None, IDC_CROSS).unwrap_or_default(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let Ok(window) = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            class,
            w!("Snip"),
            WS_POPUP,
            x,
            y,
            w,
            h,
            None,
            None,
            Some(instance),
            None,
        ) else {
            let _ = DeleteObject(bright.into());
            let _ = DeleteObject(dim.into());
            return;
        };
        let mut s = Snip {
            window,
            instance,
            origin: (x, y),
            w,
            h,
            bright,
            dim,
            pixels,
            record,
            delay: 0,
            buttons: Vec::new(),
            hot: None,
            windows,
            hover_window: None,
            from: None,
            to: POINT::default(),
            path: Vec::new(),
            scale: 1.0,
            opened: Instant::now(),
        };
        // Every snip starts on Rectangle, whatever the last one used (a
        // delayed one on what was picked before the wait).
        let start_mode =
            if record && matches!(start_mode, Mode::Window | Mode::Free) { Mode::Rect } else { start_mode };
        set_mode(start_mode);
        layout_bar(&mut s);
        *snip() = Some(s);
        SetTimer(Some(window), TIMER_OPEN, 15, None);
        let _ = ShowWindow(window, SW_SHOW);
        crate::launcher::force_foreground(window);
        let _ = SetFocus(Some(window));
    }
}

/// The bar's buttons, at the top middle of the screen the pointer is on.
unsafe fn layout_bar(s: &mut Snip) {
    let mut p = POINT::default();
    let _ = GetCursorPos(&mut p);
    let (mut dx, mut dy) = (96u32, 96u32);
    let _ = GetDpiForMonitor(MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST), MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
    s.scale = dx.max(96) as f32 / 96.0;
    let k = s.scale;
    let local = POINT { x: p.x - s.origin.0, y: p.y - s.origin.1 };
    let m = screen_at(s, local);
    let mut buttons: Vec<Button> = vec![Button::Mode(Mode::Rect)];
    if !s.record {
        buttons.push(Button::Mode(Mode::Window));
    }
    buttons.push(Button::Mode(Mode::Screen));
    if !s.record {
        buttons.push(Button::Mode(Mode::Free));
        buttons.push(Button::Delay);
    } else {
        buttons.push(Button::Sound);
    }
    buttons.push(Button::Record);
    buttons.push(Button::Close);
    // Each button fits its icon and label (layout units, then pixels).
    let widths: Vec<i32> = buttons
        .iter()
        .map(|b| {
            let text = label(s, *b);
            let w = if text.is_empty() {
                BUTTON_H
            } else {
                let tw = with_gfx(|g| g.measure(&text, &g.fonts.body)).unwrap_or(text.len() as f32 * 7.0);
                14.0 + 16.0 + 8.0 + tw.ceil() + 14.0
            };
            (w * k).round() as i32
        })
        .collect();
    let gap = (GAP * k).round() as i32;
    let group = (GROUP_GAP * k).round() as i32;
    let total: i32 = widths.iter().sum::<i32>()
        + buttons.iter().skip(1).map(|b| if starts_group(*b) { group } else { gap }).sum::<i32>();
    let mut x = (m.left + m.right) / 2 - total / 2;
    let y = m.top + (TOP * k).round() as i32;
    let h = (BUTTON_H * k).round() as i32;
    s.buttons = buttons
        .into_iter()
        .zip(widths)
        .enumerate()
        .map(|(i, (b, w))| {
            if i > 0 {
                x += if starts_group(b) { group } else { gap };
            }
            let r = RECT { left: x, top: y, right: x + w, bottom: y + h };
            x += w;
            (b, r)
        })
        .collect();
}

/// The bar's measurements, in layout units.
const BUTTON_H: f32 = 34.0;
const GAP: f32 = 2.0;
const GROUP_GAP: f32 = 13.0;
const PAD: f32 = 5.0;
const TOP: f32 = 16.0;

/// Buttons that start a new group on the bar, with a divider before them.
fn starts_group(b: Button) -> bool {
    matches!(b, Button::Delay | Button::Sound | Button::Record | Button::Close)
}

fn bar_rect(s: &Snip) -> RECT {
    let first = s.buttons.first().map(|b| b.1).unwrap_or_default();
    let last = s.buttons.last().map(|b| b.1).unwrap_or_default();
    let pad = (PAD * s.scale).round() as i32;
    RECT { left: first.left - pad, top: first.top - pad, right: last.right + pad, bottom: last.bottom + pad }
}

/// Everything the bar paints: its shadow, the hint under it, and the room it
/// slides in from.
fn bar_area(s: &Snip) -> RECT {
    let b = bar_rect(s);
    let k = s.scale;
    let side = (24.0 * k) as i32;
    RECT {
        left: b.left - side,
        top: b.top - (TOP * k) as i32,
        right: b.right + side,
        bottom: b.bottom + (52.0 * k) as i32,
    }
}

fn label(s: &Snip, b: Button) -> String {
    match b {
        Button::Mode(Mode::Rect) => "Rectangle".into(),
        Button::Mode(Mode::Window) => "Window".into(),
        Button::Mode(Mode::Screen) => "Full screen".into(),
        Button::Mode(Mode::Free) => "Freeform".into(),
        Button::Delay if s.delay == 0 => "No delay".into(),
        Button::Delay => format!("{} s delay", s.delay),
        Button::Sound if SOUND.load(Ordering::Relaxed) => "Sound on".into(),
        Button::Sound => "Sound off".into(),
        Button::Record if s.record => "Snip".into(),
        Button::Record => "Record".into(),
        Button::Close => String::new(),
    }
}

/// Each button's glyph in Windows' icon font, and a plain one if it's missing.
fn glyph(s: &Snip, b: Button) -> (char, &'static str) {
    match b {
        Button::Mode(Mode::Rect) => ('\u{E8B3}', "\u{25AD}"),
        Button::Mode(Mode::Window) => ('\u{E8A7}', "\u{29C9}"),
        Button::Mode(Mode::Screen) => ('\u{E7F4}', "\u{25A3}"),
        Button::Mode(Mode::Free) => ('\u{E70F}', "\u{270E}"),
        Button::Delay => ('\u{E916}', "\u{23F1}"),
        Button::Sound if SOUND.load(Ordering::Relaxed) => ('\u{E767}', "\u{266A}"),
        Button::Sound => ('\u{E74F}', "\u{266A}"),
        Button::Record if s.record => ('\u{E722}', "\u{2702}"),
        Button::Record => ('\u{E714}', "\u{25CF}"),
        Button::Close => ('\u{E711}', "\u{2715}"),
    }
}

/// What to do next, under the bar.
fn hint(s: &Snip) -> String {
    let what = match mode() {
        Mode::Rect if s.record => "Drag round what to record, or click for a whole screen",
        Mode::Rect => "Drag round what to snip, or click for a whole screen",
        Mode::Window => "Click a window",
        Mode::Screen if s.record => "Click the screen to record",
        Mode::Screen => "Click a screen",
        Mode::Free => "Draw round what to snip",
    };
    format!("{what}   \u{b7}   Enter: every screen   \u{b7}   Esc to leave")
}

fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// Closes the snip window and frees the capture.
unsafe fn finish() {
    let Some(s) = snip().take() else { return };
    let _ = ReleaseCapture();
    let _ = DestroyWindow(s.window);
    let _ = DeleteObject(s.bright.into());
    let _ = DeleteObject(s.dim.into());
}

/// Scanline fill: for each row of the box `r`, which pixels are inside the outline `path`.
fn inside_mask(path: &[POINT], r: RECT) -> Vec<bool> {
    let (w, h) = ((r.right - r.left) as usize, (r.bottom - r.top) as usize);
    let mut mask = vec![false; w * h];
    if path.len() < 3 {
        return mask;
    }
    for row in 0..h {
        let y = r.top as f64 + row as f64 + 0.5;
        let mut xs: Vec<f64> = Vec::new();
        for i in 0..path.len() {
            let (a, b) = (path[i], path[(i + 1) % path.len()]);
            let (ay, by) = (a.y as f64, b.y as f64);
            if (ay <= y && by > y) || (by <= y && ay > y) {
                xs.push(a.x as f64 + (y - ay) / (by - ay) * (b.x - a.x) as f64);
            }
        }
        xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        for pair in xs.chunks(2) {
            if let [x0, x1] = pair {
                let from = ((x0 - r.left as f64).round().max(0.0) as usize).min(w);
                let to = ((x1 - r.left as f64).round().max(0.0) as usize).min(w);
                mask[row * w + from..row * w + to].fill(true);
            }
        }
    }
    mask
}

/// Keeps `r` of the capture (only what's inside `outline`, if it's a
/// freeform one): on the clipboard, in Screenshots, and into the editor.
unsafe fn keep(r: RECT, outline: Option<Vec<POINT>>) {
    let picture: Option<(Vec<u32>, i32, i32)> = snip().as_ref().map(|s| {
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        let mut out = Vec::with_capacity((w * h) as usize);
        for y in r.top..r.bottom {
            let start = (y * s.w + r.left) as usize;
            out.extend_from_slice(&s.pixels[start..start + w as usize]);
        }
        if let Some(path) = &outline {
            // Outside the shape: see-through.
            for (px, inside) in out.iter_mut().zip(inside_mask(path, r)) {
                if !inside {
                    *px = 0;
                }
            }
        }
        (out, w, h)
    });
    let owner = snip().as_ref().map(|s| s.window);
    let Some((pixels, w, h)) = picture else { return finish() };
    // Onto the clipboard while the snip window is still there to own it:
    // the clipboard can't be opened for a window that's gone.
    let copied = copy_image(owner, &pixels, w, h);
    finish();
    if copied {
        msg::note("Snip copied to the clipboard");
    }
    let saved = screenshots_path().and_then(|p| save_image(&p, &pixels, w, h).then_some(p));
    config::log(&format!("snip: {w}x{h}, copied: {copied}, saved: {saved:?}"));
    crate::snipedit::open(pixels, w, h, saved);
}

/// Where a new screenshot goes: Pictures\Screenshots\Screenshot (date time).png.
fn screenshots_path() -> Option<PathBuf> {
    let dir = unsafe {
        match SHGetKnownFolderPath(&FOLDERID_Screenshots, KF_FLAG_DEFAULT, None) {
            Ok(p) => {
                let path = p.to_string().ok().map(PathBuf::from);
                CoTaskMemFree(Some(p.0 as *const c_void));
                path
            }
            Err(_) => None,
        }
    }
    .or_else(|| std::env::var_os("USERPROFILE").map(|u| PathBuf::from(u).join("Pictures\\Screenshots")))?;
    let _ = std::fs::create_dir_all(&dir);
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    Some(dir.join(format!(
        "Screenshot {:04}-{:02}-{:02} {:02}{:02}{:02}.png",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
    )))
}

/// Puts a picture on the clipboard (see-through parts become white, as
/// most programs pasting a picture expect).
pub(crate) unsafe fn copy_image(owner: Option<HWND>, pixels: &[u32], w: i32, h: i32) -> bool {
    let header = BITMAPINFOHEADER {
        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: w,
        biHeight: h,
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB.0,
        biSizeImage: (w * h * 4) as u32,
        ..Default::default()
    };
    let size = std::mem::size_of::<BITMAPINFOHEADER>() + pixels.len() * 4;
    let Ok(global) = GlobalAlloc(GMEM_MOVEABLE, size) else { return false };
    let at = GlobalLock(global) as *mut u8;
    if at.is_null() {
        return false;
    }
    std::ptr::copy_nonoverlapping(
        &header as *const BITMAPINFOHEADER as *const u8,
        at,
        std::mem::size_of::<BITMAPINFOHEADER>(),
    );
    let rows = at.add(std::mem::size_of::<BITMAPINFOHEADER>()) as *mut u32;
    for y in 0..h as usize {
        for x in 0..w as usize {
            let p = pixels[y * w as usize + x];
            let p = if p >> 24 == 0 { 0xFFFF_FFFF } else { p };
            *rows.add((h as usize - 1 - y) * w as usize + x) = p;
        }
    }
    let _ = GlobalUnlock(global);
    // Another program may have the clipboard open for a moment.
    let mut open = OpenClipboard(owner).is_ok();
    for _ in 0..10 {
        if open {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(15));
        open = OpenClipboard(owner).is_ok();
    }
    if !open {
        let _ = windows::Win32::Foundation::GlobalFree(Some(global));
        return false;
    }
    let _ = EmptyClipboard();
    let ok = SetClipboardData(CF_DIB, Some(HANDLE(global.0))).is_ok();
    let _ = CloseClipboard();
    if !ok {
        let _ = windows::Win32::Foundation::GlobalFree(Some(global));
    }
    ok
}

/// Saves a picture: PNG (see-through kept), or JPEG if the name says .jpg.
pub(crate) unsafe fn save_image(path: &Path, pixels: &[u32], w: i32, h: i32) -> bool {
    let jpeg = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("jpg") || e.eq_ignore_ascii_case("jpeg"));
    let go = || -> windows::core::Result<()> {
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let stream = wic.CreateStream()?;
        stream.InitializeFromFilename(&HSTRING::from(path.as_os_str()), GENERIC_WRITE.0)?;
        let container = if jpeg { GUID_ContainerFormatJpeg } else { GUID_ContainerFormatPng };
        let encoder = wic.CreateEncoder(&container, std::ptr::null())?;
        encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
        let mut frame: Option<IWICBitmapFrameEncode> = None;
        encoder.CreateNewFrame(&mut frame, std::ptr::null_mut())?;
        let frame = frame.ok_or(windows::core::Error::from(E_FAIL))?;
        frame.Initialize(None)?;
        frame.SetSize(w as u32, h as u32)?;
        if jpeg {
            let mut format = GUID_WICPixelFormat24bppBGR;
            frame.SetPixelFormat(&mut format)?;
            let mut rgb = Vec::with_capacity((w * h * 3) as usize);
            for p in pixels {
                // See-through becomes white.
                let p = if p >> 24 == 0 { 0x00FF_FFFF } else { *p };
                rgb.extend_from_slice(&[(p & 0xFF) as u8, ((p >> 8) & 0xFF) as u8, ((p >> 16) & 0xFF) as u8]);
            }
            frame.WritePixels(h as u32, (w * 3) as u32, &rgb)?;
        } else {
            let mut format = GUID_WICPixelFormat32bppBGRA;
            frame.SetPixelFormat(&mut format)?;
            let bytes = std::slice::from_raw_parts(pixels.as_ptr() as *const u8, pixels.len() * 4);
            frame.WritePixels(h as u32, (w * 4) as u32, bytes)?;
        }
        frame.Commit()?;
        encoder.Commit()
    };
    go().is_ok()
}

fn point(lp: LPARAM) -> POINT {
    POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 }
}

fn contains(r: &RECT, p: POINT) -> bool {
    p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom
}

fn grown(r: RECT, by: i32) -> RECT {
    RECT { left: r.left - by, top: r.top - by, right: r.right + by, bottom: r.bottom + by }
}

/// A bar button was clicked.
unsafe fn press(b: Button) {
    let (window, instance) = {
        let mut guard = snip();
        let Some(s) = guard.as_mut() else { return };
        match b {
            Button::Mode(m) => {
                set_mode(m);
                s.from = None;
                s.path.clear();
            }
            Button::Sound => SOUND.store(!SOUND.load(Ordering::Relaxed), Ordering::Relaxed),
            Button::Record => {
                s.record = !s.record;
                if s.record && matches!(mode(), Mode::Window | Mode::Free) {
                    set_mode(Mode::Rect);
                }
                layout_bar(s);
            }
            Button::Delay => {
                s.delay = match s.delay {
                    0 => 3,
                    3 => 5,
                    5 => 10,
                    _ => 0,
                };
            }
            Button::Close => {}
        }
        (s.window, s.instance)
    };
    if b == Button::Close {
        finish();
        return;
    }
    // A delay: close now, and snip again (from a fresh capture) once it's up.
    let delay = snip().as_ref().map(|s| s.delay).unwrap_or(0);
    if b == Button::Delay && delay > 0 {
        finish();
        msg::note(format!("Snipping in {delay} seconds\u{2026}"));
        let instance = instance.0 as isize;
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(delay as u64));
            msg::post(msg::SNIP_NOW, instance as usize, 0);
        });
        return;
    }
    let _ = InvalidateRect(Some(window), None, false);
}

/// The area picked: snip it, or record it.
unsafe fn done(r: RECT, outline: Option<Vec<POINT>>) {
    let record = snip().as_ref().map(|s| (s.record, s.origin));
    match record {
        Some((true, origin)) => {
            finish();
            let screen = RECT {
                left: r.left + origin.0,
                top: r.top + origin.1,
                right: r.right + origin.0,
                bottom: r.bottom + origin.1,
            };
            crate::recorder::start(screen, SOUND.load(Ordering::Relaxed));
        }
        Some((false, _)) => keep(r, outline),
        None => {}
    }
}

/// Snips again after a delay (`msg::SNIP_NOW`).
pub fn again(instance: usize) {
    open(HINSTANCE(instance as *mut c_void), false, mode());
}

fn overlaps(a: &RECT, b: &RECT) -> bool {
    a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
}

/// Draws with Direct2D into `area` (window pixels) of `back`, blended over
/// what's there at `alpha`; `draw` gets the area's size in layout units.
unsafe fn overlay(
    back: HDC,
    area: RECT,
    scale: f32,
    alpha: f32,
    draw: impl FnOnce(&crate::gfx::Painter, &crate::gfx::Fonts),
) {
    let (w, h) = (area.right - area.left, area.bottom - area.top);
    if w <= 0 || h <= 0 || alpha <= 0.0 {
        return;
    }
    let Some((bitmap, _)) = dib(w, h) else { return };
    let mem = CreateCompatibleDC(Some(back));
    let old = SelectObject(mem, bitmap.into());
    let drawn = with_gfx(|g| {
        if g.begin_dc(mem, w, h, scale, Color(0.0, 0.0, 0.0, 0.0)).is_err() {
            return false;
        }
        draw(&g.painter(), &g.fonts);
        g.end().is_ok()
    })
    .unwrap_or(false);
    if drawn {
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: (alpha.clamp(0.0, 1.0) * 255.0) as u8,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let _ = AlphaBlend(back, area.left, area.top, w, h, mem, 0, 0, w, h, blend);
    }
    SelectObject(mem, old);
    let _ = DeleteDC(mem);
    let _ = DeleteObject(bitmap.into());
}

/// The bar: dark glass with a soft shadow, icon-and-label buttons in groups,
/// the picked ones in the accent colour, and a hint underneath. It drops in
/// from the top edge as the snip opens.
unsafe fn draw_bar(back: HDC, s: &Snip, paint: &RECT) {
    let area = bar_area(s);
    if !overlaps(&area, paint) {
        return;
    }
    let k = s.scale;
    let e = ease(s.opened.elapsed().as_secs_f32() * 1000.0 / OPEN_MS);
    let slide = (1.0 - e) * -18.0;
    let unit = |r: RECT| {
        (
            (r.left - area.left) as f32 / k,
            (r.top - area.top) as f32 / k + slide,
            (r.right - r.left) as f32 / k,
            (r.bottom - r.top) as f32 / k,
        )
    };
    let accent = crate::theme::accent();
    let hint = hint(s);
    let hint_w = with_gfx(|g| g.measure(&hint, &g.fonts.small)).unwrap_or(hint.len() as f32 * 5.5) + 28.0;
    overlay(back, area, k, e, |p, f| {
        let (bx, by, bw, bh) = unit(bar_rect(s));
        let r = bh / 2.0;
        // A soft shadow, then the glass and its edge.
        for i in 1..=8 {
            let d = i as f32 * 1.6;
            p.rrect(bx - d, by - d + 3.0, bw + 2.0 * d, bh + 2.0 * d, r + d, Color(0.0, 0.0, 0.0, 0.045));
        }
        p.rrect(bx, by, bw, bh, r, Color(0.11, 0.11, 0.12, 0.96));
        p.gradient_rrect(
            bx,
            by,
            bw,
            bh,
            r,
            &[(0.0, Color::white(0.07)), (0.5, Color::white(0.015)), (1.0, Color::white(0.0))],
        );
        p.stroke_rrect(bx + 0.5, by + 0.5, bw - 1.0, bh - 1.0, r - 0.5, Color::white(0.12), 1.0);
        for (i, (b, rect)) in s.buttons.iter().enumerate() {
            let (x, y, w, h) = unit(*rect);
            if i > 0 && starts_group(*b) {
                let dx = x - GROUP_GAP / 2.0;
                p.line(dx, y + 9.0, dx, y + h - 9.0, Color::white(0.14), 1.0);
            }
            let on = match b {
                Button::Mode(m) => *m == mode(),
                Button::Record => s.record,
                Button::Sound => SOUND.load(Ordering::Relaxed),
                Button::Delay => s.delay > 0,
                Button::Close => false,
            };
            let hot = s.hot == Some(*b);
            let br = h / 2.0;
            if on {
                p.rrect(x, y, w, h, br, accent.alpha(if hot { 1.0 } else { 0.88 }));
                p.gradient_rrect(x, y, w, h, br, &[(0.0, Color::white(0.18)), (1.0, Color::white(0.0))]);
            } else if hot {
                p.rrect(x, y, w, h, br, Color::white(if *b == Button::Close { 0.0 } else { 0.09 }));
                if *b == Button::Close {
                    p.rrect(x, y, w, h, br, Color(0.77, 0.17, 0.11, 0.9));
                }
            }
            let ink = Color::white(if on || hot { 1.0 } else { 0.86 });
            let text = label(s, *b);
            let (icon, plain) = glyph(s, *b);
            let icon_x = if text.is_empty() { x } else { x + 12.0 };
            let icon_w = if text.is_empty() { w } else { 20.0 };
            match &f.icons {
                Some(font) => p.text(&icon.to_string(), font, icon_x, y, icon_w, h, Align::Center, ink),
                None => p.text(plain, &f.body, icon_x, y, icon_w, h, Align::Center, ink),
            }
            if !text.is_empty() {
                p.text(&text, &f.body, x + 38.0, y, w - 38.0 - 10.0, h, Align::Left, ink);
            }
        }
        // The hint, in a small pill under the bar.
        let tw = hint_w;
        let (hx, hy, hh) = (bx + bw / 2.0 - tw / 2.0, by + bh + 8.0, 24.0);
        p.rrect(hx, hy, tw, hh, hh / 2.0, Color(0.08, 0.08, 0.09, 0.82));
        p.text(&hint, &f.small, hx, hy, tw, hh, Align::Center, Color::white(0.78));
    });
}

/// The selection's outline: a thin white edge with handles at its corners
/// and the middle of each side.
unsafe fn frame(dc: HDC, r: RECT, scale: f32) {
    let edge = CreateSolidBrush(COLORREF(0x00FFFFFF));
    let _ = FrameRect(dc, &grown(r, 1), edge);
    let shade = CreateSolidBrush(COLORREF(0x00303030));
    let _ = FrameRect(dc, &grown(r, 2), shade);
    let k = (3.0 * scale).round() as i32;
    let (cx, cy) = ((r.left + r.right) / 2, (r.top + r.bottom) / 2);
    let big = r.right - r.left > 12 * k && r.bottom - r.top > 12 * k;
    for (x, y) in [(r.left, r.top), (r.right, r.top), (r.left, r.bottom), (r.right, r.bottom)]
        .into_iter()
        .chain(big.then_some([(cx, r.top), (cx, r.bottom), (r.left, cy), (r.right, cy)]).into_iter().flatten())
    {
        let dot = RECT { left: x - k, top: y - k, right: x + k, bottom: y + k };
        let _ = FillRect(dc, &grown(dot, 1), shade);
        let _ = FillRect(dc, &dot, edge);
    }
    let _ = DeleteObject(edge.into());
    let _ = DeleteObject(shade.into());
}

/// Where the size of what's lit is shown: under its bottom right corner, or
/// inside it when that's off the screen.
fn size_rect(s: &Snip, r: RECT) -> RECT {
    let k = s.scale;
    let (w, h) = ((96.0 * k) as i32, (24.0 * k) as i32);
    let gap = (8.0 * k) as i32;
    let mut top = r.bottom + gap;
    if top + h > s.h {
        top = r.bottom - gap - h;
    }
    let left = (r.right - w).max(0);
    RECT { left, top, right: left + w, bottom: top + h }
}

/// Everything drawn for a lit area: its outline, handles and size label.
fn lit_area(s: &Snip, r: RECT) -> [RECT; 2] {
    [grown(r, (4.0 * s.scale) as i32 + 2), size_rect(s, r)]
}

unsafe fn draw_size(back: HDC, s: &Snip, r: RECT, paint: &RECT) {
    let at = size_rect(s, r);
    if !overlaps(&at, paint) {
        return;
    }
    let text = format!("{} \u{d7} {}", r.right - r.left, r.bottom - r.top);
    overlay(back, at, s.scale, 1.0, |p, f| {
        let (w, h) = (96.0, 24.0);
        p.rrect(0.0, 0.0, w, h, h / 2.0, Color(0.08, 0.08, 0.09, 0.88));
        p.stroke_rrect(0.5, 0.5, w - 1.0, h - 1.0, h / 2.0 - 0.5, Color::white(0.12), 1.0);
        p.text(&text, &f.label, 0.0, 0.0, w, h, Align::Center, Color::white(0.95));
    });
}

/// What's lit up, bright on the dimmed capture.
fn lit(s: &Snip) -> Option<RECT> {
    match mode() {
        Mode::Rect => selection(s),
        Mode::Window => s.hover_window,
        _ => None,
    }
}

/// Paints `paint` (window pixels) off screen, then puts it up in one go, so
/// nothing flickers while a selection is dragged.
unsafe fn paint(dc: HDC, s: &Snip, paint: RECT) {
    let (pw, ph) = (paint.right - paint.left, paint.bottom - paint.top);
    if pw <= 0 || ph <= 0 {
        return;
    }
    let back = CreateCompatibleDC(Some(dc));
    let buffer = CreateCompatibleBitmap(dc, pw, ph);
    let old_buffer = SelectObject(back, buffer.into());
    // Draw in window pixels.
    let _ = SetViewportOrgEx(back, -paint.left, -paint.top, None);
    let src = CreateCompatibleDC(Some(dc));
    let old = SelectObject(src, s.dim.into());
    let _ = BitBlt(back, paint.left, paint.top, pw, ph, Some(src), paint.left, paint.top, SRCCOPY);
    let lit = lit(s);
    if let Some(r) = lit {
        SelectObject(src, s.bright.into());
        let _ = BitBlt(back, r.left, r.top, r.right - r.left, r.bottom - r.top, Some(src), r.left, r.top, SRCCOPY);
    }
    SelectObject(src, old);
    let _ = DeleteDC(src);
    if mode() == Mode::Free && s.path.len() > 1 {
        let pen = CreatePen(PS_SOLID, (2.0 * s.scale).round() as i32, COLORREF(0x00FFFFFF));
        let op = SelectObject(back, pen.into());
        let _ = Polyline(back, &s.path);
        SelectObject(back, op);
        let _ = DeleteObject(pen.into());
    }
    if let Some(r) = lit {
        frame(back, r, s.scale);
        draw_size(back, s, r, &paint);
    }
    draw_bar(back, s, &paint);
    let _ = SetViewportOrgEx(back, 0, 0, None);
    let _ = BitBlt(dc, paint.left, paint.top, pw, ph, Some(back), 0, 0, SRCCOPY);
    SelectObject(back, old_buffer);
    let _ = DeleteObject(buffer.into());
    let _ = DeleteDC(back);
}

unsafe extern "system" fn proc(hwnd: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match m {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut ps);
            if let Some(s) = snip().as_ref() {
                paint(dc, s, ps.rcPaint);
            }
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let p = point(lp);
            let on_bar = snip().as_ref().is_some_and(|s| contains(&bar_rect(s), p));
            if !on_bar {
                if let Some(s) = snip().as_mut() {
                    s.from = Some(p);
                    s.to = p;
                    s.path = vec![p];
                }
                SetCapture(hwnd);
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let p = point(lp);
            let mut guard = snip();
            let Some(s) = guard.as_mut() else { return LRESULT(0) };
            let hot = s.buttons.iter().find(|(_, r)| contains(r, p)).map(|(b, _)| *b);
            let mut redraw: Vec<RECT> = Vec::new();
            if hot != s.hot {
                s.hot = hot;
                redraw.push(bar_area(s));
            }
            let before = lit(s);
            match mode() {
                Mode::Rect if s.from.is_some() => s.to = p,
                Mode::Free if s.from.is_some() => {
                    if let Some(last) = s.path.last().copied() {
                        redraw.push(grown(rect_of(last, p, s.w, s.h), (3.0 * s.scale) as i32 + 2));
                    }
                    s.path.push(p);
                }
                Mode::Window => s.hover_window = s.windows.iter().find(|r| contains(r, p)).copied(),
                _ => {}
            }
            let after = lit(s);
            if before != after {
                let area = bar_area(s);
                for r in [before, after].into_iter().flatten() {
                    let parts = lit_area(s, r);
                    if parts.iter().any(|q| overlaps(q, &area)) {
                        redraw.push(area);
                    }
                    redraw.extend(parts);
                }
            }
            drop(guard);
            for r in redraw {
                let _ = InvalidateRect(Some(hwnd), Some(&r), false);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let _ = ReleaseCapture();
            let p = point(lp);
            let hot = snip().as_ref().and_then(|s| s.buttons.iter().find(|(_, r)| contains(r, p)).map(|(b, _)| *b));
            let dragging = snip().as_ref().is_some_and(|s| s.from.is_some());
            if let (Some(b), false) = (hot, dragging) {
                press(b);
                return LRESULT(0);
            }
            let picked = snip().as_mut().and_then(|s| {
                s.to = p;
                let r = match mode() {
                    Mode::Rect => selection(s).or_else(|| s.from.is_some().then(|| screen_at(s, p))),
                    Mode::Window => s.hover_window,
                    Mode::Screen => Some(screen_at(s, p)),
                    Mode::Free => {
                        let (mut l, mut t, mut r, mut b) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
                        for q in &s.path {
                            l = l.min(q.x);
                            t = t.min(q.y);
                            r = r.max(q.x);
                            b = b.max(q.y);
                        }
                        let bx = RECT { left: l.max(0), top: t.max(0), right: r.min(s.w), bottom: b.min(s.h) };
                        big_enough(&bx).then_some(bx)
                    }
                };
                s.from = None;
                let outline = (mode() == Mode::Free).then(|| std::mem::take(&mut s.path));
                r.map(|r| (r, outline))
            });
            match picked {
                Some((r, outline)) => done(r, outline),
                None => {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_OPEN => {
            let done = snip().as_ref().map(|s| {
                let _ = InvalidateRect(Some(hwnd), Some(&bar_area(s)), false);
                s.opened.elapsed().as_secs_f32() * 1000.0 > OPEN_MS
            });
            if done != Some(false) {
                let _ = KillTimer(Some(hwnd), TIMER_OPEN);
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            finish();
            LRESULT(0)
        }
        WM_KEYDOWN if wp.0 as u16 == VK_ESCAPE.0 => {
            finish();
            LRESULT(0)
        }
        // Enter: every screen.
        WM_KEYDOWN if wp.0 as u16 == VK_RETURN.0 => {
            let all = snip().as_ref().map(|s| RECT { left: 0, top: 0, right: s.w, bottom: s.h });
            if let Some(r) = all {
                done(r, None);
            }
            LRESULT(0)
        }
        WM_ACTIVATE if wp.0 & 0xFFFF == 0 => {
            if snip().as_ref().is_some_and(|s| s.window == hwnd) {
                finish();
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
    fn freeform_fills_its_inside() {
        // A square outline, 2..8 each way, in a box 0..10.
        let path = [POINT { x: 2, y: 2 }, POINT { x: 8, y: 2 }, POINT { x: 8, y: 8 }, POINT { x: 2, y: 8 }];
        let m = inside_mask(&path, RECT { left: 0, top: 0, right: 10, bottom: 10 });
        assert!(m[5 * 10 + 5]);
        assert!(!m[0]);
        assert!(!m[5 * 10 + 9]);
        assert_eq!(m.iter().filter(|x| **x).count(), 36);
    }
}
