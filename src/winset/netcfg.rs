//! Network & internet, in depth: each adapter's details (addresses, gateway,
//! DNS, MAC, speed), its IP and DNS settings (automatic, or set by hand,
//! with DNS over HTTPS), the Wi-Fi connection's properties, metered
//! connection, the proxy, and network reset. As the adapter pages of
//! Windows' Settings > Network & internet.
//!
//! Changing an adapter's addresses needs administrator rights (Windows'
//! Settings has a service that holds them; Cascade asks, once per change,
//! and runs Windows' own `netsh`). Reading needs nothing.

use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::Mutex;
use std::time::Duration;

use windows::core::{HSTRING, PWSTR};
use windows::Networking::Connectivity::{NetworkCostType, NetworkInformation};
use windows::Win32::Foundation::{HANDLE, HWND};
use windows::Win32::NetworkManagement::IpHelper::*;
use windows::Win32::NetworkManagement::WiFi::*;
use windows::Win32::Networking::WinInet::*;
use windows::Win32::Networking::WinSock::SOCKET_ADDRESS;

use super::{changed, confirm, pick, run_admin, run_hidden, tell};
use crate::config;

// ------------------------------------------------------------- adapters --

#[derive(Clone, Default, PartialEq)]
struct Adapter {
    /// Its GUID ("{...}"): how Windows names it in the registry.
    guid: String,
    /// The name netsh and Windows' adapter list use ("Wi-Fi", "Ethernet").
    alias: String,
    description: String,
    wifi: bool,
    up: bool,
    mac: String,
    /// Bits a second.
    speed: u64,
    dhcp: bool,
    ipv4: Vec<(Ipv4Addr, u8)>,
    ipv6: Vec<Ipv6Addr>,
    gateways: Vec<String>,
    dns: Vec<String>,
    /// DNS servers set by hand (the registry's NameServer), not from DHCP.
    dns_manual: bool,
}

fn sockaddr(a: &SOCKET_ADDRESS) -> Option<std::net::IpAddr> {
    if a.lpSockaddr.is_null() {
        return None;
    }
    unsafe {
        let family = (*a.lpSockaddr).sa_family.0;
        let bytes = std::slice::from_raw_parts(a.lpSockaddr as *const u8, a.iSockaddrLength.max(0) as usize);
        match family {
            2 if bytes.len() >= 8 => Some(Ipv4Addr::new(bytes[4], bytes[5], bytes[6], bytes[7]).into()),
            23 if bytes.len() >= 24 => {
                let mut b = [0u8; 16];
                b.copy_from_slice(&bytes[8..24]);
                Some(Ipv6Addr::from(b).into())
            }
            _ => None,
        }
    }
}

