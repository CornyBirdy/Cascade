//! Windows shell integration: single instance, tray icon, fullscreen
//! detection, start-with-Windows, the native flyout and the context menu.

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::Registry::*;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::msg;

/// Returns false if Cascade is already running.
pub fn claim_single_instance() -> bool {
    unsafe {
        let handle = CreateMutexW(None, true, w!("Local\\CascadeNotchSingleInstance"));
        // The mutex handle is intentionally leaked; it lives as long as we do.
        handle.is_ok() && GetLastError() != ERROR_ALREADY_EXISTS
    }
}

// ------------------------------------------------------------ fullscreen --

/// Registers a zero-size app bar. It reserves no space, but the shell then
/// tells us (ABN_FULLSCREENAPP) whenever a fullscreen app opens or closes,
/// which is exactly how the taskbar knows to get out of the way.
pub fn register_appbar(hwnd: HWND) {
    let mut abd = APPBARDATA {
        cbSize: std::mem::size_of::<APPBARDATA>() as u32,
        hWnd: hwnd,
        uCallbackMessage: msg::APPBAR,
        ..Default::default()
    };
    unsafe { SHAppBarMessage(ABM_NEW, &mut abd) };
}

pub fn unregister_appbar(hwnd: HWND) {
    let mut abd = APPBARDATA { cbSize: std::mem::size_of::<APPBARDATA>() as u32, hWnd: hwnd, ..Default::default() };
    unsafe { SHAppBarMessage(ABM_REMOVE, &mut abd) };
}

/// Whether something fullscreen is in front right now (used at startup).
pub fn fullscreen_now() -> bool {
    matches!(
        unsafe { SHQueryUserNotificationState() },
        Ok(QUNS_BUSY) | Ok(QUNS_RUNNING_D3D_FULL_SCREEN) | Ok(QUNS_PRESENTATION_MODE)
    )
}

// ------------------------------------------------------------------ tray --

fn make_icon() -> HICON {
    // A small notch silhouette, drawn per pixel with soft edges.
    const S: i32 = 32;
    unsafe {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: S,
                biHeight: -S,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let Ok(color) = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0) else {
            return LoadIconW(None, IDI_APPLICATION).unwrap_or_default();
        };
        let px = std::slice::from_raw_parts_mut(bits as *mut u32, (S * S) as usize);
        // Rounded rect body (2..30 x 8..24), radius 7; white fill, dark rim.
        let (x0, x1, y0, y1, r) = (2.0f32, 30.0f32, 8.0f32, 24.0f32, 7.0f32);
        for y in 0..S {
            for x in 0..S {
                let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                let cx = fx.clamp(x0 + r, x1 - r);
                let cy = fy.clamp(y0 + r, y1 - r);
                let d = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt() - r;
                let a = (0.5 - d).clamp(0.0, 1.0);
                let inner = (-1.0 - d).clamp(0.0, 1.0);
                let shade = 0.2 + 0.75 * inner;
                let c = (shade * 255.0) as u32;
                let al = (a * 255.0) as u32;
                px[(y * S + x) as usize] = (al << 24) | (c << 16) | (c << 8) | c;
            }
        }
        let mask = CreateBitmap(S, S, 1, 1, None);
        let ii = ICONINFO { fIcon: true.into(), xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        let icon = CreateIconIndirect(&ii).unwrap_or_default();
        let _ = DeleteObject(mask.into());
        let _ = DeleteObject(color.into());
        icon
    }
}

pub struct Tray {
    data: NOTIFYICONDATAW,
}

impl Tray {
    pub fn new(hwnd: HWND) -> Self {
        let mut data = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: 1,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
            uCallbackMessage: msg::TRAY,
            hIcon: make_icon(),
            ..Default::default()
        };
        for (d, s) in data.szTip.iter_mut().zip("Cascade".encode_utf16()) {
            *d = s;
        }
        let t = Self { data };
        t.add();
        t
    }

    /// Also called when Explorer restarts and the tray is rebuilt.
    pub fn add(&self) {
        unsafe {
            let _ = Shell_NotifyIconW(NIM_ADD, &self.data);
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &self.data);
            let _ = DestroyIcon(self.data.hIcon);
        }
    }
}

