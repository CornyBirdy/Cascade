//! USB devices plugged in or out, announced in the notch: "Flight controller
//! (STM32) on COM5 · click to open Betaflight".
//!
//! Windows tells the notch's window (`WM_DEVICECHANGE`) when a USB device
//! arrives or goes, and when a serial port does; nothing is polled. Which
//! devices are worth a pop-up, what they're called and what clicking it opens
//! come from a small built-in list for FPV gear, headsets and phones, and
//! from `devices.txt` in the data folder, which wins over it.

use std::ffi::c_void;

use windows::core::{GUID, HSTRING};
use windows::Win32::Foundation::{ERROR_SUCCESS, HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};

use crate::config;

/// GUID_DEVINTERFACE_USB_DEVICE: every USB device (not each of its parts).
const USB_DEVICE: GUID = GUID::from_u128(0xA5DCBF10_6530_11D2_901F_00C04FB951ED);

const DBT_DEVICEARRIVAL: usize = 0x8000;
const DBT_DEVICEREMOVECOMPLETE: usize = 0x8004;
const DBT_DEVTYP_PORT: u32 = 3;
const DBT_DEVTYP_DEVICEINTERFACE: u32 = 5;

/// DEV_BROADCAST_HDR.
#[repr(C)]
#[allow(dead_code)]
struct Header {
    size: u32,
    kind: u32,
    reserved: u32,
}

/// DEV_BROADCAST_DEVICEINTERFACE_W, with room for no name (for registering).
/// Only Windows reads it.
#[repr(C)]
#[allow(dead_code)]
struct InterfaceFilter {
    size: u32,
    kind: u32,
    reserved: u32,
    class: GUID,
    name: [u16; 1],
}

/// Asks Windows to tell `hwnd` about USB devices coming and going (serial
/// ports are announced to every window anyway).
pub fn listen(hwnd: HWND) {
    use windows::Win32::UI::WindowsAndMessaging::{RegisterDeviceNotificationW, DEVICE_NOTIFY_WINDOW_HANDLE};
    let filter = InterfaceFilter {
        size: std::mem::size_of::<InterfaceFilter>() as u32,
        kind: DBT_DEVTYP_DEVICEINTERFACE,
        reserved: 0,
        class: USB_DEVICE,
        name: [0],
    };
    unsafe {
        let _ = RegisterDeviceNotificationW(
            HANDLE(hwnd.0),
            &filter as *const InterfaceFilter as *const c_void,
            DEVICE_NOTIFY_WINDOW_HANDLE,
        );
    }
}

/// What a `WM_DEVICECHANGE` was about.
#[derive(Debug, PartialEq)]
pub enum Change {
    /// A USB device, by its interface path (`\\?\USB#VID_0483&PID_5740#...`).
    Arrived(String),
    Removed(String),
    /// A serial port, e.g. "COM5".
    Port(String),
}

/// Reads a `WM_DEVICECHANGE`.
///
/// # Safety
/// `lp` must be the message's own lParam.
pub unsafe fn parse(wp: WPARAM, lp: LPARAM) -> Option<Change> {
    let arrived = match wp.0 {
        DBT_DEVICEARRIVAL => true,
        DBT_DEVICEREMOVECOMPLETE => false,
        _ => return None,
    };
    let hdr = lp.0 as *const Header;
    if hdr.is_null() {
        return None;
    }
    let size = (*hdr).size as usize;
    let text = |offset: usize| {
        if size <= offset {
            return String::new();
        }
        let p = (hdr as *const u8).add(offset) as *const u16;
        let n = (size - offset) / 2;
        let s = std::slice::from_raw_parts(p, n);
        let end = s.iter().position(|&c| c == 0).unwrap_or(n);
        String::from_utf16_lossy(&s[..end])
    };
    match (*hdr).kind {
        // DEV_BROADCAST_DEVICEINTERFACE_W: header, then the class GUID, then the name.
        DBT_DEVTYP_DEVICEINTERFACE => {
            let path = text(12 + 16);
            Some(if arrived { Change::Arrived(path) } else { Change::Removed(path) })
        }
        // DEV_BROADCAST_PORT_W: header, then the name.
        DBT_DEVTYP_PORT if arrived => Some(Change::Port(text(12))),
        _ => None,
    }
}

/// The vendor and product ids in a device path or instance id.
pub fn ids(path: &str) -> Option<(u16, u16)> {
    let up = path.to_ascii_uppercase();
    let hex = |tag: &str| {
        let i = up.find(tag)? + tag.len();
        u16::from_str_radix(up.get(i..i + 4)?, 16).ok()
    };
    Some((hex("VID_")?, hex("PID_")?))
}

/// The registry key of the device behind interface `path`:
/// `\\?\USB#VID_0483&PID_5740#123#{guid}` -> `USB\VID_0483&PID_5740\123`.
fn instance(path: &str) -> Option<String> {
    let mut parts = path.trim_start_matches("\\\\?\\").split('#');
    let (bus, hw, serial) = (parts.next()?, parts.next()?, parts.next()?);
    Some(format!("{bus}\\{hw}\\{serial}"))
}