fn read_adapters() -> Vec<Adapter> {
    let mut out = Vec::new();
    unsafe {
        let flags = GAA_FLAG_INCLUDE_GATEWAYS | GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST;
        let mut size = 32 * 1024u32;
        let mut buf: Vec<u64> = Vec::new();
        for _ in 0..3 {
            buf = vec![0u64; size as usize / 8 + 1];
            let r =
                GetAdaptersAddresses(0, flags, None, Some(buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH), &mut size);
            if r == 0 {
                break;
            }
            if r != 111 {
                // Not "buffer too small": nothing to read.
                return out;
            }
        }
        let mut p = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
        while !p.is_null() {
            let a = &*p;
            p = a.Next;
            // Ethernet (6) and Wi-Fi (71); not loopback, tunnels or the like.
            if a.IfType != 6 && a.IfType != 71 {
                continue;
            }
            let guid = a.AdapterName.to_string().unwrap_or_default();
            let mac = a.PhysicalAddress[..(a.PhysicalAddressLength as usize).min(8)]
                .iter()
                .map(|b| format!("{b:02X}"))
                .collect::<Vec<_>>()
                .join("-");
            let mut ad = Adapter {
                alias: a.FriendlyName.to_string().unwrap_or_default(),
                description: a.Description.to_string().unwrap_or_default(),
                wifi: a.IfType == 71,
                up: a.OperStatus.0 == 1,
                mac,
                speed: a.TransmitLinkSpeed.min(a.ReceiveLinkSpeed.max(a.TransmitLinkSpeed)),
                dhcp: a.Anonymous2.Flags & 0x4 != 0,
                ..Default::default()
            };
            let mut u = a.FirstUnicastAddress;
            while !u.is_null() {
                match sockaddr(&(*u).Address) {
                    Some(std::net::IpAddr::V4(v4)) => ad.ipv4.push((v4, (*u).OnLinkPrefixLength)),
                    Some(std::net::IpAddr::V6(v6)) if (v6.segments()[0] & 0xffc0) != 0xfe80 => ad.ipv6.push(v6),
                    _ => {}
                }
                u = (*u).Next;
            }
            let mut g = a.FirstGatewayAddress;
            while !g.is_null() {
                if let Some(ip) = sockaddr(&(*g).Address) {
                    ad.gateways.push(ip.to_string());
                }
                g = (*g).Next;
            }
            let mut d = a.FirstDnsServerAddress;
            while !d.is_null() {
                if let Some(ip) = sockaddr(&(*d).Address) {
                    // fec0:0:0:ffff::1 and the like: Windows' placeholders when there's no DNS.
                    if !ip.to_string().starts_with("fec0:") {
                        ad.dns.push(ip.to_string());
                    }
                }
                d = (*d).Next;
            }
            let key = format!("SYSTEM\\CurrentControlSet\\Services\\Tcpip\\Parameters\\Interfaces\\{guid}");
            ad.dns_manual = super::reg_text(windows::Win32::System::Registry::HKEY_LOCAL_MACHINE, &key, "NameServer")
                .is_some_and(|v| !v.trim().is_empty());
            ad.guid = guid;
            out.push(ad);
        }
    }
    // Connected ones first.
    out.sort_by_key(|a| (!a.up, a.gateways.is_empty(), a.alias.clone()));
    out
}

/// The adapters, read in the background every few seconds (the page asks every frame).
fn adapters() -> Vec<Adapter> {
    ADAPTERS.get(Duration::from_secs(3), read_adapters).unwrap_or_default()
}

static ADAPTERS: super::Slow<Vec<Adapter>> = super::Slow::new();

/// The adapter the page is on (its GUID), and what's been typed for it.
#[derive(Default)]
struct Form {
    guid: Option<String>,
    ip_manual: Option<bool>,
    ip: Option<String>,
    mask: Option<String>,
    gateway: Option<String>,
    dns_manual: Option<bool>,
    dns1: Option<String>,
    dns2: Option<String>,
    doh: Option<bool>,
}

static FORM: Mutex<Form> = Mutex::new(Form {
    guid: None,
    ip_manual: None,
    ip: None,
    mask: None,
    gateway: None,
    dns_manual: None,
    dns1: None,
    dns2: None,
    doh: None,
});

fn form() -> std::sync::MutexGuard<'static, Form> {
    FORM.lock().unwrap_or_else(|e| e.into_inner())
}

fn chosen() -> Option<Adapter> {
    let list = adapters();
    let want = form().guid.clone();
    list.iter().find(|a| Some(&a.guid) == want.as_ref()).or(list.first()).cloned()
}

fn mask_of(prefix: u8) -> Ipv4Addr {
    let bits = if prefix == 0 { 0 } else { u32::MAX << (32 - prefix.min(32) as u32) };
    Ipv4Addr::from(bits)
}

/// A subnet mask as typed: dotted (255.255.255.0) or a prefix length (24 or /24).
fn parse_mask(s: &str) -> Option<Ipv4Addr> {
    let s = s.trim().trim_start_matches('/');
    if let Ok(n) = s.parse::<u8>() {
        return (n <= 32).then(|| mask_of(n));
    }
    let m: Ipv4Addr = s.parse().ok()?;
    let bits = u32::from(m);
    // A real mask: ones, then zeros.
    (bits.leading_ones() + bits.trailing_zeros() == 32).then_some(m)
}

fn speed_text(bits: u64) -> String {
    match bits {
        0 => "unknown".into(),
        b if b >= 1_000_000_000 => format!("{:.1} Gbps", b as f64 / 1e9).replace(".0 ", " "),
        b => format!("{} Mbps", b / 1_000_000),
    }
}

