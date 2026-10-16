//! Per-card readings from NVIDIA's driver (NVML): load, temperature, video
//! memory, power, clock and fan, for each NVIDIA card in the machine.
//!
//! `nvml.dll` comes with the driver, so nothing extra is shipped; it's loaded
//! at run time only while a GPU reading is on screen, and let go again after.
//! Starting NVML takes a moment, so that happens on a short-lived thread.
//! Machines without an NVIDIA card (or driver) simply get no readings and the
//! performance-counter figure is used instead.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};

use windows::core::{s, w, HSTRING, PCSTR};
use windows::Win32::Foundation::{FreeLibrary, HMODULE};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32};

type Device = *mut c_void;
type Ret = i32;

// Laid out as NVML fills them in; not every field is read.
#[repr(C)]
#[derive(Default)]
#[allow(dead_code)]
struct Utilization {
    gpu: u32,
    memory: u32,
}

#[repr(C)]
#[derive(Default)]
#[allow(dead_code)]
struct Memory {
    total: u64,
    free: u64,
    used: u64,
}

type FnVoid = unsafe extern "C" fn() -> Ret;
type FnCount = unsafe extern "C" fn(*mut u32) -> Ret;
type FnHandle = unsafe extern "C" fn(u32, *mut Device) -> Ret;
type FnName = unsafe extern "C" fn(Device, *mut u8, u32) -> Ret;
type FnTemp = unsafe extern "C" fn(Device, u32, *mut u32) -> Ret;
type FnUtil = unsafe extern "C" fn(Device, *mut Utilization) -> Ret;
type FnMem = unsafe extern "C" fn(Device, *mut Memory) -> Ret;
type FnU32 = unsafe extern "C" fn(Device, *mut u32) -> Ret;
type FnClock = unsafe extern "C" fn(Device, u32, *mut u32) -> Ret;

/// One card's readings. Anything the card or driver doesn't report is None.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Gpu {
    /// Short name, e.g. "RTX 5070 Ti".
    pub name: String,
    /// Percent.
    pub load: Option<f32>,
    /// Degrees Celsius.
    pub temp: Option<f32>,
    /// Gigabytes.
    pub vram_used: Option<f32>,
    pub vram_total: Option<f32>,
    /// Watts.
    pub power: Option<f32>,
    /// Megahertz.
    pub clock: Option<u32>,
    /// Percent.
    pub fan: Option<u32>,
}

/// "NVIDIA GeForce RTX 5070 Ti" -> "RTX 5070 Ti".
pub fn short_name(name: &str) -> String {
    let n = name.trim();
    let n = n.strip_prefix("NVIDIA").unwrap_or(n).trim_start();
    let n = n.strip_prefix("GeForce").unwrap_or(n).trim_start();
    if n.is_empty() {
        name.trim().to_string()
    } else {
        n.to_string()
    }
}

struct Api {
    lib: HMODULE,
    shutdown: FnVoid,
    temp: Option<FnTemp>,
    util: Option<FnUtil>,
    mem: Option<FnMem>,
    power: Option<FnU32>,
    clock: Option<FnClock>,
    fan: Option<FnU32>,
    devices: Vec<(Device, String)>,
}

// NVML is thread-safe, and the handles only ever move from the thread that
// opened it to the UI thread.
unsafe impl Send for Api {}

impl Api {
    unsafe fn open() -> Option<Self> {
        let lib = LoadLibraryExW(w!("nvml.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32).ok().or_else(|| {
            // Older drivers keep it with nvidia-smi instead.
            let base = std::env::var("ProgramFiles").unwrap_or_else(|_| "C:\\Program Files".into());
            let path = HSTRING::from(format!("{base}\\NVIDIA Corporation\\NVSMI\\nvml.dll"));
            LoadLibraryExW(&path, None, Default::default()).ok()
        })?;
        let find = |name: PCSTR| GetProcAddress(lib, name);
        macro_rules! get {
            ($name:expr, $ty:ty) => {
                find($name).map(|f| std::mem::transmute::<unsafe extern "system" fn() -> isize, $ty>(f))
            };
        }
        let init: Option<FnVoid> = get!(s!("nvmlInit_v2"), FnVoid);
        let shutdown: Option<FnVoid> = get!(s!("nvmlShutdown"), FnVoid);
        let count: Option<FnCount> = get!(s!("nvmlDeviceGetCount_v2"), FnCount);
        let handle: Option<FnHandle> = get!(s!("nvmlDeviceGetHandleByIndex_v2"), FnHandle);
        let name: Option<FnName> = get!(s!("nvmlDeviceGetName"), FnName);
        let (Some(init), Some(shutdown), Some(count), Some(handle)) = (init, shutdown, count, handle) else {
            let _ = FreeLibrary(lib);
            return None;
        };
        if init() != 0 {
            let _ = FreeLibrary(lib);
            return None;
        }
        let mut n = 0u32;
        let _ = count(&mut n);
        let mut devices = Vec::new();
        for i in 0..n.min(8) {
            let mut dev: Device = std::ptr::null_mut();
            if handle(i, &mut dev) != 0 || dev.is_null() {
                continue;
            }
            let mut buf = [0u8; 96];
            let label = match name {
                Some(f) if f(dev, buf.as_mut_ptr(), buf.len() as u32) == 0 => {
                    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
                    short_name(&String::from_utf8_lossy(&buf[..end]))
                }
                _ => format!("GPU {}", i + 1),
            };
            devices.push((dev, label));
        }
        if devices.is_empty() {
            shutdown();
            let _ = FreeLibrary(lib);
            return None;
        }
        Some(Self {
            lib,
            shutdown,
            temp: get!(s!("nvmlDeviceGetTemperature"), FnTemp),
            util: get!(s!("nvmlDeviceGetUtilizationRates"), FnUtil),
            mem: get!(s!("nvmlDeviceGetMemoryInfo"), FnMem),
            power: get!(s!("nvmlDeviceGetPowerUsage"), FnU32),
            clock: get!(s!("nvmlDeviceGetClockInfo"), FnClock),
            fan: get!(s!("nvmlDeviceGetFanSpeed"), FnU32),
            devices,
        })
    }

