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

use std::ffi::c_void;

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{ERROR_SUCCESS, HANDLE};
use windows::Win32::System::Registry::*;
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Input::KeyboardAndMouse::*;

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
    let ids: Vec<&[u8]> = ids.chunks_exact(16).collect();
    let current = binary(&key, w!("CurrentVirtualDesktop"))
        .or_else(|| binary(&session_key(), w!("CurrentVirtualDesktop")))
        .and_then(|c| ids.iter().position(|id| *id == c.as_slice()))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_read_as_windows_writes_them() {
        let b = [0x78, 0x56, 0x34, 0x12, 0xBC, 0x9A, 0xF0, 0xDE, 0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF];
        assert_eq!(guid_text(&b), "{12345678-9ABC-DEF0-0123-456789ABCDEF}");
    }
}
