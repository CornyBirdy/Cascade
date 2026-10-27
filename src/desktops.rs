//! Windows' virtual desktops: how many there are, which one you're on and
//! what they're called, for the taskbar's desktop dots and Task View
//! (Win+Tab).
//!
//! Windows has no public way to list desktops, but Explorer keeps them in
//! the registry (their ids, the current one and their names), so they're
//! read from there. A thread waits on that key (RegNotifyChangeKeyValue) and
//! says when it changes; nothing is polled. Switching and making desktops go
//! through Windows' own shortcuts (Ctrl+Win+Left/Right, Ctrl+Win+D), so it's
//! the same as doing it from the keyboard.
//!
//! Moving a window to another desktop has no such shortcut, and the public
//! `IVirtualDesktopManager::MoveWindowToDesktop` only moves the caller's own
//! windows. Task View's drag uses the shell's internal desktop manager, the
//! way Explorer does (see `move_window`).

use std::ffi::c_void;

use windows::core::{interface, w, IUnknown, IUnknown_Vtbl, Interface, Result, BOOL, GUID, HRESULT, HSTRING, PCWSTR};
use windows::Win32::Foundation::{ERROR_SUCCESS, E_INVALIDARG, HANDLE, HWND, LPARAM};
use windows::Win32::System::Com::{CoCreateInstance, IServiceProvider, CLSCTX_ALL};
use windows::Win32::System::Registry::*;
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::Common::IObjectArray;
use windows::Win32::UI::Shell::{IVirtualDesktopManager, VirtualDesktopManager};
use windows::Win32::UI::WindowsAndMessaging::EnumWindows;

use crate::msg;

const KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\VirtualDesktops";

#[derive(Clone, Debug, PartialEq)]
pub struct Desktops {
    /// Each desktop's name ("Desktop 2" when it has none), in order.
    pub names: Vec<String>,
    pub current: usize,
}

impl Default for Desktops {
    fn default() -> Self {
        Self { names: vec!["Desktop 1".into()], current: 0 }
    }
}

/// A registry value's bytes.
fn binary(key: &HSTRING, value: PCWSTR) -> Option<Vec<u8>> {
    let mut size = 0u32;
    unsafe {
        if RegGetValueW(HKEY_CURRENT_USER, key, value, RRF_RT_REG_BINARY, None, None, Some(&mut size)) != ERROR_SUCCESS
        {
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        let ok = RegGetValueW(
            HKEY_CURRENT_USER,
            key,
            value,
            RRF_RT_REG_BINARY,
            None,
            Some(buf.as_mut_ptr() as *mut c_void),
            Some(&mut size),
        ) == ERROR_SUCCESS;
        buf.truncate(size as usize);
        ok.then_some(buf)
    }
}

fn text(key: &HSTRING, value: PCWSTR) -> Option<String> {
    let mut buf = [0u16; 256];
    let mut size = (buf.len() * 2) as u32;
    unsafe {
        let ok = RegGetValueW(
            HKEY_CURRENT_USER,
            key,
            value,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut c_void),
            Some(&mut size),
        ) == ERROR_SUCCESS;
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        ok.then(|| String::from_utf16_lossy(&buf[..end]))
    }
}

/// A desktop id's 16 bytes as Windows writes it: {XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}.
fn guid_text(b: &[u8]) -> String {
    let d1 = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
    let d2 = u16::from_le_bytes([b[4], b[5]]);
    let d3 = u16::from_le_bytes([b[6], b[7]]);
    let tail: String = b[10..16].iter().map(|x| format!("{x:02X}")).collect();
    format!("{{{d1:08X}-{d2:04X}-{d3:04X}-{:02X}{:02X}-{tail}}}", b[8], b[9])
}

/// Where Windows 10 keeps the current desktop (Windows 11 keeps it in `KEY`).
fn session_key() -> HSTRING {
    let mut session = 0u32;
    unsafe {
        let _ = ProcessIdToSessionId(GetCurrentProcessId(), &mut session);
    }
    HSTRING::from(format!(
        "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\SessionInfo\\{session}\\VirtualDesktops"
    ))
}

/// The desktops as they are now (one, if Windows has never had more).
pub fn read() -> Desktops {
    let key = HSTRING::from(KEY);
    let Some(ids) = binary(&key, w!("VirtualDesktopIDs")).filter(|b| b.len() >= 16) else {
        return Desktops::default();
    };
    let ids: &[[u8; 16]] = ids.as_chunks::<16>().0;
    let current = binary(&key, w!("CurrentVirtualDesktop"))
        .or_else(|| binary(&session_key(), w!("CurrentVirtualDesktop")))
        .and_then(|c| ids.iter().position(|id| id.as_slice() == c.as_slice()))
        .unwrap_or(0);
    let names = ids
        .iter()
        .enumerate()
        .map(|(i, id)| {
            let sub = HSTRING::from(format!("{KEY}\\Desktops\\{}", guid_text(id)));
            text(&sub, w!("Name")).filter(|n| !n.trim().is_empty()).unwrap_or_else(|| format!("Desktop {}", i + 1))
        })
        .collect();
    Desktops { names, current }
}