/// What Windows calls the device ("USB Serial Device", "Quest 2").
fn windows_name(path: &str) -> Option<String> {
    let key = HSTRING::from(format!("SYSTEM\\CurrentControlSet\\Enum\\{}", instance(path)?));
    for value in ["FriendlyName", "DeviceDesc"] {
        let mut buf = [0u16; 256];
        let mut size = (buf.len() * 2) as u32;
        let found = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                &key,
                &HSTRING::from(value),
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr() as *mut c_void),
                Some(&mut size),
            ) == ERROR_SUCCESS
        };
        if found {
            let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            let s = String::from_utf16_lossy(&buf[..end]);
            // "@usb.inf,%usb.devicedesc%;USB Composite Device"
            let s = s.rsplit(';').next().unwrap_or(&s).trim().to_string();
            if !s.is_empty() {
                return Some(s);
            }
        }
    }
    None
}

/// A device worth a pop-up: what to call it, and what clicking it opens.
#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    pub name: String,
    /// A program, file or link; None to only say it's there.
    pub open: Option<String>,
    /// What `open` is, for the pop-up ("Betaflight").
    pub open_label: Option<String>,
    /// It talks over a serial port, so the port it gets is worth saying.
    pub serial: bool,
}

/// One line of the device list: ids (PID None = any), name ("-" = never
/// announce it) and what to open.
#[derive(Clone, Debug, PartialEq)]
struct Rule {
    vid: u16,
    pid: Option<u16>,
    name: String,
    open: Option<String>,
}

const BETAFLIGHT: &str = "https://app.betaflight.com";

/// A device on the built-in list: ids (PID None = any), name, what clicking
/// opens, and whether it talks over a serial port.
struct Known(u16, Option<u16>, &'static str, Option<&'static str>, bool);

/// FPV gear, headsets and phones.
const KNOWN: &[Known] = &[
    // STM32 and AT32 flight controllers (Betaflight, INAV, EdgeTX radios in VCP mode).
    Known(0x0483, Some(0x5740), "Flight controller (STM32)", Some(BETAFLIGHT), true),
    Known(0x2E3C, Some(0x5740), "Flight controller (AT32)", Some(BETAFLIGHT), true),
    Known(0x0483, Some(0xDF11), "Flight controller in DFU mode", Some(BETAFLIGHT), false),
    Known(0x2E3C, Some(0xDF11), "Flight controller in DFU mode (AT32)", Some(BETAFLIGHT), false),
    // ArduPilot boards (ChibiOS).
    Known(0x1209, Some(0x5740), "Flight controller (ArduPilot)", None, true),
    Known(0x1209, Some(0x5741), "Flight controller (ArduPilot)", None, true),
    // EdgeTX / OpenTX radios plugged in as a joystick, for simulators.
    Known(0x1209, Some(0x4F54), "Radio as a joystick", Some("joy.cpl"), false),
    // USB-serial bridges on ExpressLRS modules and receivers, and ESP32 boards.
    Known(0x10C4, Some(0xEA60), "USB serial (CP210x)", None, true),
    Known(0x1A86, Some(0x7523), "USB serial (CH340)", None, true),
    Known(0x1A86, Some(0x55D4), "USB serial (CH9102)", None, true),
    Known(0x303A, None, "ESP32 board", None, true),
    // Headsets and goggles.
    Known(0x2833, None, "Meta Quest", None, false),
    Known(0x2CA3, None, "DJI device", None, false),
    // Phones.
    Known(0x04E8, Some(0x6860), "Samsung phone", None, false),
    Known(0x18D1, None, "Android phone", None, false),
];

fn list_path() -> std::path::PathBuf {
    config::dir().join("devices.txt")
}

const TEMPLATE: &str = "\
# USB devices Cascade announces in the notch when they're plugged in, one per line:
#
#   VID:PID = Name | what clicking the pop-up opens (a program, file or link)
#
# The ids are in hex (Device Manager > the device > Details > Hardware Ids).
# Use * for any product of that vendor, leave out the | part to open nothing,
# and a name of - to never announce that device. These win over Cascade's
# own list. For example:
#
# 0483:5740 = My quad | https://app.betaflight.com
# 2833:* = Quest 2 | C:\\Program Files\\Oculus\\Support\\oculus-client\\OculusClient.exe
# 046D:* = -
";

/// Opens the device list in the text editor, writing the explained empty
/// one first if there isn't one.
pub fn edit_list() {
    let path = list_path();
    if !path.exists() {
        let _ = std::fs::create_dir_all(config::dir());
        let _ = std::fs::write(&path, TEMPLATE);
    }
    crate::shell::open_file(&path);
}

fn parse_rules(text: &str) -> Vec<Rule> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (ids, rest) = line.split_once('=')?;
            let (vid, pid) = ids.trim().split_once(':')?;
            let vid = u16::from_str_radix(vid.trim(), 16).ok()?;
            let pid = match pid.trim() {
                "*" => None,
                p => Some(u16::from_str_radix(p, 16).ok()?),
            };
            let (name, open) = match rest.split_once('|') {
                Some((n, o)) => (n.trim(), Some(o.trim().trim_matches('"').to_string()).filter(|o| !o.is_empty())),
                None => (rest.trim(), None),
            };
            Some(Rule { vid, pid, name: name.to_string(), open })
        })
        .collect()
}

