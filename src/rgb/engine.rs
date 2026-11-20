//! The lighting engine (`cascade.exe --rgb-engine`): a small process of its
//! own, at normal priority, so lighting never slows the taskbar or takes it
//! down. It keeps OpenRGB running and connected, puts each device in direct
//! mode (or the hardware mode the profile leaves it on), works out every
//! LED's colour from the profile's layers (see `effects`) and sends what
//! changed. Still effects are sent once; moving ones at the frame rate set
//! (30 by default). What reacting effects need is only read while a layer
//! uses it: the music (a loopback capture), temperatures, the screen (shrunk
//! to 64 x 36), keys pressed (a keyboard hook).
//!
//! It switches profile with the app in front (a profile names the programs
//! it's for), turns the lights off while the PC is locked or its screens are
//! asleep, and picks up `lighting.json` as the Lighting app saves it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::core::{w, HSTRING};
use windows::Win32::Foundation::*;
use windows::Win32::UI::Accessibility::HWINEVENTHOOK;
use windows::Win32::UI::WindowsAndMessaging::*;

use super::effects::{self, Context, Kind, Led, Rgb};
use super::model::{led_positions, Lighting};
use super::openrgb::{pack, Client, Controller, Mode};
use crate::config;

/// What the window thread hears and the render thread reads.
#[derive(Default)]
struct Shared {
    /// The app in front (exe name, lower case).
    foreground: Mutex<Option<String>>,
    /// Locked, or the screens asleep.
    idle: AtomicBool,
    /// Keys pressed: (virtual-key code, when).
    keys: Mutex<Vec<(u16, Instant)>>,
    /// The render thread wants the keyboard hook.
    want_keys: AtomicBool,
    /// Audio: (spectrum bands, level), and whether it's wanted.
    audio: Mutex<(Vec<f32>, f32)>,
    want_audio: AtomicBool,
}

static SHARED: std::sync::OnceLock<Arc<Shared>> = std::sync::OnceLock::new();

fn shared() -> Arc<Shared> {
    SHARED.get_or_init(|| Arc::new(Shared::default())).clone()
}

/// A device as the engine drives it.
struct Device {
    c: Controller,
    /// Each LED's place and zone, for the effects.
    leds: Vec<(Led, usize)>,
    /// What was last sent, and when.
    sent: Vec<u32>,
    sent_at: Instant,
    /// On one of its own modes (the profile says so): not driven.
    hardware: bool,
    off: bool,
}

fn exe_of(h: HWND) -> Option<String> {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    let path = crate::dock::process_path(pid)?;
    Some(path.rsplit('\\').next().unwrap_or(&path).to_ascii_lowercase())
}

// ------------------------------------------------------------ the inputs --

