//! Snipping, as the Snipping Tool: Win+Shift+S (or Print Screen) freezes
//! and dims the screens, with a bar at the top to pick how: a rectangle you
//! drag, a window you click, a whole screen, or a freeform shape you draw.
//! The bar also sets a delay (3, 5 or 10 seconds, to set up a menu first)
//! and switches to recording (Win+Shift+R starts there): drag an area or
//! click a screen, with or without the PC's sound (see `recorder`).
//!
//! A snip goes on the clipboard, is saved in Pictures\Screenshots, and
//! opens in the editor to mark up, crop, copy text out of, or save
//! elsewhere (see `snipedit`). Esc or a right-click leaves without one.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;

use windows::core::{w, HSTRING};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_INPROC_SERVER};
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, SetFocus, VK_ESCAPE, VK_RETURN};
use windows::Win32::UI::Shell::{FOLDERID_Screenshots, SHGetKnownFolderPath, KF_FLAG_DEFAULT};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::{config, msg};

const CF_DIB: u32 = 8;

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Rect,
    Window,
    Screen,
    Free,
}

/// Remembered between snips, as the Snipping Tool does.
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
}

unsafe impl Send for Snip {}

static SNIP: Mutex<Option<Snip>> = Mutex::new(None);

fn snip() -> std::sync::MutexGuard<'static, Option<Snip>> {
    SNIP.lock().unwrap_or_else(|e| e.into_inner())
}

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
        };
        if record && matches!(mode(), Mode::Window | Mode::Free) {
            set_mode(Mode::Rect);
        }
        layout_bar(&mut s);
        *snip() = Some(s);
        let _ = ShowWindow(window, SW_SHOW);
        crate::launcher::force_foreground(window);
        let _ = SetFocus(Some(window));
    }
}

/// The bar's buttons, at the top middle of the screen the pointer is on.
unsafe fn layout_bar(s: &mut Snip) {
    let mut p = POINT::default();
    let _ = GetCursorPos(&mut p);
    let local = POINT { x: p.x - s.origin.0, y: p.y - s.origin.1 };
    let m = screen_at(s, local);
    let mut buttons: Vec<(Button, i32)> = Vec::new();
    buttons.push((Button::Mode(Mode::Rect), 92));
    if !s.record {
        buttons.push((Button::Mode(Mode::Window), 80));
    }
    buttons.push((Button::Mode(Mode::Screen), 98));
    if !s.record {
        buttons.push((Button::Mode(Mode::Free), 86));
        buttons.push((Button::Delay, 92));
    } else {
        buttons.push((Button::Sound, 96));
    }
    buttons.push((Button::Record, 96));
    buttons.push((Button::Close, 40));
    let total: i32 = buttons.iter().map(|b| b.1 + 4).sum::<i32>() + 8;
    let mut x = (m.left + m.right) / 2 - total / 2 + 8;
    let y = m.top + 18;
    s.buttons = buttons
        .into_iter()
        .map(|(b, w)| {
            let r = RECT { left: x, top: y, right: x + w, bottom: y + 36 };
            x += w + 4;
            (b, r)
        })
        .collect();
}

fn bar_rect(s: &Snip) -> RECT {
    let first = s.buttons.first().map(|b| b.1).unwrap_or_default();
    let last = s.buttons.last().map(|b| b.1).unwrap_or_default();
    RECT { left: first.left - 8, top: first.top - 6, right: last.right + 8, bottom: last.bottom + 6 }
}

fn label(s: &Snip, b: Button) -> String {
    match b {
        Button::Mode(Mode::Rect) => "\u{25AD} Rectangle".into(),
        Button::Mode(Mode::Window) => "\u{29C9} Window".into(),
        Button::Mode(Mode::Screen) => "\u{25A3} Full screen".into(),
        Button::Mode(Mode::Free) => "\u{270E} Freeform".into(),
        Button::Delay if s.delay == 0 => "\u{23F1} No delay".into(),
        Button::Delay => format!("\u{23F1} {} s delay", s.delay),
        Button::Sound if SOUND.load(Ordering::Relaxed) => "\u{266A} Sound on".into(),
        Button::Sound => "\u{266A} Sound off".into(),
        Button::Record if s.record => "\u{2702} Snip".into(),
        Button::Record => "\u{25CF} Record".into(),
        Button::Close => "\u{2715}".into(),
    }
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
    finish();
    let Some((pixels, w, h)) = picture else { return };
    let copied = copy_image(owner, &pixels, w, h);
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
    if OpenClipboard(owner).is_err() {
        return false;
    }
    let _ = EmptyClipboard();
    let ok = SetClipboardData(CF_DIB, Some(HANDLE(global.0))).is_ok();
    let _ = CloseClipboard();
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
    start(HINSTANCE(instance as *mut c_void), false);
}

