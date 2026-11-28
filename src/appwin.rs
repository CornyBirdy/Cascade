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
use windows::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
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
    crate::shellmode::detect();
    let cfg = crate::config::Config::load();
    crate::theme::set(&cfg.theme);
    crate::theme::set_accent(&cfg.accent, cfg.accent_glass);
    crate::theme::refresh_accent();
    crate::menu::configure(cfg.menu_animation);
}

// ------------------------------------------- a frame with no title bar --
//
// An app can put its own content (Files' tabs) where the title bar was, with
// the minimise, maximise and close buttons drawn beside it, as Explorer
// does: `frameless` removes the caption (keeping the resize edges, the
// shadow and the rounded corners), `edge_hit` gives back the top resize
// edge, and the app draws the three buttons itself (`caption_at` finds
// which is under the pointer, `caption_press` presses it).

/// The width of each caption button (as Windows 11's).
pub const CAPTION_W: f32 = 46.0;

/// Takes the title bar away (call after the window is made).
pub fn frameless(hwnd: HWND) {
    unsafe {
        let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER);
    }
}

fn frame_px(hwnd: HWND) -> (i32, i32) {
    unsafe {
        let dpi = GetDpiForWindow(hwnd).max(96);
        let pad = GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi);
        (GetSystemMetricsForDpi(SM_CXFRAME, dpi) + pad, GetSystemMetricsForDpi(SM_CYFRAME, dpi) + pad)
    }
}

pub fn maximized(hwnd: HWND) -> bool {
    unsafe { IsZoomed(hwnd).as_bool() }
}

/// For WM_NCCALCSIZE: the client area reaches the top (no caption), inside
/// the side and bottom edges; maximised, inside the top edge too (which
/// Windows puts off the screen).
pub fn frameless_calc(hwnd: HWND, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    if wp.0 == 0 {
        return None;
    }
    let (fx, fy) = frame_px(hwnd);
    unsafe {
        let params = &mut *(lp.0 as *mut NCCALCSIZE_PARAMS);
        let r = &mut params.rgrc[0];
        r.left += fx;
        r.right -= fx;
        r.bottom -= fy;
        if maximized(hwnd) {
            r.top += fy;
        }
    }
    Some(LRESULT(0))
}

/// For WM_NCHITTEST: the top resize edge (Windows still gives the others).
pub fn edge_hit(hwnd: HWND, lp: LPARAM) -> Option<LRESULT> {
    if maximized(hwnd) {
        return None;
    }
    let mut pt = POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 };
    unsafe {
        let _ = ScreenToClient(hwnd, &mut pt);
    }
    let (fx, fy) = frame_px(hwnd);
    if pt.y >= fy {
        return None;
    }
    let (w, _) = client(hwnd);
    let w = (w * scale(hwnd)) as i32;
    Some(LRESULT(if pt.x < fx {
        HTTOPLEFT
    } else if pt.x >= w - fx {
        HTTOPRIGHT
    } else {
        HTTOP
    } as isize))
}

/// Which caption button (0 minimise, 1 maximise, 2 close) is at `x`, `y`
/// (logical) in a window `w` wide whose buttons are `h` tall.
pub fn caption_at(x: f32, y: f32, w: f32, h: f32) -> Option<u8> {
    if y < 0.0 || y >= h || x < w - 3.0 * CAPTION_W {
        return None;
    }
    Some(((x - (w - 3.0 * CAPTION_W)) / CAPTION_W).floor().clamp(0.0, 2.0) as u8)
}

/// Presses caption button `b` (as `caption_at` numbers them).
pub fn caption_press(hwnd: HWND, b: u8) {
    unsafe {
        let cmd = match b {
            0 => SC_MINIMIZE,
            1 if maximized(hwnd) => SC_RESTORE,
            1 => SC_MAXIMIZE,
            _ => SC_CLOSE,
        };
        let _ = PostMessageW(Some(hwnd), WM_SYSCOMMAND, WPARAM(cmd as usize), LPARAM(0));
    }
}

// ------------------------------------------------------- app identities --
//
// Each of Cascade's apps is its own app on the taskbar (Windows' and
// Cascade's), with its own icon, rather than all being "Cascade": each
// window says which it is (its AppUserModelID), and `shell::open` starts one
// from that id.

