//! OpenRGB itself: finding it, starting it out of sight as the server
//! Cascade's lighting talks to, getting it when it isn't there, and spotting
//! the vendors' own RGB apps, which fight it (and Cascade) for the devices.
//!
//! OpenRGB is free and open source (GPL-2.0, openrgb.org); Cascade only talks
//! to it over its SDK, as any other program can. Lighting on RAM, motherboards
//! and graphics cards goes through their SMBus, which needs administrator
//! rights and the PawnIO driver (pawnio.eu); USB devices (keyboards, mice,
//! headsets, fan hubs, LED controllers) need neither.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// OpenRGB's Windows build (portable, 64-bit), from its own release page.
pub const DOWNLOAD: &str =
    "https://codeberg.org/OpenRGB/OpenRGB/releases/download/release_1.1/OpenRGB_1.1_Windows_64_8e16de2.zip";
const TASK: &str = "Cascade OpenRGB";

/// Where Cascade keeps the copy it gets.
pub fn dir() -> PathBuf {
    crate::config::dir().join("OpenRGB")
}

fn find_in(dir: &Path, depth: u32) -> Option<PathBuf> {
    let exe = dir.join("OpenRGB.exe");
    if exe.is_file() {
        return Some(exe);
    }
    if depth == 0 {
        return None;
    }
    std::fs::read_dir(dir).ok()?.flatten().filter(|e| e.path().is_dir()).find_map(|e| find_in(&e.path(), depth - 1))
}

/// OpenRGB.exe: the one set in the settings, Cascade's own copy, or an installed one.
pub fn find(custom: &str) -> Option<PathBuf> {
    let custom = custom.trim();
    if !custom.is_empty() && Path::new(custom).is_file() {
        return Some(PathBuf::from(custom));
    }
    find_in(&dir(), 2).or_else(|| {
        ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"]
            .iter()
            .filter_map(std::env::var_os)
            .map(PathBuf::from)
            .flat_map(|base| [base.join("OpenRGB"), base.join("Programs").join("OpenRGB")])
            .find_map(|d| find_in(&d, 1))
    })
}

/// Whether the server is answering on `port`.
pub fn running(port: u16) -> bool {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok()
}

/// What OpenRGB is started with: its SDK server on `port`, no window.
pub fn args(port: u16) -> Vec<String> {
    vec!["--server".into(), "--server-port".into(), port.to_string(), "--noautoconnect".into()]
}

/// Starts OpenRGB as the server (with administrator rights through its
/// approved task when `admin`). False if there's no OpenRGB to start.
pub fn start(custom: &str, port: u16, admin: bool) -> bool {
    if admin && crate::helper::run_task(TASK) {
        return true;
    }
    let Some(exe) = find(custom) else { return false };
    spawn(&exe, port)
}

fn spawn(exe: &Path, port: u16) -> bool {
    use std::os::windows::process::CommandExt;
    let mut cmd = std::process::Command::new(exe);
    if let Some(d) = exe.parent() {
        cmd.current_dir(d);
    }
    // CREATE_NO_WINDOW: no console flashing up for it.
    cmd.args(args(port)).creation_flags(0x0800_0000);
    let ok = cmd.spawn().is_ok();
    crate::config::log(&format!("lighting: started {} ({ok})", exe.display()));
    ok
}

/// `cascade.exe --openrgb`, run by the task with administrator rights:
/// starts OpenRGB with them (it inherits them).
pub fn run_elevated() {
    let l = super::model::Lighting::load();
    if let Some(exe) = find(&l.settings.openrgb) {
        spawn(&exe, l.settings.port);
    }
}

/// PawnIO's installer (it takes `-install -silent`).
const PAWNIO_SETUP: &str = "https://github.com/namazso/PawnIO.Setup/releases/latest/download/PawnIO_setup.exe";

/// Whether the PawnIO driver is installed (what OpenRGB reaches RAM,
/// motherboard and graphics card lighting through).
pub fn pawnio_installed() -> bool {
    use windows::Win32::System::Registry::HKEY_LOCAL_MACHINE;
    crate::winset::reg_text(
        HKEY_LOCAL_MACHINE,
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\PawnIO",
        "InstallLocation",
    )
    .is_some()
        || std::path::Path::new(r"C:\Windows\System32\drivers\PawnIO.sys").exists()
}

/// Whether OpenRGB is set up to start with administrator rights.
pub fn admin_ready() -> bool {
    crate::helper::task_exists(TASK)
}

