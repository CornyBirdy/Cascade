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
static DESKTOP: AtomicIsize = AtomicIsize::new(0);
/// How minimised windows were arranged before Cascade hid them (-1: not changed).
static OLD_ARRANGE: AtomicI32 = AtomicI32::new(-1);

/// Whether Explorer is the shell: it has made its taskbar. Told by the
/// program behind the window, since Cascade's own hidden tray window (this
/// copy's, or another's) has the same class.
fn explorer_is_shell() -> bool {
    unsafe {
        let mut after: Option<HWND> = None;
        while let Ok(h) = FindWindowExW(None, after, w!("Shell_TrayWnd"), PCWSTR::null()) {
            after = Some(h);
            let mut pid = 0u32;
            GetWindowThreadProcessId(h, Some(&mut pid));
            let explorer =
                crate::dock::process_path(pid).is_some_and(|p| p.to_ascii_lowercase().ends_with("\\explorer.exe"));
            if explorer {
                return true;
            }
        }
    }
    false
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

pub fn set_intent(intent: Intent) {
    if !active() {
        return;
    }
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
                Ok(mut child) => {
                    let _ = child.wait();
                }
                Err(e) => config::log(&format!("shell guard: Cascade wouldn't start: {e}")),
            },
        }
        if shutting_down() {
            return;
        }
        let intent = take_intent();
        match intent {
            Some(Intent::SessionEnd) => return,
            Some(Intent::Quit) => {
                // Cascade starts Explorer itself on its way out; this is in case it couldn't.
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

/// Starts the guard if this shell-mode Cascade has none (it was started some
/// other way: Setup, a full restart). The guard finds this copy and waits on it.
fn ensure_guard() {
    unsafe {
        let Ok(mutex) = CreateMutexW(None, false, GUARD_MUTEX) else { return };
        let running = GetLastError() == ERROR_ALREADY_EXISTS;
        let _ = CloseHandle(mutex);
        if running {
            return;
        }
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
    });
}

/// Called once Cascade's windows are gone: after Quit, Explorer takes over.
pub fn on_exit() {
    if active() && QUIT.load(Ordering::Acquire) {
        start_explorer(true);
    }
}

// ------------------------------------------------------------ work area --

/// (window, its screen, strip height): the space kept free along the bottom
/// of each screen for a taskbar.
static RESERVED: Mutex<Vec<(isize, RECT, i32)>> = Mutex::new(Vec::new());

fn reserved() -> std::sync::MutexGuard<'static, Vec<(isize, RECT, i32)>> {
    RESERVED.lock().unwrap_or_else(|e| e.into_inner())
}

