//! Bluetooth devices for the notch's Controls: the ones paired with this PC,
//! which are connected, and connecting or disconnecting the ones that play
//! sound (headphones, speakers, earbuds) with a click.
//!
//! A thread of its own does the work, only when asked (the Controls tab's
//! Bluetooth list opening, or a device picked). Connecting asks the device's
//! audio driver to reconnect it, as Windows' own Connect button does, so it
//! works for anything with sound; mice, keyboards and the like connect by
//! themselves when they wake, so picking one of those opens Windows'
//! Bluetooth settings instead.

use std::ffi::c_void;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Duration;

use windows::core::{Interface, GUID, HSTRING, PWSTR};
use windows::Devices::Bluetooth::{BluetoothConnectionStatus, BluetoothDevice, BluetoothLEDevice, BluetoothMajorClass};
use windows::Devices::Enumeration::DeviceInformation;
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::*;
use windows::Win32::Media::KernelStreaming::{IKsControl, KSIDENTIFIER, KSIDENTIFIER_0, KSIDENTIFIER_0_0};
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::System::Com::*;

use crate::msg;

/// What sort of thing a device is, for its icon.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Audio,
    Input,
    Phone,
    Other,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    pub name: String,
    pub connected: bool,
    pub kind: Kind,
    /// It plays sound, so Cascade can connect and disconnect it.
    pub audio: bool,
}

/// What the UI is told (boxed, through `msg::BLUETOOTH`).
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub devices: Vec<Device>,
    /// Still being read.
    pub loading: bool,
    /// What happened to the last device picked, to show for a moment.
    pub note: Option<String>,
}

pub enum Cmd {
    /// List the paired devices.
    List,
    /// Connect (true) or disconnect the device with this name.
    Set(String, bool),
}

pub fn spawn() -> Sender<Cmd> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        run(rx)
    });
    tx
}

fn post(report: Report) {
    let ptr = Box::into_raw(Box::new(report));
    if !msg::post(msg::BLUETOOTH, 0, ptr as isize) {
        drop(unsafe { Box::from_raw(ptr) });
    }
}

fn run(rx: Receiver<Cmd>) {
    let mut devices = Vec::new();
    while let Ok(cmd) = rx.recv() {
        let mut picked = None;
        for c in std::iter::once(cmd).chain(rx.try_iter()) {
            if let Cmd::Set(name, connect) = c {
                picked = Some((name, connect));
            }
        }
        match picked {
            None => {
                post(Report { devices: devices.clone(), loading: true, note: None });
                devices = list();
                post(Report { devices: devices.clone(), loading: false, note: None });
            }
            Some((name, connect)) => {
                let ok = unsafe { set_audio(&name, connect) };
                let note = match (ok, connect) {
                    (false, _) => format!("Couldn't reach {name}"),
                    (true, true) => format!("Connecting {name}\u{2026}"),
                    (true, false) => format!("Disconnecting {name}\u{2026}"),
                };
                post(Report { devices: devices.clone(), loading: false, note: Some(note) });
                // Give it a moment to come or go, then say how it went.
                if ok {
                    std::thread::sleep(Duration::from_millis(2500));
                    devices = list();
                    let now = devices.iter().find(|d| d.name == name).is_some_and(|d| d.connected);
                    let note = (now != connect).then(|| {
                        if connect {
                            format!("{name} didn't connect. Is it on and nearby?")
                        } else {
                            format!("{name} is still connected.")
                        }
                    });
                    post(Report { devices: devices.clone(), loading: false, note });
                }
            }
        }
    }
}

/// The paired devices, connected ones first.
fn list() -> Vec<Device> {
    let names = unsafe { audio_endpoints() }.into_iter().map(|e| e.0).collect::<Vec<_>>();
    let plays_sound = |name: &str| {
        let n = name.to_lowercase();
        names.iter().any(|(iface, friendly)| iface.contains(&n) || friendly.contains(&format!("({n})")))
    };
    let mut out: Vec<Device> = Vec::new();
    // Classic Bluetooth (most headphones, speakers, phones, mice).
    let classic = BluetoothDevice::GetDeviceSelectorFromPairingState(true)
        .and_then(|sel| DeviceInformation::FindAllAsyncAqsFilter(&sel))
        .and_then(|op| op.get());
    if let Ok(all) = classic {
        for i in 0..all.Size().unwrap_or(0) {
            let Ok(info) = all.GetAt(i) else { continue };
            let Ok(id) = info.Id() else { continue };
            let Ok(dev) = BluetoothDevice::FromIdAsync(&id).and_then(|op| op.get()) else { continue };
            let name = dev.Name().map(|n| n.to_string_lossy()).unwrap_or_default();
            if name.is_empty() {
                continue;
            }
            let connected = dev.ConnectionStatus().is_ok_and(|s| s == BluetoothConnectionStatus::Connected);
            let kind = match dev.ClassOfDevice().and_then(|c| c.MajorClass()) {
                Ok(BluetoothMajorClass::AudioVideo) => Kind::Audio,
                Ok(BluetoothMajorClass::Peripheral) => Kind::Input,
                Ok(BluetoothMajorClass::Phone) => Kind::Phone,
                _ => Kind::Other,
            };
            let audio = plays_sound(&name);
            out.push(Device { name, connected, kind: if audio { Kind::Audio } else { kind }, audio });
        }
    }
    // Bluetooth LE (newer mice, keyboards, controllers).
    let le = BluetoothLEDevice::GetDeviceSelectorFromPairingState(true)
        .and_then(|sel| DeviceInformation::FindAllAsyncAqsFilter(&sel))
        .and_then(|op| op.get());
    if let Ok(all) = le {
        for i in 0..all.Size().unwrap_or(0) {
            let Ok(info) = all.GetAt(i) else { continue };
            let Ok(id) = info.Id() else { continue };
            let Ok(dev) = BluetoothLEDevice::FromIdAsync(&id).and_then(|op| op.get()) else { continue };
            let name = dev.Name().map(|n| n.to_string_lossy()).unwrap_or_default();
            // A device with both shows once.
            if name.is_empty() || out.iter().any(|d| d.name == name) {
                continue;
            }
            let connected = dev.ConnectionStatus().is_ok_and(|s| s == BluetoothConnectionStatus::Connected);
            let audio = plays_sound(&name);
            out.push(Device { name, connected, kind: if audio { Kind::Audio } else { Kind::Other }, audio });
        }
    }
    out.sort_by(|a, b| b.connected.cmp(&a.connected).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    out
}

/// PKEY_DeviceInterface_FriendlyName: the device's own name ("WH-1000XM4").
const IFACE_NAME: PROPERTYKEY = PROPERTYKEY { fmtid: GUID::from_u128(0x026e516e_b814_414b_83cd_856d6fef4822), pid: 2 };
/// PKEY_Device_FriendlyName: "Headphones (WH-1000XM4)".
const FRIENDLY_NAME: PROPERTYKEY =
    PROPERTYKEY { fmtid: GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0), pid: 14 };