/// Turns administrator rights on (PawnIO installed if it isn't, the task that
/// starts OpenRGB with them made, and a running OpenRGB stopped so it comes
/// back with them) or off, all under one permission prompt. `step` hears
/// what's happening. Slow: call off the window's thread.
pub fn set_admin(on: bool, step: &dyn Fn(String)) -> Result<(), String> {
    let mut parts: Vec<String> = Vec::new();
    if on {
        let d = dir();
        std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
        if !pawnio_installed() {
            let setup = d.join("PawnIO_setup.exe");
            let mut last = String::new();
            for _ in 0..3 {
                step("Getting the PawnIO driver\u{2026}".into());
                match crate::http::download(PAWNIO_SETUP, &setup, &|done, total| {
                    if let Some(pct) = (done * 100).checked_div(total) {
                        step(format!("Getting the PawnIO driver\u{2026} {pct}%"));
                    }
                }) {
                    Ok(_) => {
                        last.clear();
                        break;
                    }
                    Err(e) => last = e,
                }
            }
            if !last.is_empty() {
                return Err(format!("couldn't get PawnIO ({last})"));
            }
            parts.push(format!("\"{}\" -install -silent", setup.display()));
        }
        let xml = crate::helper::task_file(
            TASK,
            "--openrgb",
            "Starts OpenRGB with administrator rights for Cascade's lighting (RAM, motherboard and graphics card RGB).",
        )
        .ok_or("couldn't write the task")?;
        parts.push(format!("schtasks /Create /F /TN \"{TASK}\" /XML \"{}\"", xml.display()));
    } else {
        parts.push(format!("schtasks /Delete /F /TN \"{TASK}\""));
    }
    // A running OpenRGB is stopped either way, to come back with (or without) the rights.
    parts.push("taskkill /F /IM OpenRGB.exe".into());
    step("Waiting for Windows to ask for permission\u{2026}".into());
    // Starts with `cd` so cmd keeps every quote (see `cmd /?` on /C and quotes).
    let line = format!("cd /d \"{}\" & {}", dir().display(), parts.join(" & "));
    let r = crate::winset::run_admin(&line);
    let _ = std::fs::remove_file(dir().join("PawnIO_setup.exe"));
    if r.is_none() {
        return Err("Windows' permission prompt was turned down".into());
    }
    crate::config::log(&format!("lighting: administrator rights {}", if on { "on" } else { "off" }));
    if on && !admin_ready() {
        return Err("the task wasn't made".into());
    }
    if on && !pawnio_installed() {
        return Err("PawnIO didn't install (a restart may finish it)".into());
    }
    Ok(())
}

/// Gets OpenRGB (its portable build) into Cascade's folder, telling `progress`
/// how far it is (0..1). Slow: call off the window's thread.
pub fn download(progress: &dyn Fn(f32)) -> Result<PathBuf, String> {
    let d = dir();
    std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
    let zip = d.join("OpenRGB.zip");
    let mut last = String::new();
    for i in 0..3 {
        match crate::http::download(DOWNLOAD, &zip, &|done, total| {
            if total > 0 {
                progress(done as f32 / total as f32);
            }
        }) {
            Ok(n) if n > 1_000_000 => {
                last.clear();
                break;
            }
            Ok(n) => last = format!("only {n} bytes came"),
            Err(e) => last = e,
        }
        crate::config::log(&format!("lighting: OpenRGB download try {}: {last}", i + 1));
        std::thread::sleep(Duration::from_secs(2));
    }
    if !last.is_empty() {
        return Err(last);
    }
    // Windows' own tar unpacks zips.
    let ok = crate::winset::run_hidden("tar.exe", &["-xf", &zip.to_string_lossy(), "-C", &d.to_string_lossy()], true);
    let _ = std::fs::remove_file(&zip);
    if !ok {
        return Err("couldn't unpack it".into());
    }
    let exe = find_in(&d, 2).ok_or_else(|| "OpenRGB.exe wasn't in the download".to_string())?;
    crate::config::log(&format!("lighting: OpenRGB is at {}", exe.display()));
    Ok(exe)
}

/// The vendors' RGB apps that are running (they hold their devices, and
/// fight OpenRGB for them): (process name, what it is).
pub fn rivals() -> Vec<&'static str> {
    const KNOWN: [(&str, &str); 14] = [
        ("icue.exe", "Corsair iCUE"),
        ("corsair.service.exe", "Corsair iCUE (service)"),
        ("razer synapse service.exe", "Razer Synapse"),
        ("razerappengine.exe", "Razer Synapse"),
        ("lghub.exe", "Logitech G Hub"),
        ("lghub_agent.exe", "Logitech G Hub"),
        ("armourycrate.service.exe", "ASUS Armoury Crate"),
        ("lightingservice.exe", "ASUS Aura (Lighting Service)"),
        ("rgbfusion.exe", "Gigabyte RGB Fusion"),
        ("gcc.exe", "Gigabyte Control Center"),
        ("signalrgb.exe", "SignalRGB"),
        ("msi.centralserver.exe", "MSI Center"),
        ("nzxt cam.exe", "NZXT CAM"),
        ("ngenuity2.exe", "HyperX NGENUITY"),
    ];
    use windows::Win32::System::Diagnostics::ToolHelp::*;
    let mut out: Vec<&'static str> = Vec::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return out };
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        while ok {
            let n = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(0);
            let name = String::from_utf16_lossy(&e.szExeFile[..n]).to_ascii_lowercase();
            if let Some((_, what)) = KNOWN.iter().find(|(p, _)| *p == name) {
                if !out.contains(what) {
                    out.push(what);
                }
            }
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = windows::Win32::Foundation::CloseHandle(snap);
    }
    out
}
