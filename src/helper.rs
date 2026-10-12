//! The optional admin helper (`cascade.exe --key-helper`).
//!
//! Windows hides keys typed into apps running as administrator (Task
//! Manager, installers, admin terminals) from normal apps, so over those
//! Cascade can't see the Win key and the Windows Start menu opens instead.
//! This tiny second copy runs with admin rights from a scheduled task (you
//! approve it once) and only watches the Win key: no windows on screen, no
//! timers, no polling. It also brings Cascade's Start menu to the front
//! when Windows won't let Cascade do it itself.

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows::Win32::System::Threading::{
    AttachThreadInput, CreateMutexW, GetCurrentProcess, GetCurrentThreadId, GetExitCodeProcess, OpenProcessToken,
    WaitForSingleObject, INFINITE,
};
use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::keyhook;

const TASK: &str = "Cascade Win key helper";
const CLASS: PCWSTR = w!("CascadeKeyHelper");
/// Bring the window in wParam to the front.
const FOCUS: u32 = WM_APP + 1;
const QUIT: u32 = WM_APP + 2;
const MINIMIZE: u32 = WM_APP + 3;

/// The main Cascade window, if Cascade is running.
pub fn main_window() -> Option<HWND> {
    unsafe { FindWindowW(w!("CascadeWindow"), w!("Cascade")).ok() }
}

fn helper_window() -> Option<HWND> {
    unsafe { FindWindowW(CLASS, PCWSTR::null()).ok() }
}

pub fn running() -> bool {
    helper_window().is_some()
}

/// Whether this process runs with admin rights.
fn is_elevated() -> bool {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut e = TOKEN_ELEVATION::default();
        let mut len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut e as *mut _ as *mut std::ffi::c_void),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        )
        .is_ok();
        let _ = CloseHandle(token);
        ok && e.TokenIsElevated != 0
    }
}

/// Called by the key hook (in the helper): tells the main window something
/// happened (a lone Win press, an Alt+Tab step).
pub fn post_main(m: u32, wparam: usize) {
    if let Some(h) = main_window() {
        unsafe {
            let _ = PostMessageW(Some(h), m, WPARAM(wparam), LPARAM(0));
        }
    }
}

/// Asks the helper to minimise `hwnd` (an admin window Cascade can't touch).
pub fn request_minimize(hwnd: HWND) {
    if let Some(h) = helper_window() {
        unsafe {
            let _ = PostMessageW(Some(h), MINIMIZE, WPARAM(hwnd.0 as usize), LPARAM(0));
        }
    }
}

/// Asks the helper to bring `hwnd` to the front (from Cascade).
pub fn request_focus(hwnd: HWND) {
    if let Some(h) = helper_window() {
        unsafe {
            let _ = PostMessageW(Some(h), FOCUS, WPARAM(hwnd.0 as usize), LPARAM(0));
        }
    }
}

unsafe extern "system" fn proc(hwnd: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match m {
        FOCUS => {
            let target = HWND(wp.0 as *mut _);
            if IsIconic(target).as_bool() {
                let _ = ShowWindow(target, SW_RESTORE);
            }
            let fg = GetForegroundWindow();
            let fg_thread = GetWindowThreadProcessId(fg, None);
            let me = GetCurrentThreadId();
            let attach = fg_thread != 0 && fg_thread != me;
            if attach {
                let _ = AttachThreadInput(me, fg_thread, true);
            }
            let _ = SetForegroundWindow(target);
            if attach {
                let _ = AttachThreadInput(me, fg_thread, false);
            }
            LRESULT(0)
        }
        QUIT => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        keyhook::KEY_CHECK_MSG => {
            SetTimer(Some(hwnd), 1, 150, None);
            LRESULT(0)
        }
        WM_TIMER => {
            let _ = KillTimer(Some(hwnd), 1);
            keyhook::fix_stuck();
            LRESULT(0)
        }
        MINIMIZE => {
            let _ = ShowWindow(HWND(wp.0 as *mut _), SW_MINIMIZE);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, m, wp, lp),
    }
}

/// Runs the helper until Cascade asks it to stop.
pub fn run() {
    unsafe {
        let Ok(mutex) = CreateMutexW(None, true, w!("Local\\CascadeKeyHelper")) else { return };
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let _ = CloseHandle(mutex);
            return;
        }
        let Ok(module) = GetModuleHandleW(None) else { return };
        let instance: HINSTANCE = module.into();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(proc),
            hInstance: instance,
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        // A normal (never shown) window, so Cascade can find it by class.
        let Ok(hwnd) = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            CLASS,
            w!(""),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance),
            None,
        ) else {
            return;
        };
        // Let Cascade (not elevated) post these two messages to us.
        for m in [FOCUS, QUIT, MINIMIZE] {
            let _ = ChangeWindowMessageFilterEx(hwnd, m, MSGFLT_ALLOW, None);
        }
        keyhook::set_helper_mode(hwnd);
        let cfg = crate::config::Config::load();
        keyhook::configure(cfg.launcher && cfg.launcher_win_key, cfg.alt_tab);
        crate::config::log(&format!("helper: started (admin: {})", is_elevated()));
        let mut m = MSG::default();
        while GetMessageW(&mut m, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&m);
            DispatchMessageW(&m);
        }
        keyhook::configure(false, false);
    }
}

