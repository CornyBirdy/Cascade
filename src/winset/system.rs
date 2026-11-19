//! Time & language, Privacy & security, and System: the time zone and
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

// ---------------------------------------------------- formats and clock --

const LOCALE_USER_DEFAULT: u32 = 0x400;
const W32TIME: &str = "SYSTEM\\CurrentControlSet\\Services\\W32Time\\Parameters";
const TZ_AUTO: &str = "SYSTEM\\CurrentControlSet\\Services\\tzautoupdate";
const DATE_FORMATS: [&str; 6] = ["d/MM/yyyy", "dd/MM/yyyy", "d/M/yy", "MM/dd/yyyy", "yyyy-MM-dd", "dd-MMM-yy"];
const DAYS: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];

fn locale(lctype: u32) -> String {
    use windows::Win32::Globalization::GetLocaleInfoEx;
    let mut buf = [0u16; 80];
    let n = unsafe { GetLocaleInfoEx(windows::core::PCWSTR::null(), lctype, Some(&mut buf)) } as usize;
    String::from_utf16_lossy(&buf[..n.saturating_sub(1)])
}

fn set_locale(lctype: u32, value: &str) {
    use windows::Win32::Globalization::SetLocaleInfoW;
    let r = unsafe { SetLocaleInfoW(LOCALE_USER_DEFAULT, lctype, &HSTRING::from(value)) };
    config::log(&format!("time: locale {lctype:#x} = {value} ({r:?})"));
    super::broadcast("intl");
}

/// Today in a date format, to show what each one looks like.
fn sample(format: &str) -> String {
    use windows::Win32::Globalization::GetDateFormatEx;
    let mut buf = [0u16; 64];
    let n = unsafe {
        GetDateFormatEx(
            windows::core::PCWSTR::null(),
            windows::Win32::Globalization::ENUM_DATE_FORMATS_FLAGS(0),
            None,
            &HSTRING::from(format),
            Some(&mut buf),
            windows::core::PCWSTR::null(),
        )
    } as usize;
    String::from_utf16_lossy(&buf[..n.saturating_sub(1)])
}

// -------------------------------------------------------------- privacy --

/// More of Windows' app permissions (its consent store's names for them).
fn capability(key: &str) -> Option<&'static str> {
    Some(match key {
        "win_cap_contacts" => "contacts",
        "win_cap_calendar" => "appointments",
        "win_cap_calls" => "phoneCall",
        "win_cap_account" => "userAccountInformation",
        "win_cap_documents" => "documentsLibrary",
        "win_cap_pictures" => "picturesLibrary",
        "win_cap_videos" => "videosLibrary",
        "win_cap_files" => "broadFileSystemAccess",
        "win_cap_screenshots" => "graphicsCaptureProgrammatic",
        "win_cap_radios" => "radios",
        _ => return None,
    })
}

/// Plain on/off settings kept in one registry value: (key, value, what "on" is).
fn switch(key: &str) -> Option<(&'static str, &'static str, u32)> {
    Some(match key {
        "win_clip_history" => ("Software\\Microsoft\\Clipboard", "EnableClipboardHistory", 1),
        "win_file_ext" => ("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Advanced", "HideFileExt", 0),
        "win_hidden" => ("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Advanced", "Hidden", 1),
        "win_storage_sense" => {
            ("Software\\Microsoft\\Windows\\CurrentVersion\\StorageSense\\Parameters\\StoragePolicy", "01", 1)
        }
        "win_ad_id" => ("Software\\Microsoft\\Windows\\CurrentVersion\\AdvertisingInfo", "Enabled", 1),
        "win_track_launches" => {
            ("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Advanced", "Start_TrackProgs", 1)
        }
        "win_tailored" => {
            ("Software\\Microsoft\\Windows\\CurrentVersion\\Privacy", "TailoredExperiencesWithDiagnosticDataEnabled", 1)
        }
        _ => return None,
    })
}