unsafe fn draw_bar(dc: HDC, s: &Snip) {
    let bar = bar_rect(s);
    let bg = CreateSolidBrush(COLORREF(0x00282020));
    let rgn = CreateRoundRectRgn(bar.left, bar.top, bar.right, bar.bottom, 14, 14);
    let _ = FillRgn(dc, rgn, bg);
    let _ = DeleteObject(rgn.into());
    let _ = DeleteObject(bg.into());
    SetBkMode(dc, TRANSPARENT);
    let font = CreateFontW(
        -13,
        0,
        0,
        0,
        500,
        0,
        0,
        0,
        DEFAULT_CHARSET,
        OUT_DEFAULT_PRECIS,
        CLIP_DEFAULT_PRECIS,
        CLEARTYPE_QUALITY,
        0,
        w!("Segoe UI"),
    );
    let old = SelectObject(dc, font.into());
    let accent = crate::theme::accent();
    let accent =
        COLORREF(((accent.2 * 255.0) as u32) << 16 | ((accent.1 * 255.0) as u32) << 8 | (accent.0 * 255.0) as u32);
    for (b, r) in &s.buttons {
        let on = match b {
            Button::Mode(m) => *m == mode(),
            Button::Record => s.record,
            Button::Sound => SOUND.load(Ordering::Relaxed),
            Button::Delay => s.delay > 0,
            Button::Close => false,
        };
        let fill = if on {
            Some(accent)
        } else if s.hot == Some(*b) {
            Some(COLORREF(0x00483E3E))
        } else {
            None
        };
        if let Some(c) = fill {
            let br = CreateSolidBrush(c);
            let rgn = CreateRoundRectRgn(r.left, r.top, r.right, r.bottom, 10, 10);
            let _ = FillRgn(dc, rgn, br);
            let _ = DeleteObject(rgn.into());
            let _ = DeleteObject(br.into());
        }
        SetTextColor(dc, COLORREF(0x00FFFFFF));
        let mut text: Vec<u16> = label(s, *b).encode_utf16().collect();
        let mut tr = *r;
        DrawTextW(dc, &mut text, &mut tr, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
    }
    SelectObject(dc, old);
    let _ = DeleteObject(font.into());
}

unsafe fn frame(dc: HDC, r: RECT) {
    let edge = CreateSolidBrush(COLORREF(0x00FFFFFF));
    let _ = FrameRect(dc, &grown(r, 1), edge);
    let _ = DeleteObject(edge.into());
}

unsafe extern "system" fn proc(hwnd: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match m {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut ps);
            if let Some(s) = snip().as_ref() {
                let mem = CreateCompatibleDC(Some(dc));
                let p = ps.rcPaint;
                let old = SelectObject(mem, s.dim.into());
                let _ =
                    BitBlt(dc, p.left, p.top, p.right - p.left, p.bottom - p.top, Some(mem), p.left, p.top, SRCCOPY);
                // What would be taken, bright.
                let lit = match mode() {
                    Mode::Rect => selection(s),
                    Mode::Window if s.hover_window.is_some() => s.hover_window,
                    _ => None,
                };
                if let Some(r) = lit {
                    SelectObject(mem, s.bright.into());
                    let _ = BitBlt(
                        dc,
                        r.left,
                        r.top,
                        r.right - r.left,
                        r.bottom - r.top,
                        Some(mem),
                        r.left,
                        r.top,
                        SRCCOPY,
                    );
                    frame(dc, r);
                }
                SelectObject(mem, old);
                let _ = DeleteDC(mem);
                if mode() == Mode::Free && s.path.len() > 1 {
                    let pen = CreatePen(PS_SOLID, 2, COLORREF(0x00FFFFFF));
                    let op = SelectObject(dc, pen.into());
                    let _ = Polyline(dc, &s.path);
                    SelectObject(dc, op);
                    let _ = DeleteObject(pen.into());
                }
                draw_bar(dc, s);
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
                redraw.push(bar_rect(s));
            }
            match mode() {
                Mode::Rect if s.from.is_some() => {
                    redraw.extend(selection(s).map(|r| grown(r, 3)));
                    s.to = p;
                    redraw.extend(selection(s).map(|r| grown(r, 3)));
                    redraw.push(bar_rect(s));
                }
                Mode::Free if s.from.is_some() => {
                    if let Some(last) = s.path.last().copied() {
                        redraw.push(grown(rect_of(last, p, s.w, s.h), 3));
                    }
                    s.path.push(p);
                }
                Mode::Window => {
                    let under = s.windows.iter().find(|r| contains(r, p)).copied();
                    if under != s.hover_window {
                        redraw.extend(s.hover_window.map(|r| grown(r, 3)));
                        redraw.extend(under.map(|r| grown(r, 3)));
                        s.hover_window = under;
                        redraw.push(bar_rect(s));
                    }
                }
                _ => {}
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