// ------------------------------------------------------------- autostart --

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const RUN_VALUE: PCWSTR = w!("Cascade");

pub fn autostart_enabled() -> bool {
    unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE, RRF_RT_REG_SZ, None, None, None) == ERROR_SUCCESS }
}

pub fn set_autostart(on: bool) {
    unsafe {
        if on {
            let mut buf = [0u16; 1024];
            let n = GetModuleFileNameW(None, &mut buf) as usize;
            let cmd: Vec<u16> =
                format!("\"{}\"", String::from_utf16_lossy(&buf[..n])).encode_utf16().chain([0]).collect();
            let _ = RegSetKeyValueW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                RUN_VALUE,
                REG_SZ.0,
                Some(cmd.as_ptr() as *const _),
                (cmd.len() * 2) as u32,
            );
        } else {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE);
        }
    }
}

// -------------------------------------------------------- native flyout --

/// Windows 11 draws its volume/brightness flyout in a small XAML host window
/// owned by Explorer. Making it fully transparent hides the flyout without
/// touching Explorer otherwise; restoring flips it back.
pub fn set_flyout_hidden(hide: bool) {
    unsafe {
        let Ok(tray) = FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()) else { return };
        let mut explorer = 0u32;
        GetWindowThreadProcessId(tray, Some(&mut explorer));
        let (sw, sh) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
        let mut after: Option<HWND> = None;
        while let Ok(h) = FindWindowExW(None, after, w!("XamlExplorerHostIslandWindow"), PCWSTR::null()) {
            after = Some(h);
            let mut pid = 0u32;
            GetWindowThreadProcessId(h, Some(&mut pid));
            let ex = GetWindowLongW(h, GWL_EXSTYLE) as u32;
            let mut r = RECT::default();
            let small = GetWindowRect(h, &mut r).is_ok() && (r.right - r.left) < sw / 2 && (r.bottom - r.top) < sh / 2;
            if pid != explorer || ex & WS_EX_NOACTIVATE.0 == 0 || !small {
                continue;
            }
            let mut alpha = 255u8;
            let hidden = ex & WS_EX_LAYERED.0 != 0
                && GetLayeredWindowAttributes(h, None, Some(&mut alpha), None).is_ok()
                && alpha == 0;
            if hide && !hidden {
                SetWindowLongW(h, GWL_EXSTYLE, (ex | WS_EX_LAYERED.0 | WS_EX_TRANSPARENT.0) as i32);
                let _ = SetLayeredWindowAttributes(h, COLORREF(0), 0, LWA_ALPHA);
            } else if !hide && hidden {
                let _ = SetLayeredWindowAttributes(h, COLORREF(0), 255, LWA_ALPHA);
                SetWindowLongW(h, GWL_EXSTYLE, (ex & !(WS_EX_LAYERED.0 | WS_EX_TRANSPARENT.0)) as i32);
            }
        }
    }
}

// --------------------------------------------------------------- taskbar --

fn taskbars() -> Vec<HWND> {
    let mut out = Vec::new();
    unsafe {
        if let Ok(h) = FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()) {
            out.push(h);
        }
        let mut after: Option<HWND> = None;
        while let Ok(h) = FindWindowExW(None, after, w!("Shell_SecondaryTrayWnd"), PCWSTR::null()) {
            after = Some(h);
            out.push(h);
        }
    }
    out
}

pub fn is_taskbar(hwnd: HWND) -> bool {
    taskbars().contains(&hwnd)
}

fn taskbar_marker() -> std::path::PathBuf {
    crate::config::dir().join("taskbar-hidden")
}

fn set_taskbar_state(state: u32) {
    let Some(&tray) = taskbars().first() else { return };
    let mut abd = APPBARDATA {
        cbSize: std::mem::size_of::<APPBARDATA>() as u32,
        hWnd: tray,
        lParam: LPARAM(state as isize),
        ..Default::default()
    };
    unsafe { SHAppBarMessage(ABM_SETSTATE, &mut abd) };
}