fn adapter_info(a: &Adapter) -> String {
    if !a.up {
        return format!("{}: not connected. MAC {}.", a.description, a.mac);
    }
    let v4: Vec<String> = a.ipv4.iter().map(|(ip, p)| format!("{ip}/{p}")).collect();
    let mut parts = vec![
        format!("{} at {}", a.description, speed_text(a.speed)),
        format!(
            "IPv4 {} ({})",
            if v4.is_empty() { "none".into() } else { v4.join(", ") },
            if a.dhcp { "DHCP" } else { "manual" }
        ),
    ];
    if !a.gateways.is_empty() {
        parts.push(format!("gateway {}", a.gateways.join(", ")));
    }
    if !a.dns.is_empty() {
        parts.push(format!("DNS {}", a.dns.join(", ")));
    }
    if let Some(v6) = a.ipv6.first() {
        parts.push(format!("IPv6 {v6}"));
    }
    parts.push(format!("MAC {}", a.mac));
    parts.join(" \u{b7} ")
}

// ------------------------------------------------------------- applying --

/// DNS servers Windows knows a DNS-over-HTTPS address for.
fn doh_known() -> Vec<String> {
    let out = super::run_output("netsh.exe", &["dns", "show", "encryption"]).unwrap_or_default();
    out.lines()
        .filter_map(|l| {
            let l = l.trim();
            let (k, v) = l.split_once(':')?;
            (k.trim().eq_ignore_ascii_case("Encryption settings for")).then(|| v.trim().to_string())
        })
        .collect()
}

fn doh_on(a: &Adapter) -> bool {
    let key = format!(
        "SYSTEM\\CurrentControlSet\\Services\\Dnscache\\InterfaceSpecificParameters\\{}\\DohInterfaceSettings\\Doh",
        a.guid
    );
    a.dns.iter().any(|ip| {
        super::reg_dword(windows::Win32::System::Registry::HKEY_LOCAL_MACHINE, &format!("{key}\\{ip}"), "DohFlags")
            .is_some()
    })
}

