//! Sound: the output and input devices (the default for each, as Windows'
//! Settings > System > Sound picks it), the volume and the microphone level.
//! The classic Sound control panel and volume mixer have the rest (each
//! device's properties, enhancements, spatial sound), and are a click away.

use std::ffi::c_void;

use windows::core::{Interface, GUID, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{HWND, PROPERTYKEY};
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::System::Com::*;

use crate::config;

const FRIENDLY_NAME: PROPERTYKEY =
    PROPERTYKEY { fmtid: GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0), pid: 14 };
const POLICY_CONFIG: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);
const IID_POLICY_CONFIG: GUID = GUID::from_u128(0xf8679f50_850a_41cf_9c72_430f290290c8);
const SET_DEFAULT_ENDPOINT: usize = 13;

fn flow(input: bool) -> EDataFlow {
    if input {
        eCapture
    } else {
        eRender
    }
}

unsafe fn enumerator() -> Option<IMMDeviceEnumerator> {
    let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()
}

unsafe fn take(p: PWSTR) -> String {
    let s = p.to_string().unwrap_or_default();
    CoTaskMemFree(Some(p.0 as *const _));
    s
}

/// The devices for output (or `input`): (id, name).
fn devices(input: bool) -> Vec<(String, String)> {
    devices_in(flow(input), DEVICE_STATE_ACTIVE)
}

/// The devices for a direction (eAll for both) in a state (active, disabled...): (id, name).
fn devices_in(dir: EDataFlow, state: DEVICE_STATE) -> Vec<(String, String)> {
    let mut out = Vec::new();
    unsafe {
        let Some(e) = enumerator() else { return out };
        let Ok(list) = e.EnumAudioEndpoints(dir, state) else { return out };
        for i in 0..list.GetCount().unwrap_or(0) {
            let Ok(d) = list.Item(i) else { continue };
            let Ok(id) = d.GetId() else { continue };
            let name = d
                .OpenPropertyStore(STGM_READ)
                .and_then(|s| s.GetValue(&FRIENDLY_NAME))
                .and_then(|v| PropVariantToStringAlloc(&v))
                .map(|p| take(p))
                .unwrap_or_else(|_| "Audio device".into());
            out.push((take(id), name));
        }
    }
    out
}

fn default_id(input: bool) -> Option<String> {
    unsafe { enumerator()?.GetDefaultAudioEndpoint(flow(input), eConsole).ok()?.GetId().ok().map(|p| take(p)) }
}

/// Makes `id` the default device for every role, as Windows' sound settings do.
fn set_default(id: &str) -> bool {
    let id = HSTRING::from(id);
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let Ok(policy) = CoCreateInstance::<_, windows::core::IUnknown>(&POLICY_CONFIG, None, CLSCTX_ALL) else {
            return false;
        };
        let mut raw: *mut c_void = std::ptr::null_mut();
        if policy.query(&IID_POLICY_CONFIG, &mut raw).is_err() || raw.is_null() {
            return false;
        }
        type SetDefault = unsafe extern "system" fn(*mut c_void, PCWSTR, ERole) -> windows::core::HRESULT;
        let table = *(raw as *const *const usize);
        let set: SetDefault = std::mem::transmute::<usize, SetDefault>(*table.add(SET_DEFAULT_ENDPOINT));
        let mut ok = true;
        for role in [eConsole, eMultimedia, eCommunications] {
            ok &= set(raw, PCWSTR(id.as_ptr()), role).is_ok();
        }
        drop(windows::core::IUnknown::from_raw(raw));
        ok
    }
}

unsafe fn endpoint(input: bool) -> Option<IAudioEndpointVolume> {
    enumerator()?.GetDefaultAudioEndpoint(flow(input), eConsole).ok()?.Activate(CLSCTX_ALL, None).ok()
}

/// The default device's level, 0 to 1.
pub fn level(input: bool) -> f32 {
    unsafe { endpoint(input).and_then(|e| e.GetMasterVolumeLevelScalar().ok()).unwrap_or(0.0) }
}

