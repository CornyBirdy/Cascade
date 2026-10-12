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
use windows::Win32::System::Threading::{
    AttachThreadInput, CreateMutexW, GetCurrentThreadId, WaitForSingleObject, INFINITE,
};
use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::{keyhook, msg};

const TASK: &str = "Cascade Win key helper";
const CLASS: PCWSTR = w!("CascadeKeyHelper");
/// Bring the window in wParam to the front.
const FOCUS: u32 = WM_APP + 1;
const QUIT: u32 = WM_APP + 2;

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

/// Called by the key hook (in the helper) on a lone Win press.
pub fn lone_win() {
    if let Some(h) = main_window() {
        unsafe {
            let _ = PostMessageW(Some(h), msg::OPEN_LAUNCHER, WPARAM(0), LPARAM(0));
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
        for m in [FOCUS, QUIT] {
            let _ = ChangeWindowMessageFilterEx(hwnd, m, MSGFLT_ALLOW, None);
        }
        keyhook::set_helper_mode();
        keyhook::set(true);
        let mut m = MSG::default();
        while GetMessageW(&mut m, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&m);
            DispatchMessageW(&m);
        }
        keyhook::set(false);
    }
}

// ----------------------------------------------------------- installing --

fn task_xml(exe: &str) -> String {
    let user = format!(
        "{}\\{}",
        std::env::var("USERDOMAIN").unwrap_or_default(),
        std::env::var("USERNAME").unwrap_or_default()
    );
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>Lets Cascade's Start menu open over apps run as administrator.</Description></RegistrationInfo>
  <Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{user}</UserId></LogonTrigger></Triggers>
  <Principals><Principal id="Author"><UserId>{user}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>HighestAvailable</RunLevel></Principal></Principals>
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
  <Actions Context="Author"><Exec><Command>{exe}</Command><Arguments>--key-helper</Arguments></Exec></Actions>
</Task>
"#,
        user = esc(&user),
        exe = esc(exe),
    )
}

/// Runs schtasks.exe with `args`, elevated (one permission prompt) or not,
/// and waits for it. Returns false if it couldn't run or was declined.
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
        if ShellExecuteExW(&mut info).is_err() || info.hProcess.is_invalid() {
            return false;
        }
        let _ = WaitForSingleObject(info.hProcess, INFINITE);
        let _ = CloseHandle(info.hProcess);
        true
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
        let xml = task_xml(&exe.to_string_lossy());
        let path = crate::config::dir().join("key-helper-task.xml");
        let _ = std::fs::create_dir_all(crate::config::dir());
        // Task Scheduler wants the file in UTF-16.
        let mut bytes = vec![0xFF, 0xFE];
        bytes.extend(xml.encode_utf16().flat_map(u16::to_le_bytes));
        if std::fs::write(&path, bytes).is_err() {
            return;
        }
        let args = format!("/Create /F /TN \"{TASK}\" /XML \"{}\"", path.display());
        if schtasks(&args, true) {
            schtasks(&format!("/Run /TN \"{TASK}\""), false);
        }
        let _ = std::fs::remove_file(path);
    });
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