/// Save: what the form says, through netsh, as administrator (Windows asks).
fn save() {
    let Some(a) = chosen() else { return };
    let f = form();
    let ip_manual = f.ip_manual.unwrap_or(!a.dhcp);
    let dns_manual = f.dns_manual.unwrap_or(a.dns_manual);
    let cur_ip = a.ipv4.first();
    let ip = f.ip.clone().unwrap_or_else(|| cur_ip.map(|(i, _)| i.to_string()).unwrap_or_default());
    let mask = f.mask.clone().unwrap_or_else(|| cur_ip.map(|(_, p)| mask_of(*p).to_string()).unwrap_or_default());
    let gw = f.gateway.clone().unwrap_or_else(|| a.gateways.first().cloned().unwrap_or_default());
    let dns1 = f.dns1.clone().unwrap_or_else(|| a.dns.first().cloned().unwrap_or_default());
    let dns2 = f.dns2.clone().unwrap_or_else(|| a.dns.get(1).cloned().unwrap_or_default());
    let doh = f.doh;
    drop(f);
    let name = a.alias.replace('"', "");
    let mut cmds = Vec::new();
    if ip_manual {
        let (Ok(ipv), Some(m)) = (ip.trim().parse::<Ipv4Addr>(), parse_mask(&mask)) else {
            tell("Type an IP address (like 192.168.1.20) and a subnet mask (255.255.255.0, or 24).".into());
            return;
        };
        let gw = gw.trim();
        let gateway = if gw.is_empty() {
            "none".to_string()
        } else if gw.parse::<Ipv4Addr>().is_ok() {
            gw.to_string()
        } else {
            tell(format!("\u{201c}{gw}\u{201d} isn't an IPv4 address for the gateway."));
            return;
        };
        cmds.push(format!(
            "netsh interface ipv4 set address name=\"{name}\" source=static address={ipv} mask={m} gateway={gateway}"
        ));
    } else {
        cmds.push(format!("netsh interface ipv4 set address name=\"{name}\" source=dhcp"));
    }
    let servers: Vec<String> =
        [dns1.trim(), dns2.trim()].iter().filter(|s| !s.is_empty()).map(|s| s.to_string()).collect();
    if dns_manual {
        if servers.is_empty() || servers.iter().any(|s| s.parse::<std::net::IpAddr>().is_err()) {
            tell("Type a preferred DNS server's address (like 1.1.1.1), and an alternate one if you like.".into());
            return;
        }
        cmds.push(format!(
            "netsh interface ipv4 set dnsservers name=\"{name}\" source=static address={} register=primary validate=no",
            servers[0]
        ));
        if let Some(s2) = servers.get(1) {
            cmds.push(format!("netsh interface ipv4 add dnsservers name=\"{name}\" address={s2} index=2 validate=no"));
        }
    } else {
        cmds.push(format!("netsh interface ipv4 set dnsservers name=\"{name}\" source=dhcp"));
    }
    // DNS over HTTPS, for servers Windows knows the HTTPS address of (as its Settings offers).
    let doh_key = format!(
        "HKLM\\SYSTEM\\CurrentControlSet\\Services\\Dnscache\\InterfaceSpecificParameters\\{}\\DohInterfaceSettings\\Doh",
        a.guid
    );
    match doh {
        Some(true) if dns_manual => {
            for s in &servers {
                cmds.push(format!("reg add \"{doh_key}\\{s}\" /v DohFlags /t REG_QWORD /d 1 /f"));
            }
        }
        Some(false) | Some(true) => cmds.push(format!("reg delete \"{doh_key}\" /f")),
        None => {}
    }
    cmds.push("ipconfig /flushdns".into());
    let line = cmds.join(" & ");
    std::thread::spawn(move || {
        let r = run_admin(&line);
        config::log(&format!("network: saved IP settings for {name}: {r:?}"));
        *form() = Form { guid: Some(a.guid.clone()), ..Default::default() };
        ADAPTERS.forget();
        changed();
    });
}

// ----------------------------------------------------------------- wi-fi --

unsafe fn wlan() -> Option<(HANDLE, windows::core::GUID)> {
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

/// The Wi-Fi connection: (network, its properties in a line).
fn wifi_now() -> Option<(String, String)> {
    unsafe {
        let (h, guid) = wlan()?;
        let mut size = 0u32;
        let mut data: *mut std::ffi::c_void = std::ptr::null_mut();
        let r = WlanQueryInterface(h, &guid, wlan_intf_opcode_current_connection, None, &mut size, &mut data, None);
        if r != 0 || data.is_null() {
            WlanCloseHandle(h, None);
            return None;
        }
        let c = &*(data as *const WLAN_CONNECTION_ATTRIBUTES);
        let assoc = &c.wlanAssociationAttributes;
        let ssid = String::from_utf8_lossy(&assoc.dot11Ssid.ucSSID[..(assoc.dot11Ssid.uSSIDLength as usize).min(32)])
            .into_owned();
        let phy = match assoc.dot11PhyType.0 {
            4 => "802.11a",
            5 => "802.11b",
            6 => "802.11g",
            7 => "Wi-Fi 4 (802.11n)",
            8 => "Wi-Fi 5 (802.11ac)",
            10 => "Wi-Fi 6 (802.11ax)",
            11 => "Wi-Fi 7 (802.11be)",
            _ => "Wi-Fi",
        };
        let security = match c.wlanSecurityAttributes.dot11AuthAlgorithm.0 {
            1 => "Open",
            2 => "WEP",
            3 | 4 => "WPA",
            6 => "WPA2-Enterprise",
            7 => "WPA2-Personal",
            8 | 11 => "WPA3-Enterprise",
            9 => "WPA3-Personal",
            10 => "Enhanced Open",
            _ => "Secured",
        };
        let signal = assoc.wlanSignalQuality;
        let (rx, tx) = (assoc.ulRxRate / 1000, assoc.ulTxRate / 1000);
        let bssid = assoc.dot11Bssid.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":");
        WlanFreeMemory(data);
        let mut channel: *mut std::ffi::c_void = std::ptr::null_mut();
        let ch = if WlanQueryInterface(h, &guid, wlan_intf_opcode_channel_number, None, &mut size, &mut channel, None)
            == 0
            && !channel.is_null()
        {
            let n = *(channel as *const u32);
            WlanFreeMemory(channel);
            Some(n)
        } else {
            None
        };
        WlanCloseHandle(h, None);
        let band = match ch {
            Some(n) if n <= 14 => format!(", 2.4 GHz channel {n}"),
            Some(n) => format!(", 5/6 GHz channel {n}"),
            None => String::new(),
        };
        Some((
            ssid.clone(),
            format!(
                "{ssid}: {phy}{band}, {security}, signal {signal}%, {rx}/{tx} Mbps (down/up), access point {bssid}."
            ),
        ))
    }
}

