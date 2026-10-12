//! Cascade's own resource use, shown in Settings > About so you can check it.

use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

fn filetime(f: FILETIME) -> u64 {
    ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64
}

fn duration(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs} s"),
        60..3600 => format!("{} min", secs / 60),
        _ => format!("{} h {} min", secs / 3600, secs / 60 % 60),
    }
}

/// "Memory 18 MB, CPU 0.02% of one core over the last 3 h 12 min (2.4 s used)".
pub fn summary() -> String {
    unsafe {
        let process = GetCurrentProcess();
        let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        let mut mem = PROCESS_MEMORY_COUNTERS { cb: size, ..Default::default() };
        let memory = if K32GetProcessMemoryInfo(process, &mut mem, size).as_bool() {
            format!("Memory {:.0} MB", mem.WorkingSetSize as f64 / (1024.0 * 1024.0))
        } else {
            "Memory unknown".to_string()
        };
        let (mut created, mut exited, mut kernel, mut user) =
            (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
        if GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user).is_err() {
            return memory;
        }
        let mut now = FILETIME::default();
        GetSystemTimeAsFileTime(&mut now);
        // FILETIME ticks are 100 ns.
        let used = (filetime(kernel) + filetime(user)) as f64 / 1e7;
        let up = filetime(now).saturating_sub(filetime(created)) as f64 / 1e7;
        if up < 1.0 {
            return memory;
        }
        format!(
            "{memory}, CPU {:.2}% of one core over {} ({used:.1} s used)",
            used / up * 100.0,
            duration(up as u64)
        )
    }
}