/// The Bluetooth audio driver's property set, and its two one-shot requests.
const BT_AUDIO: GUID = GUID::from_u128(0x7fa06c40_b8f6_4c7e_8556_e8c33a12e54d);
const ONESHOT_RECONNECT: u32 = 0;
const ONESHOT_DISCONNECT: u32 = 1;
const KSPROPERTY_TYPE_GET: u32 = 1;

unsafe fn take(p: PWSTR) -> String {
    let s = p.to_string().unwrap_or_default();
    CoTaskMemFree(Some(p.0 as *const _));
    s
}

/// Every Bluetooth sound endpoint, connected or not: its names (interface,
/// friendly, in lower case) and the driver filter behind it.
unsafe fn audio_endpoints() -> Vec<((String, String), String)> {
    let mut out = Vec::new();
    let Ok(e) = CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL) else {
        return out;
    };
    let Ok(all) = e.EnumAudioEndpoints(eAll, DEVICE_STATE(DEVICE_STATE_ACTIVE.0 | DEVICE_STATE_UNPLUGGED.0)) else {
        return out;
    };
    for i in 0..all.GetCount().unwrap_or(0) {
        let Ok(d) = all.Item(i) else { continue };
        let Some(filter) = filter_of(&d) else { continue };
        if !filter.to_ascii_lowercase().contains("bth") {
            continue;
        }
        let read = |key: &PROPERTYKEY| {
            d.OpenPropertyStore(STGM_READ)
                .and_then(|s| s.GetValue(key))
                .and_then(|v| PropVariantToStringAlloc(&v))
                .map(|p| take(p).to_lowercase())
                .unwrap_or_default()
        };
        out.push(((read(&IFACE_NAME), read(&FRIENDLY_NAME)), filter));
    }
    out
}

/// The driver filter (a device id) an endpoint is connected to.
unsafe fn filter_of(d: &IMMDevice) -> Option<String> {
    let topology: IDeviceTopology = d.Activate(CLSCTX_ALL, None).ok()?;
    let other = topology.GetConnector(0).ok()?.GetConnectedTo().ok()?;
    let part: IPart = other.cast().ok()?;
    Some(take(part.GetTopologyObject().ok()?.GetDeviceId().ok()?))
}

/// Asks the Bluetooth audio driver to connect or disconnect device `name`
/// (each of its filters: music and hands-free are separate). True if it took
/// the request.
unsafe fn set_audio(name: &str, connect: bool) -> bool {
    let Ok(e) = CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL) else {
        return false;
    };
    let n = name.to_lowercase();
    let mut filters: Vec<String> = audio_endpoints()
        .into_iter()
        .filter(|((iface, friendly), _)| iface.contains(&n) || friendly.contains(&format!("({n})")))
        .map(|(_, f)| f)
        .collect();
    filters.sort();
    filters.dedup();
    let mut ok = false;
    for f in filters {
        let Ok(dev) = e.GetDevice(&HSTRING::from(f)) else { continue };
        let Ok(ks) = dev.Activate::<IKsControl>(CLSCTX_ALL, None) else { continue };
        let property = KSIDENTIFIER {
            Anonymous: KSIDENTIFIER_0 {
                Anonymous: KSIDENTIFIER_0_0 {
                    Set: BT_AUDIO,
                    Id: if connect { ONESHOT_RECONNECT } else { ONESHOT_DISCONNECT },
                    Flags: KSPROPERTY_TYPE_GET,
                },
            },
        };
        let mut returned = 0u32;
        ok |= ks
            .KsProperty(
                &property,
                std::mem::size_of::<KSIDENTIFIER>() as u32,
                std::ptr::null_mut::<c_void>(),
                0,
                &mut returned,
            )
            .is_ok();
    }
    ok
}
