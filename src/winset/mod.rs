//! Windows' own settings, on Cascade's settings pages (Display, Sound,
//! Network & Bluetooth, Personalisation, Apps, Power & battery, Time &
//! language, Privacy & accessibility, System): what Windows' Settings app
//! offers for each, read from Windows and changed through the same APIs, so
//! nothing needs that app (which, as a Store app, needs Explorer).
//!
//! Each setting is a key starting `win_`. The settings window asks `get`
//! for a value, `desc` for a live description, and hands changes to `set`
//! (toggles, sliders, text) or `action` (buttons, and the pickers, which
//! show a list to choose from).

pub mod apps;
pub mod audio;
pub mod display;
pub mod look;
pub mod net;
pub mod night;
pub mod power;
pub mod system;

use std::ffi::c_void;
use std::sync::Mutex;

use windows::core::{w, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::System::Registry::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::msg;

// ------------------------------------------------------------- registry --

pub(crate) fn reg_text(root: HKEY, key: &str, value: &str) -> Option<String> {
    let mut buf = vec![0u16; 4096];
    let mut size = (buf.len() * 2) as u32;
    let ok = unsafe {
        RegGetValueW(
            root,
            &HSTRING::from(key),
            &HSTRING::from(value),
            RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND,
            None,
            Some(buf.as_mut_ptr() as *mut c_void),
            Some(&mut size),
        )
    } == ERROR_SUCCESS;
    ok.then(|| {
        let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..n])
    })
}

pub(crate) fn reg_dword(root: HKEY, key: &str, value: &str) -> Option<u32> {
    let mut v = 0u32;
    let mut size = 4u32;
    let ok = unsafe {
        RegGetValueW(
            root,
            &HSTRING::from(key),
            &HSTRING::from(value),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut v as *mut u32 as *mut c_void),
            Some(&mut size),
        )
    } == ERROR_SUCCESS;
    ok.then_some(v)
}

pub(crate) fn set_reg_dword(root: HKEY, key: &str, value: &str, v: u32) -> bool {
    unsafe {
        RegSetKeyValueW(
            root,
            &HSTRING::from(key),
            &HSTRING::from(value),
            REG_DWORD.0,
            Some(&v as *const u32 as *const c_void),
            4,
        ) == ERROR_SUCCESS
    }
}

pub(crate) fn set_reg_text(root: HKEY, key: &str, value: &str, text: &str) -> bool {
    let wide: Vec<u16> = text.encode_utf16().chain([0]).collect();
    unsafe {
        RegSetKeyValueW(
            root,
            &HSTRING::from(key),
            &HSTRING::from(value),
            REG_SZ.0,
            Some(wide.as_ptr() as *const c_void),
            (wide.len() * 2) as u32,
        ) == ERROR_SUCCESS
    }
}

pub(crate) fn set_reg_binary(root: HKEY, key: &str, value: &str, data: &[u8]) -> bool {
    unsafe {
        RegSetKeyValueW(
            root,
            &HSTRING::from(key),
            &HSTRING::from(value),
            REG_BINARY.0,
            Some(data.as_ptr() as *const c_void),
            data.len() as u32,
        ) == ERROR_SUCCESS
    }
}

/// The subkeys of a key.
pub(crate) fn subkeys(root: HKEY, key: &str, access: REG_SAM_FLAGS) -> Vec<String> {
    let mut out = Vec::new();
    unsafe {
        let mut h = HKEY::default();
        if RegOpenKeyExW(root, &HSTRING::from(key), Some(0), KEY_READ | access, &mut h) != ERROR_SUCCESS {
            return out;
        }
        let mut name = [0u16; 256];
        for i in 0.. {
            let mut len = name.len() as u32;
            if RegEnumKeyExW(h, i, Some(PWSTR(name.as_mut_ptr())), &mut len, None, None, None, None) != ERROR_SUCCESS {
                break;
            }
            out.push(String::from_utf16_lossy(&name[..len as usize]));
        }
        let _ = RegCloseKey(h);
    }
    out
}

/// Tells every window a setting changed (`area`: "ImmersiveColorSet" and the like).
pub(crate) fn broadcast(area: &str) {
    let text = HSTRING::from(area);
    unsafe {
        let mut r = 0usize;
        let _ = SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            WPARAM(0),
            LPARAM(text.as_ptr() as isize),
            SMTO_ABORTIFHUNG,
            200,
            Some(&mut r),
        );
    }
}

