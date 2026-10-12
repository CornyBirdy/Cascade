//! One-click updates from the repo's GitHub releases.
//!
//! CI publishes every build on the default branch as release `build-N` with
//! `cascade.exe` attached, and stamps N into the exe (`CASCADE_BUILD`). The
//! updater compares numbers, downloads the newer exe next to the running one,
//! swaps the files (Windows allows renaming a running exe) and restarts.

use std::path::PathBuf;

use crate::http;
use crate::msg;

const REPO: &str = "CornyBirdy/Cascade";

/// This build's number; 0 for local builds.
pub fn current() -> u32 {
    option_env!("CASCADE_BUILD").and_then(|b| b.parse().ok()).unwrap_or(0)
}

// Outcomes posted as `msg::UPDATE` (wparam = outcome, lparam = build).
pub const AVAILABLE: usize = 1;
pub const UP_TO_DATE: usize = 2;
pub const INSTALLING: usize = 3;
pub const INSTALLED: usize = 4;
pub const FAILED: usize = 5;
pub const NO_ACCESS: usize = 6;

struct Release {
    build: u32,
    asset_url: String,
}

fn headers(token: &str, accept: &'static str) -> Vec<(&'static str, String)> {
    let mut h = vec![("Accept", accept.to_string()), ("X-GitHub-Api-Version", "2022-11-28".to_string())];
    if !token.is_empty() {
        h.push(("Authorization", format!("Bearer {token}")));
    }
    h
}

fn get(url: &str, headers: &[(&'static str, String)], follow: bool) -> Option<http::Response> {
    let refs: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (*k, v.as_str())).collect();
    http::get(url, &refs, follow)
}

/// Latest release, or Err(true) when GitHub refused access (private repo).
fn latest(token: &str) -> Result<Release, bool> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let r = get(&url, &headers(token, "application/vnd.github+json"), true).ok_or(false)?;
    if r.status == 404 || r.status == 401 || r.status == 403 {
        return Err(true);
    }
    if r.status != 200 {
        return Err(false);
    }
    let body = String::from_utf8_lossy(&r.body);
    let tag = http::string_after(&body, "\"tag_name\":").ok_or(false)?;
    let build = tag.trim_start_matches("build-").parse().map_err(|_| false)?;
    let assets = &body[body.find("\"assets\":").ok_or(false)?..];
    let asset_url = http::string_after(assets, "\"url\":").ok_or(false)?;
    Ok(Release { build, asset_url })
}

fn download(asset_url: &str, token: &str) -> Option<Vec<u8>> {
    // The asset endpoint redirects to a signed storage URL; follow it
    // ourselves so the token is never sent to the storage host.
    let r = get(asset_url, &headers(token, "application/octet-stream"), false)?;
    let body = match (r.status, r.location) {
        (200, _) => r.body,
        (301 | 302 | 303 | 307 | 308, Some(loc)) => {
            let r = http::get(&loc, &[], true)?;
            (r.status == 200).then_some(r.body)?
        }
        _ => return None,
    };
    // Sanity check: a Windows executable of a plausible size.
    (body.len() > 64 * 1024 && body.starts_with(b"MZ")).then_some(body)
}

fn exe_paths() -> Option<(PathBuf, PathBuf, PathBuf)> {
    let exe = std::env::current_exe().ok()?;
    Some((exe.clone(), exe.with_file_name("cascade.new.exe"), exe.with_file_name("cascade.old.exe")))
}

/// Removes the previous exe left behind by an update.
pub fn cleanup() {
    if let Some((_, new, old)) = exe_paths() {
        let _ = std::fs::remove_file(old);
        let _ = std::fs::remove_file(new);
    }
}

fn swap_and_relaunch(bytes: &[u8]) -> bool {
    let Some((exe, new, old)) = exe_paths() else { return false };
    if std::fs::write(&new, bytes).is_err() {
        return false;
    }
    let _ = std::fs::remove_file(&old);
    if std::fs::rename(&exe, &old).is_err() {
        let _ = std::fs::remove_file(&new);
        return false;
    }
    if std::fs::rename(&new, &exe).is_err() {
        let _ = std::fs::rename(&old, &exe);
        return false;
    }
    // The new copy waits for this one to exit (single-instance lock).
    std::process::Command::new(&exe).arg("--updated").spawn().is_ok()
}

/// Background check: posts AVAILABLE when a newer build exists.
pub fn spawn_checker(token: String) {
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(20));
        loop {
            if let Ok(r) = latest(&token) {
                if r.build > current() {
                    msg::post(msg::UPDATE, AVAILABLE, r.build as isize);
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(6 * 3600));
        }
    });
}

/// The one-click update: check, download, swap, restart.
pub fn spawn_install(token: String) {
    std::thread::spawn(move || {
        let release = match latest(&token) {
            Ok(r) => r,
            Err(true) => return void(msg::post(msg::UPDATE, NO_ACCESS, 0)),
            Err(false) => return void(msg::post(msg::UPDATE, FAILED, 0)),
        };
        if release.build <= current() {
            msg::post(msg::UPDATE, UP_TO_DATE, current() as isize);
            return;
        }
        msg::post(msg::UPDATE, INSTALLING, release.build as isize);
        match download(&release.asset_url, &token) {
            Some(bytes) if swap_and_relaunch(&bytes) => {
                msg::post(msg::UPDATE, INSTALLED, release.build as isize);
            }
            _ => {
                msg::post(msg::UPDATE, FAILED, 0);
            }
        }
    });
}

fn void(_: bool) {}
