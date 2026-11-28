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
    /// Who made it and how big it is, shown beside its name.
    hint: String,
    how: Remove,
}

enum Remove {
    /// A desktop program's own uninstaller (its command) and the registry key
    /// it's listed under (which goes when it has been uninstalled).
    Command(String, HKEY, String, REG_SAM_FLAGS),
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
            // (Opened through the right view for 32-bit programs.)
            let mut h = HKEY::default();
            let opened = unsafe {
                RegOpenKeyExW(root, &HSTRING::from(key.as_str()), Some(0), KEY_READ | access, &mut h)
                    == windows::Win32::Foundation::ERROR_SUCCESS
            };
            if !opened {
                continue;
            }
            let read = |v: &str| reg_text(h, "", v).map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
            let name = read("DisplayName");
            let cmd = read("UninstallString");
            let publisher = read("Publisher");
            let release = read("ReleaseType");
            let parent = read("ParentKeyName");
            let system = reg_dword(h, "", "SystemComponent") == Some(1);
            let no_remove = reg_dword(h, "", "NoRemove") == Some(1);
            let kb = reg_dword(h, "", "EstimatedSize").unwrap_or(0);
            unsafe {
                let _ = RegCloseKey(h);
            }
            let (Some(name), Some(cmd)) = (name, cmd) else { continue };
            // Parts of other programs, updates and Windows' own components aren't listed by Windows either,
            // nor are the ones that say they can't be removed.
            if parent.is_some() || release.is_some_and(|t| t.contains("Update")) || no_remove {
                continue;
            }
            if system && root == HKEY_CURRENT_USER {
                continue;
            }
            if out.iter().any(|o| o.name == name) {
                continue;
            }
            let hint = [publisher, (kb > 0).then(|| size_text(kb as u64 * 1024))]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join("  \u{b7}  ");
            out.push(Installed { name, hint, how: Remove::Command(cmd, root, key, access) });
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
            out.push(Installed {
                name,
                hint: "Store app".into(),
                how: Remove::Package(p.Id()?.FullName()?.to_string()),
            });
        }
        Ok(out)
    };
    get().unwrap_or_default()
}

/// "1.4 GB", "230 MB".
fn size_text(bytes: u64) -> String {
    let mb = bytes as f64 / 1_048_576.0;
    if mb >= 1024.0 {
        format!("{:.1} GB", mb / 1024.0)
    } else if mb >= 1.0 {
        format!("{} MB", mb.round() as u64)
    } else {
        format!("{} KB", (bytes / 1024).max(1))
    }
}

/// Whether the registry key still exists.
fn key_exists(root: HKEY, key: &str, access: REG_SAM_FLAGS) -> bool {
    unsafe {
        let mut h = HKEY::default();
        let ok = RegOpenKeyExW(root, &HSTRING::from(key), Some(0), KEY_READ | access, &mut h)
            == windows::Win32::Foundation::ERROR_SUCCESS;
        if ok {
            let _ = RegCloseKey(h);
        }
        ok
    }
}

/// Windows lists an installer's command as "install" (`MsiExec /I{...}`); removing is `/X`.
fn removal_command(cmd: &str) -> String {
    let lower = cmd.to_lowercase();
    if lower.contains("msiexec") {
        if let Some(i) = lower.find("/i{") {
            return format!("{}/X{}", &cmd[..i], &cmd[i + 2..]);
        }
    }
    cmd.to_string()
}

/// A glyph (of Windows' icon font) for what kind of thing `a` is: a Store
/// app, a driver, a runtime or library, or a desktop program.
fn glyph_of(a: &Installed) -> u32 {
    let n = a.name.to_lowercase();
    if matches!(a.how, Remove::Package(_)) {
        0xE719
    } else if ["driver", "nvidia", "geforce", "radeon", "chipset", "firmware", "realtek", "bluetooth"]
        .iter()
        .any(|k| n.contains(k))
    {
        0xE950
    } else if ["redistributable", "runtime", ".net", "vc++", "visual c++", "sdk", "java", "python", "node.js"]
        .iter()
        .any(|k| n.contains(k))
    {
        0xE943
    } else if ["game", "steam", "epic games", "xbox", "launcher"].iter().any(|k| n.contains(k)) {
        0xE7FC
    } else {
        0xE71D
    }
}

pub fn pick_uninstall(owner: HWND) {
    let mut all = programs();
    all.extend(packages());
    all.sort_by_key(|a| a.name.to_lowercase());
    all.dedup_by(|a, b| a.name.eq_ignore_ascii_case(&b.name));
    if all.is_empty() {
        super::tell("No apps to uninstall were found.".into());
        return;
    }
    // Their names with who made them and how big they are on the right; type to filter.
    let entries = all
        .iter()
        .enumerate()
        .map(|(i, a)| {
            crate::menu::Entry::Item(crate::menu::Item {
                id: i + 1,
                text: a.name.clone(),
                hint: a.hint.clone(),
                enabled: true,
                icon: glyph_of(a),
                ..Default::default()
            })
        })
        .collect();
    let Some(i) = crate::menu::show(owner, entries, crate::menu::Place::Cursor).checked_sub(1) else { return };
    let app = all.swap_remove(i);
    let detail = if app.hint.is_empty() { String::new() } else { format!("\n\n{}", app.hint) };
    if !confirm(owner, &format!("Uninstall {}?{detail}", app.name)) {
        return;
    }
    match app.how {
        Remove::Command(cmd, root, key, access) => {
            let is_file = |p: &str| Path::new(p).is_file();
            let cmd = removal_command(&cmd);
            let (exe, args) = crate::shellmode::split_command(&crate::shellmode::expand_env(&cmd), &is_file);
            // Uninstallers mostly need admin rights: Windows asks.
            crate::shell::launch_admin(&exe, &args);
            config::log(&format!("apps: uninstalling {} with {exe} {args}", app.name));
            // The program's entry goes when it's gone: tell the user then.
            let name = app.name.clone();
            let (root, access) = (root.0 as isize, access.0);
            std::thread::spawn(move || {
                let root = HKEY(root as *mut core::ffi::c_void);
                let access = REG_SAM_FLAGS(access);
                // Give the uninstaller a moment to start (the entry is there while it asks).
                for _ in 0..300 {
                    std::thread::sleep(std::time::Duration::from_secs(3));
                    if !key_exists(root, &key, access) {
                        config::log(&format!("apps: {name} is gone"));
                        msg::note(format!("{name} was uninstalled"));
                        return;
                    }
                }
            });
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
    // Ticked: starts with Windows. Picking one flips it, and the list stays so several can be changed.
    let mut at = windows::Win32::Foundation::POINT::default();
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut at);
    }
    let mut place = crate::menu::Place::Cursor;
    loop {
        let entries = apps
            .iter()
            .enumerate()
            .map(|(i, (name, list, _))| {
                let on = !crate::shellmode::startup_disabled(list, name);
                crate::menu::check(i + 1, name.trim_end_matches(".lnk"), on)
            })
            .collect();
        let id = crate::menu::show(owner, entries, place);
        if id == 0 {
            break;
        }
        let (name, list, root) = &apps[id - 1];
        let on = !crate::shellmode::startup_disabled(list, name);
        set_startup(name, list, *root, !on);
        super::changed();
        place = crate::menu::Place::Centre(at);
    }
}