/// Runs a program without a window, and waits for it if `wait`.
pub(crate) fn run_hidden(file: &str, args: &[&str], wait: bool) -> bool {
    use std::os::windows::process::CommandExt;
    let mut cmd = std::process::Command::new(file);
    cmd.args(args).creation_flags(0x0800_0000);
    if wait {
        cmd.status().is_ok_and(|s| s.success())
    } else {
        cmd.spawn().is_ok()
    }
}

/// Says "the settings changed": the settings window shows the new values.
pub(crate) fn changed() {
    msg::post(msg::WIN_CHANGED, 0, 0);
}

// --------------------------------------------------------------- picking --

/// Shows `options` as a menu at the pointer, the one `current` ticked;
/// returns the one picked.
pub(crate) fn pick(owner: HWND, options: &[String], current: Option<usize>) -> Option<usize> {
    let entries =
        options.iter().enumerate().map(|(i, o)| crate::menu::check(i + 1, o.as_str(), Some(i) == current)).collect();
    crate::menu::show(owner, entries, crate::menu::Place::Cursor).checked_sub(1)
}

/// Asks a yes/no question; true for yes.
pub(crate) fn confirm(owner: HWND, text: &str) -> bool {
    unsafe { MessageBoxW(Some(owner), &HSTRING::from(text), w!("Cascade"), MB_YESNO | MB_ICONQUESTION) == IDYES }
}

/// A message box, from any thread.
pub(crate) fn tell(text: String) {
    std::thread::spawn(move || unsafe {
        MessageBoxW(None, &HSTRING::from(text), w!("Cascade"), MB_OK | MB_ICONINFORMATION | MB_TOPMOST);
    });
}

// ------------------------------------------------------------- dispatch --

/// A picker or button asked for, waiting to be run outside the settings
/// window's handler (menus and boxes wait for an answer).
static PENDING: Mutex<Option<&'static str>> = Mutex::new(None);

/// Queues `action(key)` to run once the settings window's handler has returned.
pub fn queue(key: &'static str) {
    *PENDING.lock().unwrap_or_else(|e| e.into_inner()) = Some(key);
    msg::post(msg::WIN_ACTION, 0, 0);
}

/// The queued action, if there is one.
pub fn take_queued() -> Option<&'static str> {
    PENDING.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// A setting's value, as the settings window shows it.
pub fn get(key: &str) -> String {
    let b = |v: bool| v.to_string();
    match key {
        "win_screen" | "win_layout" | "win_resolution" | "win_refresh" | "win_scale" | "win_orientation" => {
            display::label(key)
        }
        "win_hdr" => b(display::hdr().is_some_and(|(_, on)| on)),
        "win_night" => b(night::settings().on),
        "win_night_strength" => night::settings().strength.to_string(),
        "win_night_schedule" => night::schedule_label(),
        "win_output" | "win_input" => audio::label(key),
        "win_volume" => audio::level(false).to_string(),
        "win_mic" => audio::level(true).to_string(),
        "win_wifi" => b(net::radio(net::RadioKind::WiFi).unwrap_or(false)),
        "win_bt" => b(net::radio(net::RadioKind::Bluetooth).unwrap_or(false)),
        "win_airplane" => b(net::airplane()),
        "win_hotspot" => b(net::hotspot_on()),
        k if k.starts_with("win_hotspot_") => net::hotspot_value(k),
        "win_join_ssid" => net::join_ssid(),
        "win_join_pw" => String::new(),
        "win_dark_apps" | "win_dark_windows" | "win_transparency" | "win_accent_bars" => b(look::flag(key)),
        "win_accent" => look::accent_label(),
        "win_wallpaper_fit" => look::fit_label(),
        "win_power_mode" | "win_power_plan" | "win_screen_ac" | "win_sleep_ac" | "win_screen_dc" | "win_sleep_dc" => {
            power::label(key)
        }
        "win_timezone" => system::timezone_label(),
        "win_camera" | "win_mic_access" | "win_location" | "win_toasts" => b(system::flag(key)),
        "win_dnd" => b(dnd()),
        "win_speech" => b(crate::dictate::online_allowed()),
        _ => String::new(),
    }
}

