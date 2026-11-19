//! Power & battery: the power mode (best efficiency, balanced, best
//! performance) and when the screen turns off and the PC sleeps, plugged in
//! and on battery. As Windows' Settings > System > Power & battery, through
//! the power-scheme API (the power mode through the call its Settings uses).
//! Lid and power-button actions are in the classic Power Options.
//!
//! Windows only applies a power mode on the Balanced plan (on High
//! performance or Ultimate the call is ignored, and its Settings hides the
//! choice), so picking a mode switches to Balanced first. The plan itself is
//! its own picker, as in the classic Power Options.

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

/// Windows' Balanced plan, the one power modes work on.
const BALANCED: GUID = GUID::from_u128(0x381b4222_f694_41f0_9685_ff5bb260df2e);

/// Every power plan: its id and name.
fn plans() -> Vec<(GUID, String)> {
    let mut out = Vec::new();
    unsafe {
        for i in 0.. {
            let mut g = GUID::zeroed();
            let mut size = std::mem::size_of::<GUID>() as u32;
            let r = PowerEnumerate(None, None, None, ACCESS_SCHEME, i, Some(&mut g as *mut GUID as *mut u8), &mut size);
            if r != ERROR_SUCCESS {
                break;
            }
            out.push((g, plan_name(&g)));
        }
    }
    out
}

fn plan_name(g: &GUID) -> String {
    unsafe {
        let mut size = 0u32;
        let _ = PowerReadFriendlyName(None, Some(g), None, None, None, &mut size);
        let mut buf = vec![0u8; size.max(2) as usize];
        if PowerReadFriendlyName(None, Some(g), None, None, Some(buf.as_mut_ptr()), &mut size) != ERROR_SUCCESS {
            return format!("{g:?}");
        }
        let wide: Vec<u16> =
            buf.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).take_while(|&c| c != 0).collect();
        String::from_utf16_lossy(&wide)
    }
}

fn set_mode(i: usize) {
    let Some(f) = powrprof(c"PowerSetActiveOverlayScheme") else { return };
    // Power modes only take on the Balanced plan.
    if active() != Some(BALANCED) {
        let r = unsafe { PowerSetActiveScheme(None, Some(&BALANCED)) };
        config::log(&format!("power: switched to the Balanced plan for a power mode (result {})", r.0));
    }
    let r = unsafe { std::mem::transmute::<unsafe extern "system" fn() -> isize, SetOverlay>(f)(&MODES[i].1) };
    config::log(&format!("power: mode {} (result {r})", MODES[i].0));
}

pub fn label(key: &str) -> String {
    if key == "win_power_plan" {
        return active().map(|g| plan_name(&g)).unwrap_or_default();
    }
    if key == "win_power_mode" {
        if active().is_some_and(|g| g != BALANCED) {
            return "Set by the power plan".into();
        }
        return mode().map(|i| MODES[i].0.to_string()).unwrap_or_else(|| "Balanced".into());
    }
    read(key).map(|s| minutes_text(s / 60)).unwrap_or_default()
}

