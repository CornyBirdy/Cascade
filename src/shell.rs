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

// ------------------------------------------------------------------ menu --

#[derive(Clone, Copy, PartialEq)]
pub enum MenuCmd {
    None,
    Autostart,
    Flyout,
    OpenConfig,
    Reload,
    Quit,
}

pub struct MenuState {
    pub autostart: bool,
    pub flyout_hidden: bool,
}

pub fn show_menu(hwnd: HWND, state: MenuState) -> MenuCmd {
    const IDS: [MenuCmd; 6] =
        [MenuCmd::None, MenuCmd::Autostart, MenuCmd::Flyout, MenuCmd::OpenConfig, MenuCmd::Reload, MenuCmd::Quit];
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return MenuCmd::None };
        let check = |on: bool| if on { MF_STRING | MF_CHECKED } else { MF_STRING };
        let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, w!("Cascade"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, check(state.autostart), 1, w!("Start with Windows"));
        let _ = AppendMenuW(menu, check(state.flyout_hidden), 2, w!("Hide Windows volume flyout"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, 3, w!("Open config file"));
        let _ = AppendMenuW(menu, MF_STRING, 4, w!("Reload config"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, 5, w!("Quit Cascade"));
        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        // Required so the menu closes when clicking elsewhere.
        let _ = SetForegroundWindow(hwnd);
        let id = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY, p.x, p.y, 0, hwnd, None);
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        IDS.get(id.0 as usize).copied().unwrap_or(MenuCmd::None)
    }
}

pub fn open_file(path: &std::path::Path) {
    unsafe {
        let _ = ShellExecuteW(None, w!("open"), &HSTRING::from(path.as_os_str()), None, None, SW_SHOWNORMAL);
    }
}
