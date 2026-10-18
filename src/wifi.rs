//! Wi-Fi networks for the notch's Controls: the ones in range, and joining
//! one, through the WLAN API (what Windows' own network list uses).
//!
//! A thread of its own does the work, only when asked (the Controls tab's
//! Wi-Fi list opening, or a network picked): it lists what Windows already
//! knows at once, asks the adapter to scan, and lists again once the scan
//! has had time to finish. Joining works for networks you've been on before
//! (their saved profile) and open ones; one that needs a password it has
//! never had goes to Windows' own picker, which asks for it.

use std::ffi::c_void;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Duration;

use windows::core::{GUID, PCWSTR};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::NetworkManagement::WiFi::*;

use crate::msg;

/// A network in range.
#[derive(Clone, Debug, PartialEq)]
pub struct Net {
    pub ssid: String,
    /// 0 to 100.
    pub signal: u32,
    pub secure: bool,
    pub connected: bool,
    /// The saved profile to join it with, if Windows has one.
    pub profile: Option<String>,
}

/// What the UI is told (boxed, through `msg::WIFI`).
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub nets: Vec<Net>,
    /// Still scanning for more.
    pub scanning: bool,
    /// No Wi-Fi adapter (or the WLAN service is off).
    pub missing: bool,
    /// What happened to the last network picked, to show for a moment.
    pub note: Option<String>,
}

pub enum Cmd {
    /// List the networks (and scan for more).
    Scan,
    /// Join this one.
    Connect(Net),
}

pub fn spawn() -> Sender<Cmd> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        // ShellExecute (Windows' network list, for a password) wants COM.
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(
                None,
                windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
            );
        }
        run(rx)
    });
    tx
}

const AVAILABLE_CONNECTED: u32 = 0x1;
const AVAILABLE_HAS_PROFILE: u32 = 0x2;
/// How long a scan takes to fill in (Windows allows it four seconds).
const SCAN_WAIT: Duration = Duration::from_millis(2500);

struct Client(HANDLE);

impl Drop for Client {
    fn drop(&mut self) {
        unsafe {
            WlanCloseHandle(self.0, None);
        }
    }
}

fn open() -> Option<Client> {
    let mut version = 0u32;
    let mut h = HANDLE::default();
    let r = unsafe { WlanOpenHandle(2, None, &mut version, &mut h) };
    (r == 0).then_some(Client(h))
}

/// Every Wi-Fi adapter's id.
fn interfaces(c: &Client) -> Vec<GUID> {
    let mut list: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
    unsafe {
        if WlanEnumInterfaces(c.0, None, &mut list) != 0 || list.is_null() {
            return Vec::new();
        }
        let n = (*list).dwNumberOfItems as usize;
        let first = std::ptr::addr_of!((*list).InterfaceInfo) as *const WLAN_INTERFACE_INFO;
        let out = (0..n).map(|i| (*first.add(i)).InterfaceGuid).collect();
        WlanFreeMemory(list as *const c_void);
        out
    }
}

fn wide(s: &[u16]) -> String {
    String::from_utf16_lossy(&s[..s.iter().position(|&c| c == 0).unwrap_or(s.len())])
}

/// What adapter `guid` can see, one entry per network name, best first.
fn networks(c: &Client, guid: &GUID) -> Vec<Net> {
    let mut list: *mut WLAN_AVAILABLE_NETWORK_LIST = std::ptr::null_mut();
    let mut out: Vec<Net> = Vec::new();
    unsafe {
        if WlanGetAvailableNetworkList(c.0, guid, 0, None, &mut list) != 0 || list.is_null() {
            return out;
        }
        let n = (*list).dwNumberOfItems as usize;
        let first = std::ptr::addr_of!((*list).Network) as *const WLAN_AVAILABLE_NETWORK;
        for i in 0..n {
            let it = &*first.add(i);
            let len = (it.dot11Ssid.uSSIDLength as usize).min(32);
            let ssid = String::from_utf8_lossy(&it.dot11Ssid.ucSSID[..len]).to_string();
            if ssid.is_empty() {
                continue; // a hidden network
            }
            let profile = (it.dwFlags & AVAILABLE_HAS_PROFILE != 0).then(|| wide(&it.strProfileName));
            let net = Net {
                ssid,
                signal: it.wlanSignalQuality.min(100),
                secure: it.bSecurityEnabled.as_bool(),
                connected: it.dwFlags & AVAILABLE_CONNECTED != 0,
                profile: profile.filter(|p| !p.is_empty()),
            };
            // Windows lists a network once per profile, and once more without
            // one: keep one, with what's known about it merged.
            match out.iter_mut().find(|o| o.ssid == net.ssid) {
                Some(o) => {
                    o.connected |= net.connected;
                    o.signal = o.signal.max(net.signal);
                    if o.profile.is_none() {
                        o.profile = net.profile;
                    }
                }
                None => out.push(net),
            }
        }
        WlanFreeMemory(list as *const c_void);
    }
    out.sort_by(|a, b| b.connected.cmp(&a.connected).then(b.signal.cmp(&a.signal)));
    out
}

