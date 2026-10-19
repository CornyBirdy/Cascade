//! Volume (event-driven via endpoint callbacks, no polling) and the
//! visualizer (loopback capture + FFT), which only runs while it is on screen.
//!
//! The visualizer listens to the app that's playing (the media session's
//! app and the processes it started), not everything the speakers play, so
//! a voice call or a game alongside the music doesn't move the bars. Where
//! that app can't be found, or Windows is older than 10 version 2004, it
//! falls back to the whole mix.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::core::{implement, w, Interface, Result, BOOL, GUID, HRESULT, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, E_FAIL, HANDLE, HWND, LPARAM, PROPERTYKEY};
use windows::Win32::Media::Audio::Endpoints::*;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForSingleObject};
use windows::Win32::System::Variant::VT_BLOB;
use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowThreadProcessId, IsWindowVisible};

use crate::msg;

pub const BANDS: usize = 5;

// ---------------------------------------------------------------- volume --

#[implement(IAudioEndpointVolumeCallback)]
struct VolumeCallback;

impl IAudioEndpointVolumeCallback_Impl for VolumeCallback_Impl {
    fn OnNotify(&self, data: *mut AUDIO_VOLUME_NOTIFICATION_DATA) -> Result<()> {
        if let Some(d) = unsafe { data.as_ref() } {
            msg::post(msg::VOLUME, (d.fMasterVolume.clamp(0.0, 1.0) * 1000.0) as usize, d.bMuted.as_bool() as isize);
        }
        Ok(())
    }
}

#[implement(IMMNotificationClient)]
struct DeviceWatcher;

impl IMMNotificationClient_Impl for DeviceWatcher_Impl {
    fn OnDeviceStateChanged(&self, _: &PCWSTR, _: DEVICE_STATE) -> Result<()> {
        Ok(())
    }
    fn OnDeviceAdded(&self, _: &PCWSTR) -> Result<()> {
        Ok(())
    }
    fn OnDeviceRemoved(&self, _: &PCWSTR) -> Result<()> {
        Ok(())
    }
    fn OnDefaultDeviceChanged(&self, flow: EDataFlow, role: ERole, _: &PCWSTR) -> Result<()> {
        if flow == eRender && role == eConsole {
            msg::post(msg::AUDIO_DEVICE, 0, 0);
        }
        Ok(())
    }
    fn OnPropertyValueChanged(&self, _: &PCWSTR, _: &PROPERTYKEY) -> Result<()> {
        Ok(())
    }
}

pub struct Volume {
    enumerator: IMMDeviceEnumerator,
    watcher: IMMNotificationClient,
    callback: IAudioEndpointVolumeCallback,
    endpoint: Option<IAudioEndpointVolume>,
}

impl Volume {
    pub fn new() -> Result<Self> {
        unsafe {
            let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let watcher: IMMNotificationClient = DeviceWatcher.into();
            enumerator.RegisterEndpointNotificationCallback(&watcher)?;
            let mut v = Self { enumerator, watcher, callback: VolumeCallback.into(), endpoint: None };
            v.rebind();
            Ok(v)
        }
    }

    /// Follows the default output device.
    pub fn rebind(&mut self) {
        unsafe {
            if let Some(ep) = self.endpoint.take() {
                let _ = ep.UnregisterControlChangeNotify(&self.callback);
            }
            let ep: Result<IAudioEndpointVolume> =
                self.enumerator.GetDefaultAudioEndpoint(eRender, eConsole).and_then(|d| d.Activate(CLSCTX_ALL, None));
            if let Ok(ep) = ep {
                let _ = ep.RegisterControlChangeNotify(&self.callback);
                self.endpoint = Some(ep);
            }
        }
    }

    pub fn get(&self) -> Option<(f32, bool)> {
        let ep = self.endpoint.as_ref()?;
        unsafe { Some((ep.GetMasterVolumeLevelScalar().ok()?, ep.GetMute().ok()?.as_bool())) }
    }