pub fn set_level(input: bool, v: f32) {
    unsafe {
        if let Some(e) = endpoint(input) {
            let _ = e.SetMasterVolumeLevelScalar(v.clamp(0.0, 1.0), std::ptr::null());
        }
    }
}

pub fn label(key: &str) -> String {
    let input = key == "win_input";
    let id = default_id(input);
    devices(input).into_iter().find(|(d, _)| Some(d) == id.as_ref()).map(|(_, n)| n).unwrap_or_else(|| "None".into())
}

pub fn pick(key: &str, owner: HWND) {
    let input = key == "win_input";
    let list = devices(input);
    let id = default_id(input);
    let names: Vec<String> = list.iter().map(|(_, n)| n.clone()).collect();
    let cur = list.iter().position(|(d, _)| Some(d) == id.as_ref());
    if let Some(i) = super::pick(owner, &names, cur) {
        let ok = set_default(&list[i].0);
        config::log(&format!(
            "sound: {} is the default {} (took: {ok})",
            list[i].1,
            if input { "input" } else { "output" }
        ));
        super::changed();
    }
}

// ---------------------------------------------------- each device's own --

/// Windows' audio policy object (the one its Sound settings use), as a raw
/// interface pointer, and a call into it by its place in the table.
struct Policy(*mut c_void);

impl Policy {
    unsafe fn new() -> Option<Policy> {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let policy = CoCreateInstance::<_, windows::core::IUnknown>(&POLICY_CONFIG, None, CLSCTX_ALL).ok()?;
        let mut raw: *mut c_void = std::ptr::null_mut();
        policy.query(&IID_POLICY_CONFIG, &mut raw).ok().ok()?;
        (!raw.is_null()).then_some(Policy(raw))
    }

    unsafe fn slot(&self, i: usize) -> usize {
        *(*(self.0 as *const *const usize)).add(i)
    }

    /// The format the device runs at (shared mode). Freed by the caller.
    unsafe fn device_format(&self, id: &HSTRING) -> Option<WAVEFORMATEXTENSIBLE> {
        type F = unsafe extern "system" fn(*mut c_void, PCWSTR, i32, *mut *mut WAVEFORMATEX) -> windows::core::HRESULT;
        let f: F = std::mem::transmute(self.slot(4));
        let mut p: *mut WAVEFORMATEX = std::ptr::null_mut();
        if f(self.0, PCWSTR(id.as_ptr()), 0, &mut p).is_err() || p.is_null() {
            return None;
        }
        let mut out = WAVEFORMATEXTENSIBLE::default();
        let n = (std::mem::size_of::<WAVEFORMATEX>() + (*p).cbSize as usize)
            .min(std::mem::size_of::<WAVEFORMATEXTENSIBLE>());
        std::ptr::copy_nonoverlapping(p as *const u8, &mut out as *mut _ as *mut u8, n);
        CoTaskMemFree(Some(p as *const _));
        Some(out)
    }

    unsafe fn set_device_format(
        &self,
        id: &HSTRING,
        format: &WAVEFORMATEXTENSIBLE,
        mix: &WAVEFORMATEXTENSIBLE,
    ) -> bool {
        type F = unsafe extern "system" fn(
            *mut c_void,
            PCWSTR,
            *const WAVEFORMATEX,
            *const WAVEFORMATEX,
        ) -> windows::core::HRESULT;
        let f: F = std::mem::transmute(self.slot(6));
        f(
            self.0,
            PCWSTR(id.as_ptr()),
            format as *const _ as *const WAVEFORMATEX,
            mix as *const _ as *const WAVEFORMATEX,
        )
        .is_ok()
    }

    unsafe fn set_default(&self, id: &HSTRING, role: ERole) -> bool {
        type F = unsafe extern "system" fn(*mut c_void, PCWSTR, ERole) -> windows::core::HRESULT;
        let f: F = std::mem::transmute(self.slot(SET_DEFAULT_ENDPOINT));
        f(self.0, PCWSTR(id.as_ptr()), role).is_ok()
    }