/// The music: a loopback capture of what the PC plays, as 16 bands and a level.
fn audio_thread() {
    use windows::Win32::Media::Audio::*;
    std::thread::spawn(|| unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
        const N: usize = 1024;
        const BANDS: usize = 16;
        let s = shared();
        while s.want_audio.load(Ordering::Relaxed) {
            let Ok((client, fmt)) = crate::audio::mix_client() else {
                std::thread::sleep(Duration::from_secs(2));
                continue;
            };
            let Ok(cap) = client.GetService::<IAudioCaptureClient>() else { break };
            let _ = client.Start();
            let mut ring = vec![0f32; N];
            let mut head = 0;
            let mut peaks = [1e-3f32; BANDS];
            let mut quiet = Instant::now();
            while s.want_audio.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(15));
                loop {
                    let Ok(n) = cap.GetNextPacketSize() else { break };
                    if n == 0 {
                        break;
                    }
                    let mut data: *mut u8 = std::ptr::null_mut();
                    let (mut frames, mut flags) = (0u32, 0u32);
                    if cap.GetBuffer(&mut data, &mut frames, &mut flags, None, None).is_err() {
                        break;
                    }
                    let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
                    for f in 0..frames as usize {
                        let mut sum = 0f32;
                        if !silent && !data.is_null() {
                            for ch in 0..fmt.channels {
                                let i = f * fmt.channels + ch;
                                sum += match (fmt.float, fmt.bits) {
                                    (true, _) => *(data as *const f32).add(i),
                                    (false, 16) => *(data as *const i16).add(i) as f32 / 32768.0,
                                    _ => *(data as *const i32).add(i) as f32 / 2147483648.0,
                                };
                            }
                        }
                        ring[head] = sum / fmt.channels.max(1) as f32;
                        head = (head + 1) % N;
                    }
                    let _ = cap.ReleaseBuffer(frames);
                    quiet = Instant::now();
                }
                let (mut re, mut im) = (vec![0f32; N], vec![0f32; N]);
                for i in 0..N {
                    let w = 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / (N - 1) as f32).cos();
                    re[i] = ring[(head + i) % N] * w;
                }
                crate::audio::fft(&mut re, &mut im);
                // Bands spaced evenly in pitch, 40 Hz to 16 kHz.
                let bin = fmt.rate / N as f32;
                let mut bands = vec![0f32; BANDS];
                for (b, out) in bands.iter_mut().enumerate() {
                    let lo_hz = 40.0 * (400.0f32).powf(b as f32 / BANDS as f32);
                    let hi_hz = 40.0 * (400.0f32).powf((b + 1) as f32 / BANDS as f32);
                    let lo = ((lo_hz / bin) as usize).max(1);
                    let hi = ((hi_hz / bin) as usize).clamp(lo + 1, N / 2);
                    let mag = (lo..hi).map(|k| (re[k] * re[k] + im[k] * im[k]).sqrt()).sum::<f32>() / (hi - lo) as f32;
                    peaks[b] = (peaks[b] * 0.995).max(mag).max(1e-3);
                    *out = (mag / peaks[b]).sqrt().min(1.0);
                }
                let level = if quiet.elapsed() > Duration::from_millis(300) {
                    0.0
                } else {
                    bands.iter().sum::<f32>() / BANDS as f32
                };
                *s.audio.lock().unwrap_or_else(|e| e.into_inner()) = (bands, level);
            }
            let _ = client.Stop();
        }
    });
}

/// The CPU's temperature from Windows' thermal zones (°C), if it has any.
struct CpuTemp {
    query: isize,
    counter: isize,
}

impl CpuTemp {
    fn new() -> Option<CpuTemp> {
        use windows::Win32::System::Performance::*;
        unsafe {
            let mut q = PDH_HQUERY::default();
            if PdhOpenQueryW(windows::core::PCWSTR::null(), 0, &mut q) != 0 {
                return None;
            }
            let mut c = PDH_HCOUNTER::default();
            if PdhAddEnglishCounterW(q, w!("\\Thermal Zone Information(*)\\Temperature"), 0, &mut c) != 0 {
                return None;
            }
            let _ = PdhCollectQueryData(q);
            Some(CpuTemp { query: q.0 as isize, counter: c.0 as isize })
        }
    }

    fn read(&self) -> Option<f32> {
        use windows::Win32::System::Performance::*;
        unsafe {
            let q = PDH_HQUERY(self.query as *mut _);
            let c = PDH_HCOUNTER(self.counter as *mut _);
            if PdhCollectQueryData(q) != 0 {
                return None;
            }
            let (mut size, mut count) = (0u32, 0u32);
            let _ = PdhGetFormattedCounterArrayW(c, PDH_FMT_DOUBLE, &mut size, &mut count, None);
            if size == 0 {
                return None;
            }
            let mut buf = vec![0u8; size as usize];
            if PdhGetFormattedCounterArrayW(c, PDH_FMT_DOUBLE, &mut size, &mut count, Some(buf.as_mut_ptr() as *mut _))
                != 0
            {
                return None;
            }
            let items = std::slice::from_raw_parts(buf.as_ptr() as *const PDH_FMT_COUNTERVALUE_ITEM_W, count as usize);
            // Kelvin; the hottest zone is the nearest to the CPU.
            items
                .iter()
                .map(|i| i.FmtValue.Anonymous.doubleValue as f32 - 273.15)
                .filter(|t| *t > 0.0 && *t < 130.0)
                .reduce(f32::max)
        }
    }
}

