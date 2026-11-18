//! Shell mode: Cascade as the Windows shell, in Explorer's place.
//!
//! Windows starts one program as "the shell" when you sign in. Normally
//! that's Explorer, which makes the desktop, the taskbar and the Start menu
//! and owns most Win+key shortcuts. With shell mode on, Windows starts
//! Cascade instead, and Explorer's taskbar and Start menu never exist: there
//! is nothing to hide and nothing to race for the Win key. File Explorer
//! windows still open; only Explorer's role as the shell is gone.
//!
//! A shell has a few jobs nobody else does, so in shell mode Cascade:
//! - hides minimised windows (with no shell, Windows leaves a stub of each at
//!   the bottom of the screen);
//! - keeps the screen space under its taskbar free itself (`reserve`), where
//!   it otherwise asks Explorer for it;
//! - makes the desktop: a window at the very bottom showing the wallpaper,
//!   which takes the focus when you click the desktop;
//! - answers the Win+key shortcuts Explorer had (`on_hotkey`);
//! - runs the programs set to start with Windows (`run_startup`).
//!
//! The safety net is the guard: `cascade.exe --shell` is what Windows really
//! starts. It has no windows and only waits. It starts Cascade proper, starts
//! it again if it dies, and if it keeps dying turns shell mode off and brings
//! Explorer back, so a bad build can't leave a blank screen.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicIsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows::core::{w, Interface, BOOL, HSTRING, PCSTR, PCWSTR, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, IPersistFile, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, STGM_READ,
};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows::Win32::System::Registry::*;
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::Threading::{
    CreateMutexW, GetCurrentProcessId, OpenEventW, OpenMutexW, OpenProcess, SetEvent, WaitForSingleObject,
    EVENT_MODIFY_STATE, INFINITE, PROCESS_ACCESS_RIGHTS, SYNCHRONIZATION_ACCESS_RIGHTS,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS};
use windows::Win32::UI::Shell::{IShellLinkW, ShellExecuteW, ShellLink};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::{config, msg, shell};

// ------------------------------------------------------------- registry --

const WINLOGON: &str = "Software\\Microsoft\\Windows NT\\CurrentVersion\\Winlogon";
const EXPLORER_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer";

/// A text value from the registry (None if it isn't there).
fn reg_text(root: HKEY, key: &str, value: &str) -> Option<String> {
    let mut buf = [0u16; 2048];
    let mut size = (buf.len() * 2) as u32;
    let ok = unsafe {
        RegGetValueW(
            root,
            &HSTRING::from(key),
            &HSTRING::from(value),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut c_void),
            Some(&mut size),
        )
    } == ERROR_SUCCESS;
    ok.then(|| {
        let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..n])
    })
}

fn set_reg_text(root: HKEY, key: &str, value: &str, text: &str) -> bool {
    let wide: Vec<u16> = text.encode_utf16().chain([0]).collect();
    unsafe {
        RegSetKeyValueW(
            root,
            &HSTRING::from(key),
            &HSTRING::from(value),
            REG_SZ.0,
            Some(wide.as_ptr() as *const c_void),
            (wide.len() * 2) as u32,
        ) == ERROR_SUCCESS
    }
}

/// This user's own choice of shell (Windows' default, Explorer, applies without one).
fn shell_value() -> Option<String> {
    reg_text(HKEY_CURRENT_USER, WINLOGON, "Shell")
}

/// Whether a Shell setting is the one Cascade writes.
fn is_ours(value: &str) -> bool {
    let name = std::env::current_exe()
        .ok()
        .and_then(|e| e.file_name().map(|n| n.to_string_lossy().to_lowercase()))
        .unwrap_or_else(|| "cascade.exe".into());
    let v = value.to_lowercase();
    v.contains(&name) && v.contains("--shell")
}

/// What Windows is told to start: the guard. Quoted only when the path needs
/// it (Windows splits the setting at commas and runs each piece as typed).
fn command() -> Option<String> {
    let exe = std::env::current_exe().ok()?.to_string_lossy().into_owned();
    Some(if exe.contains(' ') { format!("\"{exe}\" --shell") } else { format!("{exe} --shell") })
}

/// Whether shell mode is turned on: Windows starts Cascade as the shell the
/// next time you sign in.
pub fn registered() -> bool {
    shell_value().is_some_and(|v| is_ours(&v))
}

/// Turns shell mode on or off, from the next sign-in. Only this user's
/// setting is touched, so other accounts (and Safe Mode) keep Explorer.
pub fn set_registered(on: bool) -> bool {
    let ok = if on {
        // Turned on afresh: the first sign-in asks whether to keep it (see `trial`).
        let _ = std::fs::remove_file(confirmed_path());
        command().is_some_and(|c| set_reg_text(HKEY_CURRENT_USER, WINLOGON, "Shell", &c))
    } else {
        // Someone else's shell isn't Cascade's to remove.
        !registered()
            || unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(WINLOGON), w!("Shell")) } == ERROR_SUCCESS
    };
    config::log(&format!("shell mode: turned {} (took: {ok})", if on { "on" } else { "off" }));
    ok
}

// ---------------------------------------------------------------- state --

static ACTIVE: AtomicBool = AtomicBool::new(false);
/// Quit was chosen: Explorer takes over once Cascade's windows are gone.
static QUIT: AtomicBool = AtomicBool::new(false);
/// Cascade's windows are gone and Explorer is being started (see `on_exit`).
static HANDING_OVER: AtomicBool = AtomicBool::new(false);
static DESKTOP: AtomicIsize = AtomicIsize::new(0);
/// How minimised windows were arranged before Cascade hid them (-1: not changed).
static OLD_ARRANGE: AtomicI32 = AtomicI32::new(-1);

/// Whether Explorer is the shell: it has made its taskbar. Told by the
/// program behind the window, since Cascade's own hidden tray window (this
/// copy's, or another's) has the same class.
fn explorer_is_shell() -> bool {
    explorer_shell().is_some()
}

/// Explorer's taskbar and the Explorer it belongs to, while Explorer is the shell.
fn explorer_shell() -> Option<(HWND, u32)> {
    unsafe {
        let mut after: Option<HWND> = None;
        while let Ok(h) = FindWindowExW(None, after, w!("Shell_TrayWnd"), PCWSTR::null()) {
            after = Some(h);
            let mut pid = 0u32;
            GetWindowThreadProcessId(h, Some(&mut pid));
            let explorer =
                crate::dock::process_path(pid).is_some_and(|p| p.to_ascii_lowercase().ends_with("\\explorer.exe"));
            if explorer {
                return Some((h, pid));
            }
        }
    }
    None
}

/// Decides, once at startup, whether this copy is the shell: shell mode is
/// on and Explorer hasn't made a taskbar (if it has, shell mode was turned on
/// but you haven't signed out since: Explorer is still the shell, and
/// Cascade runs beside it as usual).
pub fn detect() {
    let on = registered() && !explorer_is_shell();
    ACTIVE.store(on, Ordering::Release);
}

/// Whether Cascade is the shell right now.
pub fn active() -> bool {
    ACTIVE.load(Ordering::Acquire)
}

/// Whether Explorer has made itself the shell under a Cascade that was one
/// (something started it with nothing to open, and it took the job).
pub fn explorer_took_over() -> bool {
    active() && explorer_is_shell()
}

/// Cascade's desktop window (shell mode only).
pub fn desktop() -> Option<HWND> {
    let h = DESKTOP.load(Ordering::Acquire);
    (h != 0).then_some(HWND(h as *mut c_void))
}

// --------------------------------------------------------------- intent --

/// Why Cascade is about to exit, left in a file for the guard.
#[derive(Clone, Copy, PartialEq)]
pub enum Intent {
    /// Quit was chosen: Explorer takes over for this session.
    Quit,
    /// Another copy is on its way (Restart, an update).
    Restart,
    /// You're signing out or shutting down.
    SessionEnd,
}

fn intent_path() -> PathBuf {
    config::dir().join("shell-exit")
}

/// The latest intent set in this copy (0: none; 1 quit, 2 restart, 3 session end).
static INTENT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

pub fn set_intent(intent: Intent) {
    if !active() {
        return;
    }
    INTENT.store(
        match intent {
            Intent::Quit => 1,
            Intent::Restart => 2,
            Intent::SessionEnd => 3,
        },
        Ordering::Release,
    );
    if intent == Intent::Quit {
        QUIT.store(true, Ordering::Release);
    }
    let text = match intent {
        Intent::Quit => "quit",
        Intent::Restart => "restart",
        Intent::SessionEnd => "session-end",
    };
    let _ = std::fs::create_dir_all(config::dir());
    let _ = std::fs::write(intent_path(), text);
}

fn take_intent() -> Option<Intent> {
    let text = std::fs::read_to_string(intent_path()).ok()?;
    let _ = std::fs::remove_file(intent_path());
    match text.trim() {
        "quit" => Some(Intent::Quit),
        "restart" => Some(Intent::Restart),
        "session-end" => Some(Intent::SessionEnd),
        _ => None,
    }
}

fn note_path() -> PathBuf {
    config::dir().join("shell-note")
}

/// Something the guard left to be said the next time Cascade starts (shell
/// mode was turned off because Cascade kept failing).
pub fn take_note() -> Option<String> {
    let text = std::fs::read_to_string(note_path()).ok()?;
    let _ = std::fs::remove_file(note_path());
    Some(text.trim().to_string()).filter(|t| !t.is_empty())
}

// ---------------------------------------------------------------- guard --

const GUARD_MUTEX: PCWSTR = w!("Local\\CascadeShellGuard");

fn shutting_down() -> bool {
    unsafe { GetSystemMetrics(SM_SHUTTINGDOWN) != 0 }
}