    /// Shows (enables) or hides (disables) a device, as the Sound panel's Disable does.
    unsafe fn set_visible(&self, id: &HSTRING, on: bool) -> bool {
        type F = unsafe extern "system" fn(*mut c_void, PCWSTR, i32) -> windows::core::HRESULT;
        let f: F = std::mem::transmute(self.slot(14));
        f(self.0, PCWSTR(id.as_ptr()), on as i32).is_ok()
    }
}

impl Drop for Policy {
    fn drop(&mut self) {
        unsafe { drop(windows::core::IUnknown::from_raw(self.0)) };
    }
}

/// The device the properties are for, output and input (its id; none: the default).
static CHOSEN: std::sync::Mutex<[Option<String>; 2]> = std::sync::Mutex::new([None, None]);

fn chosen(input: bool) -> Option<(String, String)> {
    let list = devices(input);
    let want = CHOSEN.lock().unwrap_or_else(|e| e.into_inner())[input as usize].clone().or_else(|| default_id(input));
    list.iter().find(|(d, _)| Some(d) == want.as_ref()).or(list.first()).cloned()
}

unsafe fn volume_of(id: &str) -> Option<IAudioEndpointVolume> {
    enumerator()?.GetDevice(&HSTRING::from(id)).ok()?.Activate(CLSCTX_ALL, None).ok()
}

/// Balance from -1 (all left) to 1 (all right), from the first two channels.
fn balance(v: &IAudioEndpointVolume) -> f32 {
    unsafe {
        if v.GetChannelCount().unwrap_or(0) < 2 {
            return 0.0;
        }
        let (l, r) = (v.GetChannelVolumeLevelScalar(0).unwrap_or(1.0), v.GetChannelVolumeLevelScalar(1).unwrap_or(1.0));
        if l <= 0.0 && r <= 0.0 {
            0.0
        } else if l >= r {
            -(1.0 - r / l)
        } else {
            1.0 - l / r
        }
    }
}

fn set_balance(v: &IAudioEndpointVolume, b: f32) {
    unsafe {
        if v.GetChannelCount().unwrap_or(0) < 2 {
            return;
        }
        let m = v.GetMasterVolumeLevelScalar().unwrap_or(1.0);
        let b = b.clamp(-1.0, 1.0);
        let _ = v.SetChannelVolumeLevelScalar(0, m * (1.0 - b.max(0.0)), std::ptr::null());
        let _ = v.SetChannelVolumeLevelScalar(1, m * (1.0 + b.min(0.0)), std::ptr::null());
    }
}

const PCM: GUID = GUID::from_u128(0x00000001_0000_0010_8000_00aa00389b71);
const FLOAT: GUID = GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);
const RATES: [u32; 6] = [44_100, 48_000, 88_200, 96_000, 176_400, 192_000];

fn format(channels: u16, mask: u32, rate: u32, bits: u16, sub: GUID) -> WAVEFORMATEXTENSIBLE {
    let container = if bits == 24 { 24 } else { bits };
    let block = channels * container / 8;
    WAVEFORMATEXTENSIBLE {
        Format: WAVEFORMATEX {
            wFormatTag: 0xFFFE,
            nChannels: channels,
            nSamplesPerSec: rate,
            nAvgBytesPerSec: rate * block as u32,
            nBlockAlign: block,
            wBitsPerSample: container,
            cbSize: 22,
        },
        Samples: WAVEFORMATEXTENSIBLE_0 { wValidBitsPerSample: bits },
        dwChannelMask: mask,
        SubFormat: sub,
    }
}

fn format_name(rate: u32, bits: u16) -> String {
    let quality = match rate {
        44_100 => " (CD Quality)",
        48_000 => " (DVD Quality)",
        r if r >= 88_200 => " (Studio Quality)",
        _ => "",
    };
    format!("{bits} bit, {rate} Hz{quality}")
}

/// The device's format now: (rate, bits, channels, mask).
fn current_format(id: &str) -> Option<(u32, u16, u16, u32)> {
    unsafe {
        let f = Policy::new()?.device_format(&HSTRING::from(id))?;
        let bits = if f.Format.wFormatTag == 0xFFFE { f.Samples.wValidBitsPerSample } else { f.Format.wBitsPerSample };
        Some((f.Format.nSamplesPerSec, bits, f.Format.nChannels, f.dwChannelMask))
    }
}