static WIFI: super::Slow<Option<(String, String)>> = super::Slow::new();
static METERED: super::Slow<Option<bool>> = super::Slow::new();
static PROXY: super::Slow<Proxy> = super::Slow::new();

fn wifi() -> Option<(String, String)> {
    WIFI.get(Duration::from_secs(3), wifi_now).flatten()
}

fn metered() -> Option<bool> {
    METERED.get(Duration::from_secs(3), read_metered).flatten()
}

fn read_metered() -> Option<bool> {
    let p = NetworkInformation::GetInternetConnectionProfile().ok()?;
    let t = p.GetConnectionCost().ok()?.NetworkCostType().ok()?;
    Some(t == NetworkCostType::Fixed || t == NetworkCostType::Variable)
}

fn set_metered(on: bool) {
    let Some((ssid, _)) = wifi() else {
        tell("Metered connection is set per Wi-Fi network: connect to one first.".into());
        return;
    };
    let cost = if on { "Fixed" } else { "Unrestricted" };
    std::thread::spawn(move || {
        let args = ["wlan", "set", "profileparameter", &format!("name={ssid}"), &format!("cost={cost}")];
        if !run_hidden("netsh.exe", &args, true) {
            // Profiles for every user need administrator rights.
            let _ =
                run_admin(&format!("netsh wlan set profileparameter name=\"{}\" cost={cost}", ssid.replace('"', "")));
        }
        config::log(&format!("network: {ssid} metered: {on}"));
        METERED.forget();
        changed();
    });
}

// ----------------------------------------------------------------- proxy --

#[derive(Clone, Default)]
struct Proxy {
    flags: u32,
    server: String,
    bypass: String,
    script: String,
}

unsafe fn take_wide(p: PWSTR) -> String {
    if p.is_null() {
        return String::new();
    }
    let s = p.to_string().unwrap_or_default();
    windows::Win32::Foundation::GlobalFree(Some(windows::Win32::Foundation::HGLOBAL(p.0 as *mut _))).ok();
    s
}

fn proxy() -> Proxy {
    PROXY.get(Duration::from_secs(5), read_proxy).unwrap_or_default()
}

fn read_proxy() -> Proxy {
    unsafe {
        let mut opts = [
            INTERNET_PER_CONN_OPTIONW { dwOption: INTERNET_PER_CONN_FLAGS, ..Default::default() },
            INTERNET_PER_CONN_OPTIONW { dwOption: INTERNET_PER_CONN_PROXY_SERVER, ..Default::default() },
            INTERNET_PER_CONN_OPTIONW { dwOption: INTERNET_PER_CONN_PROXY_BYPASS, ..Default::default() },
            INTERNET_PER_CONN_OPTIONW { dwOption: INTERNET_PER_CONN_AUTOCONFIG_URL, ..Default::default() },
        ];
        let mut list = INTERNET_PER_CONN_OPTION_LISTW {
            dwSize: std::mem::size_of::<INTERNET_PER_CONN_OPTION_LISTW>() as u32,
            pszConnection: PWSTR::null(),
            dwOptionCount: opts.len() as u32,
            dwOptionError: 0,
            pOptions: opts.as_mut_ptr(),
        };
        let mut size = list.dwSize;
        if InternetQueryOptionW(
            None,
            INTERNET_OPTION_PER_CONNECTION_OPTION,
            Some(&mut list as *mut _ as *mut _),
            &mut size,
        )
        .is_err()
        {
            return Proxy::default();
        }
        Proxy {
            flags: opts[0].Value.dwValue,
            server: take_wide(opts[1].Value.pszValue),
            bypass: take_wide(opts[2].Value.pszValue),
            script: take_wide(opts[3].Value.pszValue),
        }
    }
}

