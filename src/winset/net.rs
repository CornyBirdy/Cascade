//! Network & Bluetooth: Wi-Fi and Bluetooth on or off, airplane mode,
//! the mobile hotspot, joining a network that needs a password, and what
//! you're connected through. As the Network & internet and Bluetooth &
//! devices pages of Windows' Settings, through the same radio, tethering and
//! WLAN APIs. Pairing a new device is Windows' own Add a device wizard (a
//! classic program, so it opens without Explorer).
//!
//! Reading radios and the hotspot takes a moment, so it's done on a thread
//! and kept: the page shows what was last read and is told when it changes.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows::core::{GUID, HSTRING, PCWSTR};
use windows::Devices::Radios::{Radio, RadioKind as WinRadioKind, RadioState};
use windows::Networking::Connectivity::NetworkInformation;
use windows::Networking::HostNameType;
use windows::Networking::NetworkOperators::{NetworkOperatorTetheringManager, TetheringOperationalState};
use windows::Win32::Foundation::{HANDLE, HWND};
use windows::Win32::NetworkManagement::WiFi::*;

use crate::{config, msg};

#[derive(Clone, Copy, PartialEq)]
pub enum RadioKind {
    WiFi,
    Bluetooth,
}

#[derive(Clone, Default)]
struct State {
    read_at: Option<Instant>,
    reading: bool,
    wifi: Option<bool>,
    bt: Option<bool>,
    /// The hotspot: (on, network name, password); None if this PC can't share its connection.
    hotspot: Option<(bool, String, String)>,
    info: String,
    /// Airplane mode was turned on here, and which radios were on before it.
    airplane: Option<(bool, bool)>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    f(STATE.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(State::default))
}

/// What was last read, reading again in the background if it's a few seconds old.
fn state() -> State {
    let (s, stale) = with(|s| {
        let stale = !s.reading && s.read_at.is_none_or(|t| t.elapsed() > Duration::from_secs(4));
        if stale {
            s.reading = true;
        }
        (s.clone(), stale)
    });
    if stale {
        std::thread::spawn(read);
    }
    s
}

fn radios() -> Vec<Radio> {
    let get = || -> windows::core::Result<Vec<Radio>> {
        let _ = Radio::RequestAccessAsync()?.get();
        let list = Radio::GetRadiosAsync()?.get()?;
        Ok((0..list.Size()?).filter_map(|i| list.GetAt(i).ok()).collect())
    };
    get().unwrap_or_default()
}

fn kind(r: &Radio) -> Option<RadioKind> {
    match r.Kind().ok()? {
        WinRadioKind::WiFi => Some(RadioKind::WiFi),
        WinRadioKind::Bluetooth => Some(RadioKind::Bluetooth),
        _ => None,
    }
}

fn tethering() -> Option<NetworkOperatorTetheringManager> {
    let profile = NetworkInformation::GetInternetConnectionProfile().ok()?;
    NetworkOperatorTetheringManager::CreateFromConnectionProfile(&profile).ok()
}

/// Reads everything (on a thread of its own).
fn read() {
    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
    }
    let mut wifi = None;
    let mut bt = None;
    for r in radios() {
        let on = r.State().is_ok_and(|s| s == RadioState::On);
        match kind(&r) {
            Some(RadioKind::WiFi) => wifi = Some(on || wifi == Some(true)),
            Some(RadioKind::Bluetooth) => bt = Some(on || bt == Some(true)),
            None => {}
        }
    }
    let hotspot = tethering().and_then(|t| {
        let on = t.TetheringOperationalState().ok()? == TetheringOperationalState::On;
        let ap = t.GetCurrentAccessPointConfiguration().ok()?;
        Some((on, ap.Ssid().ok()?.to_string(), ap.Passphrase().ok()?.to_string()))
    });
    let info = connection_info();
    with(|s| {
        let differs = s.wifi != wifi || s.bt != bt || s.hotspot != hotspot || s.info != info;
        s.wifi = wifi;
        s.bt = bt;
        s.hotspot = hotspot;
        s.info = info;
        s.read_at = Some(Instant::now());
        s.reading = false;
        if differs {
            super::changed();
        }
    });
}

/// What you're connected through, and your address on it.
fn connection_info() -> String {
    let get = || -> windows::core::Result<String> {
        let Ok(profile) = NetworkInformation::GetInternetConnectionProfile() else {
            return Ok("Not connected to the internet.".into());
        };
        let name = profile.ProfileName()?.to_string();
        let adapter = profile.NetworkAdapter()?.NetworkAdapterId()?;
        let kind = if profile.IsWlanConnectionProfile()? { "Wi-Fi" } else { "Wired" };
        let mut address = String::new();
        let hosts = NetworkInformation::GetHostNames()?;
        for i in 0..hosts.Size()? {
            let h = hosts.GetAt(i)?;
            if h.Type()? != HostNameType::Ipv4 {
                continue;
            }
            if h.IPInformation()
                .and_then(|ip| ip.NetworkAdapter())
                .and_then(|a| a.NetworkAdapterId())
                .is_ok_and(|id| id == adapter)
            {
                address = h.CanonicalName()?.to_string();
            }
        }
        Ok(if address.is_empty() { format!("{kind}: {name}.") } else { format!("{kind}: {name}, address {address}.") })
    };
    get().unwrap_or_else(|_| "Not connected to the internet.".into())
}

