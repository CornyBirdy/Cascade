//! Apps: uninstalling (desktop programs, by their own uninstaller, and Store
//! apps, removed for you as Windows' Settings does), and the startup apps
//! (on or off, the same switch as Task Manager's Startup page). Windows'
//! classic Programs and Features and Windows Features are a click away.

use std::path::Path;

use windows::core::HSTRING;
use windows::Management::Deployment::PackageManager;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Registry::*;

use super::{confirm, reg_dword, reg_text, set_reg_binary, subkeys};
use crate::{config, msg};

/// Something that can be uninstalled.
struct Installed {
    name: String,
    how: Remove,
}

enum Remove {
    /// A desktop program's own uninstaller (its command).
    Command(String),
    /// A Store app's package (its full name).
    Package(String),
}

const UNINSTALL: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall";

/// The desktop programs Windows lists in Installed apps.
fn programs() -> Vec<Installed> {
    let mut out: Vec<Installed> = Vec::new();
    for (root, access) in [
        (HKEY_LOCAL_MACHINE, KEY_WOW64_64KEY),
        (HKEY_LOCAL_MACHINE, KEY_WOW64_32KEY),
        (HKEY_CURRENT_USER, REG_SAM_FLAGS(0)),
    ] {
        for sub in subkeys(root, UNINSTALL, access) {
            let key = format!("{UNINSTALL}\\{sub}");
            let read = |v: &str| -> Option<String> {
                // (Opened through the right view for 32-bit programs.)
                unsafe {
                    let mut h = HKEY::default();
                    if RegOpenKeyExW(root, &HSTRING::from(key.as_str()), Some(0), KEY_READ | access, &mut h)
                        != windows::Win32::Foundation::ERROR_SUCCESS
                    {
                        return None;
                    }
                    let r = reg_text(h, "", v);
                    let _ = RegCloseKey(h);
                    r
                }
            };
            let Some(name) = read("DisplayName").filter(|n| !n.trim().is_empty()) else { continue };
            // Parts of other programs, updates and Windows' own components aren't listed by Windows either.
            if read("ParentKeyName").is_some() || read("ReleaseType").is_some_and(|t| t.contains("Update")) {
                continue;
            }
            if reg_dword(root, &key, "SystemComponent") == Some(1) && access == REG_SAM_FLAGS(0) {
                continue;
            }
            let Some(cmd) = read("UninstallString").filter(|c| !c.trim().is_empty()) else { continue };
            if out.iter().any(|o| o.name == name) {
                continue;
            }
            out.push(Installed { name, how: Remove::Command(cmd) });
        }
    }
    out
}

/// This user's Store apps that can be removed (not frameworks, nor Windows' own).
fn packages() -> Vec<Installed> {
    let get = || -> windows::core::Result<Vec<Installed>> {
        let pm = PackageManager::new()?;
        let mut out = Vec::new();
        for p in pm.FindPackagesByUserSecurityId(&HSTRING::new())? {
            if p.IsFramework().unwrap_or(true) || p.IsResourcePackage().unwrap_or(false) {
                continue;
            }
            use windows::ApplicationModel::PackageSignatureKind;
            if matches!(p.SignatureKind(), Ok(PackageSignatureKind::System)) {
                continue;
            }
            let name = p.DisplayName().map(|n| n.to_string()).unwrap_or_default();
            if name.is_empty() || name.starts_with("ms-resource:") {
                continue;
            }
            out.push(Installed { name, how: Remove::Package(p.Id()?.FullName()?.to_string()) });
        }
        Ok(out)
    };
    get().unwrap_or_default()
}