/// The main screen, shrunk to 64 x 36, as RGB bytes.
fn grab_screen() -> Option<(usize, usize, Vec<u8>)> {
    use windows::Win32::Graphics::Gdi::*;
    const W: i32 = 64;
    const H: i32 = 36;
    unsafe {
        let (sw, sh) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: W,
                biHeight: -H,
                biPlanes: 1,
                biBitCount: 32,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let bmp = CreateDIBSection(Some(mem), &info, DIB_RGB_COLORS, &mut bits, None, 0).ok();
        let mut out = None;
        if let Some(bmp) = bmp {
            let old = SelectObject(mem, bmp.into());
            SetStretchBltMode(mem, HALFTONE);
            if StretchBlt(mem, 0, 0, W, H, Some(screen), 0, 0, sw, sh, SRCCOPY).as_bool() && !bits.is_null() {
                let px = std::slice::from_raw_parts(bits as *const u8, (W * H * 4) as usize);
                let rgb: Vec<u8> = px.as_chunks::<4>().0.iter().flat_map(|p| [p[2], p[1], p[0]]).collect();
                out = Some((W as usize, H as usize, rgb));
            }
            SelectObject(mem, old);
            let _ = DeleteObject(bmp.into());
        }
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        out
    }
}

// ------------------------------------------------------------ rendering --

/// Works out a hardware mode for `d` as the profile has it set.
fn hardware_mode(d: &Controller, hw: &super::model::Hardware) -> Option<(usize, Mode)> {
    let i = d.modes.iter().position(|m| m.name.eq_ignore_ascii_case(&hw.mode))?;
    let mut m = d.modes[i].clone();
    let scale = |lo: u32, hi: u32, t: f32| {
        // Some devices count speed downwards (max < min).
        let (lo, hi) = (lo as f32, hi as f32);
        (lo + (hi - lo) * t.clamp(0.0, 1.0)).round() as u32
    };
    m.speed = scale(m.speed_min, m.speed_max, hw.speed);
    if m.brightness_max != m.brightness_min {
        m.brightness = scale(m.brightness_min, m.brightness_max, hw.brightness);
    }
    m.direction = hw.direction;
    if !hw.colours.is_empty() && m.colors_max > 0 {
        let n = (hw.colours.len() as u32).clamp(m.colors_min.max(1), m.colors_max) as usize;
        m.colors = (0..n)
            .map(|k| hw.colours[k.min(hw.colours.len() - 1)])
            .map(|c| pack((c >> 16) as u8, (c >> 8) as u8, c as u8))
            .collect();
    }
    Some((i, m))
}

struct Engine {
    lighting: Lighting,
    modified: Option<std::time::SystemTime>,
    client: Option<Client>,
    devices: Vec<Device>,
    started_server: Option<Instant>,
    profile: usize,
    blank: bool,
    start: Instant,
    cpu: Option<CpuTemp>,
    nvml: crate::nvml::Nvml,
    temps: (f32, f32, Instant),
    screen: (Option<(usize, usize, Vec<u8>)>, Instant),
    configured: bool,
}

impl Engine {
    fn reload(&mut self) -> bool {
        let m = std::fs::metadata(Lighting::path()).and_then(|m| m.modified()).ok();
        if m == self.modified && self.modified.is_some() {
            return false;
        }
        self.modified = m;
        self.lighting = Lighting::load();
        true
    }

    fn connect(&mut self) {
        let s = &self.lighting.settings;
        if !super::hw::running(s.port) {
            // Started once; after that it's given time to come up.
            if self.started_server.is_none_or(|t| t.elapsed() > Duration::from_secs(30)) {
                self.started_server = Some(Instant::now());
                if !super::hw::start(&s.openrgb, s.port, s.admin) {
                    config::log("lighting: OpenRGB isn't here yet (the Lighting app can get it)");
                }
            }
            return;
        }
        match Client::connect(s.port, "Cascade") {
            Ok(mut c) => match c.controllers() {
                Ok(list) => {
                    config::log(&format!("lighting: connected (protocol {}), {} devices", c.version, list.len()));
                    self.client = Some(c);
                    self.set_devices(list);
                }
                Err(e) => config::log(&format!("lighting: couldn't read the devices: {e}")),
            },
            Err(e) => config::log(&format!("lighting: couldn't connect to OpenRGB: {e}")),
        }
    }