/// A friendly label for what `open` opens.
fn open_label(open: &str) -> String {
    let lower = open.to_ascii_lowercase();
    if lower.contains("betaflight") {
        return "Betaflight".into();
    }
    if lower == "joy.cpl" {
        return "Game controllers".into();
    }
    if let Some(host) = lower.strip_prefix("https://").or_else(|| lower.strip_prefix("http://")) {
        return host.split('/').next().unwrap_or(host).trim_start_matches("www.").to_string();
    }
    let file = open.rsplit(['\\', '/']).next().unwrap_or(open);
    let stem = file.rsplit_once('.').map_or(file, |(s, _)| s);
    stem.to_string()
}

/// What to say about device `vid:pid`, by the user's list, then the
/// built-in one; with `all`, any other device too, by Windows' name for it.
fn lookup(rules: &[Rule], vid: u16, pid: u16, all: bool, name: impl FnOnce() -> Option<String>) -> Option<Device> {
    let fits = |v: u16, p: Option<u16>| v == vid && p.is_none_or(|p| p == pid);
    // An exact product beats a whole vendor.
    let rule =
        rules.iter().find(|r| r.pid == Some(pid) && r.vid == vid).or_else(|| rules.iter().find(|r| fits(r.vid, r.pid)));
    let known = KNOWN.iter().find(|k| k.1 == Some(pid) && k.0 == vid).or_else(|| KNOWN.iter().find(|k| fits(k.0, k.1)));
    let serial = known.is_some_and(|k| k.4);
    if let Some(r) = rule {
        if r.name == "-" {
            return None;
        }
        let open = r.open.clone().or_else(|| known.and_then(|k| k.3).map(String::from));
        return Some(Device { name: r.name.clone(), open_label: open.as_deref().map(open_label), open, serial });
    }
    if let Some(k) = known {
        let open = k.3.map(String::from);
        return Some(Device { name: k.2.to_string(), open_label: open.as_deref().map(open_label), open, serial });
    }
    if all {
        return Some(Device { name: name()?, open: None, open_label: None, serial: false });
    }
    None
}

/// The device behind interface `path`, if it's one to announce. `all`: any
/// USB device, not only the ones on the lists.
pub fn describe(path: &str, all: bool) -> Option<Device> {
    let (vid, pid) = ids(path)?;
    let rules = std::fs::read_to_string(list_path()).map(|t| parse_rules(&t)).unwrap_or_default();
    lookup(&rules, vid, pid, all, || windows_name(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_device_paths() {
        let p = "\\\\?\\USB#VID_0483&PID_5740#0x00000000001A#{a5dcbf10-6530-11d2-901f-00c04fb951ed}";
        assert_eq!(ids(p), Some((0x0483, 0x5740)));
        assert_eq!(instance(p).as_deref(), Some("USB\\VID_0483&PID_5740\\0x00000000001A"));
        assert_eq!(ids("usb#vid_2833&pid_0186#1"), Some((0x2833, 0x0186)));
        assert_eq!(ids("\\\\?\\HID#nothing"), None);
    }

    #[test]
    fn lists_decide() {
        let rules = parse_rules(
            "# comment\n0483:5740 = My quad | https://app.betaflight.com\n2833:* = Quest 2\n046d:* = -\nbad line\n",
        );
        assert_eq!(rules.len(), 3);
        let quad = lookup(&rules, 0x0483, 0x5740, false, || None).unwrap();
        assert_eq!(quad.name, "My quad");
        assert_eq!(quad.open_label.as_deref(), Some("Betaflight"));
        assert!(quad.serial);
        // A whole vendor, with the built-in list's action kept... (none for Quest).
        let quest = lookup(&rules, 0x2833, 0x0186, false, || None).unwrap();
        assert_eq!((quest.name.as_str(), quest.open.as_deref()), ("Quest 2", None));
        // Silenced, even with every device on.
        assert_eq!(lookup(&rules, 0x046D, 0xC52B, true, || Some("Mouse".into())), None);
        // Built in.
        let dfu = lookup(&[], 0x0483, 0xDF11, false, || None).unwrap();
        assert_eq!(dfu.open.as_deref(), Some(BETAFLIGHT));
        // Unknown: only with every device on, by Windows' name.
        assert_eq!(lookup(&[], 0x1234, 0x5678, false, || Some("Thing".into())), None);
        assert_eq!(lookup(&[], 0x1234, 0x5678, true, || Some("Thing".into())).unwrap().name, "Thing");
    }

    #[test]
    fn labels_what_opens() {
        assert_eq!(open_label("https://app.betaflight.com"), "Betaflight");
        assert_eq!(open_label("https://www.example.com/x"), "example.com");
        assert_eq!(open_label("C:\\Program Files\\Oculus\\OculusClient.exe"), "OculusClient");
        assert_eq!(open_label("joy.cpl"), "Game controllers");
    }
}