/// A live description, where a row has one.
pub fn desc(key: &str) -> Option<String> {
    match key {
        "win_screen" => Some(display::screen_desc()),
        "win_hdr" => {
            display::hdr().map(|_| "Brighter highlights and richer colour in apps and games made for it.".into())
        }
        "win_net_info" => Some(net::info()),
        "win_hotspot" => Some(net::hotspot_desc()),
        "win_hotspot_devices" => Some(net::hotspot_devices()),
        "win_known" => Some(net::known_desc()),
        "win_about" => Some(system::about()),
        "win_storage" => Some(system::storage()),
        "win_layouts" => Some(system::layouts()),
        "win_battery" => system::battery(),
        "win_night" => Some(night::desc()),
        _ => None,
    }
}

/// Whether a row applies on this PC (HDR only on a screen that has it, battery only on a laptop...).
pub fn shown(key: &str) -> bool {
    match key {
        "win_hdr" => display::hdr().is_some(),
        "win_screen" | "win_layout" | "win_primary" | "win_arrange" | "win_identify" => display::count() > 1,
        "win_screen_dc" | "win_sleep_dc" | "win_battery" => system::battery().is_some(),
        "win_hotspot" => net::hotspot_available(),
        k if k.starts_with("win_hotspot_") => net::hotspot_row_shown(k),
        _ => true,
    }
}

/// A toggle, slider or text box changed.
pub fn set(key: &str, v: &str) {
    let on = v == "true";
    match key {
        "win_hdr" => display::set_hdr(on),
        "win_night" => night::update(|s| s.on = on),
        "win_night_strength" => {
            if let Ok(x) = v.parse::<f32>() {
                night::update(|s| s.strength = x);
            }
        }
        "win_volume" | "win_mic" => {
            if let Ok(x) = v.parse::<f32>() {
                audio::set_level(key == "win_mic", x);
            }
        }
        "win_wifi" => net::set_radio(net::RadioKind::WiFi, on),
        "win_bt" => net::set_radio(net::RadioKind::Bluetooth, on),
        "win_airplane" => net::set_airplane(on),
        "win_hotspot" => net::set_hotspot(on),
        "win_hotspot_name" => net::set_hotspot_name(v),
        "win_hotspot_pw" => net::set_hotspot_password(v),
        "win_hotspot_saver" => net::set_hotspot_saver(on),
        "win_join_pw" => net::set_join_password(v),
        "win_dark_apps" | "win_dark_windows" | "win_transparency" | "win_accent_bars" => look::set_flag(key, on),
        "win_camera" | "win_mic_access" | "win_location" | "win_toasts" => system::set_flag(key, on),
        "win_dnd" => set_dnd(on),
        "win_speech" => crate::dictate::set_online_allowed(on),
        _ => {}
    }
}

