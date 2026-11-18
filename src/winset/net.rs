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
use windows::Networking::NetworkOperators::{
    NetworkOperatorTetheringAccessPointConfiguration, NetworkOperatorTetheringManager, TetheringOperationalState,
    TetheringWiFiAuthenticationKind, TetheringWiFiBand,
};
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
    /// The hotspot; None if this PC can't share its connection.
    hotspot: Option<Hotspot>,
    info: String,
    /// Airplane mode was turned on here, and which radios were on before it.
    airplane: Option<(bool, bool)>,
}

/// The mobile hotspot as Windows has it set up.
#[derive(Clone, Default, PartialEq)]
struct Hotspot {
    on: bool,
    ssid: String,
    pass: String,
    band: i32,
    /// The bands and kinds of security this PC's Wi-Fi can do.
    bands: Vec<i32>,
    auth: i32,
    auths: Vec<i32>,
    /// The devices connected (a name, or their address), and how many may be.
    clients: Vec<String>,
    max: u32,
    /// The connection it shares.
    from: String,
    /// Turns itself off when nothing has been connected for a while.
    saver: Option<bool>,
}

const BANDS: [(TetheringWiFiBand, &str); 4] = [
    (TetheringWiFiBand::Auto, "Any available"),
    (TetheringWiFiBand::TwoPointFourGigahertz, "2.4 GHz"),
    (TetheringWiFiBand::FiveGigahertz, "5 GHz"),
    (TetheringWiFiBand::SixGigahertz, "6 GHz"),
];
const AUTHS: [(TetheringWiFiAuthenticationKind, &str); 3] = [
    (TetheringWiFiAuthenticationKind::Wpa2, "WPA2-Personal"),
    (TetheringWiFiAuthenticationKind::Wpa3TransitionMode, "WPA3-Personal transition"),
    (TetheringWiFiAuthenticationKind::Wpa3, "WPA3-Personal"),
];

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

/// The connection the hotspot shares: the one picked (Share from), or the one to the internet.
fn tethering() -> Option<NetworkOperatorTetheringManager> {
    let picked = share_from();
    let profile = shareable()
        .into_iter()
        .find(|(name, _)| picked.as_deref() == Some(name.as_str()))
        .map(|(_, p)| p)
        .or_else(|| NetworkInformation::GetInternetConnectionProfile().ok())?;
    NetworkOperatorTetheringManager::CreateFromConnectionProfile(&profile).ok()
}

/// Connections that reach the internet: (name, profile).
fn shareable() -> Vec<(String, windows::Networking::Connectivity::ConnectionProfile)> {
    use windows::Networking::Connectivity::NetworkConnectivityLevel;
    let Ok(list) = NetworkInformation::GetConnectionProfiles() else { return Vec::new() };
    let mut out = Vec::new();
    for i in 0..list.Size().unwrap_or(0) {
        let Ok(p) = list.GetAt(i) else { continue };
        if p.GetNetworkConnectivityLevel().ok() != Some(NetworkConnectivityLevel::InternetAccess) {
            continue;
        }
        if let Ok(name) = p.ProfileName() {
            out.push((name.to_string(), p));
        }
    }
    out
}

/// The connection picked to share (kept in Cascade's data folder), if one was.
fn share_from() -> Option<String> {
    let t = std::fs::read_to_string(config::dir().join("hotspot_from")).ok()?;
    let t = t.trim();
    (!t.is_empty()).then(|| t.to_string())
}