    fn set_devices(&mut self, list: Vec<Controller>) {
        let before = self.lighting.layout.len();
        self.lighting.place_new(&list);
        if self.lighting.layout.len() != before {
            self.lighting.save();
            self.modified = std::fs::metadata(Lighting::path()).and_then(|m| m.modified()).ok();
        }
        self.devices = list
            .into_iter()
            .map(|c| Device {
                c,
                leds: Vec::new(),
                sent: Vec::new(),
                sent_at: Instant::now(),
                hardware: false,
                off: false,
            })
            .collect();
        self.configured = false;
    }

    /// Positions, and direct or hardware mode for each device, as the profile says.
    fn configure(&mut self) {
        let Some(client) = self.client.as_mut() else { return };
        let p = &self.lighting.profiles[self.profile.min(self.lighting.profiles.len() - 1)];
        for d in &mut self.devices {
            let key = d.c.key();
            let place = self.lighting.layout.get(&key).copied().unwrap_or_default();
            d.off = place.off;
            d.leds = led_positions(&d.c, &place);
            d.sent.clear();
            match p.hardware.get(&key).and_then(|hw| hardware_mode(&d.c, hw).map(|m| (m, hw.save))) {
                Some(((i, m), save)) => {
                    d.hardware = true;
                    let _ = client.set_mode(d.c.index, i, &m, save);
                }
                None => {
                    d.hardware = false;
                    if !d.off {
                        let _ = client.custom_mode(d.c.index);
                    }
                }
            }
        }
        self.configured = true;
    }