/// Hides the Windows taskbar. Its auto-hide setting is switched on so the
/// screen space is given back; the original setting is saved to a marker
/// file so it is restored even after a crash.
pub fn hide_taskbar() {
    let marker = taskbar_marker();
    if !marker.exists() {
        let mut abd = APPBARDATA { cbSize: std::mem::size_of::<APPBARDATA>() as u32, ..Default::default() };
        let state = unsafe { SHAppBarMessage(ABM_GETSTATE, &mut abd) } as u32;
        let _ = std::fs::create_dir_all(crate::config::dir());
        let _ = std::fs::write(&marker, state.to_string());
    }
    set_taskbar_state(ABS_AUTOHIDE);
    reassert_taskbar_hidden();
}

/// Explorer re-shows the taskbar now and then (Start menu, explorer restart).
pub fn reassert_taskbar_hidden() {
    for t in taskbars() {
        unsafe {
            if IsWindowVisible(t).as_bool() {
                let _ = ShowWindow(t, SW_HIDE);
            }
        }
    }
}

/// Restores the taskbar if Cascade hid it (now or in a run that crashed).
pub fn restore_taskbar() {
    let marker = taskbar_marker();
    let Ok(text) = std::fs::read_to_string(&marker) else { return };
    set_taskbar_state(text.trim().parse().unwrap_or(ABS_ALWAYSONTOP));
    for t in taskbars() {
        unsafe {
            let _ = ShowWindow(t, SW_SHOWNA);
        }
    }
    let _ = std::fs::remove_file(marker);
}

/// Reserves a strip at the bottom of the primary monitor for `hwnd` (like the
/// taskbar does) so maximised windows stop above it. Returns the granted rect.
/// `current` is the rect granted last time; if nothing changed the shell is
/// left alone, so position-change notifications can't feed back into a loop.
pub fn reserve_bottom(hwnd: HWND, monitor: RECT, height: i32, registered: &mut bool, current: Option<RECT>) -> RECT {
    let mut abd = APPBARDATA {
        cbSize: std::mem::size_of::<APPBARDATA>() as u32,
        hWnd: hwnd,
        uCallbackMessage: msg::APPBAR,
        uEdge: ABE_BOTTOM,
        rc: RECT { top: monitor.bottom - height, ..monitor },
        ..Default::default()
    };
    unsafe {
        if !*registered {
            *registered = SHAppBarMessage(ABM_NEW, &mut abd) != 0;
        }
        SHAppBarMessage(ABM_QUERYPOS, &mut abd);
        // Windows 11 does not always move the proposed rect clear of the
        // taskbar, so make sure the dock ends above a visible bottom taskbar.
        if let Some(top) = bottom_taskbar_top(monitor) {
            abd.rc.bottom = abd.rc.bottom.min(top);
        }
        abd.rc.top = abd.rc.bottom - height;
        if current == Some(abd.rc) {
            return abd.rc;
        }
        SHAppBarMessage(ABM_SETPOS, &mut abd);
    }
    abd.rc
}

/// Top edge of the main taskbar if it is showing along the bottom of
/// `monitor` (not hidden by Cascade and not set to auto-hide).
pub fn bottom_taskbar_top(monitor: RECT) -> Option<i32> {
    let tray = *taskbars().first()?;
    unsafe {
        if !IsWindowVisible(tray).as_bool() {
            return None;
        }
        let mut abd = APPBARDATA { cbSize: std::mem::size_of::<APPBARDATA>() as u32, ..Default::default() };
        if SHAppBarMessage(ABM_GETSTATE, &mut abd) as u32 & ABS_AUTOHIDE != 0 {
            return None;
        }
        let mut abd = APPBARDATA { cbSize: std::mem::size_of::<APPBARDATA>() as u32, hWnd: tray, ..Default::default() };
        if SHAppBarMessage(ABM_GETTASKBARPOS, &mut abd) == 0 {
            return None;
        }
        let r = abd.rc;
        let at_bottom = r.bottom >= monitor.bottom - 2 && r.top > monitor.top + (monitor.bottom - monitor.top) / 2;
        let on_monitor = r.left < monitor.right && r.right > monitor.left;
        (at_bottom && on_monitor).then_some(r.top)
    }
}