    pub fn set(&self, level: f32) {
        let Some(ep) = &self.endpoint else { return };
        unsafe {
            if level > 0.0 {
                let _ = ep.SetMute(false, std::ptr::null());
            }
            let _ = ep.SetMasterVolumeLevelScalar(level.clamp(0.0, 1.0), std::ptr::null());
        }
    }

    pub fn nudge(&self, delta: f32) {
        let Some(ep) = &self.endpoint else { return };
        let Some((v, muted)) = self.get() else { return };
        unsafe {
            if muted && delta > 0.0 {
                let _ = ep.SetMute(false, std::ptr::null());
            }
            let _ = ep.SetMasterVolumeLevelScalar((v + delta).clamp(0.0, 1.0), std::ptr::null());
        }
    }
}

impl Drop for Volume {
    fn drop(&mut self) {
        unsafe {
            if let Some(ep) = self.endpoint.take() {
                let _ = ep.UnregisterControlChangeNotify(&self.callback);
            }
            let _ = self.enumerator.UnregisterEndpointNotificationCallback(&self.watcher);
        }
    }
}

// ------------------------------------------------------------ visualizer --

/// Packs band levels (0..1) into a WPARAM, 10 bits each.
pub fn pack(bands: &[f32; BANDS]) -> usize {
    bands.iter().enumerate().fold(0usize, |acc, (i, v)| acc | (((v.clamp(0.0, 1.0) * 1023.0) as usize) << (i * 10)))
}

pub fn unpack(w: usize) -> [f32; BANDS] {
    let mut out = [0.0; BANDS];
    for (i, o) in out.iter_mut().enumerate() {
        *o = ((w >> (i * 10)) & 1023) as f32 / 1023.0;
    }
    out
}

#[derive(Default)]
pub struct Visualizer {
    stop: Option<Arc<AtomicBool>>,
    /// The playing app's AppUserModelID (from the media session).
    source: String,
}

impl Visualizer {
    pub fn running(&self) -> bool {
        self.stop.is_some()
    }

    /// Listens to the app with media session `aumid` from now on.
    pub fn set_source(&mut self, aumid: &str) {
        if aumid != self.source {
            self.source = aumid.to_string();
            self.restart();
        }
    }

    pub fn set(&mut self, on: bool) {
        if on == self.running() {
            return;
        }
        if let Some(stop) = self.stop.take() {
            stop.store(true, Ordering::Relaxed);
        }
        if on {
            let stop = Arc::new(AtomicBool::new(false));
            self.stop = Some(stop.clone());
            let source = self.source.clone();
            std::thread::spawn(move || unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                let pid = source_process(&source);
                let _ = capture(&stop, pid);
            });
        }
    }

    pub fn restart(&mut self) {
        if self.running() {
            self.set(false);
            self.set(true);
        }
    }
}

impl Drop for Visualizer {
    fn drop(&mut self) {
        self.set(false);
    }
}

const FFT_SIZE: usize = 1024;
const IEEE_FLOAT: GUID = GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);
const BAND_EDGES: [f32; BANDS + 1] = [40.0, 150.0, 400.0, 1000.0, 3000.0, 10000.0];

/// The samples a capture client hands over.
struct Format {
    channels: usize,
    bits: u16,
    float: bool,
    rate: f32,
}

/// Everything the default output plays.
unsafe fn mix_client() -> Result<(IAudioClient, Format)> {
    let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
    let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
    let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
    let fmt = client.GetMixFormat()?;
    let channels = (*fmt).nChannels as usize;
    let bits = (*fmt).wBitsPerSample;
    let rate = (*fmt).nSamplesPerSec as f32;
    let tag = (*fmt).wFormatTag as u32;
    let float = if tag == 0xFFFE {
        let ext = fmt as *const WAVEFORMATEXTENSIBLE;
        std::ptr::addr_of!((*ext).SubFormat).read_unaligned() == IEEE_FLOAT
    } else {
        tag == 3
    };
    let init = client.Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, 1_000_000, 0, fmt, None);
    CoTaskMemFree(Some(fmt as *const _));
    init?;
    Ok((client, Format { channels, bits, float, rate }))
}