/// A desktop id as the 16 bytes Explorer keeps in the registry.
fn guid_bytes(g: &GUID) -> [u8; 16] {
    let mut b = [0u8; 16];
    b[0..4].copy_from_slice(&g.data1.to_le_bytes());
    b[4..6].copy_from_slice(&g.data2.to_le_bytes());
    b[6..8].copy_from_slice(&g.data3.to_le_bytes());
    b[8..16].copy_from_slice(&g.data4);
    b
}

/// The apps with a window on each desktop (indexed as `Desktops` is), by name
/// and without repeats. Asks Explorer which desktop each window is on, so
/// call it off the UI thread, on one that has initialised COM.
pub fn apps() -> Vec<Vec<String>> {
    unsafe extern "system" fn collect(h: HWND, out: LPARAM) -> BOOL {
        let (windows, own) = &mut *(out.0 as *mut (Vec<HWND>, u32));
        if crate::dock::is_app_window_any_desktop(h, *own) {
            windows.push(h);
        }
        true.into()
    }
    let mut all = (Vec::<HWND>::new(), unsafe { GetCurrentProcessId() });
    unsafe {
        let _ = EnumWindows(Some(collect), LPARAM(&mut all as *mut _ as isize));
    }
    let windows = all.0;
    let key = HSTRING::from(KEY);
    let ids = binary(&key, w!("VirtualDesktopIDs")).unwrap_or_default();
    let ids: Vec<[u8; 16]> = ids.as_chunks::<16>().0.to_vec();
    let mut found: Vec<Vec<String>> = vec![Vec::new(); ids.len().max(1)];
    let manager: Result<IVirtualDesktopManager> = unsafe { CoCreateInstance(&VirtualDesktopManager, None, CLSCTX_ALL) };
    let Ok(manager) = manager else { return found };
    for window in windows {
        let Ok(id) = (unsafe { manager.GetWindowDesktopId(window) }) else { continue };
        // With one desktop there's no list of ids: they're all on it.
        let Some(desktop) = (if ids.is_empty() { Some(0) } else { ids.iter().position(|d| *d == guid_bytes(&id)) })
        else {
            continue;
        };
        let name = crate::dock::window_app_name(window);
        if !found[desktop].contains(&name) {
            found[desktop].push(name);
        }
    }
    found
}

/// Reads `apps` on a thread of its own and posts `msg::DESK_APPS` with them.
pub fn spawn_apps() {
    std::thread::spawn(|| unsafe {
        use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
        let inited = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
        let ptr = Box::into_raw(Box::new(apps()));
        if !msg::post(msg::DESK_APPS, 0, ptr as isize) {
            drop(Box::from_raw(ptr));
        }
        if inited {
            CoUninitialize();
        }
    });
}

/// Starts the thread that posts `msg::DESKTOPS` whenever the desktops (or
/// the current one) change.
pub fn watch() {
    std::thread::spawn(|| unsafe {
        let mut keys: Vec<HKEY> = Vec::new();
        for path in [HSTRING::from(KEY), session_key()] {
            let mut k = HKEY::default();
            // Made if it isn't there yet (Explorer makes it with a second desktop anyway).
            let made = RegCreateKeyExW(
                HKEY_CURRENT_USER,
                &path,
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_NOTIFY | KEY_READ,
                None,
                &mut k,
                None,
            );
            if made == ERROR_SUCCESS {
                keys.push(k);
            }
        }
        if keys.is_empty() {
            return;
        }
        let events: Vec<HANDLE> = keys
            .iter()
            .filter_map(|_| windows::Win32::System::Threading::CreateEventW(None, false, false, PCWSTR::null()).ok())
            .collect();
        if events.len() != keys.len() {
            return;
        }
        loop {
            for (k, e) in keys.iter().zip(&events) {
                let filter = REG_NOTIFY_CHANGE_NAME | REG_NOTIFY_CHANGE_LAST_SET;
                let _ = RegNotifyChangeKeyValue(*k, true, filter, Some(*e), true);
            }
            let r = windows::Win32::System::Threading::WaitForMultipleObjects(
                &events,
                false,
                windows::Win32::System::Threading::INFINITE,
            );
            if r.0 >= events.len() as u32 {
                return;
            }
            msg::post(msg::DESKTOPS, 0, 0);
        }
    });
}

/// Presses Ctrl+Win+`vk` (`extended` for the arrow keys).
fn ctrl_win(vk: VIRTUAL_KEY, extended: bool, times: usize) {
    let key = |vk: VIRTUAL_KEY, up: bool, ext: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) }
                    | if ext { KEYEVENTF_EXTENDEDKEY } else { KEYBD_EVENT_FLAGS(0) },
                ..Default::default()
            },
        },
    };
    let mut inputs = vec![key(VK_LWIN, false, true), key(VK_LCONTROL, false, false)];
    for _ in 0..times {
        inputs.push(key(vk, false, extended));
        inputs.push(key(vk, true, extended));
    }
    inputs.push(key(VK_LCONTROL, true, false));
    inputs.push(key(VK_LWIN, true, true));
    unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
}

