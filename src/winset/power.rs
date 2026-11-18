//! Power & battery: the power mode (best efficiency, balanced, best
//! performance) and when the screen turns off and the PC sleeps, plugged in
//! and on battery. As Windows' Settings > System > Power & battery, through
//! the power-scheme API (the power mode through the call its Settings uses).
//! Lid and power-button actions are in the classic Power Options.

use windows::core::{w, GUID, PCSTR};
use windows::Win32::Foundation::{ERROR_SUCCESS, HWND};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Power::*;

use crate::config;

const VIDEO: GUID = GUID::from_u128(0x7516b95f_f776_4464_8c53_06167f40cc99);
const VIDEO_OFF: GUID = GUID::from_u128(0x3c0bc021_c8a8_4e07_a973_6b14cbcb2b7e);
const SLEEP: GUID = GUID::from_u128(0x238c9fa8_0aad_41ed_83f4_97be242c8f20);
const SLEEP_AFTER: GUID = GUID::from_u128(0x29f6c1db_86da_48c5_9fdb_f2b67b1f44da);

/// Windows' power modes (its Settings' "Power mode"), as overlay schemes.
const MODES: [(&str, GUID); 3] = [
    ("Best power efficiency", GUID::from_u128(0x961cc777_2547_4f9d_8174_7d86181b8a7a)),
    ("Balanced", GUID::from_u128(0)),
    ("Best performance", GUID::from_u128(0xded574b5_45a0_4f42_8737_46345c09c238)),
];

/// The times offered, in minutes (0: never).
const MINUTES: [u32; 13] = [1, 2, 3, 5, 10, 15, 20, 25, 30, 45, 60, 120, 0];

fn minutes_text(m: u32) -> String {
    match m {
        0 => "Never".into(),
        60 => "1 hour".into(),
        m if m % 60 == 0 => format!("{} hours", m / 60),
        1 => "1 minute".into(),
        m => format!("{m} minutes"),
    }
}

fn active() -> Option<GUID> {
    unsafe {
        let mut p: *mut GUID = std::ptr::null_mut();
        if PowerGetActiveScheme(None, &mut p) != ERROR_SUCCESS || p.is_null() {
            return None;
        }
        let g = *p;
        CoTaskMemFree(Some(p as *const _));
        Some(g)
    }
}

/// Which setting a key is: (subgroup, setting, on battery).
fn setting(key: &str) -> Option<(GUID, GUID, bool)> {
    Some(match key {
        "win_screen_ac" => (VIDEO, VIDEO_OFF, false),
        "win_screen_dc" => (VIDEO, VIDEO_OFF, true),
        "win_sleep_ac" => (SLEEP, SLEEP_AFTER, false),
        "win_sleep_dc" => (SLEEP, SLEEP_AFTER, true),
        _ => return None,
    })
}

/// The setting's value in seconds.
fn read(key: &str) -> Option<u32> {
    let (sub, set, dc) = setting(key)?;
    let scheme = active()?;
    let mut v = 0u32;
    let ok = unsafe {
        if dc {
            PowerReadDCValueIndex(None, Some(&scheme), Some(&sub), Some(&set), &mut v) == 0
        } else {
            PowerReadACValueIndex(None, Some(&scheme), Some(&sub), Some(&set), &mut v) == ERROR_SUCCESS
        }
    };
    ok.then_some(v)
}

fn write(key: &str, seconds: u32) {
    let (Some((sub, set, dc)), Some(scheme)) = (setting(key), active()) else { return };
    unsafe {
        let r = if dc {
            PowerWriteDCValueIndex(None, &scheme, Some(&sub), Some(&set), seconds)
        } else {
            PowerWriteACValueIndex(None, &scheme, Some(&sub), Some(&set), seconds).0
        };
        // Applied once the scheme is made active again.
        let _ = PowerSetActiveScheme(None, Some(&scheme));
        config::log(&format!("power: {key} = {seconds} s (result {r})"));
    }
}

type GetOverlay = unsafe extern "system" fn(*mut GUID) -> u32;
type SetOverlay = unsafe extern "system" fn(*const GUID) -> u32;

fn powrprof(name: &std::ffi::CStr) -> Option<unsafe extern "system" fn() -> isize> {
    unsafe {
        let lib = LoadLibraryW(w!("powrprof.dll")).ok()?;
        GetProcAddress(lib, PCSTR(name.as_ptr() as *const u8))
    }
}

/// The power mode in use (an index into `MODES`).
fn mode() -> Option<usize> {
    let f = powrprof(c"PowerGetEffectiveOverlayScheme")?;
    let mut g = GUID::zeroed();
    unsafe {
        let f = std::mem::transmute::<unsafe extern "system" fn() -> isize, GetOverlay>(f);
        if f(&mut g) != 0 {
            return Some(1);
        }
    }
    MODES.iter().position(|(_, m)| *m == g)
}

fn set_mode(i: usize) {
    let Some(f) = powrprof(c"PowerSetActiveOverlayScheme") else { return };
    let r = unsafe { std::mem::transmute::<unsafe extern "system" fn() -> isize, SetOverlay>(f)(&MODES[i].1) };
    config::log(&format!("power: mode {} (result {r})", MODES[i].0));
}

pub fn label(key: &str) -> String {
    if key == "win_power_mode" {
        return mode().map(|i| MODES[i].0.to_string()).unwrap_or_else(|| "Balanced".into());
    }
    read(key).map(|s| minutes_text(s / 60)).unwrap_or_default()
}

pub fn pick(key: &str, owner: HWND) {
    if key == "win_power_mode" {
        let names: Vec<String> = MODES.iter().map(|(n, _)| n.to_string()).collect();
        if let Some(i) = super::pick(owner, &names, mode()) {
            set_mode(i);
            super::changed();
        }
        return;
    }
    let names: Vec<String> = MINUTES.iter().map(|m| minutes_text(*m)).collect();
    let cur = read(key).and_then(|s| MINUTES.iter().position(|m| m * 60 == s));
    if let Some(i) = super::pick(owner, &names, cur) {
        write(key, MINUTES[i] * 60);
        super::changed();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_times() {
        assert_eq!(minutes_text(0), "Never");
        assert_eq!(minutes_text(1), "1 minute");
        assert_eq!(minutes_text(15), "15 minutes");
        assert_eq!(minutes_text(60), "1 hour");
        assert_eq!(minutes_text(120), "2 hours");
    }
}