fn set_proxy(p: &Proxy) {
    unsafe {
        let server = HSTRING::from(p.server.as_str());
        let bypass = HSTRING::from(p.bypass.as_str());
        let script = HSTRING::from(p.script.as_str());
        let wide = |h: &HSTRING| INTERNET_PER_CONN_OPTIONW_0 { pszValue: PWSTR(h.as_ptr() as *mut u16) };
        let mut opts = [
            INTERNET_PER_CONN_OPTIONW {
                dwOption: INTERNET_PER_CONN_FLAGS,
                Value: INTERNET_PER_CONN_OPTIONW_0 { dwValue: p.flags | PROXY_TYPE_DIRECT },
            },
            INTERNET_PER_CONN_OPTIONW { dwOption: INTERNET_PER_CONN_PROXY_SERVER, Value: wide(&server) },
            INTERNET_PER_CONN_OPTIONW { dwOption: INTERNET_PER_CONN_PROXY_BYPASS, Value: wide(&bypass) },
            INTERNET_PER_CONN_OPTIONW { dwOption: INTERNET_PER_CONN_AUTOCONFIG_URL, Value: wide(&script) },
        ];
        let list = INTERNET_PER_CONN_OPTION_LISTW {
            dwSize: std::mem::size_of::<INTERNET_PER_CONN_OPTION_LISTW>() as u32,
            pszConnection: PWSTR::null(),
            dwOptionCount: opts.len() as u32,
            dwOptionError: 0,
            pOptions: opts.as_mut_ptr(),
        };
        let r = InternetSetOptionW(
            None,
            INTERNET_OPTION_PER_CONNECTION_OPTION,
            Some(&list as *const _ as *const _),
            list.dwSize,
        );
        // Every program using Windows' internet settings picks it up now.
        let _ = InternetSetOptionW(None, INTERNET_OPTION_SETTINGS_CHANGED, None, 0);
        let _ = InternetSetOptionW(None, INTERNET_OPTION_REFRESH, None, 0);
        config::log(&format!("network: proxy flags {} server {} ({r:?})", p.flags, p.server));
    }
}

fn update_proxy(edit: impl FnOnce(&mut Proxy)) {
    let mut p = read_proxy();
    edit(&mut p);
    set_proxy(&p);
    PROXY.forget();
}

// -------------------------------------------------------------- the page --

const IP_MODES: [&str; 2] = ["Automatic (DHCP)", "Manual"];
const DNS_PRESETS: [(&str, &str, &str); 5] = [
    ("Cloudflare (1.1.1.1)", "1.1.1.1", "1.0.0.1"),
    ("Google (8.8.8.8)", "8.8.8.8", "8.8.4.4"),
    ("Quad9 (9.9.9.9)", "9.9.9.9", "149.112.112.112"),
    ("OpenDNS (208.67.222.222)", "208.67.222.222", "208.67.220.220"),
    ("AdGuard (94.140.14.14)", "94.140.14.14", "94.140.15.15"),
];

fn ip_manual(a: &Adapter) -> bool {
    form().ip_manual.unwrap_or(!a.dhcp)
}

fn dns_manual(a: &Adapter) -> bool {
    form().dns_manual.unwrap_or(a.dns_manual)
}