pub fn pick(key: &str, owner: HWND) {
    if key == "win_power_plan" {
        let plans = plans();
        let names: Vec<String> = plans.iter().map(|(_, n)| n.clone()).collect();
        let now = active();
        if let Some(i) = super::pick(owner, &names, plans.iter().position(|(g, _)| Some(*g) == now)) {
            let r = unsafe { PowerSetActiveScheme(None, Some(&plans[i].0)) };
            config::log(&format!("power: plan {} (result {})", plans[i].1, r.0));
            super::changed();
        }
        return;
    }
    if key == "win_power_mode" {
        let names: Vec<String> = MODES.iter().map(|(n, _)| n.to_string()).collect();
        let now = if active() == Some(BALANCED) { mode() } else { None };
        if let Some(i) = super::pick(owner, &names, now) {
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

// ------------------------------------------------- buttons, lid, saver --

const BUTTONS: GUID = GUID::from_u128(0x4f971e89_eebd_4455_a8de_9e59040e7347);
const LID: GUID = GUID::from_u128(0x5ca83367_6e45_459f_a27b_476b1d01c936);
const POWER_BUTTON: GUID = GUID::from_u128(0x7648efa3_dd9c_4e3e_b566_50f929386280);
const SLEEP_BUTTON: GUID = GUID::from_u128(0x96996bc0_ad50_47ec_923b_6f41874dd9eb);
const HIBERNATE_AFTER: GUID = GUID::from_u128(0x9d7815a6_7ee4_497e_8888_515a05f02364);
const SAVER: GUID = GUID::from_u128(0xde830923_a562_41af_a086_e3a2c6bad2da);
const SAVER_AT: GUID = GUID::from_u128(0xe69653ca_cf7f_4f05_aa73_cb833fa90ad4);
const SAVER_DIM: GUID = GUID::from_u128(0x13d09884_f74e_474a_a852_b6bde8ad03a8);

/// What a button or the lid can do, as Windows numbers them.
const ACTIONS: [&str; 5] = ["Do nothing", "Sleep", "Hibernate", "Shut down", "Turn off the display"];
const SAVER_LEVELS: [u32; 7] = [0, 10, 20, 30, 40, 50, 100];

/// The other power settings: (subgroup, setting, on battery).
fn extra(key: &str) -> Option<(GUID, GUID, bool)> {
    Some(match key {
        "win_lid_ac" => (BUTTONS, LID, false),
        "win_lid_dc" => (BUTTONS, LID, true),
        "win_pbutton_ac" => (BUTTONS, POWER_BUTTON, false),
        "win_pbutton_dc" => (BUTTONS, POWER_BUTTON, true),
        "win_sbutton_ac" => (BUTTONS, SLEEP_BUTTON, false),
        "win_hibernate_ac" => (SLEEP, HIBERNATE_AFTER, false),
        "win_hibernate_dc" => (SLEEP, HIBERNATE_AFTER, true),
        "win_saver_at" => (SAVER, SAVER_AT, true),
        "win_saver_dim" => (SAVER, SAVER_DIM, true),
        _ => return None,
    })
}

fn read_extra(key: &str) -> Option<u32> {
    let (sub, set, dc) = extra(key)?;
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

fn write_extra(key: &str, v: u32) {
    let (Some((sub, set, dc)), Some(scheme)) = (extra(key), active()) else { return };
    unsafe {
        let r = if dc {
            PowerWriteDCValueIndex(None, &scheme, Some(&sub), Some(&set), v)
        } else {
            PowerWriteACValueIndex(None, &scheme, Some(&sub), Some(&set), v).0
        };
        let _ = PowerSetActiveScheme(None, Some(&scheme));
        config::log(&format!("power: {key} = {v} (result {r})"));
    }
}

/// Whether this PC can hibernate (it's turned on and the hardware allows it).
fn can_hibernate() -> bool {
    let mut caps = SYSTEM_POWER_CAPABILITIES::default();
    unsafe { GetPwrCapabilities(&mut caps) && caps.SystemS4 && caps.HiberFilePresent }
}

fn has_lid() -> bool {
    let mut caps = SYSTEM_POWER_CAPABILITIES::default();
    unsafe { GetPwrCapabilities(&mut caps) && caps.LidPresent }
}

fn on_battery_pc() -> bool {
    let mut caps = SYSTEM_POWER_CAPABILITIES::default();
    unsafe { GetPwrCapabilities(&mut caps) && caps.SystemBatteriesPresent }
}

pub fn get(key: &str) -> Option<String> {
    let v = read_extra(key);
    extra(key)?;
    Some(match key {
        k if k.starts_with("win_hibernate") => v.map(|s| minutes_text(s / 60)).unwrap_or_default(),
        "win_saver_at" => match v {
            Some(0) | None => "Never".into(),
            Some(100) => "Always".into(),
            Some(p) => format!("At {p}%"),
        },
        "win_saver_dim" => (v.unwrap_or(0) != 100 && v.is_some()).to_string(),
        _ => v.and_then(|a| ACTIONS.get(a as usize)).map(|s| s.to_string()).unwrap_or_default(),
    })
}

pub fn desc(_key: &str) -> Option<String> {
    None
}

pub fn shown(key: &str) -> Option<bool> {
    Some(match key {
        "win_lid_ac" | "win_lid_dc" => has_lid() && (key == "win_lid_ac" || on_battery_pc()),
        "win_pbutton_dc" | "win_saver_at" | "win_saver_dim" | "win_hibernate_dc" => {
            on_battery_pc() && (!key.starts_with("win_hibernate") || can_hibernate())
        }
        "win_hibernate_ac" => can_hibernate(),
        _ => return None,
    })
}

pub fn set(key: &str, v: &str) -> Option<()> {
    match key {
        // Dim the screen in energy saver: 30% brightness, or as it was (100).
        "win_saver_dim" => write_extra(key, if v == "true" { 30 } else { 100 }),
        _ => return None,
    }
    Some(())
}

pub fn action(key: &str, owner: HWND) -> Option<()> {
    extra(key)?;
    if key.starts_with("win_hibernate") {
        let names: Vec<String> = MINUTES.iter().map(|m| minutes_text(*m)).collect();
        let cur = read_extra(key).and_then(|s| MINUTES.iter().position(|m| m * 60 == s));
        if let Some(i) = super::pick(owner, &names, cur) {
            write_extra(key, MINUTES[i] * 60);
        }
    } else if key == "win_saver_at" {
        let names: Vec<String> = SAVER_LEVELS
            .iter()
            .map(|p| match p {
                0 => "Never".into(),
                100 => "Always".into(),
                p => format!("At {p}%"),
            })
            .collect();
        let cur = read_extra(key).and_then(|v| SAVER_LEVELS.iter().position(|p| *p == v));
        if let Some(i) = super::pick(owner, &names, cur) {
            write_extra(key, SAVER_LEVELS[i]);
        }
    } else {
        let mut choices: Vec<usize> = (0..ACTIONS.len()).collect();
        if !can_hibernate() {
            choices.retain(|&a| a != 2);
        }
        let names: Vec<String> = choices.iter().map(|&a| ACTIONS[a].to_string()).collect();
        let cur = read_extra(key).and_then(|v| choices.iter().position(|&a| a as u32 == v));
        if let Some(i) = super::pick(owner, &names, cur) {
            write_extra(key, choices[i] as u32);
        }
    }
    Some(())
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