/// Waits until the Cascade whose main window is `hwnd` has exited.
fn wait_gone(hwnd: HWND) {
    const SYNCHRONIZE: u32 = 0x0010_0000;
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        match OpenProcess(PROCESS_ACCESS_RIGHTS(SYNCHRONIZE), false, pid) {
            Ok(process) => {
                let _ = WaitForSingleObject(process, INFINITE);
                let _ = CloseHandle(process);
            }
            Err(_) => {
                while IsWindow(Some(hwnd)).as_bool() {
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
        }
    }
}

/// Whether a Cascade proper is running, or starting (it takes the
/// single-instance lock before it makes any window).
fn instance_exists() -> bool {
    const SYNCHRONIZE: u32 = 0x0010_0000;
    unsafe {
        match OpenMutexW(SYNCHRONIZATION_ACCESS_RIGHTS(SYNCHRONIZE), false, w!("Local\\CascadeNotchSingleInstance")) {
            Ok(h) => {
                let _ = CloseHandle(h);
                true
            }
            Err(_) => false,
        }
    }
}

/// A running Cascade's main window, waiting up to `patience` for one: longer
/// while a copy is on its way up (at sign-in the sign-in task may have
/// started one too, and beaten the guard's own to it).
fn find_instance(patience: Duration) -> Option<HWND> {
    let began = Instant::now();
    loop {
        if let Some(h) = crate::helper::main_window() {
            return Some(h);
        }
        let limit = if instance_exists() { patience.max(Duration::from_secs(90)) } else { patience };
        if began.elapsed() >= limit {
            return None;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Waits for the Cascade the guard started to exit. One that has made no
/// window most of a minute on is stuck starting: it's ended, which counts as
/// a failed start.
fn wait_child(child: &mut std::process::Child) {
    let began = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => return,
            Ok(None) => {}
        }
        if crate::helper::main_window().is_some() {
            let _ = child.wait();
            return;
        }
        if began.elapsed() > Duration::from_secs(45) {
            config::log("shell guard: Cascade made no window in 45 s; ending it");
            let _ = child.kill();
            let _ = child.wait();
            return;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Ends the guard once Explorer has the shell. With the exit code of a
/// program that was ended on purpose: Windows starts a shell again when it
/// goes away unexpectedly, and Explorer is already on its way.
fn leave() -> ! {
    std::process::exit(1)
}

/// The guard (`cascade.exe --shell`): what Windows starts as the shell. It
/// keeps a Cascade running and falls back to Explorer when it can't.
pub fn guard() {
    unsafe {
        let Ok(mutex) = CreateMutexW(None, true, GUARD_MUTEX) else { return };
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let _ = CloseHandle(mutex);
            return;
        }
        // Kept for as long as the guard runs.
    }
    let _ = take_intent();
    // Explorer is the shell (the guard was started by hand, or again after
    // handing over): there's nothing to guard.
    if explorer_is_shell() {
        leave();
    }
    let Ok(exe) = std::env::current_exe() else { return };
    config::log(&format!("shell guard: started, {} s after Windows", shell::uptime_secs()));
    let mut fails = 0u32;
    loop {
        let began = Instant::now();
        // Wait out the copy that's running (one that asked for a guard, or
        // the next one after a restart), or start one.
        match find_instance(Duration::ZERO) {
            Some(h) => wait_gone(h),
            None => match std::process::Command::new(&exe).arg("--shell-ui").spawn() {
                Ok(mut child) => wait_child(&mut child),
                Err(e) => config::log(&format!("shell guard: Cascade wouldn't start: {e}")),
            },
        }
        if shutting_down() {
            return;
        }
        // Explorer is the shell now (it took over, or was handed over to): the guard's work is done.
        if explorer_is_shell() {
            let _ = take_intent();
            config::log("shell guard: Explorer is the shell; nothing left to guard");
            leave();
        }
        let intent = take_intent();
        match intent {
            Some(Intent::SessionEnd) => return,
            Some(Intent::Quit) => {
                // Cascade starts Explorer itself on its way out: give its
                // taskbar a while to come up, and start it from here if it doesn't.
                let until = Instant::now() + Duration::from_secs(20);
                while Instant::now() < until && !explorer_is_shell() {
                    std::thread::sleep(Duration::from_millis(250));
                }
                if !explorer_is_shell() {
                    start_explorer(true);
                }
                config::log("shell guard: Cascade quit; Explorer is the shell until you sign out");
                leave();
            }
            _ => {}
        }
        // A restart or an update starts the next copy as the old one goes.
        let patience = if intent == Some(Intent::Restart) { 40 } else { 5 };
        if find_instance(Duration::from_secs(patience)).is_some() {
            fails = 0;
            continue;
        }
        if shutting_down() {
            return;
        }
        // Nothing is carrying on with the windows Cascade hid on other desktops: show them.
        crate::vdesk::restore_from_file();
        if !registered() {
            config::log("shell guard: shell mode was turned off; starting Explorer");
            start_explorer(false);
            leave();
        }
        fails = if began.elapsed() < Duration::from_secs(60) { fails + 1 } else { 1 };
        config::log(&format!("shell guard: Cascade went away without saying why (time {fails} in a row)"));
        if fails >= 3 {
            set_registered(false);
            let _ = std::fs::write(
                note_path(),
                "Shell mode was turned off: Cascade failed to start three times in a row, so Explorer is back.",
            );
            config::log("shell guard: giving up; shell mode is off and Explorer is starting");
            start_explorer(false);
            leave();
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn guard_running() -> bool {
    const SYNCHRONIZE: u32 = 0x0010_0000;
    unsafe {
        match OpenMutexW(SYNCHRONIZATION_ACCESS_RIGHTS(SYNCHRONIZE), false, GUARD_MUTEX) {
            Ok(h) => {
                let _ = CloseHandle(h);
                true
            }
            Err(_) => false,
        }
    }
}

/// Starts the guard if this shell-mode Cascade has none (it was started some
/// other way: Setup, a full restart). The guard finds this copy and waits on it.
fn ensure_guard() {
    if guard_running() {
        return;
    }
    if let Ok(exe) = std::env::current_exe() {
        let ok = std::process::Command::new(exe).arg("--shell").spawn().is_ok();
        config::log(&format!("shell mode: no guard was running; started one: {ok}"));
    }
}

/// Starts Explorer as the shell for the rest of this session. Explorer only
/// makes its desktop and taskbar when Windows' Shell setting doesn't name
/// another program, so Cascade's entry is taken out while it starts and (to
/// `keep` shell mode on for the next sign-in) put back once its taskbar is up.
pub fn start_explorer(keep: bool) -> bool {
    let ours = shell_value().filter(|v| is_ours(v));
    if ours.is_some() {
        unsafe {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(WINLOGON), w!("Shell"));
        }
    }
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
    let started = std::process::Command::new(format!("{windir}\\explorer.exe")).spawn().is_ok();
    let until = Instant::now() + Duration::from_secs(30);
    let mut up = false;
    while started && Instant::now() < until {
        if explorer_is_shell() {
            up = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    if keep {
        if let Some(v) = ours {
            set_reg_text(HKEY_CURRENT_USER, WINLOGON, "Shell", &v);
        }
    }
    config::log(&format!("shell mode: started Explorer (ran: {started}, its taskbar came up: {up})"));
    up
}

/// `cascade.exe --shell-off`: the way out when something is wrong. Turns
/// shell mode off and brings Explorer back now, whatever state Cascade is in.
/// Typed into Task Manager's *Run new task* box (Ctrl+Shift+Esc), which is
/// there even with no shell at all.
pub fn turn_off() {
    set_registered(false);
    config::log("shell mode: turned off from the command line; bringing Explorer back");
    if explorer_is_shell() {
        return;
    }
    // A running Cascade closes; its guard, seeing shell mode off, starts Explorer.
    if let Some(main) = crate::helper::main_window() {
        unsafe {
            let _ = PostMessageW(Some(main), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
        let until = Instant::now() + Duration::from_secs(25);
        while Instant::now() < until && !explorer_is_shell() {
            std::thread::sleep(Duration::from_millis(250));
        }
    }
    crate::vdesk::restore_from_file();
    // No guard, no Cascade, or one that's stuck: start it from here.
    if !explorer_is_shell() {
        start_explorer(false);
    }
}

fn confirmed_path() -> PathBuf {
    config::dir().join("shell-confirmed")
}

/// The first sign-in after shell mode is turned on is a trial, the way
/// Windows asks whether to keep a new screen resolution: a box asks whether
/// to keep Cascade as the shell, and if nobody says yes within two minutes
/// (the screen is blank, the mouse or keys don't work, the box can't be
/// reached) shell mode goes off and Explorer comes back by itself.
fn trial() {
    if confirmed_path().exists() {
        return;
    }
    std::thread::spawn(|| unsafe {
        // Let the taskbar and the startup apps come up first.
        std::thread::sleep(Duration::from_secs(8));
        // A message box that answers itself after a while. Not in the headers; in Windows since XP.
        type MessageBoxTimeout = unsafe extern "system" fn(HWND, PCWSTR, PCWSTR, u32, u16, u32) -> i32;
        const TIMED_OUT: i32 = 32000;
        let Ok(user32) = GetModuleHandleW(w!("user32.dll")) else { return };
        let Some(f) = GetProcAddress(user32, PCSTR(c"MessageBoxTimeoutW".as_ptr() as *const u8)) else { return };
        let f = std::mem::transmute::<unsafe extern "system" fn() -> isize, MessageBoxTimeout>(f);
        let text = w!(
            "Cascade is running as the Windows shell, in Explorer's place.\n\nKeep it this way?\n\nYes: Cascade stays the shell.\nNo, or no answer within 2 minutes: shell mode is turned off and Explorer comes back.\n\n(You can change this any time in Cascade's Settings > General.)"
        );
        let style = MB_YESNO | MB_ICONQUESTION | MB_TOPMOST | MB_SETFOREGROUND;
        let answer = f(HWND::default(), text, w!("Cascade shell mode"), style.0, 0, 120_000);
        if answer == IDYES.0 {
            let _ = std::fs::write(confirmed_path(), "");
            config::log("shell mode: kept (the first-run question was answered yes)");
            return;
        }
        config::log(if answer == TIMED_OUT {
            "shell mode: nobody answered the first-run question; turning it off and bringing Explorer back"
        } else {
            "shell mode: turned down at the first-run question; bringing Explorer back"
        });
        set_registered(false);
        // Quit, as from the menu: Explorer takes over once Cascade's windows are gone.
        set_intent(Intent::Quit);
        msg::post(WM_CLOSE, 0, 0);
        // If Cascade is too stuck even to close, end it from here: the guard
        // (or, with none, a copy started for the purpose) brings Explorer back.
        std::thread::sleep(Duration::from_secs(10));
        if !HANDING_OVER.load(Ordering::Acquire) {
            config::log("shell mode: Cascade didn't close when asked; ending it");
            if !guard_running() {
                if let Ok(exe) = std::env::current_exe() {
                    let _ = std::process::Command::new(exe).arg("--shell-off").spawn();
                }
            }
            std::process::exit(0);
        }
    });
}

/// Back to Explorer now was chosen: once it's up, Cascade starts again beside it.
static BESIDE: AtomicBool = AtomicBool::new(false);

/// Called once Cascade's windows are gone: after Quit, Explorer takes over.
pub fn on_exit() {
    if active() && QUIT.load(Ordering::Acquire) {
        HANDING_OVER.store(true, Ordering::Release);
        start_explorer(true);
        if BESIDE.load(Ordering::Acquire) {
            // (It waits for this copy to have gone, as after an update.)
            if let Ok(exe) = std::env::current_exe() {
                let ok = std::process::Command::new(exe).arg("--updated").spawn().is_ok();
                config::log(&format!("shell mode: back to Explorer; Cascade started again beside it: {ok}"));
            }
        }
    }
}

/// Back to Explorer for good, now: shell mode off, Explorer the shell, and a
/// new Cascade beside it as usual. Call `close` on the main window after.
pub fn leave_now() {
    set_registered(false);
    BESIDE.store(true, Ordering::Release);
    set_intent(Intent::Quit);
}

/// Starts the helper that makes Cascade the shell now, without signing out
/// (`take_over`). True if it started: close this Cascade next, as it waits
/// for that.
pub fn switch_now() -> bool {
    if !registered() {
        set_registered(true);
    }
    let Ok(exe) = std::env::current_exe() else { return false };
    let ok = std::process::Command::new(exe).arg("--shell-now").spawn().is_ok();
    config::log(&format!("shell mode: switching now, without signing out (helper started: {ok})"));
    ok
}

/// `cascade.exe --shell-now`: makes Cascade the shell without signing out.
/// The Cascade that asked closes first (putting Explorer's taskbar back as it
/// goes). Explorer's desktops are put together into one, since a window on
/// another one would stay hidden without Explorer. Then Explorer is asked to
/// exit, as its own *Exit Explorer* does, and this becomes the guard, which
/// starts Cascade as the shell.
pub fn take_over() {
    let until = Instant::now() + Duration::from_secs(20);
    while instance_exists() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(200));
    }
    if !registered() {
        set_registered(true);
    }
    if let Some((tray, pid)) = explorer_shell() {
        crate::desktops::merge_all();
        const SYNCHRONIZE: u32 = 0x0010_0000;
        const TERMINATE: u32 = 0x0001;
        unsafe {
            let process = OpenProcess(PROCESS_ACCESS_RIGHTS(SYNCHRONIZE | TERMINATE), false, pid).ok();
            // Explorer's own "Exit Explorer".
            let _ = PostMessageW(Some(tray), 0x5B4, WPARAM(0), LPARAM(0));
            let gone = process.is_some_and(|p| WaitForSingleObject(p, 15_000) == WAIT_OBJECT_0);
            if !gone {
                config::log("shell mode: Explorer didn't exit when asked; ending it");
                if let Some(p) = process {
                    let _ = windows::Win32::System::Threading::TerminateProcess(p, 1);
                    let _ = WaitForSingleObject(p, 5_000);
                }
            }
            if let Some(p) = process {
                let _ = CloseHandle(p);
            }
        }
        config::log("shell mode: Explorer has exited; Cascade is taking over as the shell");
    }
    guard();
}

// ------------------------------------------------------------ work area --

/// (window, its screen, strip height): the space kept free along the bottom
/// of each screen for a taskbar.
static RESERVED: Mutex<Vec<(isize, RECT, i32)>> = Mutex::new(Vec::new());

fn reserved() -> std::sync::MutexGuard<'static, Vec<(isize, RECT, i32)>> {
    RESERVED.lock().unwrap_or_else(|e| e.into_inner())
}

/// The work area `screen` should have: all of it, less the tallest strip
/// reserved on it.
fn wanted_work_area(screen: RECT) -> RECT {
    let px = reserved().iter().filter(|e| e.1 == screen).map(|e| e.2).max().unwrap_or(0);
    RECT { bottom: screen.bottom - px, ..screen }
}

/// Sets a screen's work area (where maximised windows go). `tell`: Windows
/// tells every program, so they move out of the taskbar's way.
fn set_work_area(screen: RECT, mut want: RECT, tell: bool) {
    if shell::work_area_of(screen) == want {
        return;
    }
    let flags = if tell { SPIF_SENDCHANGE } else { SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0) };
    unsafe {
        let _ = SystemParametersInfoW(SPI_SETWORKAREA, 0, Some(&mut want as *mut RECT as *mut c_void), flags);
    }
    // Cascade's own windows hear it from here as well, whatever Windows told them.
    msg::post(WM_SETTINGCHANGE, SPI_SETWORKAREA.0 as usize, 0);
}

/// Brings `screen`'s work area up to date with what's reserved on it. On a
/// thread of its own, one change after another: telling every program can
/// wait on one that's stuck, and the taskbar mustn't wait with it.
fn apply_work_area(screen: RECT) {
    use std::sync::mpsc::{channel, Sender};
    static TX: Mutex<Option<Sender<RECT>>> = Mutex::new(None);
    let mut tx = TX.lock().unwrap_or_else(|e| e.into_inner());
    if tx.is_none() {
        let (sender, receiver) = channel::<RECT>();
        std::thread::spawn(move || {
            for screen in receiver {
                set_work_area(screen, wanted_work_area(screen), true);
            }
        });
        *tx = Some(sender);
    }
    if let Some(sender) = tx.as_ref() {
        let _ = sender.send(screen);
    }
}

/// Keeps `px` along the bottom of `screen` free for taskbar `hwnd`, as
/// Explorer does for app bars. Returns the strip.
pub fn reserve(hwnd: HWND, screen: RECT, px: i32) -> RECT {
    let before = {
        let mut list = reserved();
        let before = list.iter().find(|e| e.0 == hwnd.0 as isize).map(|e| e.1);
        list.retain(|e| e.0 != hwnd.0 as isize);
        list.push((hwnd.0 as isize, screen, px));
        before
    };
    // The taskbar moved to another screen: give the old one its space back.
    if let Some(old) = before.filter(|o| *o != screen) {
        apply_work_area(old);
    }
    apply_work_area(screen);
    RECT { top: screen.bottom - px, ..screen }
}

/// Gives back the space `hwnd` had reserved.
pub fn release(hwnd: HWND) {
    let screen = {
        let mut list = reserved();
        let found = list.iter().find(|e| e.0 == hwnd.0 as isize).map(|e| e.1);
        list.retain(|e| e.0 != hwnd.0 as isize);
        found
    };
    if let Some(screen) = screen {
        apply_work_area(screen);
    }
}

// -------------------------------------------------------------- desktop --

fn virtual_screen() -> RECT {
    unsafe {
        let (x, y) = (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN));
        RECT {
            left: x,
            top: y,
            right: x + GetSystemMetrics(SM_CXVIRTUALSCREEN),
            bottom: y + GetSystemMetrics(SM_CYVIRTUALSCREEN),
        }
    }
}

/// The wallpaper window inside the desktop (see `create_desktop`).
static WALLPAPER: AtomicIsize = AtomicIsize::new(0);

fn wallpaper_window() -> Option<HWND> {
    let h = WALLPAPER.load(Ordering::Acquire);
    (h != 0).then_some(HWND(h as *mut c_void))
}

/// Loads the wallpaper again (the setting or the screens changed).
fn reload_wallpaper() {
    if let Some(w) = wallpaper_window() {
        let r = virtual_screen();
        crate::wallpaper::reload(w, (r.left, r.top));
    }
}

/// Moves the desktop over every screen (they were added, removed or moved).
unsafe fn cover_screens(desktop: HWND) {
    let r = virtual_screen();
    let (w, h) = (r.right - r.left, r.bottom - r.top);
    let _ = SetWindowPos(desktop, None, r.left, r.top, w, h, SWP_NOACTIVATE | SWP_NOZORDER);
    if let Some(wall) = wallpaper_window() {
        let _ = SetWindowPos(wall, None, 0, 0, w, h, SWP_NOACTIVATE | SWP_NOZORDER);
    }
}

/// The desktop's own window (Explorer's is "Progman" too).
unsafe extern "system" fn desktop_proc(hwnd: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match m {
        // Wallpaper apps (Wallpaper Engine, Lively) ask Explorer's desktop for
        // a window behind its icons with this, then put their own in it.
        // Cascade's is there from the start (see `create_desktop`).
        0x052C => LRESULT(0),
        WM_ERASEBKGND => LRESULT(1),
        WM_SETCURSOR => {
            SetCursor(LoadCursorW(None, IDC_ARROW).ok());
            LRESULT(1)
        }
        // Always under every other window, whatever asks to move it.
        WM_WINDOWPOSCHANGING => {
            let pos = &mut *(lp.0 as *mut WINDOWPOS);
            pos.hwndInsertAfter = HWND_BOTTOM;
            pos.flags &= !SWP_NOZORDER;
            LRESULT(0)
        }
        WM_ACTIVATE => {
            if wp.0 & 0xFFFF != 0 {
                msg::post(msg::DESKTOP, 0, 0);
            }
            LRESULT(0)
        }
        WM_DISPLAYCHANGE => {
            cover_screens(hwnd);
            reload_wallpaper();
            LRESULT(0)
        }
        WM_SETTINGCHANGE => {
            // A new picture (SPI_SETDESKWALLPAPER), or Windows' desktop settings.
            // (The section name is a string Windows copies across; checked
            // for being a pointer at all, in case a program sent a number.)
            let section =
                if lp.0 > 0xFFFF { PCWSTR(lp.0 as *const u16).to_string().unwrap_or_default() } else { String::new() };
            if wp.0 as u32 == SPI_SETDESKWALLPAPER.0 || section.eq_ignore_ascii_case("Control Panel\\Desktop") {
                reload_wallpaper();
            }
            LRESULT(0)
        }
        WM_SYSCOLORCHANGE => {
            reload_wallpaper();
            LRESULT(0)
        }
        // Alt+F4 on the desktop shows the shut-down box, as on Explorer's (Cascade's power menu here).
        WM_CLOSE => LRESULT(0),
        _ => DefWindowProcW(hwnd, m, wp, lp),
    }
}

/// The window the wallpaper is drawn in, and where wallpaper apps put theirs.
unsafe extern "system" fn wallpaper_proc(hwnd: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match m {
        // The pointer over the desktop is the arrow. A wallpaper app's window
        // in here (Wallpaper Engine's) sets none, and asks this one: without
        // an answer the pointer kept whatever it last was (the busy one).
        WM_SETCURSOR => {
            SetCursor(LoadCursorW(None, IDC_ARROW).ok());
            LRESULT(1)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            crate::wallpaper::paint(hwnd);
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            desktop_menu(GetAncestor(hwnd, GA_ROOT));
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, m, wp, lp),
    }
}

/// A class of window for the desktop. Explorer's names are used, as programs
/// look for those (FindWindow "Progman", a "SHELLDLL_DefView" in it, a
/// "WorkerW" for the wallpaper).
unsafe fn register(instance: HINSTANCE, name: PCWSTR, proc: WNDPROC) {
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: proc,
        hInstance: instance,
        hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
        lpszClassName: name,
        ..Default::default()
    };
    RegisterClassExW(&wc);
}

unsafe extern "system" fn defview_proc(hwnd: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    DefWindowProcW(hwnd, m, wp, lp)
}

/// Makes the desktop and tells Windows it's the shell's. It's laid out as
/// Windows 11's own (24H2 on): a "Progman" over every screen holding a
/// "SHELLDLL_DefView" (where Explorer's icons go; empty and hidden here) and a
/// "WorkerW" behind it that shows the wallpaper. Wallpaper Engine and Lively
/// find that WorkerW and put their wallpaper in it, as they do on Explorer's.
unsafe fn create_desktop(instance: HINSTANCE) -> Option<HWND> {
    register(instance, w!("Progman"), Some(desktop_proc));
    register(instance, w!("SHELLDLL_DefView"), Some(defview_proc));
    register(instance, w!("WorkerW"), Some(wallpaper_proc));
    let r = virtual_screen();
    let (w, h) = (r.right - r.left, r.bottom - r.top);
    let hwnd = CreateWindowExW(
        WS_EX_TOOLWINDOW,
        w!("Progman"),
        w!("Program Manager"),
        WS_POPUP | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
        r.left,
        r.top,
        w,
        h,
        None,
        None,
        Some(instance),
        None,
    )
    .ok()?;
    let child = WS_CHILD | WS_CLIPCHILDREN | WS_CLIPSIBLINGS;
    let defview = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        w!("SHELLDLL_DefView"),
        w!(""),
        child,
        0,
        0,
        0,
        0,
        Some(hwnd),
        None,
        Some(instance),
        None,
    )
    .ok();
    // Behind the (empty) icon view, as Explorer's is.
    if let Ok(wall) = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        w!("WorkerW"),
        w!(""),
        child | WS_VISIBLE,
        0,
        0,
        w,
        h,
        Some(hwnd),
        None,
        Some(instance),
        None,
    ) {
        WALLPAPER.store(wall.0 as isize, Ordering::Release);
        if let Some(d) = defview {
            let _ = SetWindowPos(wall, Some(d), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
    }
    // Not in the headers, but how a shell names its desktop: GetShellWindow
    // then answers with it, for every program that asks where the desktop is.
    type SetShellWindow = unsafe extern "system" fn(HWND) -> BOOL;
    let mut is_shell = false;
    if let Ok(user32) = GetModuleHandleW(w!("user32.dll")) {
        if let Some(f) = GetProcAddress(user32, PCSTR(c"SetShellWindow".as_ptr() as *const u8)) {
            let f = std::mem::transmute::<unsafe extern "system" fn() -> isize, SetShellWindow>(f);
            is_shell = f(hwnd).as_bool();
        }
    }
    config::log(&format!("shell mode: desktop window made (Windows took it as the shell's: {is_shell})"));
    let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW;
    let _ = SetWindowPos(hwnd, Some(HWND_BOTTOM), 0, 0, 0, 0, flags);
    Some(hwnd)
}

/// The desktop's right-click menu.
unsafe fn desktop_menu(owner: HWND) {
    use crate::wallpaper::{self, Fit};
    let Ok(menu) = CreatePopupMenu() else { return };
    let Ok(fits) = CreatePopupMenu() else { return };
    let now = wallpaper::fit();
    for (i, (f, name)) in Fit::ALL.iter().enumerate() {
        let flags = if *f == now { MF_STRING | MF_CHECKED } else { MF_STRING };
        let _ = AppendMenuW(fits, flags, 100 + i, &HSTRING::from(*name));
    }
    let _ = AppendMenuW(menu, MF_STRING, 1, w!("Change wallpaper\u{2026}"));
    let _ = AppendMenuW(menu, MF_POPUP, fits.0 as usize, w!("Wallpaper fit"));
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    let _ = AppendMenuW(menu, MF_STRING, 2, w!("Task Manager"));
    let _ = AppendMenuW(menu, MF_STRING, 3, w!("Cascade settings\u{2026}"));
    let mut p = POINT::default();
    let _ = GetCursorPos(&mut p);
    let _ = SetForegroundWindow(owner);
    let id = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY, p.x, p.y, Some(0), owner, None);
    let _ = PostMessageW(Some(owner), WM_NULL, WPARAM(0), LPARAM(0));
    let _ = DestroyMenu(menu);
    match id.0 as usize {
        1 => {
            if let Some(path) = wallpaper::choose(owner) {
                wallpaper::set_picture(&path);
                reload_wallpaper();
            }
        }
        2 => shell::open("taskmgr.exe"),
        3 => {
            msg::post(msg::OPEN_SETTINGS, 0, 0);
        }
        n @ 100..=105 => {
            wallpaper::set_fit(Fit::ALL[n - 100].0);
            reload_wallpaper();
        }
        _ => {}
    }
}

/// With no shell, Windows keeps a small title-bar stub of every minimised
/// window at the bottom of the screen. A shell turns that off: its taskbar is
/// where minimised windows are.
fn hide_minimized(hide: bool) {
    const ARW_HIDE: i32 = 8;
    let none = SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0);
    unsafe {
        let mut mm = MINIMIZEDMETRICS { cbSize: std::mem::size_of::<MINIMIZEDMETRICS>() as u32, ..Default::default() };
        let size = mm.cbSize;
        if SystemParametersInfoW(
            SPI_GETMINIMIZEDMETRICS,
            size,
            Some(&mut mm as *mut MINIMIZEDMETRICS as *mut c_void),
            none,
        )
        .is_err()
        {
            return;
        }
        if hide {
            config::log(&format!("shell mode: minimised windows were arranged as {}", mm.iArrange.0));
            if mm.iArrange.0 & ARW_HIDE != 0 {
                return;
            }
            OLD_ARRANGE.store(mm.iArrange.0, Ordering::Release);
            mm.iArrange.0 |= ARW_HIDE;
        } else {
            let old = OLD_ARRANGE.swap(-1, Ordering::AcqRel);
            if old < 0 {
                return;
            }
            mm.iArrange.0 = old;
        }
        let _ = SystemParametersInfoW(
            SPI_SETMINIMIZEDMETRICS,
            size,
            Some(&mut mm as *mut MINIMIZEDMETRICS as *mut c_void),
            none,
        );
    }
}

/// Windows still leaves a stub of some minimised windows on the screen (the
/// setting above isn't always heeded without Explorer): moves any it finds
/// off the screen, as Windows does for a shell's. Their place when restored
/// isn't touched. Asked, not waited for, so a program that's stuck can't
/// hold Cascade up.
pub fn park_minimized() {
    if !active() {
        return;
    }
    for h in app_windows_any_state() {
        unsafe {
            if !IsIconic(h).as_bool() {
                continue;
            }
            let r = window_rect(h);
            if r.left > -10000 && r.top > -10000 {
                let flags = SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS;
                let _ = SetWindowPos(h, None, -32000, -32000, 0, 0, flags);
            }
        }
    }
}

/// Every visible top-level window that isn't Cascade's.
fn app_windows_any_state() -> Vec<HWND> {
    let mut all: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(collect), LPARAM(&mut all as *mut _ as isize));
    }
    let own = unsafe { GetCurrentProcessId() };
    all.retain(|&h| unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, Some(&mut pid));
        pid != own && IsWindowVisible(h).as_bool()
    });
    all
}