/// Says when Windows has made the process capture client.
#[implement(IActivateAudioInterfaceCompletionHandler)]
struct Activated(isize);

impl IActivateAudioInterfaceCompletionHandler_Impl for Activated_Impl {
    fn ActivateCompleted(&self, _: windows::core::Ref<'_, IActivateAudioInterfaceAsyncOperation>) -> Result<()> {
        unsafe { SetEvent(HANDLE(self.0 as *mut _)) }
    }
}

/// Closes an event handle when it goes.
struct Event(HANDLE);

impl Drop for Event {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// What process `pid` and the ones it started play, whatever the output
/// (Windows 10 2004 and later). Asked for as 48 kHz stereo floats.
unsafe fn app_client(pid: u32) -> Result<(IAudioClient, Format, Event)> {
    let mut params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: pid,
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
            },
        },
    };
    let raw = windows::core::imp::PROPVARIANT {
        Anonymous: windows::core::imp::PROPVARIANT_0 {
            Anonymous: windows::core::imp::PROPVARIANT_0_0 {
                vt: VT_BLOB.0,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: windows::core::imp::PROPVARIANT_0_0_0 {
                    blob: windows::core::imp::BLOB {
                        cbSize: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
                        pBlobData: &mut params as *mut _ as *mut u8,
                    },
                },
            },
        },
    };
    // The blob points at `params` on this stack: never let it be freed.
    let prop = std::mem::ManuallyDrop::new(windows::core::PROPVARIANT::from_raw(raw));
    let done = Event(CreateEventW(None, false, false, PCWSTR::null())?);
    let handler: IActivateAudioInterfaceCompletionHandler = Activated(done.0 .0 as isize).into();
    let op = ActivateAudioInterfaceAsync(
        w!("VAD\\Process_Loopback"),
        &IAudioClient::IID,
        Some(&*prop as *const _),
        &handler,
    )?;
    if WaitForSingleObject(done.0, 3000).0 != 0 {
        return Err(E_FAIL.into());
    }
    let mut result = HRESULT(0);
    let mut unknown: Option<windows::core::IUnknown> = None;
    op.GetActivateResult(&mut result, &mut unknown)?;
    result.ok()?;
    let client: IAudioClient = unknown.ok_or_else(|| windows::core::Error::from(E_FAIL))?.cast()?;
    let (channels, rate) = (2u16, 48_000u32);
    let fmt = WAVEFORMATEX {
        wFormatTag: 3, // IEEE float
        nChannels: channels,
        nSamplesPerSec: rate,
        nAvgBytesPerSec: rate * 4 * channels as u32,
        nBlockAlign: 4 * channels,
        wBitsPerSample: 32,
        cbSize: 0,
    };
    // This kind of client only works event-driven; the loop below still
    // checks on its own schedule, the event just has to be there.
    let ready = Event(CreateEventW(None, false, false, PCWSTR::null())?);
    client.Initialize(
        AUDCLNT_SHAREMODE_SHARED,
        AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
        1_000_000,
        0,
        &fmt,
        None,
    )?;
    client.SetEventHandle(ready.0)?;
    Ok((client, Format { channels: channels as usize, bits: 32, float: true, rate: rate as f32 }, ready))
}