fn account() -> String {
    use windows::Win32::UI::Shell::IsUserAnAdmin;
    let name = std::env::var("USERNAME").unwrap_or_default();
    let admin = unsafe { IsUserAnAdmin().as_bool() };
    let kind = if admin { "an administrator (Cascade is running with admin rights)" } else { "signed in" };
    format!("{name}, {kind}, on {}.", computer_name())
}

pub fn get(key: &str) -> Option<String> {
    let b = |v: bool| v.to_string();
    if let Some(cap) = capability(key) {
        return Some(b(reg_text(HKEY_CURRENT_USER, &format!("{CONSENT}\\{cap}"), "Value").is_none_or(|v| v != "Deny")));
    }
    if let Some((k, v, on)) = switch(key) {
        // Unset: Windows' default (extensions hidden, no history, hidden files hidden...).
        let default = matches!(key, "win_ad_id" | "win_track_launches");
        return Some(b(reg_dword(HKEY_CURRENT_USER, k, v).map_or(default, |x| x == on)));
    }
    Some(match key {
        "win_time_24h" => b(locale(windows::Win32::Globalization::LOCALE_SSHORTTIME).contains('H')),
        "win_time_date" => {
            let f = locale(windows::Win32::Globalization::LOCALE_SSHORTDATE);
            format!("{} ({f})", sample(&f))
        }
        "win_time_week" => {
            let d: usize = locale(windows::Win32::Globalization::LOCALE_IFIRSTDAYOFWEEK).parse().unwrap_or(0);
            DAYS[d.min(6)].to_string()
        }
        "win_time_auto" => b(reg_text(HKEY_LOCAL_MACHINE, W32TIME, "Type").is_some_and(|t| t != "NoSync")),
        "win_tz_auto" => b(reg_dword(HKEY_LOCAL_MACHINE, TZ_AUTO, "Start") == Some(3)),
        "win_rdp" => b(reg_dword(
            HKEY_LOCAL_MACHINE,
            "SYSTEM\\CurrentControlSet\\Control\\Terminal Server",
            "fDenyTSConnections",
        ) == Some(0)),
        _ => return None,
    })
}

pub fn desc(key: &str) -> Option<String> {
    Some(match key {
        "win_account" => account(),
        _ => return None,
    })
}

pub fn shown(_key: &str) -> Option<bool> {
    None
}

/// Runs a machine-wide change as administrator (Windows asks), off the window's thread.
fn admin(what: &'static str, command: String) {
    std::thread::spawn(move || {
        let r = super::run_admin(&command);
        config::log(&format!("system: {what}: {r:?}"));
        super::changed();
    });
}

pub fn set(key: &str, v: &str) -> Option<()> {
    use windows::Win32::Globalization::{LOCALE_SSHORTTIME, LOCALE_STIMEFORMAT};
    let on = v == "true";
    if let Some(cap) = capability(key) {
        set_reg_text(HKEY_CURRENT_USER, &format!("{CONSENT}\\{cap}"), "Value", if on { "Allow" } else { "Deny" });
        return Some(());
    }
    if let Some((k, val, yes)) = switch(key) {
        let x = if on {
            yes
        } else if key == "win_hidden" {
            2
        } else {
            1 - yes.min(1)
        };
        set_reg_dword(HKEY_CURRENT_USER, k, val, x);
        if matches!(key, "win_file_ext" | "win_hidden") {
            // The desktop shows files as Explorer's settings say.
            crate::msg::post(crate::msg::DESK_ICONS, 0, 0);
            super::broadcast("ShellState");
        }
        return Some(());
    }
    match key {
        "win_time_24h" => {
            set_locale(LOCALE_SSHORTTIME, if on { "HH:mm" } else { "h:mm tt" });
            set_locale(LOCALE_STIMEFORMAT, if on { "HH:mm:ss" } else { "h:mm:ss tt" });
        }
        "win_time_auto" => admin(
            "set time automatically",
            format!(
                "reg add \"HKLM\\{W32TIME}\" /v Type /t REG_SZ /d {} /f & net stop w32time & net start w32time{}",
                if on { "NTP" } else { "NoSync" },
                if on { " & w32tm /resync /force" } else { "" }
            ),
        ),
        "win_tz_auto" => admin(
            "set time zone automatically",
            format!("reg add \"HKLM\\{TZ_AUTO}\" /v Start /t REG_DWORD /d {} /f", if on { 3 } else { 4 }),
        ),
        "win_rdp" => admin(
            "remote desktop",
            format!(
                "reg add \"HKLM\\SYSTEM\\CurrentControlSet\\Control\\Terminal Server\" /v fDenyTSConnections /t REG_DWORD /d {} /f & netsh advfirewall firewall set rule group=\"@FirewallAPI.dll,-28752\" new enable={}",
                if on { 0 } else { 1 },
                if on { "Yes" } else { "No" }
            ),
        ),
        _ => return None,
    }
    Some(())
}