/// Goes from desktop `from` to desktop `to`.
pub fn switch(from: usize, to: usize) {
    if to > from {
        ctrl_win(VK_RIGHT, true, to - from);
    } else if from > to {
        ctrl_win(VK_LEFT, true, from - to);
    }
}

/// Makes a new desktop and goes to it, as Ctrl+Win+D does.
pub fn create() {
    ctrl_win(VK_D, false, 1);
}

// The shell's own desktop interfaces. They're undocumented, so only the parts
// that stay put are declared: Windows 11's manager keeps its first seven
// methods in the same order from its first release to 24H2 and 25H2 (which
// add one after `SwitchDesktop`), and a Windows that doesn't have these
// interface ids simply refuses to hand them out, so a move fails cleanly.
// Layouts as in MScholtes/VirtualDesktop.

const CLSID_IMMERSIVE_SHELL: GUID = GUID::from_u128(0xC2F03A33_21F5_47FA_B4BB_156362A2F239);
const SERVICE_VIRTUAL_DESKTOP_MANAGER: GUID = GUID::from_u128(0xC5E0CDCA_7B6E_41B2_9FC4_D93975CC467B);

// (Their methods keep the shell's own names.)
#[allow(non_snake_case)]
mod shell_api {
    use super::*;

    #[interface("1841C6D7-4F9D-42C0-AF41-8747538F10E5")]
    pub unsafe trait IApplicationViewCollection: IUnknown {
        unsafe fn GetViews(&self, views: *mut *mut c_void) -> HRESULT;
        unsafe fn GetViewsByZOrder(&self, views: *mut *mut c_void) -> HRESULT;
        unsafe fn GetViewsByAppUserModelId(&self, id: PCWSTR, views: *mut *mut c_void) -> HRESULT;
        unsafe fn GetViewForHwnd(&self, window: HWND, view: *mut *mut c_void) -> HRESULT;
    }

    #[interface("3F07F4BE-B107-441A-AF0F-39D82529072C")]
    pub unsafe trait IVirtualDesktop: IUnknown {
        unsafe fn IsViewVisible(&self, view: *mut c_void, visible: *mut i32) -> HRESULT;
        unsafe fn GetId(&self, id: *mut GUID) -> HRESULT;
    }

    #[interface("53F5CA0B-158F-4124-900C-057158060B27")]
    pub unsafe trait IVirtualDesktopManagerInternal: IUnknown {
        unsafe fn GetCount(&self, count: *mut i32) -> HRESULT;
        unsafe fn MoveViewToDesktop(&self, view: *mut c_void, desktop: *mut c_void) -> HRESULT;
        unsafe fn CanViewMoveDesktops(&self, view: *mut c_void, can: *mut i32) -> HRESULT;
        unsafe fn GetCurrentDesktop(&self, desktop: *mut *mut c_void) -> HRESULT;
        unsafe fn GetDesktops(&self, desktops: *mut *mut c_void) -> HRESULT;
    }

    pub unsafe fn try_move(window: HWND, to: usize) -> Result<()> {
        let shell: IServiceProvider = CoCreateInstance(&CLSID_IMMERSIVE_SHELL, None, CLSCTX_ALL)?;
        let manager: IVirtualDesktopManagerInternal = shell.QueryService(&SERVICE_VIRTUAL_DESKTOP_MANAGER)?;
        let views: IApplicationViewCollection = shell.QueryService(&IApplicationViewCollection::IID)?;

        let mut raw = std::ptr::null_mut();
        views.GetViewForHwnd(window, &mut raw).ok()?;
        if raw.is_null() {
            return Err(E_INVALIDARG.into());
        }
        // Only its pointer is needed (it's handed back to the manager); this
        // owns the reference until then.
        let view = IUnknown::from_raw(raw);

        raw = std::ptr::null_mut();
        manager.GetDesktops(&mut raw).ok()?;
        if raw.is_null() {
            return Err(E_INVALIDARG.into());
        }
        let desktops = IObjectArray::from_raw(raw);
        if to as u32 >= desktops.GetCount()? {
            return Err(E_INVALIDARG.into());
        }
        let desktop: IVirtualDesktop = desktops.GetAt(to as u32)?;
        manager.MoveViewToDesktop(view.as_raw(), desktop.as_raw()).ok()
    }
}

/// Moves `window` to desktop number `to` (0 is the first, as in `Desktops`),
/// wherever it is now. Talks to Explorer, so call it off the UI thread, on
/// one that has initialised COM. False if Windows wouldn't (a build whose
/// desktop interfaces differ, a window it won't move, a desktop that's gone).
pub fn move_window(window: HWND, to: usize) -> bool {
    match unsafe { shell_api::try_move(window, to) } {
        Ok(()) => true,
        Err(e) => {
            crate::config::log(&format!("moving a window to desktop {} failed: {e}", to + 1));
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_read_as_windows_writes_them() {
        let b = [0x78, 0x56, 0x34, 0x12, 0xBC, 0x9A, 0xF0, 0xDE, 0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF];
        assert_eq!(guid_text(&b), "{12345678-9ABC-DEF0-0123-456789ABCDEF}");
    }
}
