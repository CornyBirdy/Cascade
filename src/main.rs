#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![allow(clippy::too_many_arguments)]

#[cfg(not(windows))]
compile_error!("Cascade is a Windows app.");

mod audio;
mod backdrop;
mod brightness;
mod config;
mod devices;
mod dock;
mod gfx;
mod helper;
mod http;
mod icons;
mod keyhook;
mod launcher;
mod lens;
mod media;
mod mouseedge;
mod mousehook;
mod notifications;
mod nvml;
mod pomodoro;
mod quickmenu;
mod preview;
mod radios;
mod settings;
mod shell;
mod spring;
mod switcher;
mod sysmon;
mod systray;
mod theme;
mod ui;
mod update;
mod usage;
mod weather;
mod widgets;

/// Messages worker threads post to the main (notch) window.
pub mod msg {
    use std::sync::atomic::{AtomicIsize, Ordering};
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

    pub const MEDIA: u32 = WM_APP + 1;
    pub const VOLUME: u32 = WM_APP + 2;
    pub const BRIGHTNESS: u32 = WM_APP + 3;
    pub const SPECTRUM: u32 = WM_APP + 4;
    pub const AUDIO_DEVICE: u32 = WM_APP + 5;
    pub const TRAY: u32 = WM_APP + 6;
    pub const APPBAR: u32 = WM_APP + 7;
    pub const RADIOS: u32 = WM_APP + 8;
    pub const BRIGHTNESS_LEVEL: u32 = WM_APP + 9;
    pub const ICON: u32 = WM_APP + 10;
    pub const WEATHER: u32 = WM_APP + 11;
    pub const UPDATE: u32 = WM_APP + 12;
    pub const OPEN_LAUNCHER: u32 = WM_APP + 13;
    pub const OPEN_SETTINGS: u32 = WM_APP + 14;
    pub const APPS: u32 = WM_APP + 15;
    pub const TRAY_ICON: u32 = WM_APP + 17;
    pub const OUTSIDE_CLICK: u32 = WM_APP + 18;
    pub const KEY_CHECK: u32 = WM_APP + 19;
    /// Alt+Tab: wParam 0 next, 1 previous, 2 commit, 3 cancel.
    pub const SWITCH: u32 = WM_APP + 20;
    /// The notifications widget's list (a boxed `notifications::Snapshot`).
    pub const NOTIFS: u32 = WM_APP + 21;
    /// A Win+key shortcut Cascade handles (wParam: the key, e.g. 'X').
    pub const SHORTCUT: u32 = WM_APP + 22;

    static TARGET: AtomicIsize = AtomicIsize::new(0);

    pub fn set_target(hwnd: HWND) {
        TARGET.store(hwnd.0 as isize, Ordering::Release);
    }

    pub fn post(msg: u32, w: usize, l: isize) -> bool {
        let h = TARGET.load(Ordering::Acquire);
        h != 0 && unsafe { PostMessageW(Some(HWND(h as *mut _)), msg, WPARAM(w), LPARAM(l)).is_ok() }
    }
}

fn main() {
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};

    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    if std::env::args().any(|a| a == "--key-helper") {
        helper::run();
        return;
    }
    // After an update the new copy starts while the old one is still
    // shutting down, so give it a few seconds to let go.
    let updated = std::env::args().any(|a| a == "--updated");
    let mut tries = if updated { 50 } else { 1 };
    while !shell::claim_single_instance() {
        tries -= 1;
        if tries == 0 {
            // Already running (e.g. "Cascade Settings" from the Start menu):
            // ask that copy to show its settings.
            shell::signal_running_instance();
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    update::cleanup();
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    if let Err(e) = ui::run() {
        use windows::core::{w, HSTRING};
        use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR};
        unsafe {
            MessageBoxW(None, &HSTRING::from(format!("Cascade could not start:\n{e}")), w!("Cascade"), MB_ICONERROR);
        }
    }
}