fn all_networks(c: &Client, ifaces: &[GUID]) -> Vec<Net> {
    let mut all: Vec<Net> = Vec::new();
    for g in ifaces {
        for n in networks(c, g) {
            if !all.iter().any(|a| a.ssid == n.ssid) {
                all.push(n);
            }
        }
    }
    all.sort_by(|a, b| b.connected.cmp(&a.connected).then(b.signal.cmp(&a.signal)));
    all
}

fn post(report: Report) {
    let ptr = Box::into_raw(Box::new(report));
    if !msg::post(msg::WIFI, 0, ptr as isize) {
        drop(unsafe { Box::from_raw(ptr) });
    }
}

/// Joins `net` on adapter `guid`. Err: why not, or None when Windows' own
/// picker should take over (it needs a password Windows doesn't have).
fn connect(c: &Client, guid: &GUID, net: &Net) -> Result<(), Option<String>> {
    let profile: Vec<u16> = net.profile.as_deref().unwrap_or("").encode_utf16().chain([0]).collect();
    let mut ssid = DOT11_SSID { uSSIDLength: net.ssid.len().min(32) as u32, ucSSID: [0; 32] };
    ssid.ucSSID[..net.ssid.len().min(32)].copy_from_slice(&net.ssid.as_bytes()[..net.ssid.len().min(32)]);
    let params = if net.profile.is_some() {
        WLAN_CONNECTION_PARAMETERS {
            wlanConnectionMode: wlan_connection_mode_profile,
            strProfile: PCWSTR(profile.as_ptr()),
            pDot11Ssid: std::ptr::null_mut(),
            pDesiredBssidList: std::ptr::null_mut(),
            dot11BssType: dot11_BSS_type_any,
            dwFlags: 0,
        }
    } else if !net.secure {
        // An open network Windows hasn't joined before.
        WLAN_CONNECTION_PARAMETERS {
            wlanConnectionMode: wlan_connection_mode_discovery_unsecure,
            strProfile: PCWSTR::null(),
            pDot11Ssid: &mut ssid,
            pDesiredBssidList: std::ptr::null_mut(),
            dot11BssType: dot11_BSS_type_infrastructure,
            dwFlags: 0,
        }
    } else {
        return Err(None);
    };
    let r = unsafe { WlanConnect(c.0, guid, &params, None) };
    if r == 0 {
        Ok(())
    } else {
        Err(Some(format!("Couldn't join {} (error {r}).", net.ssid)))
    }
}

fn run(rx: Receiver<Cmd>) {
    // A request that came in during a scan, done next rather than lost.
    let mut pending: Option<Cmd> = None;
    loop {
        let cmd = match pending.take() {
            Some(c) => c,
            None => match rx.recv() {
                Ok(c) => c,
                Err(_) => return,
            },
        };
        let Some(c) = open() else {
            post(Report { missing: true, ..Default::default() });
            continue;
        };
        let ifaces = interfaces(&c);
        if ifaces.is_empty() {
            post(Report { missing: true, ..Default::default() });
            continue;
        }
        let note = match cmd {
            Cmd::Scan => None,
            Cmd::Connect(net) => match connect(&c, &ifaces[0], &net) {
                Ok(()) => Some(format!("Joining {}\u{2026}", net.ssid)),
                Err(Some(why)) => Some(why),
                Err(None) => {
                    // Windows asks for the password in its own list.
                    crate::shell::open("ms-availablenetworks:");
                    Some(format!("Enter the password for {} in Windows' list.", net.ssid))
                }
            },
        };
        // What's already known straight away, then again after a fresh scan.
        post(Report { nets: all_networks(&c, &ifaces), scanning: true, missing: false, note: note.clone() });
        for g in &ifaces {
            unsafe {
                WlanScan(c.0, g, None, None, None);
            }
        }
        std::thread::sleep(SCAN_WAIT);
        while let Ok(next) = rx.try_recv() {
            // A pick outranks a plain refresh.
            if pending.is_none() || matches!(next, Cmd::Connect(_)) {
                pending = Some(next);
            }
        }
        post(Report { nets: all_networks(&c, &ifaces), scanning: false, missing: false, note });
    }
}

/// Signal strength as bars: 1 to 4.
pub fn bars(signal: u32) -> u32 {
    match signal {
        0..=24 => 1,
        25..=49 => 2,
        50..=74 => 3,
        _ => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_bars() {
        assert_eq!(bars(0), 1);
        assert_eq!(bars(30), 2);
        assert_eq!(bars(74), 3);
        assert_eq!(bars(100), 4);
    }
}
