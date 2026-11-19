//! Devices: the mouse (primary button, pointer speed and precision, the
//! wheel, double-click speed), the keyboard (repeat delay and rate, the
//! text cursor's blink, access-key underlines) and paired Bluetooth devices
//! (to remove one). As Windows' Settings > Bluetooth & devices, through the
//! same system parameters it sets, so every app follows at once.

use std::ffi::c_void;

use windows::core::BOOL;
use windows::Devices::Bluetooth::{BluetoothDevice, BluetoothLEDevice};
use windows::Devices::Enumeration::DeviceInformation;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Registry::HKEY_CURRENT_USER;
use windows::Win32::UI::Input::KeyboardAndMouse::SwapMouseButton;
use windows::Win32::UI::WindowsAndMessaging::*;

use super::{changed, confirm, pick, set_reg_dword, set_reg_text, tell};
use crate::config;

const SAVE: SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS = SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(3); // SPIF_UPDATEINIFILE | SPIF_SENDCHANGE
const SPI_GETMOUSEWHEELROUTING: u32 = 0x201C;
const SPI_SETMOUSEWHEELROUTING: u32 = 0x201D;

/// A parameter read into a u32 (or an i32/BOOL, which are the same size).
fn spi_get(action: SYSTEM_PARAMETERS_INFO_ACTION) -> u32 {
    let mut v = 0u32;
    unsafe {
        let _ = SystemParametersInfoW(
            action,
            0,
            Some(&mut v as *mut u32 as *mut c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
    }
    v
}

/// Sets a parameter Windows takes in uiParam.
fn spi_set_ui(action: SYSTEM_PARAMETERS_INFO_ACTION, v: u32) {
    unsafe {
        let _ = SystemParametersInfoW(action, v, None, SAVE);
    }
}

/// Sets a parameter Windows takes as the pointer's value itself.
fn spi_set_pv(action: SYSTEM_PARAMETERS_INFO_ACTION, v: u32) {
    unsafe {
        let _ = SystemParametersInfoW(action, 0, Some(v as usize as *mut c_void), SAVE);
    }
}

fn num(v: &str) -> Option<u32> {
    v.parse::<f32>().ok().map(|f| f.round().max(0.0) as u32)
}

fn precision() -> bool {
    let mut p = [0i32; 3];
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_GETMOUSE,
            0,
            Some(p.as_mut_ptr() as *mut c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
    }
    p[2] != 0
}

fn set_precision(on: bool) {
    // Windows' own values for "Enhance pointer precision".
    let mut p: [i32; 3] = if on { [6, 10, 1] } else { [0, 0, 0] };
    unsafe {
        let _ = SystemParametersInfoW(SPI_SETMOUSE, 0, Some(p.as_mut_ptr() as *mut c_void), SAVE);
    }
}

// ------------------------------------------------------------ bluetooth --

static PAIRED: super::Slow<Vec<(String, windows::core::HSTRING)>> = super::Slow::new();

/// The paired Bluetooth devices: (name, Windows' id for it).
fn paired() -> Vec<(String, windows::core::HSTRING)> {
    let mut out: Vec<(String, windows::core::HSTRING)> = Vec::new();
    for sel in [
        BluetoothDevice::GetDeviceSelectorFromPairingState(true),
        BluetoothLEDevice::GetDeviceSelectorFromPairingState(true),
    ] {
        let Ok(all) = sel.and_then(|s| DeviceInformation::FindAllAsyncAqsFilter(&s)).and_then(|op| op.get()) else {
            continue;
        };
        for i in 0..all.Size().unwrap_or(0) {
            let Ok(info) = all.GetAt(i) else { continue };
            let (Ok(name), Ok(id)) = (info.Name(), info.Id()) else { continue };
            let name = name.to_string_lossy();
            if !name.is_empty() && !out.iter().any(|(n, _)| *n == name) {
                out.push((name, id));
            }
        }
    }
    out.sort_by_key(|(n, _)| n.to_lowercase());
    out
}

fn remove_device(owner: HWND) {
    let list = paired();
    if list.is_empty() {
        tell("No Bluetooth devices are paired with this PC.".into());
        return;
    }
    let names: Vec<String> = list.iter().map(|(n, _)| n.clone()).collect();
    let Some(i) = pick(owner, &names, None) else { return };
    let (name, id) = list[i].clone();
    if !confirm(owner, &format!("Remove \u{201c}{name}\u{201d}? To use it again, pair it again.")) {
        return;
    }
    std::thread::spawn(move || {
        let r = DeviceInformation::CreateFromIdAsync(&id)
            .and_then(|op| op.get())
            .and_then(|d| d.Pairing())
            .and_then(|p| p.UnpairAsync())
            .and_then(|op| op.get())
            .and_then(|r| r.Status());
        config::log(&format!("bluetooth: removed {name}: {r:?}"));
        PAIRED.forget();
        if !matches!(r, Ok(s) if s.0 == 0 || s.0 == 1) {
            crate::msg::note(format!("Windows wouldn't remove {name}"));
        }
        changed();
    });
}

// -------------------------------------------------------------- the page --

pub fn get(key: &str) -> Option<String> {
    let b = |v: bool| v.to_string();
    Some(match key {
        "win_mouse_primary" => {
            (if unsafe { GetSystemMetrics(SM_SWAPBUTTON) } != 0 { "Right" } else { "Left" }).to_string()
        }
        "win_mouse_speed" => spi_get(SPI_GETMOUSESPEED).to_string(),
        "win_mouse_precision" => b(precision()),
        "win_mouse_wheel" => {
            let n = spi_get(SPI_GETWHEELSCROLLLINES);
            // u32::MAX: a screen at a time.
            (if n == u32::MAX { 100 } else { n }).to_string()
        }
        "win_mouse_inactive" => b(spi_get(SYSTEM_PARAMETERS_INFO_ACTION(SPI_GETMOUSEWHEELROUTING)) != 0),
        "win_mouse_dblclick" => {
            unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime() }.to_string()
        }
        "win_kb_delay" => spi_get(SPI_GETKEYBOARDDELAY).to_string(),
        "win_kb_rate" => spi_get(SPI_GETKEYBOARDSPEED).to_string(),
        "win_kb_blink" => unsafe { GetCaretBlinkTime() }.to_string(),
        "win_kb_cues" => b(spi_get(SPI_GETKEYBOARDCUES) != 0),
        _ => return None,
    })
}

pub fn desc(key: &str) -> Option<String> {
    Some(match key {
        "win_bt_devices" => match PAIRED.get(std::time::Duration::from_secs(5), paired).map(|l| l.len()) {
            None => "Reading\u{2026}".into(),
            Some(0) => "None paired yet.".into(),
            Some(1) => "1 device paired. Pick it to remove it.".into(),
            Some(n) => format!("{n} devices paired. Pick one to remove it."),
        },
        _ => return None,
    })
}

pub fn shown(_key: &str) -> Option<bool> {
    None
}

pub fn set(key: &str, v: &str) -> Option<()> {
    let on = v == "true";
    match key {
        "win_mouse_speed" => spi_set_pv(SPI_SETMOUSESPEED, num(v)?.clamp(1, 20)),
        "win_mouse_precision" => set_precision(on),
        "win_mouse_wheel" => spi_set_ui(SPI_SETWHEELSCROLLLINES, num(v)?.clamp(1, 100)),
        "win_mouse_inactive" => {
            // 2: the wheel scrolls what's under the pointer; 0: only the window in front.
            let value = if on { 2 } else { 0 };
            spi_set_pv(SYSTEM_PARAMETERS_INFO_ACTION(SPI_SETMOUSEWHEELROUTING), value);
            set_reg_dword(HKEY_CURRENT_USER, "Control Panel\\Desktop", "MouseWheelRouting", value);
        }
        "win_mouse_dblclick" => {
            let ms = num(v)?.clamp(200, 900);
            spi_set_ui(SPI_SETDOUBLECLICKTIME, ms);
        }
        "win_kb_delay" => spi_set_ui(SPI_SETKEYBOARDDELAY, num(v)?.min(3)),
        "win_kb_rate" => spi_set_ui(SPI_SETKEYBOARDSPEED, num(v)?.min(31)),
        "win_kb_blink" => {
            let ms = num(v)?.clamp(100, 1200);
            unsafe {
                let _ = SetCaretBlinkTime(ms);
            }
            set_reg_text(HKEY_CURRENT_USER, "Control Panel\\Desktop", "CursorBlinkRate", &ms.to_string());
        }
        "win_kb_cues" => spi_set_pv(SPI_SETKEYBOARDCUES, on as u32),
        _ => return None,
    }
    config::log(&format!("devices: {key} = {v}"));
    Some(())
}

pub fn action(key: &str, owner: HWND) -> Option<()> {
    match key {
        "win_mouse_primary" => {
            let names = vec!["Left".to_string(), "Right".to_string()];
            let swapped = unsafe { GetSystemMetrics(SM_SWAPBUTTON) } != 0;
            if let Some(i) = pick(owner, &names, Some(swapped as usize)) {
                unsafe {
                    let _: BOOL = SwapMouseButton(i == 1);
                }
                set_reg_text(
                    HKEY_CURRENT_USER,
                    "Control Panel\\Mouse",
                    "SwapMouseButtons",
                    if i == 1 { "1" } else { "0" },
                );
            }
        }
        "win_mouse_pointers" => {
            crate::shell::launch("control.exe", "main.cpl,,1", false);
        }
        "win_mouse_more" => {
            crate::shell::launch("control.exe", "main.cpl", false);
        }
        "win_bt_devices" => remove_device(owner),
        _ => return None,
    }
    Some(())
}
