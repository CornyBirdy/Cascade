//! Debloat: switches for what Windows does that you didn't ask for:
//! telemetry, Copilot, Recall, ads and suggestions in Start and on the lock
//! screen, Bing in Windows' search, Edge running in the background,
//! feedback prompts, typing data, activity history and sharing updates
//! with strangers' PCs.
//!
//! Every switch can be turned back off: it only writes a policy or setting,
//! and turning it off deletes that value again, so Windows goes back to its
//! own default. Nothing is uninstalled. (Windows Home and Pro keep sending
//! the "required" level of diagnostic data whatever any setting says; only
//! the optional data and the telemetry service itself can be turned off.)

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Registry::{HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

use super::{changed, del_reg_value, reg_dword, run_admin, set_reg_dword};
use crate::config;

const CDM: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\ContentDeliveryManager";

/// A user setting: values written (key, value, data when on). Off deletes them.
fn user_values(key: &str) -> Option<&'static [(&'static str, &'static str, u32)]> {
    Some(match key {
        "win_db_copilot" => &[("Software\\Policies\\Microsoft\\Windows\\WindowsCopilot", "TurnOffWindowsCopilot", 1)],
        "win_db_recall" => &[
            ("Software\\Policies\\Microsoft\\Windows\\WindowsAI", "DisableAIDataAnalysis", 1),
            ("Software\\Policies\\Microsoft\\Windows\\WindowsAI", "DisableClickToDo", 1),
        ],
        "win_db_suggestions" => &[
            (CDM, "SubscribedContent-338388Enabled", 0),
            (CDM, "SubscribedContent-338389Enabled", 0),
            (CDM, "SubscribedContent-353694Enabled", 0),
            (CDM, "SubscribedContent-353696Enabled", 0),
            (CDM, "SubscribedContent-310093Enabled", 0),
            (CDM, "SystemPaneSuggestionsEnabled", 0),
            (CDM, "SilentInstalledAppsEnabled", 0),
            (CDM, "SoftLandingEnabled", 0),
            (CDM, "OemPreInstalledAppsEnabled", 0),
            (CDM, "PreInstalledAppsEnabled", 0),
        ],
        "win_db_lockscreen" => {
            &[(CDM, "RotatingLockScreenOverlayEnabled", 0), (CDM, "SubscribedContent-338387Enabled", 0)]
        }
        "win_db_web_search" => &[
            ("Software\\Policies\\Microsoft\\Windows\\Explorer", "DisableSearchBoxSuggestions", 1),
            ("Software\\Microsoft\\Windows\\CurrentVersion\\Search", "BingSearchEnabled", 0),
        ],
        "win_db_edge" => &[
            ("Software\\Policies\\Microsoft\\Edge", "StartupBoostEnabled", 0),
            ("Software\\Policies\\Microsoft\\Edge", "BackgroundModeEnabled", 0),
        ],
        "win_db_feedback" => &[
            ("Software\\Microsoft\\Siuf\\Rules", "NumberOfSIUFInPeriod", 0),
            ("Software\\Microsoft\\Siuf\\Rules", "PeriodInNanoSeconds", 0),
        ],
        "win_db_typing" => &[
            ("Software\\Microsoft\\InputPersonalization", "RestrictImplicitTextCollection", 1),
            ("Software\\Microsoft\\InputPersonalization", "RestrictImplicitInkCollection", 1),
            ("Software\\Microsoft\\InputPersonalization\\TrainedDataStore", "HarvestContacts", 0),
            ("Software\\Microsoft\\Personalization\\Settings", "AcceptedPrivacyPolicy", 0),
        ],
        _ => return None,
    })
}

/// A machine-wide setting: values in HKLM (key, value, data when on), and
/// services to turn off (name, start type to go back to).
type Values = &'static [(&'static str, &'static str, u32)];
type Services = &'static [(&'static str, &'static str)];

fn machine(key: &str) -> Option<(Values, Services)> {
    Some(match key {
        "win_db_telemetry" => (
            &[("SOFTWARE\\Policies\\Microsoft\\Windows\\DataCollection", "AllowTelemetry", 1)],
            &[("DiagTrack", "auto"), ("dmwappushservice", "demand")],
        ),
        "win_db_activity" => (
            &[
                ("SOFTWARE\\Policies\\Microsoft\\Windows\\System", "EnableActivityFeed", 0),
                ("SOFTWARE\\Policies\\Microsoft\\Windows\\System", "PublishUserActivities", 0),
                ("SOFTWARE\\Policies\\Microsoft\\Windows\\System", "UploadUserActivities", 0),
            ],
            &[],
        ),
        "win_db_delivery" => {
            (&[("SOFTWARE\\Policies\\Microsoft\\Windows\\DeliveryOptimization", "DODownloadMode", 0)], &[])
        }
        _ => return None,
    })
}