/// The process to listen to for media session `aumid`: the top of the app's
/// own process tree (a browser's main process, not its audio helper), so its
/// whole tree is heard. None if it can't be told (the whole mix is used).
fn source_process(aumid: &str) -> Option<u32> {
    if aumid.is_empty() {
        return None;
    }
    let procs = processes();
    let exe = |pid: u32| procs.iter().find(|p| p.0 == pid).map(|p| p.2.as_str());
    // Up the tree while the parent is the same program.
    let root = |mut pid: u32| {
        for _ in 0..16 {
            let Some(&(_, parent, ref name)) = procs.iter().find(|p| p.0 == pid) else { break };
            if parent == 0 || parent == pid || exe(parent) != Some(name.as_str()) {
                break;
            }
            pid = parent;
        }
        pid
    };
    let lower = aumid.to_ascii_lowercase();
    if let Some((family, _)) = lower.split_once('!') {
        // A Store app: "Name_publisherid!App". Its processes run from
        // WindowsApps\Name_version_arch__publisherid\.
        let (name, publisher) = family.rsplit_once('_')?;
        let (dir, tail) = (format!("\\windowsapps\\{name}_"), format!("__{publisher}\\"));
        let found = procs.iter().find(|p| {
            crate::dock::process_path(p.0)
                .map(|path| path.to_ascii_lowercase())
                .is_some_and(|path| path.contains(&dir) && path.contains(&tail))
        })?;
        return Some(root(found.0));
    }
    // An app that names its own windows with the id (Firefox, among others).
    if let Some(pid) = window_with_aumid(aumid) {
        return Some(root(pid));
    }
    // Otherwise the id is the program's name: "Spotify.exe", "Chrome", "MSEdge".
    let stem = lower.trim_end_matches(".exe");
    let found = procs.iter().find(|p| p.2.trim_end_matches(".exe") == stem)?;
    Some(root(found.0))
}

/// Every process: (id, parent, exe name in lower case).
fn processes() -> Vec<(u32, u32, String)> {
    let mut out = Vec::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return out };
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut more = Process32FirstW(snap, &mut e).is_ok();
        while more {
            let n = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
            let name = String::from_utf16_lossy(&e.szExeFile[..n]).to_ascii_lowercase();
            out.push((e.th32ProcessID, e.th32ParentProcessID, name));
            more = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    out
}

/// The process with a visible window carrying AppUserModelID `aumid`.
fn window_with_aumid(aumid: &str) -> Option<u32> {
    struct Find<'a> {
        aumid: &'a str,
        pid: u32,
    }
    unsafe extern "system" fn each(h: HWND, lp: LPARAM) -> BOOL {
        let f = &mut *(lp.0 as *mut Find);
        if IsWindowVisible(h).as_bool() && crate::dock::app_user_model_id(h).is_some_and(|a| a == f.aumid) {
            GetWindowThreadProcessId(h, Some(&mut f.pid));
            return BOOL(0);
        }
        BOOL(1)
    }
    let mut f = Find { aumid, pid: 0 };
    unsafe {
        let _ = EnumWindows(Some(each), LPARAM(&mut f as *mut _ as isize));
    }
    (f.pid != 0).then_some(f.pid)
}