    fn frame(&mut self) -> Duration {
        let reloaded = self.reload();
        let s = shared();
        let settings = self.lighting.settings.clone();
        let fg = s.foreground.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let want = self.lighting.profile_for(fg.as_deref());
        if want != self.profile || reloaded {
            self.profile = want;
            self.configured = false;
        }
        // Off: black once, then nothing.
        let dark = !settings.on || (settings.off_when_idle && s.idle.load(Ordering::Relaxed));
        if self.client.is_none() {
            if !dark {
                self.connect();
            }
            if self.client.is_none() {
                return Duration::from_secs(2);
            }
        }
        if self.client.as_mut().is_some_and(|c| c.take_list_changed()) {
            if let Ok(list) = self.client.as_mut().map(|c| c.controllers()).unwrap_or(Ok(Vec::new())) {
                self.set_devices(list);
            }
        }
        if dark {
            if !self.blank {
                self.blank = true;
                let client = self.client.as_mut().expect("connected");
                for d in &self.devices {
                    let _ = client.custom_mode(d.c.index);
                    let _ = client.set_leds(d.c.index, &vec![0; d.c.leds.len()]);
                }
            }
            return Duration::from_millis(300);
        }
        if self.blank {
            self.blank = false;
            self.configured = false;
        }
        if !self.configured {
            self.configure();
        }
        let profile = self.lighting.profiles[self.profile.min(self.lighting.profiles.len() - 1)].clone();
        let layers: Vec<&super::model::Layer> = profile.layers.iter().filter(|l| l.on).collect();
        let kinds: Vec<Kind> = layers.iter().map(|l| l.effect.kind).collect();
        let moving = kinds.iter().any(|k| k.moves());
        // What the layers react to, read only while one does.
        s.want_keys.store(kinds.iter().any(|k| matches!(k, Kind::TypeKey | Kind::TypeRipple)), Ordering::Relaxed);
        let audio = kinds.iter().any(|k| matches!(k, Kind::AudioVisualizer | Kind::AudioPulse));
        if audio && !s.want_audio.swap(true, Ordering::Relaxed) {
            audio_thread();
        } else if !audio {
            s.want_audio.store(false, Ordering::Relaxed);
        }
        let mut ctx = Context { t: self.start.elapsed().as_secs_f32(), ..Default::default() };
        if audio {
            let (b, l) = s.audio.lock().unwrap_or_else(|e| e.into_inner()).clone();
            ctx.bands = b;
            ctx.level = l;
        }
        if kinds.contains(&Kind::Temperature) {
            if self.temps.2.elapsed() > Duration::from_secs(1) {
                if self.cpu.is_none() {
                    self.cpu = CpuTemp::new();
                }
                let cpu = self.cpu.as_ref().and_then(|c| c.read()).unwrap_or(self.temps.0);
                let gpu = self.nvml.read().iter().filter_map(|g| g.temp).reduce(f32::max).unwrap_or(self.temps.1);
                self.temps = (cpu, gpu, Instant::now());
            }
            ctx.cpu_temp = self.temps.0;
            ctx.gpu_temp = self.temps.1;
        }
        if kinds.contains(&Kind::ScreenAmbience) {
            let every = Duration::from_millis(1000 / settings.screen_fps.clamp(1, 60) as u64);
            if self.screen.1.elapsed() >= every {
                self.screen = (grab_screen(), Instant::now());
            }
            ctx.screen = self.screen.0.clone();
        }
        if s.want_keys.load(Ordering::Relaxed) {
            let mut keys = s.keys.lock().unwrap_or_else(|e| e.into_inner());
            keys.retain(|(_, t)| t.elapsed() < Duration::from_secs(3));
            // Where each key is: the first LED that lights it.
            let mut at: HashMap<u16, (f32, f32)> = HashMap::new();
            for d in &self.devices {
                for (l, _) in &d.leds {
                    if let Some(k) = l.key {
                        at.entry(k).or_insert((l.x, l.y));
                    }
                }
            }
            ctx.keys = keys
                .iter()
                .filter_map(|(vk, t)| at.get(vk).map(|(x, y)| (*vk, *x, *y, t.elapsed().as_secs_f32())))
                .collect();
        }
        let bright = (settings.brightness * profile.brightness).clamp(0.0, 1.0);
        let client = self.client.as_mut().expect("connected");
        let mut failed = false;
        for d in &mut self.devices {
            if d.hardware || d.off {
                continue;
            }
            let key = d.c.key();
            let colours: Vec<u32> = d
                .leds
                .iter()
                .map(|(led, zone)| {
                    let mut c = Rgb::BLACK;
                    for l in &layers {
                        if l.covers(&key, *zone) {
                            c = effects::blend(
                                c,
                                effects::colour(&l.effect, led, &ctx),
                                l.blend,
                                l.opacity.clamp(0.0, 1.0),
                            );
                        }
                    }
                    let [r, g, b] = c.scale(bright).to_bytes();
                    pack(r, g, b)
                })
                .collect();
            // Only what changed; and every few seconds anyway (some devices drop back to their own mode).
            if colours != d.sent || d.sent_at.elapsed() > Duration::from_secs(4) {
                if client.set_leds(d.c.index, &colours).is_err() {
                    failed = true;
                    break;
                }
                d.sent = colours;
                d.sent_at = Instant::now();
            }
        }
        if failed {
            config::log("lighting: lost OpenRGB; reconnecting");
            self.client = None;
            return Duration::from_secs(1);
        }
        if moving {
            Duration::from_millis(1000 / settings.fps.clamp(5, 120) as u64)
        } else {
            Duration::from_millis(250)
        }
    }
}

fn render_thread() {
    std::thread::spawn(|| {
        let mut e = Engine {
            lighting: Lighting::load(),
            modified: None,
            client: None,
            devices: Vec::new(),
            started_server: None,
            profile: 0,
            blank: false,
            start: Instant::now(),
            cpu: None,
            nvml: crate::nvml::Nvml::default(),
            temps: (0.0, 0.0, Instant::now() - Duration::from_secs(5)),
            screen: (None, Instant::now() - Duration::from_secs(5)),
            configured: false,
        };
        loop {
            let t = Instant::now();
            let wait = e.frame();
            if let Some(rest) = wait.checked_sub(t.elapsed()) {
                std::thread::sleep(rest);
            }
        }
    });
}

// --------------------------------------------------------- the window --