pub fn work_area() -> RECT {
    let mut r = RECT::default();
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut r as *mut _ as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
    }
    r
}

// ------------------------------------------------------------------ menu --

#[derive(Clone, Copy, PartialEq)]
pub enum MenuCmd {
    None,
    Notch,
    Dock,
    DockAutohide,
    HideTaskbar,
    Autostart,
    Flyout,
    OpenConfig,
    Reload,
    Quit,
}

pub struct MenuState {
    pub notch: bool,
    pub dock: bool,
    pub dock_autohide: bool,
    pub hide_taskbar: bool,
    pub autostart: bool,
    pub flyout_hidden: bool,
}

fn check(on: bool) -> MENU_ITEM_FLAGS {
    if on {
        MF_STRING | MF_CHECKED
    } else {
        MF_STRING
    }
}

/// Shows `menu` at the cursor and returns the chosen item id (0 = none).
fn track(hwnd: HWND, menu: HMENU) -> usize {
    unsafe {
        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        // Required so the menu closes when clicking elsewhere.
        let _ = SetForegroundWindow(hwnd);
        let id = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY, p.x, p.y, Some(0), hwnd, None);
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        id.0 as usize
    }
}

pub fn show_menu(hwnd: HWND, state: MenuState) -> MenuCmd {
    const IDS: [MenuCmd; 10] = [
        MenuCmd::None,
        MenuCmd::Notch,
        MenuCmd::Dock,
        MenuCmd::DockAutohide,
        MenuCmd::HideTaskbar,
        MenuCmd::Autostart,
        MenuCmd::Flyout,
        MenuCmd::OpenConfig,
        MenuCmd::Reload,
        MenuCmd::Quit,
    ];
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return MenuCmd::None };
        let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, w!("Cascade"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, check(state.notch), 1, w!("Show notch"));
        let _ = AppendMenuW(menu, check(state.dock), 2, w!("Show dock"));
        let flags = if state.dock { check(state.dock_autohide) } else { check(state.dock_autohide) | MF_GRAYED };
        let _ = AppendMenuW(menu, flags, 3, w!("Auto-hide dock"));
        let _ = AppendMenuW(menu, check(state.hide_taskbar), 4, w!("Hide Windows taskbar"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, check(state.autostart), 5, w!("Start with Windows"));
        let _ = AppendMenuW(menu, check(state.flyout_hidden), 6, w!("Hide Windows volume flyout"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, 7, w!("Open config file"));
        let _ = AppendMenuW(menu, MF_STRING, 8, w!("Reload config"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, 9, w!("Quit Cascade"));
        IDS.get(track(hwnd, menu)).copied().unwrap_or(MenuCmd::None)
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum AppCmd {
    None,
    NewWindow,
    TogglePin,
    CloseAll,
}

/// Right-click menu for a dock icon.
pub fn show_app_menu(hwnd: HWND, name: &str, pinned: bool, windows: usize) -> AppCmd {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return AppCmd::None };
        let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, &HSTRING::from(name));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, 1, w!("New window"));
        let _ = AppendMenuW(menu, MF_STRING, 2, if pinned { w!("Unpin from dock") } else { w!("Pin to dock") });
        if windows > 0 {
            let label = if windows == 1 { "Close window".to_string() } else { format!("Close {windows} windows") };
            let _ = AppendMenuW(menu, MF_STRING, 3, &HSTRING::from(label));
        }
        match track(hwnd, menu) {
            1 => AppCmd::NewWindow,
            2 => AppCmd::TogglePin,
            3 => AppCmd::CloseAll,
            _ => AppCmd::None,
        }
    }
}

pub fn open(target: &str) {
    unsafe {
        let _ = ShellExecuteW(None, w!("open"), &HSTRING::from(target), None, None, SW_SHOWNORMAL);
    }
}

pub fn open_file(path: &std::path::Path) {
    unsafe {
        let _ = ShellExecuteW(None, w!("open"), &HSTRING::from(path.as_os_str()), None, None, SW_SHOWNORMAL);
    }
}