/// Cascade's apps as the taskbar knows them: (AppUserModelID, name, how
/// it's started, Windows' stock icon for it, and a file whose icon is
/// better where there is one: Windows' own app of the same kind).
pub const OWN_APPS: [(&str, &str, &str, i32, &str); 12] = [
    ("Cascade.Files", "Files", "--files", 3, ""),
    ("Cascade.Photos", "Photos", "--view", 72, ""),
    ("Cascade.DiskUsage", "Disk Usage", "--disk", 8, ""),
    ("Cascade.Calculator", "Calculator", "--calc", 2, "calc.exe"),
    ("Cascade.Camera", "Camera", "--camera", 73, ""),
    ("Cascade.Notepad", "Notepad", "--notepad", 1, "notepad.exe"),
    ("Cascade.Clock", "Clock", "--clock", 2, "timedate.cpl"),
    ("Cascade.MediaPlayer", "Media Player", "--player", 73, "%ProgramFiles%\\Windows Media Player\\wmplayer.exe"),
    ("Cascade.SoundRecorder", "Sound Recorder", "--soundrec", 71, ""),
    ("Cascade.Paint", "Paint", "--paint", 72, "mspaint.exe"),
    ("Cascade.Snip", "Snip", "--snip", 72, ""),
    ("Cascade.QuickShare", "Quick Share", "--quickshare-send", 72, ""),
];

/// The taskbar's id for one of Cascade's apps (`cascade-app:<AppUserModelID>`).
pub fn own_app_id(aumid: &str) -> String {
    format!("cascade-app:{aumid}")
}

/// Cascade's app `aumid`: its name, the flag that starts it and its icon.
pub fn own_app(aumid: &str) -> Option<(&'static str, &'static str, i32)> {
    OWN_APPS.iter().find(|a| a.0.eq_ignore_ascii_case(aumid)).map(|a| (a.1, a.2, a.3))
}

/// The icon in `file` (in System32 unless it says where; %vars% expanded).
fn file_icon(file: &str, large: bool) -> Option<HICON> {
    use windows::Win32::UI::Shell::ExtractIconExW;
    if file.is_empty() {
        return None;
    }
    let mut path = file.to_string();
    for (var, val) in std::env::vars() {
        path = path.replace(&format!("%{var}%"), &val);
    }
    if !path.contains('\\') {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        path = format!("{root}\\System32\\{path}");
    }
    if !std::path::Path::new(&path).exists() {
        return None;
    }
    let mut icon = HICON::default();
    let got = unsafe {
        if large {
            ExtractIconExW(&HSTRING::from(path), 0, Some(&mut icon), None, 1)
        } else {
            ExtractIconExW(&HSTRING::from(path), 0, None, Some(&mut icon), 1)
        }
    };
    (got > 0 && !icon.is_invalid()).then_some(icon)
}

/// The icon of Cascade's app `aumid` (`large` or small): its own (see
/// `appicons`), else Windows' icon for its kind of app.
pub fn own_app_icon(aumid: &str, large: bool) -> Option<HICON> {
    use windows::Win32::UI::Shell::*;
    if crate::appicons::has(aumid) {
        let size = unsafe { GetSystemMetrics(if large { SM_CXICON } else { SM_CXSMICON }) };
        if let Some(icon) = crate::appicons::hicon(aumid, size.max(16)) {
            return Some(icon);
        }
    }
    let (_, _, stock) = own_app(aumid)?;
    let file = OWN_APPS.iter().find(|a| a.0.eq_ignore_ascii_case(aumid)).map_or("", |a| a.4);
    if let Some(icon) = file_icon(file, large) {
        return Some(icon);
    }
    let mut info = SHSTOCKICONINFO { cbSize: std::mem::size_of::<SHSTOCKICONINFO>() as u32, ..Default::default() };
    let size = if large { SHGSI_LARGEICON } else { SHGSI_SMALLICON };
    unsafe { SHGetStockIconInfo(SHSTOCKICONID(stock), SHGSI_ICON | size, &mut info).ok()? };
    (!info.hIcon.is_invalid()).then_some(info.hIcon)
}

/// Marks `hwnd` as Cascade's app `aumid`: its own taskbar button and icon.
pub fn identity(hwnd: HWND, aumid: &str) {
    use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, SHGetPropertyStoreForWindow};
    const PKEY_APP_USER_MODEL_ID: windows::Win32::Foundation::PROPERTYKEY = windows::Win32::Foundation::PROPERTYKEY {
        fmtid: windows::core::GUID::from_u128(0x9F4C2855_9F79_4B39_A8D0_E1D42DE1D5F3),
        pid: 5,
    };
    unsafe {
        if let Ok(store) = SHGetPropertyStoreForWindow::<IPropertyStore>(hwnd) {
            let value = windows::Win32::System::Com::StructuredStorage::PROPVARIANT::from(aumid);
            let _ = store.SetValue(&PKEY_APP_USER_MODEL_ID, &value);
            let _ = store.Commit();
        }
        for (large, which) in [(true, ICON_BIG), (false, ICON_SMALL)] {
            if let Some(icon) = own_app_icon(aumid, large) {
                SendMessageW(hwnd, WM_SETICON, Some(WPARAM(which as usize)), Some(LPARAM(icon.0 as isize)));
            }
        }
    }
}