    fn read(&self) -> Vec<Gpu> {
        const GB: f32 = 1024.0 * 1024.0 * 1024.0;
        self.devices
            .iter()
            .map(|&(dev, ref name)| unsafe {
                let mut g = Gpu { name: name.clone(), ..Default::default() };
                if let Some(f) = self.util {
                    let mut u = Utilization::default();
                    if f(dev, &mut u) == 0 {
                        g.load = Some(u.gpu.min(100) as f32);
                    }
                }
                if let Some(f) = self.temp {
                    let mut t = 0u32;
                    // Sensor 0: the GPU die.
                    if f(dev, 0, &mut t) == 0 {
                        g.temp = Some(t as f32);
                    }
                }
                if let Some(f) = self.mem {
                    let mut m = Memory::default();
                    if f(dev, &mut m) == 0 && m.total > 0 {
                        g.vram_used = Some(m.used as f32 / GB);
                        g.vram_total = Some(m.total as f32 / GB);
                    }
                }
                if let Some(f) = self.power {
                    let mut mw = 0u32;
                    if f(dev, &mut mw) == 0 {
                        g.power = Some(mw as f32 / 1000.0);
                    }
                }
                if let Some(f) = self.clock {
                    let mut mhz = 0u32;
                    // Clock 0: graphics.
                    if f(dev, 0, &mut mhz) == 0 {
                        g.clock = Some(mhz);
                    }
                }
                if let Some(f) = self.fan {
                    let mut pct = 0u32;
                    if f(dev, &mut pct) == 0 {
                        g.fan = Some(pct.min(100));
                    }
                }
                g
            })
            .collect()
    }
}

impl Drop for Api {
    fn drop(&mut self) {
        unsafe {
            (self.shutdown)();
            let _ = FreeLibrary(self.lib);
        }
    }
}

/// Set once NVML turned out not to be there, so it isn't looked for again.
static MISSING: AtomicBool = AtomicBool::new(false);

#[derive(Default)]
enum State {
    #[default]
    Off,
    Opening(Receiver<Option<Api>>),
    Ready(Api),
}

/// NVML while GPU readings are wanted (see the module docs).
#[derive(Default)]
pub struct Nvml {
    state: State,
}

impl Nvml {
    /// The cards' readings now, starting NVML first if needed (until it's up,
    /// and on machines without it, there are none).
    pub fn read(&mut self) -> Vec<Gpu> {
        if let State::Opening(rx) = &self.state {
            match rx.try_recv() {
                Ok(Some(api)) => self.state = State::Ready(api),
                Ok(None) | Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    MISSING.store(true, Ordering::Relaxed);
                    self.state = State::Off;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        match &self.state {
            State::Ready(api) => api.read(),
            State::Off if !MISSING.load(Ordering::Relaxed) => {
                let (tx, rx) = channel();
                std::thread::spawn(move || {
                    let _ = tx.send(unsafe { Api::open() });
                });
                self.state = State::Opening(rx);
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// Lets go of NVML (and its DLL) until it's wanted again.
    pub fn close(&mut self) {
        // One still opening is dropped when its thread finds no one listening.
        self.state = State::Off;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_lose_the_brand() {
        assert_eq!(short_name("NVIDIA GeForce RTX 5070 Ti"), "RTX 5070 Ti");
        assert_eq!(short_name("NVIDIA GeForce GTX 1660 SUPER"), "GTX 1660 SUPER");
        assert_eq!(short_name("NVIDIA RTX A2000"), "RTX A2000");
        assert_eq!(short_name("Quadro P400"), "Quadro P400");
        assert_eq!(short_name("NVIDIA"), "NVIDIA");
    }
}
