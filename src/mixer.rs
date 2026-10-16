//! The volume mixer on the notch's Controls: each app playing sound with
//! its own volume, and the output device to pick.
//!
//! It's read when Controls opens (and again after a change), straight from
//! Windows' audio sessions; nothing is watched in between. Picking an output
//! uses the same interface Windows' own sound settings use to make a device
//! the default (it has no public name, but has been the same since Windows 7).

use std::ffi::c_void;

use windows::core::{Interface, Result, GUID, HSTRING, PWSTR};
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::System::Com::*;

/// PKEY_Device_FriendlyName: "Speakers (Realtek(R) Audio)".
const FRIENDLY_NAME: PROPERTYKEY =
    PROPERTYKEY { fmtid: GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0), pid: 14 };

/// The policy object behind Windows' sound settings, and its interface.
const POLICY_CONFIG: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);
const IID_POLICY_CONFIG: GUID = GUID::from_u128(0xf8679f50_850a_41cf_9c72_430f290290c8);
/// SetDefaultEndpoint's slot in that interface's table (after IUnknown's
/// three methods and ten others).
const SET_DEFAULT_ENDPOINT: usize = 13;

pub struct App {
    /// The app's name ("Spotify"), or "System sounds".
    pub name: String,
    pub level: f32,
    pub muted: bool,
    control: ISimpleAudioVolume,
}

pub struct Output {
    id: String,
    pub name: String,
}

#[derive(Default)]
pub struct Mixer {
    pub apps: Vec<App>,
    pub outputs: Vec<Output>,
    /// The default output's place in `outputs`.
    pub current: usize,
}

unsafe fn take(p: PWSTR) -> String {
    let s = p.to_string().unwrap_or_default();
    CoTaskMemFree(Some(p.0 as *const _));
    s
}

/// What to call the app with process `pid`.
fn app_name(pid: u32) -> String {
    let Some(path) = crate::dock::process_path(pid) else { return "App".into() };
    crate::icons::exe_description(&path).unwrap_or_else(|| {
        let file = path.rsplit(['\\', '/']).next().unwrap_or(&path);
        file.trim_end_matches(".exe").trim_end_matches(".EXE").to_string()
    })
}

impl Mixer {
    /// The apps playing through the default output, and the outputs there are.
    pub fn read() -> Self {
        Self::try_read().unwrap_or_default()
    }

    fn try_read() -> Result<Self> {
        unsafe {
            let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let default = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
            let default_id = take(default.GetId()?);

            let mut outputs = Vec::new();
            let devices = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
            for i in 0..devices.GetCount()? {
                let Ok(d) = devices.Item(i) else { continue };
                let Ok(id) = d.GetId().map(|p| take(p)) else { continue };
                let name = d
                    .OpenPropertyStore(STGM_READ)
                    .and_then(|s| s.GetValue(&FRIENDLY_NAME))
                    .and_then(|v| PropVariantToStringAlloc(&v))
                    .map(|p| take(p))
                    .unwrap_or_else(|_| "Output".into());
                outputs.push(Output { id, name });
            }
            let current = outputs.iter().position(|o| o.id == default_id).unwrap_or(0);

            let mut apps = Vec::new();
            let manager: IAudioSessionManager2 = default.Activate(CLSCTX_ALL, None)?;
            let sessions = manager.GetSessionEnumerator()?;
            for i in 0..sessions.GetCount()? {
                let Ok(session) = sessions.GetSession(i) else { continue };
                let Ok(session2) = session.cast::<IAudioSessionControl2>() else { continue };
                if session.GetState().is_ok_and(|s| s == AudioSessionStateExpired) {
                    continue;
                }
                let Ok(control) = session.cast::<ISimpleAudioVolume>() else { continue };
                let pid = session2.GetProcessId().unwrap_or(0);
                let name = if pid == 0 { "System sounds".to_string() } else { app_name(pid) };
                // One row per app (a browser plays through several sessions).
                if apps.iter().any(|a: &App| a.name == name) {
                    continue;
                }
                let level = control.GetMasterVolume().unwrap_or(1.0);
                let muted = control.GetMute().map(|m| m.as_bool()).unwrap_or(false);
                apps.push(App { name, level, muted, control });
            }
            // The system's own sounds last.
            apps.sort_by_key(|a| a.name == "System sounds");
            Ok(Self { apps, outputs, current })
        }
    }

    /// Sets app `i`'s volume (0 to 1), unmuting it.
    pub fn set_level(&mut self, i: usize, level: f32) {
        let Some(a) = self.apps.get_mut(i) else { return };
        a.level = level.clamp(0.0, 1.0);
        unsafe {
            if a.muted && level > 0.0 {
                a.muted = false;
                let _ = a.control.SetMute(false, std::ptr::null());
            }
            let _ = a.control.SetMasterVolume(a.level, std::ptr::null());
        }
    }

    /// Mutes or unmutes app `i`.
    pub fn toggle_mute(&mut self, i: usize) {
        let Some(a) = self.apps.get_mut(i) else { return };
        a.muted = !a.muted;
        unsafe {
            let _ = a.control.SetMute(a.muted, std::ptr::null());
        }
    }

    /// Makes output `i` the default, for everything (as Windows' own picker
    /// does). True if Windows took it.
    pub fn set_output(&mut self, i: usize) -> bool {
        let Some(o) = self.outputs.get(i) else { return false };
        let id = HSTRING::from(o.id.as_str());
        let ok = unsafe {
            let Ok(policy) = CoCreateInstance::<_, windows::core::IUnknown>(&POLICY_CONFIG, None, CLSCTX_ALL) else {
                return false;
            };
            let mut raw: *mut c_void = std::ptr::null_mut();
            if policy.query(&IID_POLICY_CONFIG, &mut raw).is_err() || raw.is_null() {
                return false;
            }
            // SetDefaultEndpoint(this, device id, role), for each role.
            type SetDefault =
                unsafe extern "system" fn(*mut c_void, windows::core::PCWSTR, ERole) -> windows::core::HRESULT;
            let table = *(raw as *const *const usize);
            let set: SetDefault = std::mem::transmute::<usize, SetDefault>(*table.add(SET_DEFAULT_ENDPOINT));
            let mut ok = true;
            for role in [eConsole, eMultimedia, eCommunications] {
                ok &= set(raw, windows::core::PCWSTR(id.as_ptr()), role).is_ok();
            }
            // Let go of the reference `query` took.
            drop(windows::core::IUnknown::from_raw(raw));
            ok
        };
        if ok {
            self.current = i;
        }
        ok
    }
}