pub fn action(key: &str, owner: HWND) -> Option<()> {
    use windows::Win32::Globalization::{LOCALE_IFIRSTDAYOFWEEK, LOCALE_SSHORTDATE};
    match key {
        "win_time_date" => {
            let now = locale(LOCALE_SSHORTDATE);
            let names: Vec<String> = DATE_FORMATS.iter().map(|f| format!("{}  ({f})", sample(f))).collect();
            if let Some(i) = super::pick(owner, &names, DATE_FORMATS.iter().position(|f| *f == now)) {
                set_locale(LOCALE_SSHORTDATE, DATE_FORMATS[i]);
            }
        }
        "win_time_week" => {
            let names: Vec<String> = DAYS.iter().map(|d| d.to_string()).collect();
            let now: usize = locale(LOCALE_IFIRSTDAYOFWEEK).parse().unwrap_or(0);
            if let Some(i) = super::pick(owner, &names, Some(now.min(6))) {
                set_locale(LOCALE_IFIRSTDAYOFWEEK, &i.to_string());
            }
        }
        "win_advanced_startup" => {
            if super::confirm(owner, "Restart into Windows' recovery options now (startup settings, safe mode, system restore, UEFI)? Save your work first.") {
                run_hidden("shutdown.exe", &["/r", "/o", "/t", "0"], false);
            }
        }
        "win_reset_pc" => {
            crate::shell::launch("systemreset.exe", "", true);
        }
        "win_restore" => {
            crate::shell::launch("rstrui.exe", "", true);
        }
        "win_accounts" => {
            crate::shell::launch("netplwiz.exe", "", false);
        }
        "win_lock" => unsafe {
            let _ = windows::Win32::System::Shutdown::LockWorkStation();
        },
        "win_lockscreen" => {
            // Windows keeps the lock screen picture as a machine-wide setting (as its Settings' service does).
            crate::shell::off_thread(|| {
                let filters = [("Pictures", "*.jpg;*.jpeg;*.png;*.bmp"), ("All files", "*.*")];
                let Some(path) = crate::shell::choose_file("Choose a lock screen picture", &filters) else { return };
                let p = path.to_string_lossy().replace('"', "");
                let key = "HKLM\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\PersonalizationCSP";
                let r = super::run_admin(&format!(
                    "reg add \"{key}\" /v LockScreenImagePath /t REG_SZ /d \"{p}\" /f & reg add \"{key}\" /v LockScreenImageUrl /t REG_SZ /d \"{p}\" /f & reg add \"{key}\" /v LockScreenImageStatus /t REG_DWORD /d 1 /f"
                ));
                config::log(&format!("look: lock screen picture {p}: {r:?}"));
            });
        }
        "win_env" => {
            crate::shell::launch("rundll32.exe", "sysdm.cpl,EditEnvironmentVariables", false);
        }
        _ => return None,
    }
    Some(())
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