pub fn radio(k: RadioKind) -> Option<bool> {
    let s = state();
    match k {
        RadioKind::WiFi => s.wifi,
        RadioKind::Bluetooth => s.bt,
    }
}

pub fn set_radio(k: RadioKind, on: bool) {
    std::thread::spawn(move || {
        let target = if on { RadioState::On } else { RadioState::Off };
        for r in radios().into_iter().filter(|r| kind(r) == Some(k)) {
            let _ = r.SetStateAsync(target).and_then(|op| op.get());
        }
        read();
    });
}

/// Airplane mode: every radio off (and back as they were).
pub fn airplane() -> bool {
    let s = state();
    s.airplane.is_some() && s.wifi != Some(true) && s.bt != Some(true)
}

pub fn set_airplane(on: bool) {
    let before = with(|s| {
        let was = (s.wifi == Some(true), s.bt == Some(true));
        if on {
            s.airplane = Some(was);
            None
        } else {
            s.airplane.take()
        }
    });
    std::thread::spawn(move || {
        for r in radios() {
            let back_on = match kind(&r) {
                Some(RadioKind::WiFi) => before.is_none_or(|b| b.0),
                Some(RadioKind::Bluetooth) => before.is_none_or(|b| b.1),
                None => continue,
            };
            let target = if !on && back_on { RadioState::On } else { RadioState::Off };
            let _ = r.SetStateAsync(target).and_then(|op| op.get());
        }
        config::log(&format!("network: airplane mode {}", if on { "on" } else { "off" }));
        read();
    });
}

// -------------------------------------------------------------- hotspot --

pub fn hotspot_available() -> bool {
    state().hotspot.is_some()
}

pub fn hotspot_on() -> bool {
    state().hotspot.is_some_and(|h| h.0)
}

pub fn hotspot_desc() -> String {
    match state().hotspot {
        Some((_, ssid, pass)) => {
            format!("Share this PC's internet over Wi-Fi as \u{201c}{ssid}\u{201d}, password {pass}.")
        }
        None => "This PC's connection can't be shared.".into(),
    }
}

pub fn set_hotspot(on: bool) {
    std::thread::spawn(move || {
        unsafe {
            let _ =
                windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
        }
        let result = tethering().map(|t| {
            let op = if on { t.StartTetheringAsync() } else { t.StopTetheringAsync() };
            op.and_then(|o| o.get()).and_then(|r| r.Status()).map(|s| s.0)
        });
        config::log(&format!("network: hotspot {} ({result:?})", if on { "on" } else { "off" }));
        if !matches!(result, Some(Ok(0))) && on {
            msg::note("The hotspot didn't start (Windows said no)");
        }
        read();
    });
}

// ------------------------------------------------------------- joining --

/// The network picked to join, its security, and the password typed.
static JOIN: Mutex<(String, Option<DOT11_AUTH_ALGORITHM>, String)> = Mutex::new((String::new(), None, String::new()));

fn join_state() -> std::sync::MutexGuard<'static, (String, Option<DOT11_AUTH_ALGORITHM>, String)> {
    JOIN.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn join_ssid() -> String {
    let s = join_state().0.clone();
    if s.is_empty() {
        "Choose\u{2026}".into()
    } else {
        s
    }
}

pub fn set_join_password(p: &str) {
    join_state().2 = p.to_string();
}

/// The first wireless interface and the WLAN handle (closed by the caller).
unsafe fn wlan() -> Option<(HANDLE, GUID)> {
    let mut version = 0u32;
    let mut h = HANDLE::default();
    if WlanOpenHandle(2, None, &mut version, &mut h) != 0 {
        return None;
    }
    let mut list: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
    if WlanEnumInterfaces(h, None, &mut list) != 0 || list.is_null() || (*list).dwNumberOfItems == 0 {
        WlanCloseHandle(h, None);
        return None;
    }
    let guid = (*list).InterfaceInfo[0].InterfaceGuid;
    WlanFreeMemory(list as *const _);
    Some((h, guid))
}

/// The networks in range: (name, security, signal).
fn in_range() -> Vec<(String, DOT11_AUTH_ALGORITHM, u32)> {
    let mut out: Vec<(String, DOT11_AUTH_ALGORITHM, u32)> = Vec::new();
    unsafe {
        let Some((h, guid)) = wlan() else { return out };
        let mut list: *mut WLAN_AVAILABLE_NETWORK_LIST = std::ptr::null_mut();
        if WlanGetAvailableNetworkList(h, &guid, 0, None, &mut list) == 0 && !list.is_null() {
            let n = (*list).dwNumberOfItems as usize;
            let items = std::slice::from_raw_parts((*list).Network.as_ptr(), n);
            for net in items {
                let len = (net.dot11Ssid.uSSIDLength as usize).min(32);
                let ssid = String::from_utf8_lossy(&net.dot11Ssid.ucSSID[..len]).into_owned();
                if ssid.is_empty() || out.iter().any(|o| o.0 == ssid) {
                    continue;
                }
                out.push((ssid, net.dot11DefaultAuthAlgorithm, net.wlanSignalQuality));
            }
            WlanFreeMemory(list as *const _);
        }
        WlanCloseHandle(h, None);
    }
    out.sort_by_key(|n| std::cmp::Reverse(n.2));
    out
}

