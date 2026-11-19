//! Accessibility: text size, the mouse pointer's size, the text cursor's
//! thickness, animations, scrollbars, how long notifications stay, high
//! contrast, and the keyboard helpers (Sticky, Filter and Toggle keys, and
//! the Shift-five-times shortcut that pops Sticky keys up mid-game). As
//! Windows' Settings > Accessibility, through the system parameters it
//! sets; Magnifier, Narrator and the on-screen keyboard are a click away.

use std::ffi::c_void;

use windows::core::{w, PWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Registry::HKEY_CURRENT_USER;
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use super::{broadcast, pick, reg_dword, set_reg_dword};
use crate::config;

const SAVE: SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS = SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(3);
const NONE: SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS = SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0);
/// Not in the headers: the pointer size Windows' Settings sets (32 + 16 a step).
const SPI_SETCURSORBASESIZE: u32 = 0x2029;
const ACCESSIBILITY: &str = "Software\\Microsoft\\Accessibility";
const SCROLLBARS: &str = "Control Panel\\Accessibility";
const DURATIONS: [u32; 6] = [5, 7, 15, 30, 60, 300];

fn get_u32(action: SYSTEM_PARAMETERS_INFO_ACTION) -> u32 {
    let mut v = 0u32;
    unsafe {
        let _ = SystemParametersInfoW(action, 0, Some(&mut v as *mut u32 as *mut c_void), NONE);
    }
    v
}

fn set_pv(action: SYSTEM_PARAMETERS_INFO_ACTION, v: u32) {
    unsafe {
        let _ = SystemParametersInfoW(action, 0, Some(v as usize as *mut c_void), SAVE);
    }
}

/// A flag in one of the keyboard helpers' structs: (get, set, size, flag).
fn helper(key: &str) -> Option<(SYSTEM_PARAMETERS_INFO_ACTION, SYSTEM_PARAMETERS_INFO_ACTION, u32)> {
    Some(match key {
        "win_acc_sticky" => (SPI_GETSTICKYKEYS, SPI_SETSTICKYKEYS, SKF_STICKYKEYSON.0),
        "win_acc_sticky_shortcut" => (SPI_GETSTICKYKEYS, SPI_SETSTICKYKEYS, SKF_HOTKEYACTIVE.0),
        "win_acc_filter" => (SPI_GETFILTERKEYS, SPI_SETFILTERKEYS, FKF_FILTERKEYSON),
        "win_acc_toggle" => (SPI_GETTOGGLEKEYS, SPI_SETTOGGLEKEYS, TKF_TOGGLEKEYSON),
        _ => return None,
    })
}

/// The struct each helper's flags live in: cbSize, dwFlags, then (for Filter keys) more.
#[repr(C)]
#[derive(Default)]
struct Flags {
    size: u32,
    flags: u32,
    rest: [u32; 4],
}

fn struct_size(get: SYSTEM_PARAMETERS_INFO_ACTION) -> u32 {
    if get == SPI_GETFILTERKEYS {
        std::mem::size_of::<FILTERKEYS>() as u32
    } else {
        8
    }
}

fn read_helper(get: SYSTEM_PARAMETERS_INFO_ACTION) -> Flags {
    let mut f = Flags { size: struct_size(get), ..Default::default() };
    unsafe {
        let _ = SystemParametersInfoW(get, f.size, Some(&mut f as *mut Flags as *mut c_void), NONE);
    }
    f
}

fn high_contrast() -> bool {
    let mut hc = HIGHCONTRASTW { cbSize: std::mem::size_of::<HIGHCONTRASTW>() as u32, ..Default::default() };
    unsafe {
        let _ = SystemParametersInfoW(SPI_GETHIGHCONTRAST, hc.cbSize, Some(&mut hc as *mut _ as *mut c_void), NONE);
    }
    hc.dwFlags.0 & HCF_HIGHCONTRASTON.0 != 0
}

fn set_high_contrast(on: bool) {
    let mut hc = HIGHCONTRASTW { cbSize: std::mem::size_of::<HIGHCONTRASTW>() as u32, ..Default::default() };
    unsafe {
        let _ = SystemParametersInfoW(SPI_GETHIGHCONTRAST, hc.cbSize, Some(&mut hc as *mut _ as *mut c_void), NONE);
        hc.dwFlags = if on {
            HIGHCONTRASTW_FLAGS(hc.dwFlags.0 | HCF_HIGHCONTRASTON.0)
        } else {
            HIGHCONTRASTW_FLAGS(hc.dwFlags.0 & !HCF_HIGHCONTRASTON.0)
        };
        if on && hc.lpszDefaultScheme.is_null() {
            hc.lpszDefaultScheme = PWSTR(w!("High Contrast Black").as_ptr() as *mut u16);
        }
        let _ = SystemParametersInfoW(SPI_SETHIGHCONTRAST, hc.cbSize, Some(&mut hc as *mut _ as *mut c_void), SAVE);
    }
}