/// Tells Windows which window is the "task manager" (the taskbar, to it):
/// how a shell says it looks after minimised windows. Not in the headers.
unsafe fn set_taskman(window: HWND) {
    type SetTaskmanWindow = unsafe extern "system" fn(HWND) -> BOOL;
    let Ok(user32) = GetModuleHandleW(w!("user32.dll")) else { return };
    if let Some(f) = GetProcAddress(user32, PCSTR(c"SetTaskmanWindow".as_ptr() as *const u8)) {
        let f = std::mem::transmute::<unsafe extern "system" fn() -> isize, SetTaskmanWindow>(f);
        let ok = f(window).as_bool();
        config::log(&format!("shell mode: taskbar registered with Windows: {ok}"));
    }
}

/// Tells Windows the shell is up, so the sign-in screen gives way to the
/// desktop at once (it waits for this, or for a time-out).
fn signal_ready() {
    unsafe {
        if let Ok(event) = OpenEventW(EVENT_MODIFY_STATE, false, w!("ShellDesktopSwitchEvent")) {
            let _ = SetEvent(event);
            let _ = CloseHandle(event);
        }
    }
}

// ------------------------------------------------------------ start, end --

/// Takes up the shell's jobs. Called once Cascade's own windows exist.
pub fn init(instance: HINSTANCE, main: HWND) {
    config::log(&format!("shell mode: Cascade is the shell (started {} s after Windows)", shell::uptime_secs()));
    // Follow the exe if it has moved since shell mode was turned on.
    if let (Some(now), Some(want)) = (shell_value(), command()) {
        if is_ours(&now) && now != want {
            set_reg_text(HKEY_CURRENT_USER, WINLOGON, "Shell", &want);
        }
    }
    hide_minimized(true);
    unsafe { set_taskman(main) };
    park_minimized();
    crate::vdesk::load();
    if let Some(h) = unsafe { create_desktop(instance) } {
        DESKTOP.store(h.0 as isize, Ordering::Release);
    }
    register_hotkeys(main);
    signal_ready();
    ensure_guard();
    trial();
    reload_wallpaper();
    std::thread::spawn(|| {
        // Let Cascade's taskbar and tray settle first: programs add their tray icons as they start.
        std::thread::sleep(Duration::from_secs(2));
        run_startup();
    });
}

