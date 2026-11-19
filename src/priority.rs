//! How much of the PC Cascade gets: the taskbar, Start and the notch are what
//! you touch to get anywhere, so they should never be the thing that lags.
//!
//! - **CPU:** the process runs in the High priority class (`priority =
//!   high`, the default; `above_normal` or `normal` to share more), and the
//!   window thread one step above that, so a busy game or a compile doesn't
//!   keep a click waiting. Realtime is never used: it can starve Windows
//!   itself. Programs Cascade starts don't inherit it (Windows starts them at
//!   Normal unless told otherwise).
//! - **Efficiency mode:** Windows 11 throttles processes it thinks are in the
//!   background (EcoQoS: slower cores, lower clocks), which a taskbar always
//!   looks like. Cascade opts out, and keeps its timers precise.
//! - **Memory:** a game that fills the RAM makes Windows page idle programs
//!   out, and the taskbar then stutters the first time it's used again. A
//!   floor on Cascade's working set (a soft one: Windows can still take it
//!   under real pressure) keeps it in memory.
//! - **GPU:** its drawing is scheduled ahead of normal work on the graphics
//!   card, where Windows allows it.

use windows::core::{s, w};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Memory::{
    SetProcessWorkingSetSizeEx, QUOTA_LIMITS_HARDWS_MAX_DISABLE, QUOTA_LIMITS_HARDWS_MIN_DISABLE,
};
use windows::Win32::System::Threading::*;

use crate::config;

/// Applies `level` ("high", "above_normal" or "normal") to the whole process.
pub fn apply(level: &str) {
    unsafe {
        let me = GetCurrentProcess();
        let class = match level {
            "normal" => NORMAL_PRIORITY_CLASS,
            "above_normal" => ABOVE_NORMAL_PRIORITY_CLASS,
            _ => HIGH_PRIORITY_CLASS,
        };
        let cpu = SetPriorityClass(me, class).is_ok();
        // Out of Efficiency mode / EcoQoS, timers included.
        let state = PROCESS_POWER_THROTTLING_STATE {
            Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED | PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
            StateMask: 0,
        };
        let eco = SetProcessInformation(
            me,
            ProcessPowerThrottling,
            &state as *const _ as *const _,
            std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        )
        .is_ok();
        // Kept in memory: a soft floor of 48 MB (Windows may still trim under real pressure).
        let floor = if level == "normal" { 0 } else { 48 << 20 };
        let mem = floor == 0
            || SetProcessWorkingSetSizeEx(
                me,
                floor,
                512 << 20,
                QUOTA_LIMITS_HARDWS_MIN_DISABLE | QUOTA_LIMITS_HARDWS_MAX_DISABLE,
            )
            .is_ok();
        let gpu = gpu_priority(me, level);
        config::log(&format!(
            "priority: {level} (cpu {cpu}, out of efficiency mode {eco}, memory floor {mem}, gpu {gpu})"
        ));
    }
}

/// The window thread, a step above the rest of the process (call on it).
pub fn boost_ui_thread(level: &str) {
    let p = if level == "normal" { THREAD_PRIORITY_NORMAL } else { THREAD_PRIORITY_ABOVE_NORMAL };
    unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), p);
    }
}

/// The GPU scheduling class of the process (gdi32's D3DKMT call, as Task
/// Manager and games use). High where Windows allows it, else Above normal.
unsafe fn gpu_priority(me: HANDLE, level: &str) -> bool {
    type SetClass = unsafe extern "system" fn(HANDLE, i32) -> i32;
    // D3DKMT_SCHEDULINGPRIORITYCLASS: 2 normal, 3 above normal, 4 high.
    let want = match level {
        "normal" => 2,
        "above_normal" => 3,
        _ => 4,
    };
    let Ok(gdi) = LoadLibraryW(w!("gdi32.dll")) else { return false };
    let Some(f) = GetProcAddress(gdi, s!("D3DKMTSetProcessSchedulingPriorityClass")) else { return false };
    let f = std::mem::transmute::<unsafe extern "system" fn() -> isize, SetClass>(f);
    (2..=want).rev().any(|c| f(me, c) >= 0)
}