fn all_set(root: HKEY, values: &[(&str, &str, u32)]) -> bool {
    values.iter().all(|(k, v, data)| reg_dword(root, k, v) == Some(*data))
}

fn service_off(name: &str) -> bool {
    reg_dword(HKEY_LOCAL_MACHINE, &format!("SYSTEM\\CurrentControlSet\\Services\\{name}"), "Start") == Some(4)
}

fn is_on(key: &str) -> Option<bool> {
    if let Some(values) = user_values(key) {
        return Some(all_set(HKEY_CURRENT_USER, values));
    }
    let (values, services) = machine(key)?;
    Some(all_set(HKEY_LOCAL_MACHINE, values) && services.iter().all(|(s, _)| service_off(s)))
}

const KEYS: [&str; 11] = [
    "win_db_telemetry",
    "win_db_copilot",
    "win_db_recall",
    "win_db_suggestions",
    "win_db_lockscreen",
    "win_db_web_search",
    "win_db_edge",
    "win_db_feedback",
    "win_db_typing",
    "win_db_activity",
    "win_db_delivery",
];

fn apply(key: &str, on: bool) {
    if let Some(values) = user_values(key) {
        for (k, v, data) in values {
            if on {
                set_reg_dword(HKEY_CURRENT_USER, k, v, *data);
            } else {
                del_reg_value(HKEY_CURRENT_USER, k, v);
            }
        }
        config::log(&format!("debloat: {key} {}", if on { "on" } else { "off" }));
        return;
    }
    let Some((values, services)) = machine(key) else { return };
    let mut cmds: Vec<String> = Vec::new();
    for (k, v, data) in values {
        cmds.push(if on {
            format!("reg add \"HKLM\\{k}\" /v {v} /t REG_DWORD /d {data} /f")
        } else {
            format!("reg delete \"HKLM\\{k}\" /v {v} /f")
        });
    }
    for (s, back) in services {
        if on {
            cmds.push(format!("sc config {s} start= disabled"));
            cmds.push(format!("sc stop {s}"));
        } else {
            cmds.push(format!("sc config {s} start= {back}"));
            if *back == "auto" {
                cmds.push(format!("sc start {s}"));
            }
        }
    }
    let line = cmds.join(" & ");
    let key = key.to_string();
    std::thread::spawn(move || {
        let r = run_admin(&line);
        config::log(&format!("debloat: {key} {} ({r:?})", if on { "on" } else { "off" }));
        changed();
    });
}

pub fn get(key: &str) -> Option<String> {
    is_on(key).map(|b| b.to_string())
}

pub fn desc(key: &str) -> Option<String> {
    Some(match key {
        "win_db_status" => {
            let n = KEYS.iter().filter(|k| is_on(k) == Some(true)).count();
            format!(
                "{n} of {} on. Each only writes a setting: turning it off puts Windows' own default back. Nothing is uninstalled.",
                KEYS.len()
            )
        }
        _ => return None,
    })
}

pub fn shown(_key: &str) -> Option<bool> {
    None
}

pub fn set(key: &str, v: &str) -> Option<()> {
    is_on(key)?;
    apply(key, v == "true");
    Some(())
}

pub fn action(key: &str, _owner: HWND) -> Option<()> {
    match key {
        // Everything that needs no permission at once; the machine-wide ones ask once together.
        "win_db_all" | "win_db_none" => {
            let on = key == "win_db_all";
            for k in KEYS.iter().filter(|k| user_values(k).is_some()) {
                apply(k, on);
            }
            let mut cmds = Vec::new();
            for k in KEYS.iter().filter(|k| machine(k).is_some()) {
                let (values, services) = machine(k)?;
                for (rk, v, data) in values {
                    cmds.push(if on {
                        format!("reg add \"HKLM\\{rk}\" /v {v} /t REG_DWORD /d {data} /f")
                    } else {
                        format!("reg delete \"HKLM\\{rk}\" /v {v} /f")
                    });
                }
                for (s, back) in services {
                    cmds.push(if on {
                        format!("sc config {s} start= disabled & sc stop {s}")
                    } else {
                        format!("sc config {s} start= {back}")
                    });
                }
            }
            let line = cmds.join(" & ");
            std::thread::spawn(move || {
                let r = run_admin(&line);
                config::log(&format!("debloat: everything {} ({r:?})", if on { "on" } else { "off" }));
                changed();
            });
        }
        _ => return None,
    }
    Some(())
}