/// Hands the shell's jobs back (Cascade is closing).
pub fn shutdown(main: HWND) {
    if !active() {
        return;
    }
    unregister_hotkeys(main);
    // Windows on other desktops come back, unless the next copy is on its way to carry on with them.
    if INTENT.load(Ordering::Acquire) != 2 {
        crate::vdesk::restore_all();
    }
    let h = DESKTOP.swap(0, Ordering::AcqRel);
    WALLPAPER.store(0, Ordering::Release);
    if h != 0 {
        unsafe {
            let _ = DestroyWindow(HWND(h as *mut c_void));
        }
    }
    crate::wallpaper::release();
    hide_minimized(false);
    // Every screen gets its whole area back. Here and now (Cascade is about
    // to exit), and quietly: whoever is the shell next sets its own.
    reserved().clear();
    for s in screens() {
        set_work_area(s.all, s.all, false);
    }
}

// -------------------------------------------------------------- hotkeys --

#[derive(Clone, Copy, PartialEq)]
pub enum Snap {
    Left,
    Right,
    Up,
    Down,
}

/// What a shortcut does. The ones that need Cascade's own windows (`Search`,
/// `Start`, `App`, `Snip`) are handed back by `on_hotkey`; the rest are done here.
#[derive(Clone, Copy, PartialEq)]
pub enum Hotkey {
    Files,
    Run,
    Desktop,
    MinimizeAll,
    RestoreAll,
    Settings,
    Lock,
    Search,
    Start,
    Snap(Snap),
    /// Win+Shift+Left / Right: to the next screen that way.
    Screen(bool),
    /// Win+1 to Win+9: the nth app on the taskbar.
    App(usize),
    /// Ctrl+Win+Left / Right: the previous / next desktop.
    DeskStep(bool),
    /// Ctrl+Win+D: a new desktop, and go there.
    DeskNew,
    /// Ctrl+Win+F4: close this desktop.
    DeskClose,
    /// Win+Space: the next keyboard language.
    Language,
    /// Win+P: how several screens are used.
    Project,
    Snip,
    Magnifier,
    TaskManager,
}

const HOTKEY_BASE: i32 = 0x5C00;
const CTRL: u32 = 0x2;
const SHIFT: u32 = 0x4;
const WIN: u32 = 0x8;
const NO_REPEAT: u32 = 0x4000;

