//! The frame for Cascade's own apps (the file manager, the disk map, the
//! photo viewer): each runs as its own `cascade.exe --files` / `--disk` /
//! `--view` process, so one going wrong never takes the taskbar with it, and
//! each runs at normal priority, as apps should. A window with Cascade's
//! icon and a dark title bar in the colour of its content, drawn with
//! Direct2D like the rest of Cascade.

use windows::core::{BOOL, HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWINDOWATTRIBUTE};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::gfx::{Color, Fonts, Gfx, Painter};

/// An app window's background, and its title bar's (the same colour).
pub const BG: Color = Color(0.125, 0.125, 0.13, 1.0);
const CAPTION: u32 = 0x0021_2020;
pub const PANEL: Color = Color(0.155, 0.155, 0.16, 1.0);
pub const LINE: Color = Color(1.0, 1.0, 1.0, 0.07);

pub fn instance() -> HINSTANCE {
    unsafe { GetModuleHandleW(None).map(|m| HINSTANCE(m.0)).unwrap_or_default() }
}

/// Registers a window class `class` with `proc`, and makes a window of it,
/// `w` x `h` (logical pixels) on the screen the pointer is on, shown.
pub fn create(class: PCWSTR, title: &str, w: i32, h: i32, proc: WNDPROC) -> Option<HWND> {
    unsafe {
        let inst = instance();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_DBLCLKS,
            lpfnWndProc: proc,
            hInstance: inst,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hIcon: crate::shell::app_icon(32).unwrap_or_default(),
            hIconSm: crate::shell::app_icon(16).unwrap_or_default(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        // Sized for the screen the pointer is on.
        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        let mon = MonitorFromPoint(p, MONITOR_DEFAULTTOPRIMARY);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut mi);
        let mut dpi = (96u32, 96u32);
        let _ = windows::Win32::UI::HiDpi::GetDpiForMonitor(
            mon,
            windows::Win32::UI::HiDpi::MDT_EFFECTIVE_DPI,
            &mut dpi.0,
            &mut dpi.1,
        );
        let k = dpi.0 as f32 / 96.0;
        let work = mi.rcWork;
        let (ww, wh) = (
            ((w as f32 * k) as i32).min(work.right - work.left - 40),
            ((h as f32 * k) as i32).min(work.bottom - work.top - 40),
        );
        let (x, y) = (work.left + (work.right - work.left - ww) / 2, work.top + (work.bottom - work.top - wh) / 2);
        let hwnd = CreateWindowExW(
            WS_EX_APPWINDOW,
            class,
            &HSTRING::from(title),
            WS_OVERLAPPEDWINDOW,
            x,
            y,
            ww,
            wh,
            None,
            None,
            Some(inst),
            None,
        )
        .ok()?;
        let dark = BOOL(1);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as *const _, 4);
        let caption = COLORREF(CAPTION);
        let _ = DwmSetWindowAttribute(hwnd, DWMWINDOWATTRIBUTE(35), &caption as *const _ as *const _, 4);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        Some(hwnd)
    }
}

/// The window's scale (1.0 at 100%).
pub fn scale(hwnd: HWND) -> f32 {
    unsafe { GetDpiForWindow(hwnd).max(96) as f32 / 96.0 }
}

/// The client area in logical pixels.
pub fn client(hwnd: HWND) -> (f32, f32) {
    let mut r = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut r);
    }
    let k = scale(hwnd);
    ((r.right - r.left) as f32 / k, (r.bottom - r.top) as f32 / k)
}

/// Paints the window (for its WM_PAINT): `draw` gets a painter, the fonts
/// and the size in logical pixels.
pub fn paint(hwnd: HWND, gfx: &mut Gfx, draw: impl FnOnce(&Painter, &Fonts, f32, f32)) {
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let dc = BeginPaint(hwnd, &mut ps);
        let mut r = RECT::default();
        let _ = GetClientRect(hwnd, &mut r);
        let k = scale(hwnd);
        let (pw, ph) = (r.right - r.left, r.bottom - r.top);
        if pw > 0 && ph > 0 && gfx.begin_dc(dc, pw, ph, k, BG).is_ok() {
            {
                let p = gfx.painter();
                draw(&p, &gfx.fonts, pw as f32 / k, ph as f32 / k);
            }
            let _ = gfx.end();
        }
        let _ = EndPaint(hwnd, &ps);
    }
}

pub fn redraw(hwnd: HWND) {
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

/// A mouse message's position in logical pixels.
pub fn mouse(hwnd: HWND, lp: LPARAM) -> (f32, f32) {
    let k = scale(hwnd);
    ((lp.0 & 0xFFFF) as i16 as f32 / k, ((lp.0 >> 16) & 0xFFFF) as i16 as f32 / k)
}

/// Runs the window's messages until it's closed.
pub fn run() {
    unsafe {
        let mut m = MSG::default();
        while GetMessageW(&mut m, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&m);
            DispatchMessageW(&m);
        }
    }
}

/// Starts this exe again with `args` (another app window, its own process).
pub fn launch(args: &[&str]) {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).args(args).spawn();
    }
}

/// "1.4 GB" and the like.
pub fn size_text(b: u64) -> String {
    const UNITS: [&str; 5] = ["bytes", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    match u {
        0 => format!("{b} bytes"),
        _ if v >= 100.0 => format!("{v:.0} {}", UNITS[u]),
        _ => format!("{v:.1} {}", UNITS[u]),
    }
}

/// Common setup for an app process: COM for the shell, the theme's accent.
pub fn init() {
    unsafe {
        let _ = windows::Win32::System::Ole::OleInitialize(None);
    }
    let cfg = crate::config::Config::load();
    crate::theme::set(&cfg.theme);
    crate::theme::set_accent(&cfg.accent, cfg.accent_glass);
    crate::theme::refresh_accent();
}