pub fn pick_ssid(owner: HWND) {
    let nets = in_range();
    if nets.is_empty() {
        super::tell("No Wi-Fi networks found (is Wi-Fi on?).".into());
        return;
    }
    let names: Vec<String> = nets
        .iter()
        .map(|(s, auth, q)| {
            let lock = if *auth == DOT11_AUTH_ALGO_80211_OPEN { "" } else { " \u{1F512}" };
            format!("{s}{lock}  ({q}%)")
        })
        .collect();
    if let Some(i) = super::pick(owner, &names, None) {
        let mut j = join_state();
        j.0 = nets[i].0.clone();
        j.1 = Some(nets[i].1);
        drop(j);
        super::changed();
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

/// A Wi-Fi profile for joining `ssid`, as Windows writes one.
fn profile_xml(ssid: &str, auth: DOT11_AUTH_ALGORITHM, password: &str) -> String {
    let ssid = xml_escape(ssid);
    let security = if auth == DOT11_AUTH_ALGO_80211_OPEN {
        "<authEncryption><authentication>open</authentication><encryption>none</encryption><useOneX>false</useOneX></authEncryption>".to_string()
    } else {
        let method = match auth {
            DOT11_AUTH_ALGO_WPA3_SAE => "WPA3SAE",
            DOT11_AUTH_ALGO_WPA_PSK => "WPAPSK",
            _ => "WPA2PSK",
        };
        format!(
            "<authEncryption><authentication>{method}</authentication><encryption>AES</encryption><useOneX>false</useOneX></authEncryption>\
             <sharedKey><keyType>passPhrase</keyType><protected>false</protected><keyMaterial>{}</keyMaterial></sharedKey>",
            xml_escape(password)
        )
    };
    format!(
        "<?xml version=\"1.0\"?><WLANProfile xmlns=\"http://www.microsoft.com/networking/WLAN/profile/v1\">\
         <name>{ssid}</name><SSIDConfig><SSID><name>{ssid}</name></SSID></SSIDConfig>\
         <connectionType>ESS</connectionType><connectionMode>auto</connectionMode>\
         <MSM><security>{security}</security></MSM></WLANProfile>"
    )
}

/// Joins the network picked, with the password typed (saved, as Windows does, to join it again later).
pub fn join() {
    let (ssid, auth, password) = join_state().clone();
    let Some(auth) = auth else {
        super::tell("Choose a network first.".into());
        return;
    };
    if auth != DOT11_AUTH_ALGO_80211_OPEN && password.len() < 8 {
        super::tell("A Wi-Fi password is at least 8 characters.".into());
        return;
    }
    std::thread::spawn(move || unsafe {
        let Some((h, guid)) = wlan() else { return };
        let xml = HSTRING::from(profile_xml(&ssid, auth, &password));
        let mut reason = 0u32;
        let set = WlanSetProfile(h, &guid, 0, &xml, PCWSTR::null(), true, None, &mut reason);
        let name = HSTRING::from(ssid.as_str());
        let params = WLAN_CONNECTION_PARAMETERS {
            wlanConnectionMode: wlan_connection_mode_profile,
            strProfile: PCWSTR(name.as_ptr()),
            pDot11Ssid: std::ptr::null_mut(),
            pDesiredBssidList: std::ptr::null_mut(),
            dot11BssType: dot11_BSS_type_any,
            dwFlags: 0,
        };
        let joined = if set == 0 { WlanConnect(h, &guid, &params, None) } else { set };
        WlanCloseHandle(h, None);
        config::log(&format!("network: joining {ssid} (profile {set}, reason {reason}, connect {joined})"));
        msg::note(if joined == 0 { format!("Joining {ssid}\u{2026}") } else { format!("Couldn't join {ssid}") });
        std::thread::sleep(Duration::from_secs(5));
        read();
    });
}

pub fn info() -> String {
    let i = state().info;
    if i.is_empty() {
        "Reading\u{2026}".into()
    } else {
        i
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_a_wifi_profile() {
        let x = profile_xml("Home <5G>", DOT11_AUTH_ALGO_RSNA_PSK, "p&ss\"word");
        assert!(x.contains("<name>Home &lt;5G&gt;</name>"));
        assert!(x.contains("<authentication>WPA2PSK</authentication>"));
        assert!(x.contains("<keyMaterial>p&amp;ss&quot;word</keyMaterial>"));
        let open = profile_xml("Cafe", DOT11_AUTH_ALGO_80211_OPEN, "");
        assert!(open.contains("<authentication>open</authentication>") && !open.contains("sharedKey"));
        assert!(profile_xml("x", DOT11_AUTH_ALGO_WPA3_SAE, "12345678").contains("WPA3SAE"));
    }
}