/// (modifiers, key, what it does): the shortcuts Explorer answers, which
/// nobody does when it isn't the shell. Win+X, Win+V, Win+N, Win+A, Win+Tab
/// and the Win key alone are Cascade's already (see `keyhook`).
fn hotkeys() -> Vec<(u32, u32, Hotkey)> {
    const LEFT: u32 = 0x25;
    const UP: u32 = 0x26;
    const RIGHT: u32 = 0x27;
    const DOWN: u32 = 0x28;
    const ESCAPE: u32 = 0x1B;
    let mut keys = vec![
        (WIN, b'E' as u32, Hotkey::Files),
        (WIN, b'R' as u32, Hotkey::Run),
        (WIN, b'D' as u32, Hotkey::Desktop),
        (WIN, b'M' as u32, Hotkey::MinimizeAll),
        (WIN | SHIFT, b'M' as u32, Hotkey::RestoreAll),
        (WIN, b'I' as u32, Hotkey::Settings),
        (WIN, b'L' as u32, Hotkey::Lock),
        (WIN, b'S' as u32, Hotkey::Search),
        (WIN, b'Q' as u32, Hotkey::Search),
        (CTRL, ESCAPE, Hotkey::Start),
        (WIN, LEFT, Hotkey::Snap(Snap::Left)),
        (WIN, RIGHT, Hotkey::Snap(Snap::Right)),
        (WIN, UP, Hotkey::Snap(Snap::Up)),
        (WIN, DOWN, Hotkey::Snap(Snap::Down)),
        (WIN | SHIFT, LEFT, Hotkey::Screen(false)),
        (WIN | SHIFT, RIGHT, Hotkey::Screen(true)),
        (WIN | SHIFT, b'S' as u32, Hotkey::Snip),
        // The + key, on the main block and on the number pad.
        (WIN, 0xBB, Hotkey::Magnifier),
        (WIN, 0x6B, Hotkey::Magnifier),
        (CTRL | SHIFT, ESCAPE, Hotkey::TaskManager),
        (CTRL | WIN, LEFT, Hotkey::DeskStep(false)),
        (CTRL | WIN, RIGHT, Hotkey::DeskStep(true)),
        (CTRL | WIN, b'D' as u32, Hotkey::DeskNew),
        (CTRL | WIN, 0x73, Hotkey::DeskClose),
        (WIN, 0x20, Hotkey::Language),
        (WIN, b'P' as u32, Hotkey::Project),
    ];
    for n in 0..9u32 {
        keys.push((WIN, b'1' as u32 + n, Hotkey::App(n as usize)));
    }
    keys
}

fn register_hotkeys(main: HWND) {
    let mut refused = Vec::new();
    for (i, (mods, vk, _)) in hotkeys().into_iter().enumerate() {
        let mods = HOT_KEY_MODIFIERS(mods | NO_REPEAT);
        if unsafe { RegisterHotKey(Some(main), HOTKEY_BASE + i as i32, mods, vk) }.is_err() {
            refused.push(i);
        }
    }
    // Windows keeps a few for itself (Win+L, Ctrl+Shift+Esc): those still work, just not through here.
    config::log(&format!("shell mode: shortcuts registered ({} of them Windows kept: {refused:?})", refused.len()));
}

fn unregister_hotkeys(main: HWND) {
    for i in 0..hotkeys().len() {
        unsafe {
            let _ = UnregisterHotKey(Some(main), HOTKEY_BASE + i as i32);
        }
    }
}

/// A registered shortcut was pressed (WM_HOTKEY's id). Does what it can
/// here; returns the ones that need Cascade's windows.
pub fn on_hotkey(id: i32) -> Option<Hotkey> {
    let key = hotkeys().get(usize::try_from(id - HOTKEY_BASE).ok()?)?.2;
    match key {
        Hotkey::Files => open_files(),
        Hotkey::Run => run_dialog(),
        Hotkey::Desktop => show_desktop(),
        Hotkey::MinimizeAll => minimize_all(),
        Hotkey::RestoreAll => restore_all(),
        Hotkey::Settings => open_settings("ms-settings:"),
        Hotkey::Lock => shell::power(shell::Power::Lock),
        Hotkey::Snap(dir) => snap(dir),
        Hotkey::Screen(right) => move_screen(right),

        Hotkey::Magnifier => shell::open("magnify.exe"),
        Hotkey::TaskManager => shell::open("taskmgr.exe"),
        Hotkey::DeskStep(right) => crate::vdesk::step(right),
        Hotkey::DeskNew => crate::vdesk::create(true),
        Hotkey::DeskClose => crate::vdesk::close(crate::vdesk::read().current),
        // Windows' Snipping Tool is a Store app: Cascade's own snip instead.
        Hotkey::Search | Hotkey::Start | Hotkey::App(_) | Hotkey::Snip | Hotkey::Language | Hotkey::Project => {
            return Some(key)
        }
    }
    None
}

/// The classic Control Panel page nearest to a `ms-settings:` link, for when
/// Windows' Settings app won't open.
fn classic_page(uri: &str) -> (&'static str, &'static str) {
    let page = uri.to_ascii_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| page.contains(w));
    if has(&["network", "wifi", "ethernet", "vpn", "proxy"]) {
        ("ncpa.cpl", "")
    } else if has(&["sound", "volume"]) {
        ("mmsys.cpl", "")
    } else if has(&["appsfeatures", "installed-apps", "optionalfeatures"]) {
        ("appwiz.cpl", "")
    } else if has(&["power", "battery"]) {
        ("powercfg.cpl", "")
    } else if has(&["dateandtime", "regionformatting"]) {
        ("timedate.cpl", "")
    } else if has(&["about", "system"]) {
        ("sysdm.cpl", "")
    } else if has(&["mouse"]) {
        ("main.cpl", "")
    } else if has(&["printers", "bluetooth", "connecteddevices"]) {
        ("control.exe", "printers")
    } else {
        ("control.exe", "")
    }
}

/// Opens a `ms-settings:` page. Windows' Settings is a Store-style app, and
/// those are opened through Explorer (asking for one without it can hang the
/// asker for a minute), so the nearest classic Control Panel page opens
/// instead, straight away. Says so the first time.
pub fn open_settings(uri: &str) {
    // Cascade's own page for it, where there's one (see `winset`).
    if let Some(page) = crate::winset::page_for(uri) {
        config::log(&format!("shell mode: {uri} -> Cascade's settings, page {page}"));
        msg::post(msg::OPEN_SETTINGS_AT, page + 1, 0);
        return;
    }
    static TOLD: AtomicBool = AtomicBool::new(false);
    let (file, args) = classic_page(uri);
    config::log(&format!("shell mode: {uri} -> {file} {args}"));
    let (file, args) = (file.to_string(), args.to_string());
    std::thread::spawn(move || {
        shell::launch(&file, &args, false);
    });
    if !TOLD.swap(true, Ordering::AcqRel) {
        msg::note("Windows' Settings needs Explorer: opening Control Panel instead");
    }
}

// ------------------------------------------------------------- launching --

/// Opens `target` as the shell: on a thread of its own, since asking Windows
/// to open something can wait on Explorer, and without it that's a wait for
/// nothing. Store-style apps go by the routes that don't need Explorer
/// (`open_packaged`); links to Explorer's own pages go to what's left of them.
pub fn open(target: &str, admin: bool) {
    let lower = target.trim().to_ascii_lowercase();
    if is_bare_explorer(target) {
        open_files();
        return;
    }
    if lower.starts_with("ms-settings:") {
        open_settings(target);
        return;
    }
    // Explorer's notification centre and quick settings: Cascade's widgets and Controls are those.
    if lower.starts_with("ms-actioncenter:") {
        config::log(&format!("shell mode: {target} is Explorer's; nothing to open"));
        return;
    }
    // Explorer's network list: the classic Network Connections window is what's left.
    if lower.starts_with("ms-availablenetworks:") {
        shell::launch("ncpa.cpl", "", false);
        return;
    }
    let target = target.to_string();
    std::thread::spawn(move || unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        if let Some(aumid) = packaged_id(&target) {
            open_packaged(&aumid, admin);
        } else if admin {
            shell::open_as_admin_direct(&target);
        } else {
            shell::open_direct(&target);
        }
        // Some ways of opening things finish on the thread that asked, after it has returned.
        std::thread::sleep(Duration::from_secs(5));
    });
}

/// The app id of a Store-style (packaged) app in `shell:AppsFolder\<id>`:
/// its package family (name, then `_` and 13 letters and digits), `!` and its id.
fn packaged_id(target: &str) -> Option<String> {
    let id = target.get(..17).filter(|p| p.eq_ignore_ascii_case("shell:AppsFolder\\")).map(|_| &target[17..])?;
    let (family, app) = id.split_once('!')?;
    let (_, publisher) = family.rsplit_once('_')?;
    let ok = !app.is_empty()
        && !id.contains('\\')
        && publisher.len() == 13
        && publisher.chars().all(|c| c.is_ascii_alphanumeric());
    ok.then(|| id.to_string())
}

/// Opens a packaged app without Explorer, by the first of these that works:
/// Windows' activation (as Explorer would; it may well need Explorer itself),
/// the app's own command (its execution alias, as typed in a terminal), or
/// for a desktop app in a package (Claude, Terminal and many more) its
/// program, started directly. Says so when none does.
fn open_packaged(aumid: &str, admin: bool) {
    let Some((family, app)) = aumid.split_once('!') else { return };
    // Windows' Settings, from the Start menu's list: as its links do.
    if family.to_ascii_lowercase().starts_with("windows.immersivecontrolpanel_") {
        open_settings("ms-settings:");
        return;
    }
    if !admin {
        match activate(aumid) {
            Ok(pid) => {
                config::log(&format!("shell mode: opened {aumid} through Windows (process {pid})"));
                return;
            }
            Err(e) => config::log(&format!("shell mode: Windows wouldn't open {aumid} ({e}); trying its program")),
        }
    }
    if let Some(alias) = execution_alias(family, app) {
        let ok = start(&alias, admin);
        config::log(&format!("shell mode: started {aumid} by its command {} ({ok})", alias.display()));
        if ok {
            return;
        }
    }
    match package_program(family, app) {
        Some((exe, true)) => {
            let ok = start(&exe, admin);
            config::log(&format!("shell mode: started {aumid} from its package, {} ({ok})", exe.display()));
            if ok {
                return;
            }
        }
        Some((_, false)) => config::log(&format!("shell mode: {aumid} is a Store app, which only Explorer opens")),
        None => config::log(&format!("shell mode: couldn't find {aumid}'s package")),
    }
    let name = app_name(aumid);
    msg::note(format!("{name} needs Explorer to open. Right-click the taskbar > Quit to Explorer to use it."));
}

/// Windows' own way to open a packaged app. Waited on for a few seconds at
/// most: without Explorer it can hang instead of failing.
fn activate(aumid: &str) -> Result<u32, String> {
    use windows::Win32::UI::Shell::{ApplicationActivationManager, IApplicationActivationManager, AO_NONE};
    let (tx, rx) = std::sync::mpsc::channel();
    let id = aumid.to_string();
    std::thread::spawn(move || unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let result = CoCreateInstance::<_, IApplicationActivationManager>(
            &ApplicationActivationManager,
            None,
            windows::Win32::System::Com::CLSCTX_ALL,
        )
        .and_then(|m| m.ActivateApplication(&HSTRING::from(id.as_str()), &HSTRING::new(), AO_NONE))
        .map_err(|e| e.to_string());
        let _ = tx.send(result);
    });
    rx.recv_timeout(Duration::from_secs(4)).unwrap_or_else(|_| Err("no answer in 4 s".into()))
}