unsafe extern "system" fn key_hook(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && (wp.0 as u32 == WM_KEYDOWN || wp.0 as u32 == WM_SYSKEYDOWN) {
        let k = &*(lp.0 as *const KBDLLHOOKSTRUCT);
        let s = shared();
        let mut keys = s.keys.lock().unwrap_or_else(|e| e.into_inner());
        // Held keys repeat: only the first press counts.
        if !keys.iter().any(|(vk, t)| *vk == k.vkCode as u16 && t.elapsed() < Duration::from_millis(400)) {
            keys.push((k.vkCode as u16, Instant::now()));
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

unsafe extern "system" fn win_event(_: HWINEVENTHOOK, _: u32, hwnd: HWND, _: i32, _: i32, _: u32, _: u32) {
    let exe = exe_of(hwnd);
    *shared().foreground.lock().unwrap_or_else(|e| e.into_inner()) = exe;
}

const TIMER_HOOKS: usize = 1;

unsafe extern "system" fn proc(hwnd: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    thread_local! {
        static HOOK: std::cell::Cell<Option<HHOOK>> = const { std::cell::Cell::new(None) };
    }
    match m {
        WM_TIMER if wp.0 == TIMER_HOOKS => {
            // The keyboard hook only while type lighting is on.
            let want = shared().want_keys.load(Ordering::Relaxed);
            HOOK.with(|h| match (want, h.get()) {
                (true, None) => h.set(SetWindowsHookExW(WH_KEYBOARD_LL, Some(key_hook), None, 0).ok()),
                (false, Some(hook)) => {
                    let _ = UnhookWindowsHookEx(hook);
                    h.set(None);
                }
                _ => {}
            });
            LRESULT(0)
        }
        // Locked and unlocked (WTS_SESSION_LOCK / UNLOCK).
        0x02B1 => {
            match wp.0 {
                7 => shared().idle.store(true, Ordering::Relaxed),
                8 => shared().idle.store(false, Ordering::Relaxed),
                _ => {}
            }
            LRESULT(0)
        }
        // The screens going to sleep and waking (GUID_CONSOLE_DISPLAY_STATE).
        WM_POWERBROADCAST if wp.0 == 0x8013 => {
            let ps = &*(lp.0 as *const windows::Win32::System::Power::POWERBROADCAST_SETTING);
            let state = ps.Data[0];
            shared().idle.store(state == 0, Ordering::Relaxed);
            LRESULT(1)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, m, wp, lp),
    }
}

/// `cascade.exe --rgb-engine`.
pub fn run() {
    unsafe {
        let _ = windows::Win32::System::Threading::CreateMutexW(None, true, w!("CascadeRgbEngine"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return;
        }
        let inst = crate::appwin::instance();
        let wc = WNDCLASSW {
            lpfnWndProc: Some(proc),
            hInstance: inst,
            lpszClassName: w!("CascadeRgbEngine"),
            ..Default::default()
        };
        RegisterClassW(&wc);
        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("CascadeRgbEngine"),
            &HSTRING::from("Cascade lighting"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(inst),
            None,
        ) else {
            return;
        };
        let _ = windows::Win32::System::RemoteDesktop::WTSRegisterSessionNotification(hwnd, 0);
        const DISPLAY_STATE: windows::core::GUID =
            windows::core::GUID::from_u128(0x6fe69556_704a_47a0_8f24_c28d936fda47);
        let _ = windows::Win32::System::Power::RegisterPowerSettingNotification(
            HANDLE(hwnd.0),
            &DISPLAY_STATE,
            REGISTER_NOTIFICATION_FLAGS(0),
        );
        let _ = windows::Win32::UI::Accessibility::SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            None,
            Some(win_event),
            0,
            0,
            WINEVENT_OUTOFCONTEXT,
        );
        win_event(HWINEVENTHOOK::default(), 0, GetForegroundWindow(), 0, 0, 0, 0);
        SetTimer(Some(hwnd), TIMER_HOOKS, 1000, None);
        config::log("lighting: engine started");
        render_thread();
        let mut m = MSG::default();
        while GetMessageW(&mut m, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&m);
            DispatchMessageW(&m);
        }
    }
}

/// Starts the engine (if lighting is on and it isn't running) or stops it.
pub fn ensure(on: bool) {
    unsafe {
        let running = FindWindowExW(Some(HWND_MESSAGE), None, w!("CascadeRgbEngine"), None).ok();
        match (on, running) {
            (true, None) => crate::appwin::launch(&["--rgb-engine"]),
            (false, Some(h)) => {
                let _ = PostMessageW(Some(h), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
            _ => {}
        }
    }
}
