//! Gaming: Game Mode, the Game Bar and background recording, and graphics:
//! which GPU each app runs on, hardware-accelerated GPU scheduling, and the
//! default graphics settings (optimisations for windowed games, variable
//! refresh rate, Auto HDR). As Windows' Settings > Gaming and System >
//! Display > Graphics, through the same settings, so games and DirectX
//! follow them as they do Windows' own pages.

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

use super::{changed, pick, reg_dword, reg_text, run_admin, set_reg_dword, set_reg_text, tell};
use crate::config;

const GAMEBAR: &str = "Software\\Microsoft\\GameBar";
const DVR_STORE: &str = "System\\GameConfigStore";
const DVR: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\GameDVR";
const GPU_PREFS: &str = "Software\\Microsoft\\DirectX\\UserGpuPreferences";
const GLOBAL: &str = "DirectXUserGlobalSettings";
const DRIVERS: &str = "SYSTEM\\CurrentControlSet\\Control\\GraphicsDrivers";

/// The GPU choices for an app, as Windows' Graphics page names them.
const GPU_CHOICES: [&str; 3] = ["Let Windows decide", "Power saving", "High performance"];

/// One of the default graphics settings ("VRROptimizeEnable" and the like):
/// Windows keeps them as `name=value;` pairs in one string.
fn global(name: &str) -> Option<bool> {
    let s = reg_text(HKEY_CURRENT_USER, GPU_PREFS, GLOBAL)?;
    s.split(';').find_map(|kv| kv.split_once('=').filter(|(k, _)| *k == name).map(|(_, v)| v == "1"))
}

fn set_global(name: &str, on: bool) {
    let s = reg_text(HKEY_CURRENT_USER, GPU_PREFS, GLOBAL).unwrap_or_default();
    let mut pairs: Vec<(String, String)> = s
        .split(';')
        .filter_map(|kv| kv.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())))
        .filter(|(k, _)| k != name)
        .collect();
    pairs.push((name.to_string(), (on as u8).to_string()));
    let text: String = pairs.iter().map(|(k, v)| format!("{k}={v};")).collect();
    set_reg_text(HKEY_CURRENT_USER, GPU_PREFS, GLOBAL, &text);
}

/// The apps given a GPU: (exe path, choice index).
fn gpu_apps() -> Vec<(String, usize)> {
    let mut out = Vec::new();
    unsafe {
        use windows::core::{HSTRING, PWSTR};
        use windows::Win32::System::Registry::*;
        let mut h = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, &HSTRING::from(GPU_PREFS), Some(0), KEY_READ, &mut h).0 != 0 {
            return out;
        }
        let mut name = vec![0u16; 1024];
        let mut data = vec![0u8; 512];
        for i in 0.. {
            let mut nlen = name.len() as u32;
            let mut dlen = data.len() as u32;
            let r = RegEnumValueW(
                h,
                i,
                Some(PWSTR(name.as_mut_ptr())),
                &mut nlen,
                None,
                None,
                Some(data.as_mut_ptr()),
                Some(&mut dlen),
            );
            if r.0 != 0 {
                break;
            }
            let n = String::from_utf16_lossy(&name[..nlen as usize]);
            if n == GLOBAL {
                continue;
            }
            let wide: Vec<u16> = data[..dlen as usize]
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .take_while(|&c| c != 0)
                .collect();
            let v = String::from_utf16_lossy(&wide);
            let pref = v
                .split(';')
                .find_map(|kv| kv.strip_prefix("GpuPreference="))
                .and_then(|p| p.parse::<usize>().ok())
                .unwrap_or(0)
                .min(2);
            out.push((n, pref));
        }
        let _ = RegCloseKey(h);
    }
    out.sort_by_key(|(p, _)| p.to_lowercase());
    out
}

fn set_gpu(exe: &str, choice: Option<usize>) {
    match choice {
        Some(c) => {
            set_reg_text(HKEY_CURRENT_USER, GPU_PREFS, exe, &format!("GpuPreference={c};"));
        }
        None => unsafe {
            use windows::core::HSTRING;
            let _ = windows::Win32::System::Registry::RegDeleteKeyValueW(
                HKEY_CURRENT_USER,
                &HSTRING::from(GPU_PREFS),
                &HSTRING::from(exe),
            );
        },
    }
    config::log(&format!("gaming: GPU for {exe}: {choice:?}"));
}

fn file_name(path: &str) -> String {
    path.rsplit(['\\', '/']).next().unwrap_or(path).to_string()
}

/// Hardware-accelerated GPU scheduling: on, off, or None where the GPU can't.
fn hags() -> Option<bool> {
    reg_dword(HKEY_LOCAL_MACHINE, DRIVERS, "HwSchMode").map(|v| v == 2)
}