/// The app's execution alias: the command a terminal knows it by (wt,
/// winget, python...), which Windows keeps in WindowsApps under the family.
fn execution_alias(family: &str, app: &str) -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("Microsoft\\WindowsApps").join(family);
    let mut found: Vec<PathBuf> = std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("exe")))
        .collect();
    found.sort();
    let named = found.iter().position(|p| p.file_stem().is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(app)));
    named.map(|i| found.swap_remove(i)).or_else(|| found.into_iter().next())
}

/// The program of app `app` in the installed package of `family`, and
/// whether it's a desktop program (one that can be started directly).
fn package_program(family: &str, app: &str) -> Option<(PathBuf, bool)> {
    use windows::Win32::Storage::Packaging::Appx::{GetPackagePathByFullName, GetPackagesByPackageFamily};
    unsafe {
        let family = HSTRING::from(family);
        let (mut count, mut len) = (0u32, 0u32);
        let _ = GetPackagesByPackageFamily(&family, &mut count, None, &mut len, None);
        if count == 0 || len == 0 {
            return None;
        }
        let mut names = vec![PWSTR::null(); count as usize];
        let mut buf = vec![0u16; len as usize];
        if GetPackagesByPackageFamily(
            &family,
            &mut count,
            Some(names.as_mut_ptr()),
            &mut len,
            Some(PWSTR(buf.as_mut_ptr())),
        ) != ERROR_SUCCESS
        {
            return None;
        }
        for name in names.into_iter().take(count as usize) {
            let mut plen = 0u32;
            let _ = GetPackagePathByFullName(PCWSTR(name.0), &mut plen, None);
            if plen == 0 {
                continue;
            }
            let mut path = vec![0u16; plen as usize];
            if GetPackagePathByFullName(PCWSTR(name.0), &mut plen, Some(PWSTR(path.as_mut_ptr()))) != ERROR_SUCCESS {
                continue;
            }
            let end = path.iter().position(|&c| c == 0).unwrap_or(path.len());
            let root = PathBuf::from(String::from_utf16_lossy(&path[..end]));
            let Ok(xml) = std::fs::read_to_string(root.join("AppxManifest.xml")) else { continue };
            if let Some((exe, desktop)) = manifest_app(&xml, app) {
                return Some((root.join(exe.replace('/', "\\")), desktop));
            }
        }
        None
    }
}

/// Attribute `name` of the XML start tag `tag` (`name="value"`).
fn attribute(tag: &str, name: &str) -> Option<String> {
    let mut rest = tag;
    while let Some(i) = rest.find(name) {
        let before = rest[..i].chars().last();
        let after = &rest[i + name.len()..];
        let after = after.trim_start();
        if before.is_some_and(|c| c.is_whitespace()) {
            if let Some(v) = after.strip_prefix('=') {
                let v = v.trim_start();
                let quote = v.chars().next()?;
                if quote == '"' || quote == '\'' {
                    let v = &v[1..];
                    return v.find(quote).map(|end| v[..end].to_string());
                }
            }
        }
        rest = &rest[i + name.len()..];
    }
    None
}

/// In a package's AppxManifest.xml: the program of the <Application> with
/// id `app`, and whether it's a desktop program (full trust) rather than a
/// Store app.
fn manifest_app(xml: &str, app: &str) -> Option<(String, bool)> {
    let mut rest = xml;
    while let Some(i) = rest.find("<Application") {
        let after = &rest[i + "<Application".len()..];
        // Not <Applications>, nor an <ApplicationContentUriRules> and the like.
        if !after.starts_with(|c: char| c.is_whitespace()) {
            rest = after;
            continue;
        }
        let end = after.find('>').unwrap_or(after.len());
        let tag = &after[..end];
        if attribute(tag, "Id").is_some_and(|id| id.eq_ignore_ascii_case(app)) {
            let exe = attribute(tag, "Executable")?;
            let entry = attribute(tag, "EntryPoint").unwrap_or_default();
            let behaviour = attribute(tag, "uap10:RuntimeBehavior").unwrap_or_default();
            let desktop = entry.eq_ignore_ascii_case("Windows.FullTrustApplication")
                || behaviour.eq_ignore_ascii_case("packagedClassicApp")
                || behaviour.eq_ignore_ascii_case("win32App");
            return Some((exe, desktop));
        }
        rest = &after[end..];
    }
    None
}

/// Starts a program, as administrator if `admin` (Windows asks first).
fn start(exe: &Path, admin: bool) -> bool {
    if admin {
        return shell::launch(&exe.to_string_lossy(), "", true);
    }
    let mut cmd = std::process::Command::new(exe);
    if let Some(dir) = exe.parent() {
        cmd.current_dir(dir);
    }
    cmd.spawn().is_ok()
}

/// What to call a packaged app in a message: its name in the Start menu's list, else its id.
fn app_name(aumid: &str) -> String {
    let (family, app) = aumid.split_once('!').unwrap_or((aumid, ""));
    let name = family.split('_').next().unwrap_or(family);
    let name = name.rsplit('.').next().unwrap_or(name);
    if name.is_empty() {
        app.to_string()
    } else {
        name.to_string()
    }
}

/// Whether `target` is Explorer itself with nothing to open.
pub fn is_bare_explorer(target: &str) -> bool {
    let t = target.trim().trim_matches('"').to_ascii_lowercase();
    t == "explorer.exe" || t == "explorer" || t.ends_with("\\explorer.exe")
}

/// A File Explorer window. Always with a folder to open: asked for nothing in
/// particular while it isn't the shell, Explorer may try to become it.
pub fn open_files() {
    // Quick access (Home on Windows 11).
    shell::launch("explorer.exe", "shell:::{679f85cb-0220-4080-b29b-5540cc05aab6}", false);
}

/// The Run box (Win+R). Explorer's own shows it; this is the same dialog,
/// asked of Windows directly.
pub fn run_dialog() {
    static OPEN: AtomicBool = AtomicBool::new(false);
    if OPEN.swap(true, Ordering::AcqRel) {
        return;
    }
    std::thread::spawn(|| unsafe {
        type RunFileDlg = unsafe extern "system" fn(HWND, HICON, PCWSTR, PCWSTR, PCWSTR, u32);
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        if let Ok(shell32) = LoadLibraryW(w!("shell32.dll")) {
            // Exported by number only.
            if let Some(f) = GetProcAddress(shell32, PCSTR(std::ptr::without_provenance(61))) {
                let f = std::mem::transmute::<unsafe extern "system" fn() -> isize, RunFileDlg>(f);
                f(HWND::default(), HICON::default(), PCWSTR::null(), PCWSTR::null(), PCWSTR::null(), 0);
            }
        }
        OPEN.store(false, Ordering::Release);
    });
}

// ------------------------------------------------------ windows: desktop --

unsafe extern "system" fn collect(h: HWND, out: LPARAM) -> BOOL {
    (*(out.0 as *mut Vec<HWND>)).push(h);
    true.into()
}

/// The app windows on screen, front to back.
fn app_windows() -> Vec<HWND> {
    let mut all: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(collect), LPARAM(&mut all as *mut _ as isize));
    }
    let own = unsafe { GetCurrentProcessId() };
    all.retain(|&h| crate::dock::is_app_window(h, own));
    all
}

/// The windows Show desktop put away, front one first.
static PUT_AWAY: Mutex<Vec<isize>> = Mutex::new(Vec::new());

fn put_away() -> std::sync::MutexGuard<'static, Vec<isize>> {
    PUT_AWAY.lock().unwrap_or_else(|e| e.into_inner())
}

fn minimize(h: HWND) {
    unsafe {
        let _ = ShowWindowAsync(h, SW_SHOWMINNOACTIVE);
    }
    // Windows won't let Cascade touch an admin app's window; the helper can.
    if crate::helper::window_above_us(h) {
        crate::helper::request_minimize(h);
    }
}

/// Win+M: every window out of the way.
pub fn minimize_all() {
    let open: Vec<HWND> = app_windows().into_iter().filter(|&h| unsafe { !IsIconic(h).as_bool() }).collect();
    if open.is_empty() {
        return;
    }
    for &h in &open {
        minimize(h);
    }
    *put_away() = open.iter().map(|h| h.0 as isize).collect();
    if let Some(d) = desktop() {
        unsafe {
            let _ = SetForegroundWindow(d);
        }
    }
}

/// Win+Shift+M: brings back what Show desktop or Win+M put away.
pub fn restore_all() {
    let list: Vec<isize> = put_away().drain(..).collect();
    // Back to front, so the window that was in front ends up there again.
    for h in list.into_iter().rev() {
        let h = HWND(h as *mut c_void);
        unsafe {
            if IsWindow(Some(h)).as_bool() && IsIconic(h).as_bool() {
                let _ = ShowWindowAsync(h, SW_RESTORE);
            }
        }
    }
}

/// Win+D: the desktop, and back again if nothing was touched since.
pub fn show_desktop() {
    let untouched = {
        let list = put_away();
        !list.is_empty()
            && list.iter().all(|&h| {
                let h = HWND(h as *mut c_void);
                unsafe { !IsWindow(Some(h)).as_bool() || IsIconic(h).as_bool() }
            })
    };
    let any_open = app_windows().into_iter().any(|h| unsafe { !IsIconic(h).as_bool() });
    if untouched && !any_open {
        restore_all();
    } else {
        minimize_all();
    }
}

// ----------------------------------------------------- windows: snapping --

/// A screen: all of it, and the part clear of taskbars.
#[derive(Clone, Copy)]
struct Screen {
    all: RECT,
    work: RECT,
}

unsafe extern "system" fn collect_screens(mon: HMONITOR, _: HDC, _: *mut RECT, out: LPARAM) -> BOOL {
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    if GetMonitorInfoW(mon, &mut mi).as_bool() {
        (*(out.0 as *mut Vec<Screen>)).push(Screen { all: mi.rcMonitor, work: mi.rcWork });
    }
    true.into()
}

/// The screens, left to right.
fn screens() -> Vec<Screen> {
    let mut list: Vec<Screen> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(collect_screens), LPARAM(&mut list as *mut _ as isize));
    }
    list.sort_by_key(|s| (s.all.left, s.all.top));
    list
}

fn screen_of(h: HWND) -> Option<Screen> {
    unsafe {
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        GetMonitorInfoW(MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST), &mut mi)
            .as_bool()
            .then_some(Screen { all: mi.rcMonitor, work: mi.rcWork })
    }
}

fn window_rect(h: HWND) -> RECT {
    let mut r = RECT::default();
    unsafe {
        let _ = GetWindowRect(h, &mut r);
    }
    r
}

