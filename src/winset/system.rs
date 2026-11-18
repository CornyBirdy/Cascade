//! Time & language, Privacy & accessibility, and System: the time zone and
//! clock sync, keyboard layouts, which apps may use the camera, microphone
//! and location, Windows' notifications, and About (this PC, Windows,
//! storage, battery, updates). As the matching pages of Windows' Settings;
//! what only Windows' own classic windows can change opens those.

use windows::core::HSTRING;
use windows::Win32::Foundation::HWND;
use windows::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetLogicalDriveStringsW};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_WOW64_64KEY};
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Time::GetDynamicTimeZoneInformation;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyboardLayoutList, HKL};

use super::{reg_dword, reg_text, run_hidden, set_reg_dword, set_reg_text, subkeys};
use crate::config;

const ZONES: &str = "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\Time Zones";
const CONSENT: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\CapabilityAccessManager\\ConsentStore";
const PUSH: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\PushNotifications";

// ----------------------------------------------------------- time zone --

/// The time zones: (Windows' id for it, what it's called), by their offset from UTC.
fn zones() -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = subkeys(HKEY_LOCAL_MACHINE, ZONES, KEY_WOW64_64KEY)
        .into_iter()
        .filter_map(|id| {
            let name = reg_text(HKEY_LOCAL_MACHINE, &format!("{ZONES}\\{id}"), "Display")?;
            Some((id, name))
        })
        .collect();
    out.sort_by_key(|(_, n)| (offset_minutes(n), n.clone()));
    out
}

/// "(UTC+09:30) Adelaide" -> 570.
fn offset_minutes(display: &str) -> i32 {
    let Some(rest) = display.strip_prefix("(UTC") else { return 0 };
    let sign = match rest.chars().next() {
        Some('-') => -1,
        Some('+') => 1,
        _ => return 0,
    };
    let digits: String = rest[1..].chars().take(5).collect();
    let (h, m) = digits.split_once(':').unwrap_or((&digits, "0"));
    sign * (h.parse::<i32>().unwrap_or(0) * 60 + m.parse::<i32>().unwrap_or(0))
}

fn current_zone() -> String {
    let mut tz = Default::default();
    unsafe { GetDynamicTimeZoneInformation(&mut tz) };
    let n = tz.TimeZoneKeyName.iter().position(|&c| c == 0).unwrap_or(tz.TimeZoneKeyName.len());
    String::from_utf16_lossy(&tz.TimeZoneKeyName[..n])
}

pub fn timezone_label() -> String {
    let id = current_zone();
    reg_text(HKEY_LOCAL_MACHINE, &format!("{ZONES}\\{id}"), "Display").unwrap_or(id)
}

pub fn pick_timezone(owner: HWND) {
    let list = zones();
    let cur = current_zone();
    let names: Vec<String> = list.iter().map(|(_, n)| n.clone()).collect();
    if let Some(i) = super::pick(owner, &names, list.iter().position(|(id, _)| *id == cur)) {
        // Any user may change the time zone; tzutil is Windows' own way.
        let id = list[i].0.clone();
        std::thread::spawn(move || {
            let ok = run_hidden("tzutil.exe", &["/s", &id], true);
            config::log(&format!("time: zone set to {id} (took: {ok})"));
            super::changed();
        });
    }
}

/// Sets the clock from the internet now (Windows asks for admin rights).
pub fn sync_time() {
    let ok = crate::shell::launch("cmd.exe", "/c net start w32time & w32tm /resync /force", true);
    config::log(&format!("time: sync asked for (started: {ok})"));
}

/// The keyboard layouts installed, for the page.
pub fn layouts() -> String {
    let mut list = [HKL::default(); 32];
    let n = unsafe { GetKeyboardLayoutList(Some(&mut list)) } as usize;
    let names: Vec<String> = list[..n.min(32)].iter().map(|h| super::super::lang::layout_name(*h)).collect();
    if names.is_empty() {
        return "No keyboard layouts.".into();
    }
    format!("{}. Switch with Win+Space or the taskbar's language button.", names.join(", "))
}

// -------------------------------------------------------------- privacy --

fn consent_key(key: &str) -> Option<String> {
    let cap = match key {
        "win_camera" => "webcam",
        "win_mic_access" => "microphone",
        "win_location" => "location",
        _ => return None,
    };
    Some(format!("{CONSENT}\\{cap}"))
}