pub fn get(key: &str) -> Option<String> {
    let b = |v: bool| v.to_string();
    Some(match key {
        "win_game_mode" => b(reg_dword(HKEY_CURRENT_USER, GAMEBAR, "AutoGameModeEnabled") != Some(0)),
        "win_game_bar" => b(reg_dword(HKEY_CURRENT_USER, GAMEBAR, "UseNexusForGameBarEnabled") != Some(0)),
        "win_game_capture" => b(reg_dword(HKEY_CURRENT_USER, DVR, "AppCaptureEnabled") == Some(1)),
        "win_gpu_hags" => b(hags().unwrap_or(false)),
        "win_gpu_windowed" => b(global("SwapEffectUpgradeEnable").unwrap_or(false)),
        "win_gpu_vrr" => b(global("VRROptimizeEnable").unwrap_or(false)),
        "win_gpu_autohdr" => b(global("AutoHDREnable").unwrap_or(false)),
        _ => return None,
    })
}

pub fn desc(key: &str) -> Option<String> {
    Some(match key {
        "win_gpu_list" => {
            let apps = gpu_apps();
            if apps.is_empty() {
                "No apps have a GPU set: Windows picks for each.".into()
            } else {
                let list: Vec<String> =
                    apps.iter().take(4).map(|(p, c)| format!("{} ({})", file_name(p), GPU_CHOICES[*c])).collect();
                let more = if apps.len() > 4 { format!(" and {} more", apps.len() - 4) } else { String::new() };
                format!("{}{more}. Pick one to change it.", list.join(", "))
            }
        }
        _ => return None,
    })
}

pub fn shown(key: &str) -> Option<bool> {
    Some(match key {
        "win_gpu_hags" => hags().is_some(),
        "win_gpu_list" => !gpu_apps().is_empty(),
        _ => return None,
    })
}

pub fn set(key: &str, v: &str) -> Option<()> {
    let on = v == "true";
    match key {
        "win_game_mode" => {
            set_reg_dword(HKEY_CURRENT_USER, GAMEBAR, "AutoGameModeEnabled", on as u32);
        }
        "win_game_bar" => {
            set_reg_dword(HKEY_CURRENT_USER, GAMEBAR, "UseNexusForGameBarEnabled", on as u32);
        }
        "win_game_capture" => {
            set_reg_dword(HKEY_CURRENT_USER, DVR, "AppCaptureEnabled", on as u32);
            set_reg_dword(HKEY_CURRENT_USER, DVR_STORE, "GameDVR_Enabled", on as u32);
        }
        "win_gpu_windowed" => set_global("SwapEffectUpgradeEnable", on),
        "win_gpu_vrr" => set_global("VRROptimizeEnable", on),
        "win_gpu_autohdr" => set_global("AutoHDREnable", on),
        "win_gpu_hags" => {
            // A machine-wide setting: Windows asks for permission, and it takes effect after a restart.
            let mode = if on { 2 } else { 1 };
            std::thread::spawn(move || {
                let r = run_admin(&format!("reg add \"HKLM\\{DRIVERS}\" /v HwSchMode /t REG_DWORD /d {mode} /f"));
                if r == Some(0) {
                    tell("Hardware-accelerated GPU scheduling changes after a restart.".into());
                }
                changed();
            });
        }
        _ => return None,
    }
    config::log(&format!("gaming: {key} = {v}"));
    Some(())
}

pub fn action(key: &str, owner: HWND) -> Option<()> {
    match key {
        "win_gpu_add" => {
            // The Open box on a thread of its own; the choice comes back to this thread through a menu after.
            crate::shell::off_thread(|| {
                let Some(exe) = crate::shell::choose_file("Choose an app", &[("Programs", "*.exe")]) else { return };
                let exe = exe.to_string_lossy().into_owned();
                *ADDING.lock().unwrap_or_else(|e| e.into_inner()) = Some(exe);
                super::queue("win_gpu_added");
            });
        }
        "win_gpu_added" => {
            let exe = ADDING.lock().unwrap_or_else(|e| e.into_inner()).take()?;
            let names: Vec<String> = GPU_CHOICES.iter().map(|s| s.to_string()).collect();
            if let Some(c) = pick(owner, &names, None) {
                set_gpu(&exe, Some(c));
            }
        }
        "win_gpu_list" => {
            let apps = gpu_apps();
            let names: Vec<String> =
                apps.iter().map(|(p, c)| format!("{}  \u{2014}  {}", file_name(p), GPU_CHOICES[*c])).collect();
            let i = pick(owner, &names, None)?;
            let (exe, cur) = apps[i].clone();
            let mut choices: Vec<String> = GPU_CHOICES.iter().map(|s| s.to_string()).collect();
            choices.push("Remove from the list".into());
            match pick(owner, &choices, Some(cur)) {
                Some(3) => set_gpu(&exe, None),
                Some(c) => set_gpu(&exe, Some(c)),
                None => {}
            }
        }
        _ => return None,
    }
    Some(())
}

/// The app just chosen in the Open box, waiting for its GPU to be picked.
static ADDING: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