fn pointer_step() -> u32 {
    let base = reg_dword(HKEY_CURRENT_USER, "Control Panel\\Cursors", "CursorBaseSize").unwrap_or(32);
    (base.saturating_sub(32) / 16 + 1).clamp(1, 15)
}

fn duration_text(s: u32) -> String {
    if s >= 60 {
        format!("{} minute{}", s / 60, if s >= 120 { "s" } else { "" })
    } else {
        format!("{s} seconds")
    }
}

pub fn get(key: &str) -> Option<String> {
    let b = |v: bool| v.to_string();
    if let Some((g, _, flag)) = helper(key) {
        return Some(b(read_helper(g).flags & flag != 0));
    }
    Some(match key {
        "win_acc_text" => {
            let pct = reg_dword(HKEY_CURRENT_USER, ACCESSIBILITY, "TextScaleFactor").unwrap_or(100);
            (pct as f32 / 100.0).to_string()
        }
        "win_acc_pointer" => pointer_step().to_string(),
        "win_acc_caret" => get_u32(SPI_GETCARETWIDTH).max(1).to_string(),
        "win_acc_anim" => b(get_u32(SPI_GETCLIENTAREAANIMATION) != 0),
        "win_acc_scrollbars" => b(reg_dword(HKEY_CURRENT_USER, SCROLLBARS, "DynamicScrollbars") == Some(0)),
        "win_acc_notif" => duration_text(get_u32(SPI_GETMESSAGEDURATION).max(5)),
        "win_acc_contrast" => b(high_contrast()),
        _ => return None,
    })
}

pub fn desc(_key: &str) -> Option<String> {
    None
}

pub fn shown(_key: &str) -> Option<bool> {
    None
}

pub fn set(key: &str, v: &str) -> Option<()> {
    let on = v == "true";
    let num = || v.parse::<f32>().ok();
    if let Some((g, s, flag)) = helper(key) {
        let mut f = read_helper(g);
        f.flags = if on { f.flags | flag } else { f.flags & !flag };
        unsafe {
            let _ = SystemParametersInfoW(s, f.size, Some(&mut f as *mut Flags as *mut c_void), SAVE);
        }
    } else {
        match key {
            "win_acc_text" => {
                let pct = (num()? * 100.0).round().clamp(100.0, 225.0) as u32;
                set_reg_dword(HKEY_CURRENT_USER, ACCESSIBILITY, "TextScaleFactor", pct);
                broadcast("WindowMetrics");
            }
            "win_acc_pointer" => {
                let step = (num()?.round() as u32).clamp(1, 15);
                let base = 32 + (step - 1) * 16;
                set_reg_dword(HKEY_CURRENT_USER, "Control Panel\\Cursors", "CursorBaseSize", base);
                set_pv(SYSTEM_PARAMETERS_INFO_ACTION(SPI_SETCURSORBASESIZE), base);
                unsafe {
                    let _ = SystemParametersInfoW(SPI_SETCURSORS, 0, None, SAVE);
                }
            }
            "win_acc_caret" => set_pv(SPI_SETCARETWIDTH, (num()?.round() as u32).clamp(1, 20)),
            "win_acc_anim" => set_pv(SPI_SETCLIENTAREAANIMATION, on as u32),
            "win_acc_scrollbars" => {
                set_reg_dword(HKEY_CURRENT_USER, SCROLLBARS, "DynamicScrollbars", (!on) as u32);
                broadcast("ImmersiveColorSet");
            }
            "win_acc_contrast" => set_high_contrast(on),
            _ => return None,
        }
    }
    config::log(&format!("accessibility: {key} = {v}"));
    Some(())
}

pub fn action(key: &str, owner: HWND) -> Option<()> {
    match key {
        "win_acc_notif" => {
            let names: Vec<String> = DURATIONS.iter().map(|s| duration_text(*s)).collect();
            let now = get_u32(SPI_GETMESSAGEDURATION);
            if let Some(i) = pick(owner, &names, DURATIONS.iter().position(|s| *s == now)) {
                set_pv(SPI_SETMESSAGEDURATION, DURATIONS[i]);
            }
        }
        "win_acc_colour" => {
            crate::shell::launch("control.exe", "/name Microsoft.EaseOfAccessCenter /page pageEasierToSee", false);
        }
        _ => return None,
    }
    Some(())
}
