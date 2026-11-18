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
    let mut out = Vec::new();
    unsafe {
        let Some(e) = enumerator() else { return out };
        let Ok(list) = e.EnumAudioEndpoints(flow(input), DEVICE_STATE_ACTIVE) else { return out };
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