/// A button or picker was clicked (run outside the settings window's handler; see `queue`).
pub fn action(key: &str, owner: HWND) {
    let open = |t: &str| crate::shell::open(t);
    match key {
        "win_screen" | "win_layout" | "win_resolution" | "win_refresh" | "win_scale" | "win_orientation" => {
            display::pick(key, owner)
        }
        "win_primary" => display::make_primary(),
        "win_identify" => display::identify(),
        "win_night_schedule" => night::pick_schedule(owner),
        "win_output" | "win_input" => audio::pick(key, owner),
        "win_sound_cpl" => open("mmsys.cpl"),
        "win_mixer" => open("sndvol.exe"),
        "win_join_ssid" => net::pick_ssid(owner),
        "win_known" => net::pick_forget(owner),
        "win_hotspot_band" | "win_hotspot_security" | "win_hotspot_from" => net::pick_hotspot(key, owner),
        "win_join" => net::join(),
        "win_bt_add" => open("DevicePairingWizard.exe"),
        "win_printers" => {
            crate::shell::launch("control.exe", "printers", false);
        }
        "win_vpn" => open("rasphone.exe"),
        "win_ncpa" => open("ncpa.cpl"),
        "win_proxy" => {
            crate::shell::launch("rundll32.exe", "inetcpl.cpl,LaunchConnectionDialog", false);
        }
        "win_accent" => look::pick_accent(owner),
        "win_wallpaper" => {
            if let Some(p) = crate::wallpaper::choose(owner) {
                crate::wallpaper::set_picture(&p);
                changed();
            }
        }
        "win_wallpaper_fit" => look::pick_fit(owner),
        "win_uninstall" => apps::pick_uninstall(owner),
        "win_startup" => apps::pick_startup(owner),
        "win_appwiz" => open("appwiz.cpl"),
        "win_features" => open("optionalfeatures.exe"),
        "win_power_mode" | "win_power_plan" | "win_screen_ac" | "win_sleep_ac" | "win_screen_dc" | "win_sleep_dc" => {
            power::pick(key, owner)
        }
        "win_power_cpl" => open("powercfg.cpl"),
        "win_timezone" => system::pick_timezone(owner),
        "win_time_sync" => system::sync_time(),
        "win_time_cpl" => open("timedate.cpl"),
        "win_intl" => open("intl.cpl"),
        "win_magnifier" => open("magnify.exe"),
        "win_narrator" => open("narrator.exe"),
        "win_osk" => open("osk.exe"),
        "win_ease" => {
            crate::shell::launch("control.exe", "/name Microsoft.EaseOfAccessCenter", false);
        }
        "win_rename" => open("SystemPropertiesComputerName.exe"),
        "win_sysprops" => open("SystemPropertiesAdvanced.exe"),
        "win_devmgr" => open("devmgmt.msc"),
        "win_cleanup" => open("cleanmgr.exe"),
        "win_update" => system::check_updates(),
        "win_taskmgr" => open("taskmgr.exe"),
        "update_branch" => crate::update::pick_channel(owner),
        "ducker_trigger" | "ducker_targets" => crate::ducker::pick_apps(owner, key),
        _ => {}
    }
}

// ------------------------------------------------------- do not disturb --

static DND: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Cascade's Do Not Disturb: notification pop-ups stay quiet (they still go in the list).
pub fn dnd() -> bool {
    DND.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn set_dnd(on: bool) {
    DND.store(on, std::sync::atomic::Ordering::Relaxed);
    let _ = std::fs::write(crate::config::dir().join("dnd"), if on { "1" } else { "0" });
}

/// Starts what runs in the background (night light), and reads Do Not Disturb back.
pub fn start() {
    let on = std::fs::read_to_string(crate::config::dir().join("dnd")).is_ok_and(|t| t.trim() == "1");
    DND.store(on, std::sync::atomic::Ordering::Relaxed);
    night::start();
}

/// Puts back what Cascade changed for as long as it runs (night light's tint).
pub fn stop() {
    night::stop();
}

/// The Cascade settings page for a `ms-settings:` link (see `settings::SECTIONS`), if there's one.
pub fn page_for(uri: &str) -> Option<usize> {
    let page = uri.to_ascii_lowercase();
    let page = page.trim_start_matches("ms-settings:");
    let has = |words: &[&str]| words.iter().any(|w| page.starts_with(w));
    use crate::settings::page;
    Some(if has(&["display", "nightlight", "screenrotation"]) {
        page::DISPLAY
    } else if has(&["sound", "apps-volume"]) {
        page::SOUND
    } else if has(&["network", "bluetooth", "connecteddevices", "devices", "printers", "mobile-devices"]) {
        page::NETWORK
    } else if has(&["personalization", "colors", "themes", "lockscreen"]) {
        page::LOOK
    } else if has(&["appsfeatures", "installed-apps", "startupapps", "defaultapps", "optionalfeatures"]) {
        page::APPS
    } else if has(&["powersleep", "batterysaver", "energy"]) {
        page::POWER
    } else if has(&["dateandtime", "regionformatting", "regionlanguage", "keyboard", "typing"]) {
        page::TIME
    } else if has(&["privacy", "easeofaccess", "notifications", "quiethours", "focus"]) {
        page::PRIVACY
    } else if page.is_empty() || has(&["about", "storagesense", "windowsupdate", "recovery", "system"]) {
        page::SYSTEM
    } else {
        return None;
    })
}

/// A PCWSTR from a static wide string (for menus built at run time).
pub(crate) fn _pcwstr(s: &HSTRING) -> PCWSTR {
    PCWSTR(s.as_ptr())
}