/// What you see of window `h`: its rect without the invisible resize border
/// Windows adds round it.
fn visible_rect(h: HWND) -> RECT {
    let mut r = RECT::default();
    let ok = unsafe {
        DwmGetWindowAttribute(
            h,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut r as *mut RECT as *mut c_void,
            std::mem::size_of::<RECT>() as u32,
        )
    }
    .is_ok();
    if ok {
        r
    } else {
        window_rect(h)
    }
}

/// Moves `h` so that what you see of it fills `to`.
fn place(h: HWND, to: RECT) {
    let (win, vis) = (window_rect(h), visible_rect(h));
    let (l, t, r, b) = (vis.left - win.left, vis.top - win.top, win.right - vis.right, win.bottom - vis.bottom);
    unsafe {
        let flags = SWP_NOZORDER | SWP_NOACTIVATE;
        let _ = SetWindowPos(
            h,
            None,
            to.left - l,
            to.top - t,
            to.right - to.left + l + r,
            to.bottom - to.top + t + b,
            flags,
        );
    }
}

fn near(a: RECT, b: RECT) -> bool {
    let close = |x: i32, y: i32| (x - y).abs() <= 3;
    close(a.left, b.left) && close(a.top, b.top) && close(a.right, b.right) && close(a.bottom, b.bottom)
}

fn half(work: RECT, left: bool) -> RECT {
    let mid = work.left + (work.right - work.left) / 2;
    if left {
        RECT { right: mid, ..work }
    } else {
        RECT { left: mid, ..work }
    }
}

/// (window, where it was before it was snapped, where it was snapped to).
static SNAPPED: Mutex<Vec<(isize, RECT, RECT)>> = Mutex::new(Vec::new());

fn snapped() -> std::sync::MutexGuard<'static, Vec<(isize, RECT, RECT)>> {
    SNAPPED.lock().unwrap_or_else(|e| e.into_inner())
}

/// The app window in front, if it's one the shortcuts may move: it has the
/// frame `style` asks for, and isn't Cascade's own.
fn front_window(style: WINDOW_STYLE) -> Option<HWND> {
    unsafe {
        let h = GetForegroundWindow();
        if h.is_invalid() || !crate::dock::is_app_window(h, GetCurrentProcessId()) {
            return None;
        }
        // Moving a window waits for its program to answer: one that's stuck is left alone.
        if IsHungAppWindow(h).as_bool() {
            return None;
        }
        (GetWindowLongW(h, GWL_STYLE) as u32 & style.0 != 0).then_some(h)
    }
}

/// Win+arrow: the window in front to the left or right half of its screen,
/// maximised, or back down.
fn snap(dir: Snap) {
    let want = match dir {
        Snap::Left | Snap::Right => WS_THICKFRAME,
        Snap::Up => WS_MAXIMIZEBOX,
        Snap::Down => WS_MINIMIZEBOX,
    };
    let Some(h) = front_window(want) else { return };
    let Some(screen) = screen_of(h) else { return };
    let zoomed = unsafe { IsZoomed(h).as_bool() };
    let key = h.0 as isize;
    match dir {
        Snap::Up => unsafe {
            if !zoomed {
                let _ = ShowWindow(h, SW_MAXIMIZE);
            }
        },
        Snap::Down => unsafe {
            if zoomed {
                let _ = ShowWindow(h, SW_RESTORE);
                return;
            }
            // Snapped by Win+Left or Right: back to where it was. Else out of the way.
            let was = {
                let mut list = snapped();
                let found = list.iter().find(|e| e.0 == key).copied();
                list.retain(|e| e.0 != key);
                found
            };
            match was.filter(|e| near(visible_rect(h), e.2)) {
                Some((_, before, _)) => {
                    let flags = SWP_NOZORDER | SWP_NOACTIVATE;
                    let (w, ht) = (before.right - before.left, before.bottom - before.top);
                    let _ = SetWindowPos(h, None, before.left, before.top, w, ht, flags);
                }
                None => {
                    let _ = ShowWindow(h, SW_MINIMIZE);
                }
            }
        },
        Snap::Left | Snap::Right => {
            let left = dir == Snap::Left;
            let mut target = half(screen.work, left);
            if !zoomed && near(visible_rect(h), target) {
                // Already there: on to the facing half of the next screen that way.
                let all = screens();
                let Some(i) = all.iter().position(|s| s.all == screen.all) else { return };
                let next = if left { i.checked_sub(1) } else { Some(i + 1) };
                let Some(next) = next.and_then(|n| all.get(n)) else { return };
                target = half(next.work, !left);
            } else {
                if zoomed {
                    unsafe {
                        let _ = ShowWindow(h, SW_RESTORE);
                    }
                }
                let mut list = snapped();
                // A window snapped already keeps the place it had before the first snap.
                let resnap = list.iter().any(|e| e.0 == key && near(visible_rect(h), e.2));
                if !resnap {
                    list.retain(|e| e.0 != key);
                    if list.len() >= 64 {
                        list.remove(0);
                    }
                    list.push((key, window_rect(h), target));
                }
            }
            if let Some(e) = snapped().iter_mut().find(|e| e.0 == key) {
                e.2 = target;
            }
            place(h, target);
        }
    }
}

/// Win+Shift+Left / Right: the window in front to the next screen that way,
/// keeping its place and share of the screen.
fn move_screen(right: bool) {
    let Some(h) = front_window(WS_CAPTION) else { return };
    let Some(from) = screen_of(h) else { return };
    let all = screens();
    if all.len() < 2 {
        return;
    }
    let Some(i) = all.iter().position(|s| s.all == from.all) else { return };
    let to = all[if right { (i + 1) % all.len() } else { (i + all.len() - 1) % all.len() }];
    let zoomed = unsafe { IsZoomed(h).as_bool() };
    if zoomed {
        unsafe {
            let _ = ShowWindow(h, SW_RESTORE);
        }
    }
    let r = window_rect(h);
    let (fw, fh) = ((from.work.right - from.work.left).max(1), (from.work.bottom - from.work.top).max(1));
    let (tw, th) = (to.work.right - to.work.left, to.work.bottom - to.work.top);
    let scale = |v: i32, of: i32, to: i32| (v as i64 * to as i64 / of as i64) as i32;
    let (w, ht) = (scale(r.right - r.left, fw, tw).min(tw), scale(r.bottom - r.top, fh, th).min(th));
    let x = (to.work.left + scale(r.left - from.work.left, fw, tw)).clamp(to.work.left, to.work.right - w);
    let y = (to.work.top + scale(r.top - from.work.top, fh, th)).clamp(to.work.top, to.work.bottom - ht);
    unsafe {
        let _ = SetWindowPos(h, None, x, y, w, ht, SWP_NOZORDER | SWP_NOACTIVATE);
        if zoomed {
            let _ = ShowWindow(h, SW_MAXIMIZE);
        }
    }
}

// --------------------------------------------------------- startup apps --

/// Replaces `%NAME%` with that environment variable, as Windows does for
/// startup entries.
pub(crate) fn expand_env(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) => {
                let name = &after[..end];
                match std::env::var(name) {
                    Ok(v) if !name.is_empty() => out.push_str(&v),
                    _ => {
                        out.push('%');
                        out.push_str(name);
                        out.push('%');
                    }
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Splits a command line into the program and its arguments. An unquoted
/// path may hold spaces (`C:\Program Files\App\app.exe /background`): the
/// program is then the shortest run of words that names a file.
pub(crate) fn split_command(cmd: &str, is_file: &dyn Fn(&str) -> bool) -> (String, String) {
    let cmd = cmd.trim();
    if let Some(rest) = cmd.strip_prefix('"') {
        return match rest.find('"') {
            Some(end) => (rest[..end].to_string(), rest[end + 1..].trim().to_string()),
            None => (rest.to_string(), String::new()),
        };
    }
    let spaces: Vec<usize> = cmd.match_indices(' ').map(|(i, _)| i).collect();
    for &i in &spaces {
        let head = &cmd[..i];
        if is_file(head) || is_file(&format!("{head}.exe")) {
            return (head.to_string(), cmd[i + 1..].trim().to_string());
        }
    }
    if is_file(cmd) || spaces.is_empty() {
        return (cmd.to_string(), String::new());
    }
    let i = spaces[0];
    (cmd[..i].to_string(), cmd[i + 1..].trim().to_string())
}

/// Whether Task Manager's Startup page (Settings > Apps > Startup) has the
/// entry `name` of `list` turned off. Windows keeps that as a few bytes per
/// entry, the first one odd for off.
pub(crate) fn startup_disabled(list: &str, name: &str) -> bool {
    let key = HSTRING::from(format!("{EXPLORER_KEY}\\StartupApproved\\{list}"));
    let value = HSTRING::from(name);
    [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE].into_iter().any(|root| {
        let mut data = [0u8; 16];
        let mut size = data.len() as u32;
        let ok = unsafe {
            RegGetValueW(
                root,
                &key,
                &value,
                RRF_RT_REG_BINARY,
                None,
                Some(data.as_mut_ptr() as *mut c_void),
                Some(&mut size),
            )
        } == ERROR_SUCCESS;
        ok && size > 0 && data[0] & 1 == 1
    })
}

/// The (name, command) values of a Run key.
pub(crate) fn run_entries(root: HKEY, key: &str, wow: bool) -> Vec<(String, String)> {
    let mut out = Vec::new();
    unsafe {
        let mut handle = HKEY::default();
        let access = if wow { KEY_READ | KEY_WOW64_32KEY } else { KEY_READ };
        if RegOpenKeyExW(root, &HSTRING::from(key), Some(0), access, &mut handle) != ERROR_SUCCESS {
            return out;
        }
        for i in 0.. {
            let mut name = [0u16; 512];
            let mut name_len = name.len() as u32;
            let mut data = [0u16; 2048];
            let mut size = (data.len() * 2) as u32;
            let mut kind = REG_VALUE_TYPE(0);
            let r = RegEnumValueW(
                handle,
                i,
                Some(PWSTR(name.as_mut_ptr())),
                &mut name_len,
                None,
                Some(&mut kind as *mut REG_VALUE_TYPE as *mut u32),
                Some(data.as_mut_ptr() as *mut u8),
                Some(&mut size),
            );
            // A value too long for the buffer is skipped; anything else wrong ends the list.
            if r == ERROR_MORE_DATA {
                continue;
            }
            if r != ERROR_SUCCESS {
                break;
            }
            if kind != REG_SZ && kind != REG_EXPAND_SZ {
                continue;
            }
            let chars = &data[..(size as usize / 2).min(data.len())];
            let end = chars.iter().position(|&c| c == 0).unwrap_or(chars.len());
            let cmd = String::from_utf16_lossy(&chars[..end]);
            if !cmd.trim().is_empty() {
                out.push((String::from_utf16_lossy(&name[..name_len as usize]), cmd));
            }
        }
        let _ = RegCloseKey(handle);
    }
    out
}

/// The programs running now: their full paths and their file names, lower case.
fn running_programs() -> (Vec<String>, Vec<String>) {
    use windows::Win32::System::Diagnostics::ToolHelp::*;
    let (mut paths, mut names) = (Vec::new(), Vec::new());
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return (paths, names) };
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut more = Process32FirstW(snap, &mut e).is_ok();
        while more {
            let end = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
            names.push(String::from_utf16_lossy(&e.szExeFile[..end]).to_lowercase());
            if let Some(p) = crate::dock::process_path(e.th32ProcessID) {
                paths.push(p.to_lowercase());
            }
            more = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    (paths, names)
}

/// Whether program `exe` (a path, or a bare name found on the PATH) is one of `running`.
fn is_running(exe: &str, running: &(Vec<String>, Vec<String>)) -> bool {
    let exe = exe.to_lowercase();
    if exe.contains('\\') || exe.contains('/') {
        return running.0.contains(&exe);
    }
    let name = if exe.ends_with(".exe") { exe } else { format!("{exe}.exe") };
    running.1.contains(&name)
}

fn start_program(file: &str, args: &str) {
    let dir = Path::new(file).parent().filter(|d| d.is_dir()).map(|d| HSTRING::from(d.as_os_str()));
    unsafe {
        let dir = dir.as_ref().map(|d| PCWSTR(d.as_ptr())).unwrap_or(PCWSTR::null());
        let _ = ShellExecuteW(None, w!("open"), &HSTRING::from(file), &HSTRING::from(args), dir, SW_SHOWNORMAL);
    }
}

/// Where a shortcut (.lnk) points.
unsafe fn link_target(path: &Path) -> Option<String> {
    let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
    let file: IPersistFile = link.cast().ok()?;
    file.Load(&HSTRING::from(path.as_os_str()), STGM_READ).ok()?;
    let mut buf = [0u16; 1024];
    link.GetPath(&mut buf, std::ptr::null_mut(), 0).ok()?;
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..end])).filter(|t| !t.is_empty())
}