pub fn flag(key: &str) -> bool {
    if key == "win_toasts" {
        return reg_dword(HKEY_CURRENT_USER, PUSH, "ToastEnabled") != Some(0);
    }
    consent_key(key).is_some_and(|k| reg_text(HKEY_CURRENT_USER, &k, "Value").is_none_or(|v| v != "Deny"))
}

pub fn set_flag(key: &str, on: bool) {
    if key == "win_toasts" {
        set_reg_dword(HKEY_CURRENT_USER, PUSH, "ToastEnabled", on as u32);
    } else if let Some(k) = consent_key(key) {
        set_reg_text(HKEY_CURRENT_USER, &k, "Value", if on { "Allow" } else { "Deny" });
    }
    config::log(&format!("privacy: {key} {}", if on { "allowed" } else { "denied" }));
    super::changed();
}

// --------------------------------------------------------------- about --

fn computer_name() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_default()
}

fn windows_version() -> String {
    const CV: &str = "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion";
    let build: u32 = reg_text(HKEY_LOCAL_MACHINE, CV, "CurrentBuild").and_then(|b| b.parse().ok()).unwrap_or(0);
    let ubr = reg_dword(HKEY_LOCAL_MACHINE, CV, "UBR").unwrap_or(0);
    let edition = reg_text(HKEY_LOCAL_MACHINE, CV, "EditionID").unwrap_or_default();
    let version = reg_text(HKEY_LOCAL_MACHINE, CV, "DisplayVersion").unwrap_or_default();
    // Windows 11 still calls itself Windows 10 in the registry's product name.
    let name = if build >= 22000 { "Windows 11" } else { "Windows 10" };
    let edition = match edition.as_str() {
        "Professional" => "Pro",
        "Core" => "Home",
        e => e,
    };
    format!("{name} {edition} {version} (build {build}.{ubr})")
}

fn processor() -> String {
    reg_text(HKEY_LOCAL_MACHINE, "HARDWARE\\DESCRIPTION\\System\\CentralProcessor\\0", "ProcessorNameString")
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

fn memory_gb() -> f64 {
    let mut m = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    unsafe {
        let _ = GlobalMemoryStatusEx(&mut m);
    }
    m.ullTotalPhys as f64 / (1u64 << 30) as f64
}

pub fn about() -> String {
    format!("{}: {}. {}, {:.0} GB of memory.", computer_name(), windows_version(), processor(), memory_gb().round())
}

pub fn storage() -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetLogicalDriveStringsW(Some(&mut buf)) } as usize;
    let drives: Vec<String> = String::from_utf16_lossy(&buf[..n.min(256)])
        .split('\0')
        .filter(|d| !d.is_empty())
        .filter_map(|d| {
            let (mut free, mut total) = (0u64, 0u64);
            unsafe { GetDiskFreeSpaceExW(&HSTRING::from(d), None, Some(&mut total), Some(&mut free)) }.ok()?;
            (total > 0).then(|| {
                format!("{} {:.0} GB free of {:.0} GB", d.trim_end_matches('\\'), free as f64 / 1e9, total as f64 / 1e9)
            })
        })
        .collect();
    drives.join(" \u{b7} ")
}

pub fn battery() -> Option<String> {
    let (level, plugged) = crate::ui::battery_now()?;
    Some(format!("{}%{}.", level, if plugged { ", plugged in" } else { ", on battery" }))
}

/// Asks Windows Update to look for updates now. They download and install
/// as Windows Update is set to (it says in the notch when one needs a restart).
pub fn check_updates() {
    let ok = run_hidden("usoclient.exe", &["StartInteractiveScan"], false);
    config::log(&format!("updates: scan asked for (started: {ok})"));
    crate::msg::note("Windows Update is looking for updates");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_utc_offsets() {
        assert_eq!(offset_minutes("(UTC+09:30) Adelaide"), 570);
        assert_eq!(offset_minutes("(UTC-05:00) Eastern Time (US & Canada)"), -300);
        assert_eq!(offset_minutes("(UTC) Coordinated Universal Time"), 0);
        assert_eq!(offset_minutes("(UTC+13:45) Chatham Islands"), 825);
    }
}
