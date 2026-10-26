//! Cascade's installer: one exe that carries Cascade itself.
//!
//! Windows Security tends to remove an unsigned program that listens to the
//! keyboard, and often does it while an update is being written. So the
//! installer asks once for permission to exclude Cascade's folders from
//! scanning, and only then puts the files there. Nothing is downloaded.

#![windows_subsystem = "windows"]

use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject, INFINITE};
use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
use windows::Win32::UI::WindowsAndMessaging::*;

/// The exe CI built just before this one (see `CASCADE_EXE` in the workflow).
static CASCADE: &[u8] = include_bytes!(env!("CASCADE_EXE"));

/// Keeps a child process from opening a console window.
const NO_WINDOW: u32 = 0x0800_0000;

fn say(text: &str, style: MESSAGEBOX_STYLE) -> MESSAGEBOX_RESULT {
    unsafe { MessageBoxW(None, &HSTRING::from(text), w!("Cascade setup"), style) }
}

/// Runs a program as administrator (Windows asks first) and waits for it.
/// True if it started and finished without an error code.
fn run_elevated(file: &str, args: &str) -> bool {
    unsafe {
        let file = HSTRING::from(file);
        let params = HSTRING::from(args);
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS,
            lpVerb: w!("runas"),
            lpFile: PCWSTR(file.as_ptr()),
            lpParameters: PCWSTR(params.as_ptr()),
            nShow: SW_HIDE.0,
            ..Default::default()
        };
        if ShellExecuteExW(&mut info).is_err() || info.hProcess.is_invalid() {
            return false;
        }
        let _ = WaitForSingleObject(info.hProcess, INFINITE);
        let mut code = 1u32;
        let _ = GetExitCodeProcess(info.hProcess, &mut code);
        let _ = CloseHandle(info.hProcess);
        code == 0
    }
}

/// Excludes the folders from Windows Security's scans.
fn exclude(folders: &[&Path]) -> bool {
    let list: Vec<String> =
        folders.iter().map(|f| format!("'{}'", f.to_string_lossy().replace('\'', "''"))).collect();
    let args = format!("-NoProfile -WindowStyle Hidden -Command \"Add-MpPreference -ExclusionPath {}\"", list.join(", "));
    run_elevated("powershell.exe", &args)
}

/// Stops a running Cascade, so its exe can be replaced.
fn stop_running() {
    let _ = Command::new("taskkill.exe").args(["/IM", "cascade.exe", "/F"]).creation_flags(NO_WINDOW).status();
}

/// Writes the exe, waiting a moment for a stopping copy to let go of it.
fn write_exe(exe: &Path) -> std::io::Result<()> {
    let mut last = Ok(());
    for _ in 0..10 {
        last = std::fs::write(exe, CASCADE);
        if last.is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    last
}

fn main() {
    let (Some(local), Some(roaming)) = (std::env::var_os("LOCALAPPDATA"), std::env::var_os("APPDATA")) else {
        say("Windows didn't say where to install to.", MB_OK | MB_ICONERROR);
        return;
    };
    let dir = Path::new(&local).join("Programs").join("Cascade");
    let data = Path::new(&roaming).join("Cascade");
    let exe = dir.join("cascade.exe");

    let ask = format!(
        "Install Cascade into\n{}\n\n\
         Cascade isn't code-signed and listens to the keyboard for the Win key, so Windows Security \
         tends to remove it. Setup asks Windows once for permission to leave Cascade's folders out of \
         its scans, then installs and starts Cascade. Running this again updates it.",
        dir.display()
    );
    if say(&ask, MB_YESNO | MB_ICONQUESTION) != IDYES {
        return;
    }

    let excluded = exclude(&[&dir, &data]);
    stop_running();
    if let Err(e) = std::fs::create_dir_all(&dir).and_then(|()| write_exe(&exe)) {
        say(&format!("Couldn't install Cascade: {e}"), MB_OK | MB_ICONERROR);
        return;
    }
    // Setup itself isn't elevated, so neither is Cascade.
    if let Err(e) = Command::new(&exe).spawn() {
        say(&format!("Cascade is installed in {} but wouldn't start: {e}", dir.display()), MB_OK | MB_ICONERROR);
        return;
    }
    if excluded {
        say("Cascade is installed and running. It adds itself to the Start menu.", MB_OK | MB_ICONINFORMATION);
    } else {
        say(
            "Cascade is installed and running, but Windows Security wasn't told to leave its folder alone \
             (permission was declined, or another antivirus is in charge). If Cascade disappears, run \
             Setup again and say yes to the permission prompt.",
            MB_OK | MB_ICONWARNING,
        );
    }
}