pub fn get(key: &str) -> Option<String> {
    let b = |v: bool| v.to_string();
    Some(match key {
        "win_adapter" => chosen().map(|a| a.alias).unwrap_or_default(),
        "win_ip_mode" => chosen().map(|a| IP_MODES[ip_manual(&a) as usize].to_string()).unwrap_or_default(),
        "win_ip_addr" | "win_ip_mask" | "win_ip_gateway" | "win_dns1" | "win_dns2" => {
            let a = chosen().unwrap_or_default();
            let f = form();
            let typed = match key {
                "win_ip_addr" => &f.ip,
                "win_ip_mask" => &f.mask,
                "win_ip_gateway" => &f.gateway,
                "win_dns1" => &f.dns1,
                _ => &f.dns2,
            };
            typed.clone().unwrap_or_else(|| match key {
                "win_ip_addr" => a.ipv4.first().map(|(i, _)| i.to_string()).unwrap_or_default(),
                "win_ip_mask" => a.ipv4.first().map(|(_, p)| mask_of(*p).to_string()).unwrap_or_default(),
                "win_ip_gateway" => a.gateways.iter().find(|g| g.contains('.')).cloned().unwrap_or_default(),
                "win_dns1" => a.dns.first().cloned().unwrap_or_default(),
                _ => a.dns.get(1).cloned().unwrap_or_default(),
            })
        }
        "win_dns_mode" => chosen().map(|a| IP_MODES[dns_manual(&a) as usize].to_string()).unwrap_or_default(),
        "win_dns_preset" => String::new(),
        "win_doh" => {
            let on = form().doh.unwrap_or_else(|| chosen().is_some_and(|a| doh_on(&a)));
            (if on { "On (automatic template)" } else { "Off" }).to_string()
        }
        "win_metered" => b(metered().unwrap_or(false)),
        "win_proxy_auto" => b(proxy().flags & PROXY_TYPE_AUTO_DETECT != 0),
        "win_proxy_script_on" => b(proxy().flags & PROXY_TYPE_AUTO_PROXY_URL != 0),
        "win_proxy_script" => proxy().script,
        "win_proxy_manual" => b(proxy().flags & PROXY_TYPE_PROXY != 0),
        "win_proxy_server" => proxy().server,
        "win_proxy_bypass" => proxy().bypass,
        _ => return None,
    })
}

pub fn desc(key: &str) -> Option<String> {
    Some(match key {
        "win_adapter_info" => chosen().map(|a| adapter_info(&a)).unwrap_or_else(|| "No network adapters.".into()),
        "win_wifi_info" => wifi().map(|(_, d)| d).unwrap_or_else(|| "Not connected to Wi-Fi.".into()),
        _ => return None,
    })
}

pub fn shown(key: &str) -> Option<bool> {
    let manual_ip = || chosen().is_some_and(|a| ip_manual(&a));
    let manual_dns = || chosen().is_some_and(|a| dns_manual(&a));
    Some(match key {
        "win_ip_addr" | "win_ip_mask" | "win_ip_gateway" => manual_ip(),
        "win_dns1" | "win_dns2" | "win_dns_preset" | "win_doh" => manual_dns(),
        "win_wifi_info" | "win_metered" => wifi().is_some(),
        "win_proxy_script" => proxy().flags & PROXY_TYPE_AUTO_PROXY_URL != 0,
        "win_proxy_server" | "win_proxy_bypass" => proxy().flags & PROXY_TYPE_PROXY != 0,
        "win_adapter" | "win_adapter_info" | "win_ip_mode" | "win_dns_mode" | "win_ip_save" => !adapters().is_empty(),
        _ => return None,
    })
}

pub fn set(key: &str, v: &str) -> Option<()> {
    let on = v == "true";
    let text = Some(v.trim().to_string());
    match key {
        "win_ip_addr" => form().ip = text,
        "win_ip_mask" => form().mask = text,
        "win_ip_gateway" => form().gateway = text,
        "win_dns1" => form().dns1 = text,
        "win_dns2" => form().dns2 = text,
        "win_metered" => set_metered(on),
        "win_proxy_auto" => update_proxy(|p| {
            p.flags = if on { p.flags | PROXY_TYPE_AUTO_DETECT } else { p.flags & !PROXY_TYPE_AUTO_DETECT }
        }),
        "win_proxy_script_on" => update_proxy(|p| {
            p.flags = if on { p.flags | PROXY_TYPE_AUTO_PROXY_URL } else { p.flags & !PROXY_TYPE_AUTO_PROXY_URL }
        }),
        "win_proxy_script" => update_proxy(|p| p.script = v.trim().to_string()),
        "win_proxy_manual" => {
            update_proxy(|p| p.flags = if on { p.flags | PROXY_TYPE_PROXY } else { p.flags & !PROXY_TYPE_PROXY })
        }
        "win_proxy_server" => update_proxy(|p| p.server = v.trim().to_string()),
        "win_proxy_bypass" => update_proxy(|p| p.bypass = v.trim().to_string()),
        _ => return None,
    }
    Some(())
}