/// Captures what `source` (a process, with what it started) plays, or the
/// whole mix, and posts the bands until `stop`.
unsafe fn capture(stop: &AtomicBool, source: Option<u32>) -> Result<()> {
    let app = source.and_then(|pid| app_client(pid).ok());
    let (client, fmt, _ready) = match app {
        Some((c, f, e)) => (c, f, Some(e)),
        None => {
            let (c, f) = mix_client()?;
            (c, f, None)
        }
    };
    let Format { channels, bits, float, rate } = fmt;
    if channels == 0 || !(float && bits == 32 || !float && (bits == 16 || bits == 32)) {
        return Ok(());
    }
    let cap: IAudioCaptureClient = client.GetService()?;
    client.Start()?;

    let window: Vec<f32> = (0..FFT_SIZE)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (FFT_SIZE - 1) as f32).cos())
        .collect();
    let mut ring = vec![0f32; FFT_SIZE];
    let mut head = 0usize;
    let mut re = vec![0f32; FFT_SIZE];
    let mut im = vec![0f32; FFT_SIZE];
    let mut peaks = [1e-3f32; BANDS];
    let mut last_post = Instant::now();
    let bin_hz = rate / FFT_SIZE as f32;

    // Short waits and frequent posts keep the bars on the beat: what reaches
    // the screen is at most ~20 ms behind what was played (the bars jump up
    // at once and only ease down; see `msg::SPECTRUM` in ui).
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(8));
        let mut fresh = false;
        loop {
            let n = cap.GetNextPacketSize()?;
            if n == 0 {
                break;
            }
            let mut data: *mut u8 = std::ptr::null_mut();
            let mut frames = 0u32;
            let mut flags = 0u32;
            cap.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
            let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
            for f in 0..frames as usize {
                let mut sum = 0f32;
                if !silent && !data.is_null() {
                    for c in 0..channels {
                        let i = f * channels + c;
                        sum += match (float, bits) {
                            (true, _) => *(data as *const f32).add(i),
                            (false, 16) => *(data as *const i16).add(i) as f32 / 32768.0,
                            _ => *(data as *const i32).add(i) as f32 / 2147483648.0,
                        };
                    }
                }
                ring[head] = sum / channels as f32;
                head = (head + 1) % FFT_SIZE;
            }
            cap.ReleaseBuffer(frames)?;
            fresh |= frames > 0;
        }

        if !fresh || last_post.elapsed() < Duration::from_millis(12) {
            continue;
        }
        last_post = Instant::now();
        for i in 0..FFT_SIZE {
            re[i] = ring[(head + i) % FFT_SIZE] * window[i];
            im[i] = 0.0;
        }
        fft(&mut re, &mut im);
        let mut bands = [0f32; BANDS];
        for (b, out) in bands.iter_mut().enumerate() {
            let lo = ((BAND_EDGES[b] / bin_hz) as usize).max(1);
            let hi = ((BAND_EDGES[b + 1] / bin_hz) as usize).clamp(lo + 1, FFT_SIZE / 2);
            let mag = (lo..hi).map(|k| (re[k] * re[k] + im[k] * im[k]).sqrt()).sum::<f32>() / (hi - lo) as f32;
            // Each band normalises against its own slowly decaying peak, so
            // quiet and loud tracks both fill the bars.
            peaks[b] = (peaks[b] * 0.996).max(mag).max(1e-3);
            *out = (mag / peaks[b]).sqrt();
        }
        msg::post(msg::SPECTRUM, pack(&bands), 0);
    }
    let _ = client.Stop();
    Ok(())
}

/// In-place iterative radix-2 FFT.
fn fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -2.0 * std::f32::consts::PI / len as f32;
        let (wr, wi) = (ang.cos(), ang.sin());
        for start in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0f32, 0.0f32);
            for k in 0..len / 2 {
                let (a, b) = (start + k, start + k + len / 2);
                let tr = re[b] * cr - im[b] * ci;
                let ti = re[b] * ci + im[b] * cr;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
                let nr = cr * wr - ci * wi;
                ci = cr * wi + ci * wr;
                cr = nr;
            }
        }
        len <<= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_roundtrip() {
        let b = [0.0, 0.25, 0.5, 0.75, 1.0];
        let u = unpack(pack(&b));
        for i in 0..BANDS {
            assert!((u[i] - b[i]).abs() < 0.002);
        }
    }

    #[test]
    fn fft_finds_tone() {
        let n = 64;
        let mut re: Vec<f32> = (0..n).map(|i| (2.0 * std::f32::consts::PI * 5.0 * i as f32 / n as f32).sin()).collect();
        let mut im = vec![0.0; n];
        fft(&mut re, &mut im);
        let mags: Vec<f32> = (0..n / 2).map(|k| (re[k] * re[k] + im[k] * im[k]).sqrt()).collect();
        let peak = mags.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap().0;
        assert_eq!(peak, 5);
    }
}