// ----------------------------------------------------------- installing --

/// `args` are the program's arguments; `elevated` picks admin rights.
fn task_xml(exe: &str, args: &str, desc: &str, elevated: bool) -> String {
    let user = format!(
        "{}\\{}",
        std::env::var("USERDOMAIN").unwrap_or_default(),
        std::env::var("USERNAME").unwrap_or_default()
    );
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>{desc}</Description></RegistrationInfo>
  <Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{user}</UserId></LogonTrigger></Triggers>
  <Principals><Principal id="Author"><UserId>{user}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>{level}</RunLevel></Principal></Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>false</StartWhenAvailable>
    <IdleSettings><StopOnIdleEnd>false</StopOnIdleEnd><RestartOnIdle>false</RestartOnIdle></IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>5</Priority>
  </Settings>
  <Actions Context="Author"><Exec><Command>{exe}</Command><Arguments>{args}</Arguments></Exec></Actions>
</Task>
"#,
        user = esc(&user),
        exe = esc(exe),
        args = esc(args),
        desc = esc(desc),
        level = if elevated { "HighestAvailable" } else { "LeastPrivilege" },
    )
}

/// Runs schtasks.exe with `args`, elevated (one permission prompt) or not,
/// and waits for it. Returns false if it couldn't run, was declined or
/// failed; what happened goes to the log.
fn schtasks(args: &str, elevated: bool) -> bool {
    unsafe {
        let file = HSTRING::from("schtasks.exe");
        let params = HSTRING::from(args);
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS,
            lpVerb: if elevated { w!("runas") } else { w!("open") },
            lpFile: PCWSTR(file.as_ptr()),
            lpParameters: PCWSTR(params.as_ptr()),
            nShow: SW_HIDE.0,
            ..Default::default()
        };
        if let Err(e) = ShellExecuteExW(&mut info) {
            crate::config::log(&format!("helper: schtasks {args} could not run: {e}"));
            return false;
        }
        if info.hProcess.is_invalid() {
            return false;
        }
        let _ = WaitForSingleObject(info.hProcess, INFINITE);
        let mut code = 1u32;
        let _ = GetExitCodeProcess(info.hProcess, &mut code);
        let _ = CloseHandle(info.hProcess);
        crate::config::log(&format!("helper: schtasks {args} -> exit code {code}"));
        code == 0
    }
}

/// Starts the helper now if it's set up but not running.
pub fn start() {
    if !running() {
        std::thread::spawn(|| schtasks(&format!("/Run /TN \"{TASK}\""), false));
    }
}

/// Stops the running helper (it is started again at the next sign-in, or
/// by `start`).
pub fn stop() {
    if let Some(h) = helper_window() {
        unsafe {
            let _ = PostMessageW(Some(h), QUIT, WPARAM(0), LPARAM(0));
        }
    }
}

/// Sets the helper up (asks for permission once) and starts it.
pub fn install() {
    std::thread::spawn(|| {
        let Ok(exe) = std::env::current_exe() else { return };
        let xml = task_xml(
            &exe.to_string_lossy(),
            "--key-helper",
            "Lets Cascade's Start menu open over apps run as administrator.",
            true,
        );
        if create_task(TASK, &xml) {
            schtasks(&format!("/Run /TN \"{TASK}\""), false);
        }
    });
}

/// Registers a scheduled task from `xml` (asks for permission once).
fn create_task(name: &str, xml: &str) -> bool {
    let path = crate::config::dir().join("task.xml");
    let _ = std::fs::create_dir_all(crate::config::dir());
    // Task Scheduler wants the file in UTF-16.
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend(xml.encode_utf16().flat_map(u16::to_le_bytes));
    if std::fs::write(&path, bytes).is_err() {
        return false;
    }
    let ok = schtasks(&format!("/Create /F /TN \"{name}\" /XML \"{}\"", path.display()), true);
    let _ = std::fs::remove_file(path);
    ok
}

const START_TASK: &str = "Cascade start at sign-in";

fn start_marker() -> std::path::PathBuf {
    crate::config::dir().join("start-task")
}

/// Whether Cascade's sign-in task was set up (a marker file, so it's cheap).
pub fn start_task_exists() -> bool {
    start_marker().exists()
}

/// Starts Cascade from a sign-in task: it runs the moment you sign in,
/// where the registry Run key waits until the desktop has finished loading.
/// Returns false if it couldn't be set up (permission declined).
pub fn install_start_task() -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let xml = task_xml(&exe.to_string_lossy(), "", "Starts Cascade as soon as you sign in.", false);
    let ok = create_task(START_TASK, &xml);
    if ok {
        let _ = std::fs::write(start_marker(), "");
    }
    ok
}

pub fn remove_start_task() {
    schtasks(&format!("/Delete /F /TN \"{START_TASK}\""), true);
    let _ = std::fs::remove_file(start_marker());
}

/// Stops the helper and removes its scheduled task (asks for permission).
pub fn uninstall() {
    stop();
    std::thread::spawn(|| schtasks(&format!("/Delete /F /TN \"{TASK}\""), true));
}

/// After an update the helper still runs the old copy: restart it.
pub fn restart() {
    if running() {
        stop();
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_millis(500));
            schtasks(&format!("/Run /TN \"{TASK}\""), false)
        });
    }
}