pub fn action(key: &str, owner: HWND) -> Option<()> {
    match key {
        "win_adapter" => {
            let list = adapters();
            let names: Vec<String> = list
                .iter()
                .map(|a| format!("{}{}", a.alias, if a.up { "" } else { " (not connected)" }))
                .collect();
            let cur = chosen().and_then(|c| list.iter().position(|a| a.guid == c.guid));
            if let Some(i) = pick(owner, &names, cur) {
                *form() = Form { guid: Some(list[i].guid.clone()), ..Default::default() };
            }
        }
        "win_ip_mode" | "win_dns_mode" => {
            let a = chosen()?;
            let cur = if key == "win_ip_mode" { ip_manual(&a) } else { dns_manual(&a) };
            let names: Vec<String> = IP_MODES.iter().map(|s| s.to_string()).collect();
            if let Some(i) = pick(owner, &names, Some(cur as usize)) {
                let mut f = form();
                if key == "win_ip_mode" {
                    f.ip_manual = Some(i == 1);
                    // A manual address needs manual DNS (there's no DHCP to give it).
                    if i == 1 {
                        f.dns_manual = Some(true);
                    }
                } else {
                    f.dns_manual = Some(i == 1);
                }
            }
        }
        "win_dns_preset" => {
            let names: Vec<String> = DNS_PRESETS.iter().map(|(n, _, _)| n.to_string()).collect();
            if let Some(i) = pick(owner, &names, None) {
                let mut f = form();
                f.dns1 = Some(DNS_PRESETS[i].1.into());
                f.dns2 = Some(DNS_PRESETS[i].2.into());
            }
        }
        "win_doh" => {
            let names = vec!["Off".to_string(), "On (automatic template)".to_string()];
            let cur = form().doh.unwrap_or_else(|| chosen().is_some_and(|a| doh_on(&a)));
            if let Some(i) = pick(owner, &names, Some(cur as usize)) {
                if i == 1 {
                    let f = form();
                    let typed: Vec<String> = [f.dns1.clone(), f.dns2.clone()].into_iter().flatten().collect();
                    drop(f);
                    let known = doh_known();
                    let servers = if typed.is_empty() { chosen().map(|a| a.dns).unwrap_or_default() } else { typed };
                    if !servers.iter().any(|s| known.contains(s)) {
                        tell("Windows only knows DNS over HTTPS for some servers (Cloudflare, Google, Quad9 and others): pick one from Use a provider first.".into());
                        return Some(());
                    }
                }
                form().doh = Some(i == 1);
            }
        }
        "win_ip_save" => save(),
        "win_net_reset" => {
            if confirm(
                owner,
                "Reset networking? Every adapter is removed and set up again with Windows' defaults (VPNs and virtual switches too), and the PC needs a restart afterwards.",
            ) {
                std::thread::spawn(|| {
                    if run_admin("netsh winsock reset & netsh int ip reset & netsh int ipv6 reset & ipconfig /flushdns").is_some() {
                        tell("Networking was reset. Restart the PC to finish.".into());
                    }
                });
            }
        }
        _ => return None,
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_masks() {
        assert_eq!(parse_mask("24"), Some(Ipv4Addr::new(255, 255, 255, 0)));
        assert_eq!(parse_mask("/16"), Some(Ipv4Addr::new(255, 255, 0, 0)));
        assert_eq!(parse_mask("255.255.255.128"), Some(Ipv4Addr::new(255, 255, 255, 128)));
        assert_eq!(parse_mask("255.0.255.0"), None);
        assert_eq!(parse_mask("33"), None);
        assert_eq!(mask_of(0), Ipv4Addr::new(0, 0, 0, 0));
    }
}