/// The formats the device takes (asked as Windows' Advanced tab does: can it play this exclusively?).
fn formats(id: &str) -> Vec<(u32, u16)> {
    let Some((_, _, ch, mask)) = current_format(id) else { return Vec::new() };
    let mut out = Vec::new();
    unsafe {
        let Some(dev) = enumerator().and_then(|e| e.GetDevice(&HSTRING::from(id)).ok()) else { return out };
        let Ok(client) = dev.Activate::<IAudioClient>(CLSCTX_ALL, None) else { return out };
        for bits in [16u16, 24, 32] {
            for rate in RATES {
                let f = format(ch, mask, rate, bits, PCM);
                if client.IsFormatSupported(AUDCLNT_SHAREMODE_EXCLUSIVE, &f.Format, None) == windows::core::HRESULT(0) {
                    out.push((rate, bits));
                }
            }
        }
    }
    out
}

fn set_format(id: &str, rate: u32, bits: u16) -> bool {
    let Some((_, _, ch, mask)) = current_format(id) else { return false };
    unsafe {
        let Some(p) = Policy::new() else { return false };
        // The engine mixes in 32-bit float at the device's rate, as Windows sets it up.
        p.set_device_format(&HSTRING::from(id), &format(ch, mask, rate, bits, PCM), &format(ch, mask, rate, 32, FLOAT))
    }
}

const AUDIO: &str = "Software\\Microsoft\\Multimedia\\Audio";
const DUCKING: [&str; 4] =
    ["Mute all other sounds", "Reduce other sounds by 80%", "Reduce other sounds by 50%", "Do nothing"];

/// Which direction a per-device key is for.
fn dir(key: &str) -> Option<bool> {
    if key.starts_with("win_snd_out_") {
        Some(false)
    } else if key.starts_with("win_snd_in_") {
        Some(true)
    } else {
        None
    }
}

pub fn get(key: &str) -> Option<String> {
    use windows::Win32::System::Registry::HKEY_CURRENT_USER;
    match key {
        "win_snd_mono" => {
            return Some(
                (super::reg_dword(HKEY_CURRENT_USER, AUDIO, "AccessibilityMonoMixState") == Some(1)).to_string(),
            )
        }
        "win_snd_ducking" => {
            let v = super::reg_dword(HKEY_CURRENT_USER, AUDIO, "UserDuckingPreference").unwrap_or(1) as usize;
            return Some(DUCKING[v.min(3)].to_string());
        }
        _ => {}
    }
    let input = dir(key)?;
    let (id, name) = chosen(input).unwrap_or_default();
    let field = key.rsplit('_').next().unwrap_or("");
    Some(match field {
        "dev" => name,
        "vol" => unsafe { volume_of(&id).and_then(|v| v.GetMasterVolumeLevelScalar().ok()).unwrap_or(0.0).to_string() },
        "bal" => unsafe { volume_of(&id).map(|v| balance(&v)).unwrap_or(0.0).to_string() },
        "mute" => unsafe { volume_of(&id).and_then(|v| v.GetMute().ok()).is_some_and(|m| m.as_bool()).to_string() },
        "format" => current_format(&id).map(|(r, b, _, _)| format_name(r, b)).unwrap_or_default(),
        _ => return None,
    })
}

pub fn desc(key: &str) -> Option<String> {
    Some(match key {
        "win_snd_disabled" => match devices_in(eAll, DEVICE_STATE_DISABLED).len() {
            0 => "None: every sound device is on.".into(),
            n => format!("{n} turned off. Pick one to turn it back on."),
        },
        _ => return None,
    })
}

pub fn shown(key: &str) -> Option<bool> {
    Some(match key {
        "win_snd_out_bal" => unsafe {
            chosen(false).and_then(|(id, _)| volume_of(&id)).is_some_and(|v| v.GetChannelCount().unwrap_or(0) >= 2)
        },
        k if k.starts_with("win_snd_in_") => !devices(true).is_empty(),
        _ => return None,
    })
}