pub fn pick_uninstall(owner: HWND) {
    let mut all = programs();
    all.extend(packages());
    all.sort_by_key(|a| a.name.to_lowercase());
    all.dedup_by(|a, b| a.name.eq_ignore_ascii_case(&b.name));
    let names: Vec<String> = all.iter().map(|a| a.name.clone()).collect();
    let Some(i) = super::pick(owner, &names, None) else { return };
    let app = all.swap_remove(i);
    if !confirm(owner, &format!("Uninstall {}?", app.name)) {
        return;
    }
    match app.how {
        Remove::Command(cmd) => {
            let is_file = |p: &str| Path::new(p).is_file();
            let (exe, args) = crate::shellmode::split_command(&crate::shellmode::expand_env(&cmd), &is_file);
            // Uninstallers mostly need admin rights: Windows asks.
            let ok = crate::shell::launch(&exe, &args, true);
            config::log(&format!("apps: uninstalling {} with {exe} {args} (started: {ok})", app.name));
        }
        Remove::Package(full) => {
            let name = app.name.clone();
            std::thread::spawn(move || {
                let r = PackageManager::new()
                    .and_then(|pm| pm.RemovePackageAsync(&HSTRING::from(full.as_str())))
                    .and_then(|op| op.get());
                let ok = r.is_ok();
                config::log(&format!("apps: removed {name} ({full}): {ok}"));
                msg::note(if ok {
                    format!("{name} was uninstalled")
                } else {
                    format!("{name} couldn't be uninstalled")
                });
            });
        }
    }
}

// ------------------------------------------------------------- startup --

const RUN: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const APPROVED: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved";

/// The startup apps: (name, which list it's in for StartupApproved, its registry root).
fn startup_apps() -> Vec<(String, &'static str, HKEY)> {
    let mut out = Vec::new();
    for (root, wow, list) in
        [(HKEY_CURRENT_USER, false, "Run"), (HKEY_LOCAL_MACHINE, false, "Run"), (HKEY_LOCAL_MACHINE, true, "Run32")]
    {
        for (name, _) in crate::shellmode::run_entries(root, RUN, wow) {
            out.push((name, list, root));
        }
    }
    let folder = std::env::var_os("APPDATA")
        .map(|a| std::path::PathBuf::from(a).join("Microsoft\\Windows\\Start Menu\\Programs\\Startup"));
    if let Some(Ok(entries)) = folder.map(std::fs::read_dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.eq_ignore_ascii_case("desktop.ini") {
                out.push((name, "StartupFolder", HKEY_CURRENT_USER));
            }
        }
    }
    out
}

/// Turns a startup app on or off, as Task Manager does: a mark in
/// StartupApproved (2 on, 3 off), kept beside the entry's own key.
fn set_startup(name: &str, list: &str, root: HKEY, on: bool) {
    let mut data = [0u8; 12];
    data[0] = if on { 2 } else { 3 };
    if !on {
        let t = unsafe { windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime() };
        data[4..8].copy_from_slice(&t.dwLowDateTime.to_le_bytes());
        data[8..12].copy_from_slice(&t.dwHighDateTime.to_le_bytes());
    }
    // A machine-wide entry's switch is kept for this user (no admin needed), as Task Manager does too.
    let _ = root;
    let ok = set_reg_binary(HKEY_CURRENT_USER, &format!("{APPROVED}\\{list}"), name, &data);
    config::log(&format!("apps: startup app {name} {} (took: {ok})", if on { "on" } else { "off" }));
}

pub fn pick_startup(owner: HWND) {
    let apps = startup_apps();
    if apps.is_empty() {
        super::tell("Nothing is set to start with Windows.".into());
        return;
    }
    // Ticked: starts with Windows. Picking one flips it.
    unsafe {
        use windows::Win32::UI::WindowsAndMessaging::*;
        let Ok(menu) = CreatePopupMenu() else { return };
        for (i, (name, list, _)) in apps.iter().enumerate() {
            let on = !crate::shellmode::startup_disabled(list, name);
            let flags = if on { MF_STRING | MF_CHECKED } else { MF_STRING };
            let label = name.trim_end_matches(".lnk");
            let _ = AppendMenuW(menu, flags, i + 1, &HSTRING::from(label));
        }
        let id = crate::menu::track(owner, menu, crate::menu::Place::Cursor);
        let _ = DestroyMenu(menu);
        if id > 0 {
            let (name, list, root) = &apps[id - 1];
            let on = !crate::shellmode::startup_disabled(list, name);
            set_startup(name, list, *root, !on);
            super::changed();
        }
    }
}