/// Sets `screen`'s work area (where maximised windows go) to the screen less
/// the tallest strip reserved on it.
fn apply_work_area(screen: RECT) {
    let px = reserved().iter().filter(|e| e.1 == screen).map(|e| e.2).max().unwrap_or(0);
    let mut want = RECT { bottom: screen.bottom - px, ..screen };
    if shell::work_area_of(screen) == want {
        return;
    }
    unsafe {
        let _ = SystemParametersInfoW(SPI_SETWORKAREA, 0, Some(&mut want as *mut RECT as *mut c_void), SPIF_SENDCHANGE);
    }
    // Windows tells every window; Cascade's own are told again once it's
    // free to listen (this is often called from inside one of its handlers).
    msg::post(WM_SETTINGCHANGE, SPI_SETWORKAREA.0 as usize, 0);
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

unsafe extern "system" fn desktop_proc(hwnd: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match m {
        // The wallpaper, drawn by Windows itself.
        WM_ERASEBKGND => {
            let _ = PaintDesktop(HDC(wp.0 as *mut c_void));
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
        // Cascade's menu (the main window shows it at the pointer).
        WM_RBUTTONUP => {
            msg::post(WM_RBUTTONUP, 0, 0);
            LRESULT(0)
        }
        WM_DISPLAYCHANGE => {
            let r = virtual_screen();
            let flags = SWP_NOACTIVATE | SWP_NOZORDER;
            let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, flags);
            let _ = InvalidateRect(Some(hwnd), None, true);
            LRESULT(0)
        }
        WM_SETTINGCHANGE if wp.0 as u32 == SPI_SETDESKWALLPAPER.0 => {
            let _ = InvalidateRect(Some(hwnd), None, true);
            LRESULT(0)
        }
        // Alt+F4 on the desktop doesn't close it.
        WM_CLOSE => LRESULT(0),
        _ => DefWindowProcW(hwnd, m, wp, lp),
    }
}

/// Makes the desktop window and tells Windows it's the shell's.
unsafe fn create_desktop(instance: HINSTANCE) -> Option<HWND> {
    let class = w!("CascadeDesktop");
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(desktop_proc),
        hInstance: instance,
        hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
        lpszClassName: class,
        ..Default::default()
    };
    RegisterClassExW(&wc);
    let r = virtual_screen();
    let hwnd = CreateWindowExW(
        WS_EX_TOOLWINDOW,
        class,
        w!("Cascade Desktop"),
        WS_POPUP | WS_CLIPCHILDREN,
        r.left,
        r.top,
        r.right - r.left,
        r.bottom - r.top,
        None,
        None,
        Some(instance),
        None,
    )
    .ok()?;
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

/// Has Windows load the wallpaper. At sign-in Explorer does that; without it
/// the desktop is a plain colour until someone asks.
fn load_wallpaper() {
    let Some(path) =
        reg_text(HKEY_CURRENT_USER, "Control Panel\\Desktop", "WallPaper").filter(|p| !p.trim().is_empty())
    else {
        return;
    };
    if !Path::new(&path).is_file() {
        return;
    }
    let mut wide: Vec<u16> = path.encode_utf16().chain([0]).collect();
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_SETDESKWALLPAPER,
            0,
            Some(wide.as_mut_ptr() as *mut c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    }
    .is_ok();
    config::log(&format!("shell mode: wallpaper loaded: {ok}"));
    if let Some(d) = desktop() {
        unsafe {
            let _ = InvalidateRect(Some(d), None, true);
        }
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
    if let Some(h) = unsafe { create_desktop(instance) } {
        DESKTOP.store(h.0 as isize, Ordering::Release);
    }
    register_hotkeys(main);
    signal_ready();
    ensure_guard();
    trial();
    std::thread::spawn(|| {
        load_wallpaper();
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
    let h = DESKTOP.swap(0, Ordering::AcqRel);
    if h != 0 {
        unsafe {
            let _ = DestroyWindow(HWND(h as *mut c_void));
        }
    }
    hide_minimized(false);
    let screens: Vec<RECT> = reserved().drain(..).map(|e| e.1).collect();
    for s in screens {
        apply_work_area(s);
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
/// `Start`, `App`) are handed back by `on_hotkey`; the rest are done here.
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
    Snip,
    Magnifier,
    TaskManager,
}

const HOTKEY_BASE: i32 = 0x5C00;
const ALT_NONE: u32 = 0;
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
    ];
    for n in 0..9u32 {
        keys.push((WIN, b'1' as u32 + n, Hotkey::App(n as usize)));
    }
    keys
}

fn register_hotkeys(main: HWND) {
    let mut refused = Vec::new();
    for (i, (mods, vk, _)) in hotkeys().into_iter().enumerate() {
        let mods = HOT_KEY_MODIFIERS(mods | ALT_NONE | NO_REPEAT);
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
        Hotkey::Snip => shell::open("ms-screenclip:"),
        Hotkey::Magnifier => shell::open("magnify.exe"),
        Hotkey::TaskManager => shell::open("taskmgr.exe"),
        Hotkey::Search | Hotkey::Start | Hotkey::App(_) => return Some(key),
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

/// Whether Windows' Settings app has a window on screen.
fn settings_window_up() -> bool {
    let mut all: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(collect), LPARAM(&mut all as *mut _ as isize));
    }
    all.into_iter().any(|h| unsafe {
        IsWindowVisible(h).as_bool()
            && crate::dock::app_user_model_id(h)
                .is_some_and(|id| id.to_ascii_lowercase().contains("immersivecontrolpanel"))
    })
}

/// Opens a `ms-settings:` page. Windows' Settings is a Store-style app, and
/// those lean on Explorer being the shell: if no Settings window has come up
/// a few seconds later, the nearest classic Control Panel page opens instead.
pub fn open_settings(uri: &str) {
    unsafe {
        let _ = ShellExecuteW(None, w!("open"), &HSTRING::from(uri), None, None, SW_SHOWNORMAL);
    }
    let uri = uri.to_string();
    std::thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let until = Instant::now() + Duration::from_secs(5);
        while Instant::now() < until {
            if settings_window_up() {
                return;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        let (file, args) = classic_page(&uri);
        config::log(&format!("shell mode: Windows' Settings didn't open for {uri}; opening {file} {args} instead"));
        shell::launch(file, args, false);
    });
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
fn expand_env(text: &str) -> String {
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
fn split_command(cmd: &str, is_file: &dyn Fn(&str) -> bool) -> (String, String) {
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
fn startup_disabled(list: &str, name: &str) -> bool {
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
fn run_entries(root: HKEY, key: &str, wow: bool) -> Vec<(String, String)> {
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
            if r == ERROR_NO_MORE_ITEMS {
                break;
            }
            if r != ERROR_SUCCESS || (kind != REG_SZ && kind != REG_EXPAND_SZ) {
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