pub fn set(key: &str, v: &str) -> Option<()> {
    use windows::Win32::System::Registry::HKEY_CURRENT_USER;
    if key == "win_snd_mono" {
        super::set_reg_dword(HKEY_CURRENT_USER, AUDIO, "AccessibilityMonoMixState", (v == "true") as u32);
        return Some(());
    }
    let input = dir(key)?;
    let (id, _) = chosen(input)?;
    unsafe {
        let vol = volume_of(&id)?;
        match key.rsplit('_').next()? {
            "vol" => {
                let _ = vol.SetMasterVolumeLevelScalar(v.parse::<f32>().ok()?.clamp(0.0, 1.0), std::ptr::null());
            }
            "bal" => set_balance(&vol, v.parse().ok()?),
            "mute" => {
                let _ = vol.SetMute(v == "true", std::ptr::null());
            }
            _ => return None,
        }
    }
    Some(())
}

pub fn action(key: &str, owner: HWND) -> Option<()> {
    use windows::Win32::System::Registry::HKEY_CURRENT_USER;
    match key {
        "win_snd_ducking" => {
            let names: Vec<String> = DUCKING.iter().map(|s| s.to_string()).collect();
            let now = super::reg_dword(HKEY_CURRENT_USER, AUDIO, "UserDuckingPreference").unwrap_or(1) as usize;
            if let Some(i) = super::pick(owner, &names, Some(now.min(3))) {
                super::set_reg_dword(HKEY_CURRENT_USER, AUDIO, "UserDuckingPreference", i as u32);
            }
            return Some(());
        }
        "win_snd_disabled" => {
            let list = devices_in(eAll, DEVICE_STATE_DISABLED);
            let names: Vec<String> = list.iter().map(|(_, n)| n.clone()).collect();
            if let Some(i) = super::pick(owner, &names, None) {
                let ok =
                    unsafe { Policy::new().is_some_and(|p| p.set_visible(&HSTRING::from(list[i].0.as_str()), true)) };
                config::log(&format!("sound: turned {} back on ({ok})", list[i].1));
            }
            return Some(());
        }
        _ => {}
    }
    let input = dir(key)?;
    let (id, name) = chosen(input).unwrap_or_default();
    match key.rsplit('_').next()? {
        "dev" => {
            let list = devices(input);
            let names: Vec<String> = list.iter().map(|(_, n)| n.clone()).collect();
            if let Some(i) = super::pick(owner, &names, list.iter().position(|(d, _)| *d == id)) {
                CHOSEN.lock().unwrap_or_else(|e| e.into_inner())[input as usize] = Some(list[i].0.clone());
            }
        }
        "format" => {
            let list = formats(&id);
            if list.is_empty() {
                super::tell(format!("{name} didn't say which formats it can play."));
                return Some(());
            }
            let now = current_format(&id).map(|(r, b, _, _)| (r, b));
            let names: Vec<String> = list.iter().map(|(r, b)| format_name(*r, *b)).collect();
            if let Some(i) = super::pick(owner, &names, list.iter().position(|f| Some(*f) == now)) {
                let ok = set_format(&id, list[i].0, list[i].1);
                config::log(&format!("sound: {name} format {} ({ok})", names[i]));
                if !ok {
                    super::tell(format!("Windows wouldn't set {name} to {}.", names[i]));
                }
            }
        }
        "default" => {
            let ok = set_default(&id);
            config::log(&format!("sound: {name} is the default ({ok})"));
        }
        "comm" => {
            let ok =
                unsafe { Policy::new().is_some_and(|p| p.set_default(&HSTRING::from(id.as_str()), eCommunications)) };
            config::log(&format!("sound: {name} is the default for calls ({ok})"));
        }
        "disable" => {
            if super::confirm(owner, &format!("Turn off {name}? Turn it back on from Turned-off devices.")) {
                let ok = unsafe { Policy::new().is_some_and(|p| p.set_visible(&HSTRING::from(id.as_str()), false)) };
                config::log(&format!("sound: turned {name} off ({ok})"));
                CHOSEN.lock().unwrap_or_else(|e| e.into_inner())[input as usize] = None;
            }
        }
        _ => return None,
    }
    Some(())
}