/// Runs what's set to start with Windows, which is Explorer's job when it's
/// the shell: the Run keys and the Startup folders, less what's been turned
/// off on Task Manager's Startup page. Once per sign-in: the same marker
/// Explorer leaves is left behind, so neither does it twice. Programs already
/// running are skipped, in case something else got to them first.
fn run_startup() {
    let mut session = 0u32;
    unsafe {
        let _ = ProcessIdToSessionId(GetCurrentProcessId(), &mut session);
    }
    let marker = HSTRING::from(format!("{EXPLORER_KEY}\\SessionInfo\\{session}\\StartupHasBeenRun"));
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, &marker, Some(0), KEY_READ, &mut key) == ERROR_SUCCESS {
            let _ = RegCloseKey(key);
            config::log("shell mode: startup apps were already run this session");
            return;
        }
        // Volatile: gone when you sign out.
        if RegCreateKeyExW(
            HKEY_CURRENT_USER,
            &marker,
            Some(0),
            None,
            REG_OPTION_VOLATILE,
            KEY_WRITE,
            None,
            &mut key,
            None,
        ) == ERROR_SUCCESS
        {
            let _ = RegCloseKey(key);
        }
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    let me = std::env::current_exe().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let running = running_programs();
    let is_file = |p: &str| Path::new(p).is_file();
    let (mut started, mut skipped) = (0, 0);
    let run = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
    let sources =
        [(HKEY_LOCAL_MACHINE, false, "Run"), (HKEY_LOCAL_MACHINE, true, "Run32"), (HKEY_CURRENT_USER, false, "Run")];
    for (root, wow, list) in sources {
        for (name, cmd) in run_entries(root, run, wow) {
            let (exe, args) = split_command(&expand_env(&cmd), &is_file);
            if exe.to_lowercase() == me || startup_disabled(list, &name) || is_running(&exe, &running) {
                skipped += 1;
                continue;
            }
            start_program(&exe, &args);
            started += 1;
            std::thread::sleep(Duration::from_millis(250));
        }
    }
    let folders = [
        std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("Microsoft\\Windows\\Start Menu\\Programs\\Startup")),
        std::env::var_os("ProgramData")
            .map(|a| PathBuf::from(a).join("Microsoft\\Windows\\Start Menu\\Programs\\Startup")),
    ];
    for folder in folders.into_iter().flatten() {
        let Ok(entries) = std::fs::read_dir(&folder) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if !path.is_file() || name.eq_ignore_ascii_case("desktop.ini") {
                continue;
            }
            let target = if name.to_lowercase().ends_with(".lnk") { unsafe { link_target(&path) } } else { None };
            let target_running = target.as_ref().is_some_and(|t| t.to_lowercase() == me || is_running(t, &running));
            if startup_disabled("StartupFolder", &name) || target_running {
                skipped += 1;
                continue;
            }
            start_program(&path.to_string_lossy(), "");
            started += 1;
            std::thread::sleep(Duration::from_millis(250));
        }
    }
    config::log(&format!(
        "shell mode: startup apps: started {started}, left {skipped} (turned off or already running)"
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_startup_commands() {
        let files = ["C:\\Program Files\\App\\app.exe", "C:\\Tools\\run.exe", "C:\\Tools\\bare.exe"];
        let is_file = |p: &str| files.contains(&p);
        let split = |c: &str| split_command(c, &is_file);
        assert_eq!(split("\"C:\\A B\\x.exe\" --min"), ("C:\\A B\\x.exe".into(), "--min".into()));
        assert_eq!(split("\"C:\\A B\\x.exe\""), ("C:\\A B\\x.exe".into(), String::new()));
        assert_eq!(
            split("C:\\Program Files\\App\\app.exe /background now"),
            ("C:\\Program Files\\App\\app.exe".into(), "/background now".into())
        );
        assert_eq!(split("C:\\Program Files\\App\\app.exe"), ("C:\\Program Files\\App\\app.exe".into(), String::new()));
        assert_eq!(split("C:\\Tools\\run.exe -a -b"), ("C:\\Tools\\run.exe".into(), "-a -b".into()));
        // The extension left off, as some entries do.
        assert_eq!(split("C:\\Tools\\bare /x"), ("C:\\Tools\\bare".into(), "/x".into()));
        // Nothing that names a file: the first word is the program.
        assert_eq!(split("SecurityHealthSystray.exe -tray"), ("SecurityHealthSystray.exe".into(), "-tray".into()));
        assert_eq!(split("notepad"), ("notepad".into(), String::new()));
    }

    #[test]
    fn expands_environment_names() {
        std::env::set_var("CASCADE_TEST_DIR", "C:\\Somewhere");
        assert_eq!(expand_env("%CASCADE_TEST_DIR%\\app.exe -x"), "C:\\Somewhere\\app.exe -x");
        assert_eq!(expand_env("100% sure"), "100% sure");
        assert_eq!(expand_env("%CASCADE_NO_SUCH_NAME%\\a"), "%CASCADE_NO_SUCH_NAME%\\a");
        assert_eq!(expand_env("plain"), "plain");
    }

    #[test]
    fn knows_explorer_with_nothing_to_open() {
        assert!(is_bare_explorer("explorer.exe"));
        assert!(is_bare_explorer("C:\\Windows\\explorer.exe"));
        assert!(is_bare_explorer("\"C:\\WINDOWS\\Explorer.EXE\""));
        assert!(!is_bare_explorer("C:\\Tools\\fileexplorer.exe "));
        assert!(!is_bare_explorer("shell:AppsFolder\\Some.App"));
    }

    #[test]
    fn tells_packaged_apps_apart() {
        assert_eq!(
            packaged_id("shell:AppsFolder\\Claude_pzs8sxrjxfjjc!Claude"),
            Some("Claude_pzs8sxrjxfjjc!Claude".into())
        );
        assert_eq!(
            packaged_id("shell:appsfolder\\Microsoft.WindowsCalculator_8wekyb3d8bbwe!App"),
            Some("Microsoft.WindowsCalculator_8wekyb3d8bbwe!App".into())
        );
        // Desktop apps' ids: an AppUserModelID, or a known folder and a path.
        assert_eq!(packaged_id("shell:AppsFolder\\com.squirrel.Discord.Discord"), None);
        assert_eq!(packaged_id("shell:AppsFolder\\{6D809377-6AF0-444B-8957-A3773F02200E}\\App\\app.exe"), None);
        assert_eq!(packaged_id("shell:AppsFolder\\Weird!Name"), None);
        assert_eq!(packaged_id("C:\\Tools\\app.exe"), None);
        assert_eq!(app_name("Claude_pzs8sxrjxfjjc!Claude"), "Claude");
        assert_eq!(app_name("Microsoft.WindowsCalculator_8wekyb3d8bbwe!App"), "WindowsCalculator");
    }

    #[test]
    fn reads_a_package_manifest() {
        let xml = r#"<?xml version="1.0"?>
<Package xmlns="x" xmlns:uap10="y">
  <Applications>
    <Application Id="Tool" Executable="tools\tool.exe" EntryPoint="Windows.FullTrustApplication">
      <uap:VisualElements DisplayName="Tool"/>
    </Application>
    <Application Id='App' Executable='Calc.exe' EntryPoint='Calc.App'/>
    <Application Id="Claude" Executable="app/claude.exe" uap10:RuntimeBehavior="packagedClassicApp"
        uap10:TrustLevel="mediumIL"/>
  </Applications>
</Package>"#;
        assert_eq!(manifest_app(xml, "Tool"), Some(("tools\\tool.exe".into(), true)));
        assert_eq!(manifest_app(xml, "app"), Some(("Calc.exe".into(), false)));
        assert_eq!(manifest_app(xml, "Claude"), Some(("app/claude.exe".into(), true)));
        assert_eq!(manifest_app(xml, "Missing"), None);
        assert_eq!(attribute(r#" AppId="a" Id="b""#, "Id"), Some("b".into()));
    }

    #[test]
    fn finds_the_classic_page_for_a_settings_link() {
        assert_eq!(classic_page("ms-settings:network-wifi"), ("ncpa.cpl", ""));
        assert_eq!(classic_page("ms-settings:bluetooth"), ("control.exe", "printers"));
        assert_eq!(classic_page("ms-settings:appsfeatures"), ("appwiz.cpl", ""));
        assert_eq!(classic_page("ms-settings:about"), ("sysdm.cpl", ""));
        assert_eq!(classic_page("ms-settings:"), ("control.exe", ""));
        assert_eq!(classic_page("ms-settings:clipboard"), ("control.exe", ""));
    }

    #[test]
    fn halves_a_screen() {
        let work = RECT { left: 100, top: 0, right: 1101, bottom: 800 };
        assert_eq!(half(work, true), RECT { left: 100, top: 0, right: 600, bottom: 800 });
        assert_eq!(half(work, false), RECT { left: 600, top: 0, right: 1101, bottom: 800 });
        assert!(near(
            RECT { left: 0, top: 0, right: 10, bottom: 10 },
            RECT { left: 2, top: -3, right: 10, bottom: 13 }
        ));
        assert!(!near(
            RECT { left: 0, top: 0, right: 10, bottom: 10 },
            RECT { left: 4, top: 0, right: 10, bottom: 10 }
        ));
    }
}
