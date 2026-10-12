//! System numbers for the stats widgets: CPU, memory, network, and (through
//! the performance counters) GPU and disk.
//!
//! Nothing here runs by itself. The app samples on a timer only while a
//! stats widget is on screen, and stops the timer (and drops the counters)
//! the moment none is.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{ERROR_SUCCESS, FILETIME};
use windows::Win32::NetworkManagement::IpHelper::{FreeMibTable, GetIfTable2, MIB_IF_TABLE2};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows::Win32::System::Performance::*;
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::GetSystemTimes;

/// Samples kept for the graphs.
pub const HISTORY: usize = 60;

#[derive(Clone, Copy, Default)]
pub struct Stats {
    pub cpu: f32,
    pub ram: f32,
    /// Gigabytes.
    pub ram_used: f32,
    pub ram_total: f32,
    pub gpu: Option<f32>,
    pub disk: Option<f32>,
    /// Bytes per second.
    pub down: f64,
    pub up: f64,
}

/// "12 KB/s", "1.2 MB/s".
pub fn speed(bytes_per_s: f64) -> String {
    let kb = bytes_per_s / 1024.0;
    if kb < 1000.0 {
        format!("{kb:.0} KB/s")
    } else {
        format!("{:.1} MB/s", kb / 1024.0)
    }
}

impl Stats {
    /// What the taskbar chips show, so they are only redrawn when it changes.
    pub fn signature(&self) -> String {
        format!(
            "{:.0}|{:.0}|{:?}|{}|{}",
            self.cpu,
            self.ram,
            self.gpu.map(|g| g.round() as i32),
            speed(self.down),
            speed(self.up)
        )
    }
}

#[derive(Default)]
pub struct History {
    pub cpu: VecDeque<f32>,
    pub ram: VecDeque<f32>,
    pub gpu: VecDeque<f32>,
    pub disk: VecDeque<f32>,
    pub down: VecDeque<f32>,
}

fn push(q: &mut VecDeque<f32>, v: f32) {
    if q.len() >= HISTORY {
        q.pop_front();
    }
    q.push_back(v);
}

/// PDH calls return a status that is 0 on success (its Rust type varies).
fn ok<T: Into<i64>>(status: T) -> bool {
    status.into() == 0
}

struct Pdh {
    query: PDH_HQUERY,
    gpu: Option<PDH_HCOUNTER>,
    disk: Option<PDH_HCOUNTER>,
    opened: Instant,
}

impl Pdh {
    fn open() -> Option<Self> {
        unsafe {
            let mut query = PDH_HQUERY::default();
            if !ok(PdhOpenQueryW(PCWSTR::null(), 0, &mut query)) {
                return None;
            }
            let add = |path: PCWSTR| {
                let mut h = PDH_HCOUNTER::default();
                ok(PdhAddEnglishCounterW(query, path, 0, &mut h)).then_some(h)
            };
            let gpu = add(w!("\\GPU Engine(*)\\Utilization Percentage"));
            let disk = add(w!("\\PhysicalDisk(_Total)\\% Disk Time"));
            // The first reading only sets the baseline for rate counters.
            let _ = PdhCollectQueryData(query);
            Some(Self { query, gpu, disk, opened: Instant::now() })
        }
    }

    fn value(&self, counter: PDH_HCOUNTER) -> Option<f64> {
        unsafe {
            let mut v = PDH_FMT_COUNTERVALUE::default();
            if !ok(PdhGetFormattedCounterValue(counter, PDH_FMT_DOUBLE, None, &mut v)) || v.CStatus > 1 {
                return None;
            }
            Some(v.Anonymous.doubleValue)
        }
    }

    /// The busiest GPU's busiest engine, like Task Manager's GPU figure.
    fn gpu_load(&self, counter: PDH_HCOUNTER) -> Option<f64> {
        unsafe {
            let (mut size, mut count) = (0u32, 0u32);
            let _ = PdhGetFormattedCounterArrayW(counter, PDH_FMT_DOUBLE, &mut size, &mut count, None);
            if size == 0 {
                return None;
            }
            let mut buf = vec![0u64; (size as usize).div_ceil(8)];
            let items = buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
            if !ok(PdhGetFormattedCounterArrayW(counter, PDH_FMT_DOUBLE, &mut size, &mut count, Some(items))) {
                return None;
            }
            // Instance names look like pid_1234_luid_0x0_0xABCD_phys_0_eng_3_engtype_3D.
            let mut engines: HashMap<(String, String), f64> = HashMap::new();
            for i in 0..count as usize {
                let item = &*items.add(i);
                if item.FmtValue.CStatus > 1 {
                    continue;
                }
                let name = item.szName.to_string().unwrap_or_default();
                let luid = name.split("_phys").next().and_then(|s| s.split("luid_").nth(1)).unwrap_or("").to_string();
                let engine = name.rsplit("engtype_").next().unwrap_or("").to_string();
                *engines.entry((luid, engine)).or_default() += item.FmtValue.Anonymous.doubleValue;
            }
            engines.values().copied().reduce(f64::max)
        }
    }
}