fn read_hotspot() -> Option<Hotspot> {
    let t = tethering()?;
    let on = t.TetheringOperationalState().ok()? == TetheringOperationalState::On;
    let ap = t.GetCurrentAccessPointConfiguration().ok()?;
    let clients = if on {
        t.GetTetheringClients()
            .ok()
            .map(|list| {
                (0..list.Size().unwrap_or(0))
                    .filter_map(|i| list.GetAt(i).ok())
                    .map(|c| {
                        let mac = c.MacAddress().map(|m| m.to_string()).unwrap_or_default();
                        let name = c
                            .HostNames()
                            .ok()
                            .and_then(|h| h.GetAt(0).ok())
                            .and_then(|h| h.DisplayName().ok())
                            .map(|n| n.to_string())
                            .filter(|n| !n.is_empty());
                        name.map_or(mac.clone(), |n| format!("{n} ({mac})"))
                    })
                    .collect()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let from = share_from()
        .or_else(|| NetworkInformation::GetInternetConnectionProfile().ok()?.ProfileName().ok().map(|n| n.to_string()))
        .unwrap_or_default();
    Some(Hotspot {
        on,
        ssid: ap.Ssid().ok()?.to_string(),
        pass: ap.Passphrase().ok()?.to_string(),
        band: ap.Band().map_or(0, |b| b.0),
        bands: BANDS
            .iter()
            .filter(|(b, _)| ap.IsBandSupported(*b).unwrap_or(*b == TetheringWiFiBand::Auto))
            .map(|(b, _)| b.0)
            .collect(),
        auth: ap.AuthenticationKind().map_or(0, |a| a.0),
        auths: AUTHS
            .iter()
            .filter(|(a, _)| {
                ap.IsAuthenticationKindSupported(*a).unwrap_or(*a == TetheringWiFiAuthenticationKind::Wpa2)
            })
            .map(|(a, _)| a.0)
            .collect(),
        clients,
        max: t.MaxClientCount().unwrap_or(0),
        from,
        saver: NetworkOperatorTetheringManager::IsNoConnectionsTimeoutEnabled().ok(),
    })
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
    let hotspot = read_hotspot();
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
    state().hotspot.is_some_and(|h| h.on)
}

pub fn hotspot_desc() -> String {
    match state().hotspot {
        Some(h) if h.on => format!(
            "Sharing {} as \u{201c}{}\u{201d}: {} of {} devices connected.",
            if h.from.is_empty() { "this PC's internet".to_string() } else { h.from },
            h.ssid,
            h.clients.len(),
            h.max
        ),
        Some(h) => format!("Share this PC's internet over Wi-Fi as \u{201c}{}\u{201d}.", h.ssid),
        None => "This PC's connection can't be shared.".into(),
    }
}

/// The devices on the hotspot, one a line.
pub fn hotspot_devices() -> String {
    match state().hotspot {
        Some(h) if h.on && !h.clients.is_empty() => h.clients.join("\n"),
        Some(h) if h.on => "No devices connected yet.".into(),
        _ => "The hotspot is off.".into(),
    }
}

/// A hotspot setting's value, as the page shows it.
pub fn hotspot_value(key: &str) -> String {
    let Some(h) = state().hotspot else { return String::new() };
    match key {
        "win_hotspot_name" => h.ssid,
        "win_hotspot_pw" => h.pass,
        "win_hotspot_band" => BANDS.iter().find(|(b, _)| b.0 == h.band).map_or("", |(_, n)| n).to_string(),
        "win_hotspot_security" => AUTHS.iter().find(|(a, _)| a.0 == h.auth).map_or("", |(_, n)| n).to_string(),
        "win_hotspot_from" => h.from,
        "win_hotspot_saver" => h.saver.unwrap_or(false).to_string(),
        _ => String::new(),
    }
}

/// Whether a hotspot row applies here (a choice of band or security only where there is one).
pub fn hotspot_row_shown(key: &str) -> bool {
    let Some(h) = state().hotspot else { return false };
    match key {
        "win_hotspot_band" => h.bands.len() > 1,
        "win_hotspot_security" => h.auths.len() > 1,
        "win_hotspot_saver" => h.saver.is_some(),
        "win_hotspot_devices" => h.on,
        _ => true,
    }
}

/// Changes the hotspot's set-up as `edit` says, on a thread of its own. If
/// it's on, it's restarted so devices see the change, as Windows does.
fn configure(
    what: &'static str,
    edit: impl FnOnce(&NetworkOperatorTetheringAccessPointConfiguration) -> windows::core::Result<()> + Send + 'static,
) {
    std::thread::spawn(move || {
        unsafe {
            let _ =
                windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
        }
        let result = (|| -> windows::core::Result<()> {
            let t = tethering().ok_or_else(|| windows::core::Error::from_hresult(windows::core::HRESULT(-1)))?;
            let ap = t.GetCurrentAccessPointConfiguration()?;
            edit(&ap)?;
            t.ConfigureAccessPointAsync(&ap)?.get()?;
            if t.TetheringOperationalState()? == TetheringOperationalState::On {
                let _ = t.StopTetheringAsync()?.get();
                let _ = t.StartTetheringAsync()?.get();
            }
            Ok(())
        })();
        config::log(&format!("network: hotspot {what} ({result:?})"));
        if result.is_err() {
            msg::note(format!("Windows wouldn't change the hotspot's {what}"));
        }
        with(|s| s.read_at = None);
        read();
    });
}

pub fn set_hotspot_name(v: &str) {
    let v = v.trim().to_string();
    if v.is_empty() || v.len() > 32 {
        super::tell("A hotspot's name needs 1 to 32 characters.".into());
        super::changed();
        return;
    }
    configure("name", move |ap| ap.SetSsid(&HSTRING::from(v.as_str())));
}

pub fn set_hotspot_password(v: &str) {
    let v = v.to_string();
    if v.len() < 8 || v.len() > 63 {
        super::tell("A hotspot's password needs 8 to 63 characters.".into());
        super::changed();
        return;
    }
    configure("password", move |ap| ap.SetPassphrase(&HSTRING::from(v.as_str())));
}

pub fn set_hotspot_saver(on: bool) {
    std::thread::spawn(move || {
        let r = if on {
            NetworkOperatorTetheringManager::EnableNoConnectionsTimeout()
        } else {
            NetworkOperatorTetheringManager::DisableNoConnectionsTimeout()
        };
        config::log(&format!("network: hotspot power saving {on} ({r:?})"));
        with(|s| s.read_at = None);
        read();
    });
}

/// A hotspot picker: the band, the security, or which connection to share.
pub fn pick_hotspot(key: &str, owner: HWND) {
    let Some(h) = state().hotspot else { return };
    match key {
        "win_hotspot_band" => {
            let list: Vec<&(TetheringWiFiBand, &str)> = BANDS.iter().filter(|(b, _)| h.bands.contains(&b.0)).collect();
            let names: Vec<String> = list.iter().map(|(_, n)| n.to_string()).collect();
            if let Some(i) = super::pick(owner, &names, list.iter().position(|(b, _)| b.0 == h.band)) {
                let band = list[i].0;
                configure("band", move |ap| ap.SetBand(band));
            }
        }
        "win_hotspot_security" => {
            let list: Vec<&(TetheringWiFiAuthenticationKind, &str)> =
                AUTHS.iter().filter(|(a, _)| h.auths.contains(&a.0)).collect();
            let names: Vec<String> = list.iter().map(|(_, n)| n.to_string()).collect();
            if let Some(i) = super::pick(owner, &names, list.iter().position(|(a, _)| a.0 == h.auth)) {
                let auth = list[i].0;
                configure("security", move |ap| ap.SetAuthenticationKind(auth));
            }
        }
        "win_hotspot_from" => {
            let names: Vec<String> = shareable().into_iter().map(|(n, _)| n).collect();
            if names.is_empty() {
                return;
            }
            if let Some(i) = super::pick(owner, &names, names.iter().position(|n| *n == h.from)) {
                let was_on = h.on;
                let _ = std::fs::write(config::dir().join("hotspot_from"), &names[i]);
                config::log(&format!("network: hotspot shares {}", names[i]));
                with(|s| s.read_at = None);
                // Sharing another connection: start again on it.
                if was_on {
                    set_hotspot(false);
                    std::thread::spawn(|| {
                        std::thread::sleep(Duration::from_secs(2));
                        set_hotspot(true);
                    });
                }
                super::changed();
            }
        }
        _ => {}
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

/// The Wi-Fi networks Windows remembers.
fn known() -> Vec<String> {
    let mut out = Vec::new();
    unsafe {
        let Some((h, guid)) = wlan() else { return out };
        let mut list: *mut WLAN_PROFILE_INFO_LIST = std::ptr::null_mut();
        if WlanGetProfileList(h, &guid, None, &mut list) == 0 && !list.is_null() {
            let n = (*list).dwNumberOfItems as usize;
            let items = std::slice::from_raw_parts((*list).ProfileInfo.as_ptr(), n);
            for p in items {
                let name = &p.strProfileName;
                let len = name.iter().position(|&c| c == 0).unwrap_or(name.len());
                out.push(String::from_utf16_lossy(&name[..len]));
            }
            WlanFreeMemory(list as *const _);
        }
        WlanCloseHandle(h, None);
    }
    out
}

/// Known networks: pick one to forget (Windows asks for its password again next time).
pub fn pick_forget(owner: HWND) {
    let nets = known();
    if nets.is_empty() {
        super::tell("Windows doesn't remember any Wi-Fi networks.".into());
        return;
    }
    let Some(i) = super::pick(owner, &nets, None) else { return };
    let name = &nets[i];
    if !super::confirm(owner, &format!("Forget \u{201c}{name}\u{201d}? Its password goes with it.")) {
        return;
    }
    unsafe {
        let Some((h, guid)) = wlan() else { return };
        let r = WlanDeleteProfile(h, &guid, &HSTRING::from(name.as_str()), None);
        WlanCloseHandle(h, None);
        config::log(&format!("network: forgot {name} (result {r})"));
    }
    super::changed();
}

/// How many networks Windows remembers, for the row's description.
pub fn known_desc() -> String {
    // The page asks on every frame: read at most every few seconds.
    static COUNT: Mutex<Option<(Instant, usize)>> = Mutex::new(None);
    let n = {
        let mut c = COUNT.lock().unwrap_or_else(|e| e.into_inner());
        match *c {
            Some((t, n)) if t.elapsed() < Duration::from_secs(3) => n,
            _ => {
                let n = known().len();
                *c = Some((Instant::now(), n));
                n
            }
        }
    };
    match n {
        0 => "Windows doesn't remember any yet.".into(),
        1 => "Windows remembers 1 network. Pick it to forget it.".into(),
        n => format!("Windows remembers {n} networks. Pick one to forget it."),
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