impl Drop for Pdh {
    fn drop(&mut self) {
        unsafe {
            let _ = PdhCloseQuery(self.query);
        }
    }
}

pub struct Sampler {
    pub stats: Stats,
    pub hist: History,
    cpu_prev: Option<(u64, u64, u64)>,
    net_prev: Option<(u64, u64, Instant)>,
    pdh: Option<Pdh>,
}

fn filetime(f: FILETIME) -> u64 {
    ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64
}

impl Sampler {
    pub fn new() -> Self {
        Self { stats: Stats::default(), hist: History::default(), cpu_prev: None, net_prev: None, pdh: None }
    }

    /// Forgets the baselines (the next samples start fresh) and frees the counters.
    pub fn reset(&mut self) {
        self.cpu_prev = None;
        self.net_prev = None;
        self.pdh = None;
        self.stats.gpu = None;
        self.stats.disk = None;
    }

    /// Takes one sample. `counters` also reads GPU and disk load, which costs
    /// a little more, so ask for it only when it will be shown.
    pub fn sample(&mut self, counters: bool) {
        self.cpu();
        self.memory();
        self.network();
        if counters {
            self.counters();
        } else if self.pdh.is_some() {
            self.pdh = None;
            self.stats.gpu = None;
            self.stats.disk = None;
        }
        let s = self.stats;
        push(&mut self.hist.cpu, s.cpu);
        push(&mut self.hist.ram, s.ram);
        push(&mut self.hist.gpu, s.gpu.unwrap_or(0.0));
        push(&mut self.hist.disk, s.disk.unwrap_or(0.0));
        push(&mut self.hist.down, s.down as f32);
    }

    fn cpu(&mut self) {
        let (mut idle, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
        if unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)) }.is_err() {
            return;
        }
        let now = (filetime(idle), filetime(kernel), filetime(user));
        if let Some((pi, pk, pu)) = self.cpu_prev {
            // Kernel time includes idle time.
            let total = now.1.wrapping_sub(pk) + now.2.wrapping_sub(pu);
            if total > 0 {
                let idle = now.0.wrapping_sub(pi) as f32;
                self.stats.cpu = (100.0 * (1.0 - idle / total as f32)).clamp(0.0, 100.0);
            }
        }
        self.cpu_prev = Some(now);
    }

    fn memory(&mut self) {
        let mut m = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
        if unsafe { GlobalMemoryStatusEx(&mut m) }.is_ok() {
            const GB: f64 = 1024.0 * 1024.0 * 1024.0;
            let total = m.ullTotalPhys as f64 / GB;
            self.stats.ram = m.dwMemoryLoad as f32;
            self.stats.ram_total = total as f32;
            self.stats.ram_used = (total - m.ullAvailPhys as f64 / GB) as f32;
        }
    }

    fn network(&mut self) {
        let (mut rx, mut tx) = (0u64, 0u64);
        unsafe {
            let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
            if GetIfTable2(&mut table) != ERROR_SUCCESS || table.is_null() {
                return;
            }
            let rows = std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize);
            for r in rows {
                // Real network cards that are up (virtual adapters would count the same traffic twice).
                let hardware = r.InterfaceAndOperStatusFlags._bitfield & 1 != 0;
                if hardware && r.OperStatus == IfOperStatusUp {
                    rx += r.InOctets;
                    tx += r.OutOctets;
                }
            }
            FreeMibTable(table as *const _);
        }
        let now = Instant::now();
        if let Some((prx, ptx, at)) = self.net_prev {
            let dt = now.duration_since(at).as_secs_f64().max(0.05);
            self.stats.down = rx.saturating_sub(prx) as f64 / dt;
            self.stats.up = tx.saturating_sub(ptx) as f64 / dt;
        }
        self.net_prev = Some((rx, tx, now));
    }

    fn counters(&mut self) {
        // The set of GPU engines changes as programs come and go: start the
        // query again now and then so new ones are picked up.
        if self.pdh.as_ref().is_some_and(|p| p.opened.elapsed() > Duration::from_secs(30)) {
            self.pdh = None;
        }
        if self.pdh.is_none() {
            self.pdh = Pdh::open();
            return; // the first reading is only a baseline
        }
        let Some(p) = &self.pdh else { return };
        if !ok(unsafe { PdhCollectQueryData(p.query) }) {
            return;
        }
        self.stats.disk = p.disk.and_then(|h| p.value(h)).map(|v| v.clamp(0.0, 100.0) as f32);
        self.stats.gpu = p.gpu.and_then(|h| p.gpu_load(h)).map(|v| v.clamp(0.0, 100.0) as f32);
    }
}
