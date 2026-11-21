//! One-click updates from the repo's GitHub releases.
//!
//! CI publishes every build on the default branch as release `build-N` with
//! `cascade.exe` attached, and stamps N into the exe (`CASCADE_BUILD`). The
//! updater compares numbers, downloads the newer exe next to the running one,
//! swaps the files (Windows allows renaming a running exe) and restarts.
//!
//! Every other branch gets one pre-release, `branch-<name>`, replaced on each
//! push and named `<branch> (build N)`. Settings > Updates > Update from picks
//! the branch (`update_branch`; empty for the default branch's releases), and
//! picking one only keeps the choice: the next update (Update now) installs that
//! branch's newest build, even when its number is lower.

use std::path::PathBuf;
use std::sync::Mutex;

use windows::Win32::Foundation::HWND;

use crate::http;
use crate::msg;

const REPO: &str = "CornyBirdy/Cascade";

/// This build's number; 0 for local builds.
pub fn current() -> u32 {
    option_env!("CASCADE_BUILD").and_then(|b| b.parse().ok()).unwrap_or(0)
}

/// The channel this exe came from: "" for the default branch (and local
/// builds), else the branch's name.
pub fn running_channel() -> &'static str {
    let branch = option_env!("CASCADE_BRANCH").unwrap_or("");
    if branch.is_empty() || Some(branch) == option_env!("CASCADE_DEFAULT_BRANCH") {
        ""
    } else {
        branch
    }
}

/// The chosen channel and the token, set from the config (`configure`).
static SETUP: Mutex<(String, String)> = Mutex::new((String::new(), String::new()));

/// Called with the config's `update_branch` and `github_token` whenever it loads.
pub fn configure(channel: &str, token: &str) {
    *SETUP.lock().unwrap_or_else(|e| e.into_inner()) = (channel.trim().to_string(), token.to_string());
}

fn setup() -> (String, String) {
    SETUP.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// What Settings shows for a channel.
pub fn channel_label(channel: &str) -> String {
    if channel.is_empty() {
        "Stable".into()
    } else {
        channel.into()
    }
}

/// A branch's release tag: `/` isn't kept in tags the API can look up.
fn tag_for(branch: &str) -> String {
    let slug: String = branch
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_' { c } else { '-' })
        .collect();
    format!("branch-{slug}")
}

/// `<branch> (build N)`, a branch release's name, split.
fn parse_branch_name(name: &str) -> Option<(String, u32)> {
    let at = name.rfind(" (build ")?;
    let build = name[at + 8..].trim_end_matches(')').parse().ok()?;
    Some((name[..at].to_string(), build))
}

// Outcomes posted as `msg::UPDATE` (wparam = outcome, lparam = build).
/// A channel was picked in Settings (lparam 1: install from it now).
pub const PICKED: usize = 7;
pub const AVAILABLE: usize = 1;
pub const UP_TO_DATE: usize = 2;
pub const INSTALLING: usize = 3;
pub const INSTALLED: usize = 4;
pub const FAILED: usize = 5;
pub const NO_ACCESS: usize = 6;
/// A check asked for found nothing newer (lParam: the build running).
pub const CHECKED: usize = 8;

/// The newest build found that's newer than this one (0: none found).
static AVAILABLE_BUILD: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
/// What the last check said, for Settings.
static STATUS: Mutex<String> = Mutex::new(String::new());

/// The newer build found, if one was.
pub fn available() -> u32 {
    AVAILABLE_BUILD.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn set_available(build: u32) {
    AVAILABLE_BUILD.store(build, std::sync::atomic::Ordering::Relaxed);
}

/// What the last check found (empty before any).
pub fn status() -> String {
    STATUS.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

pub fn set_status(s: impl Into<String>) {
    *STATUS.lock().unwrap_or_else(|e| e.into_inner()) = s.into();
}

/// Looks for a newer build now (Settings' Check for updates): posts
/// AVAILABLE, CHECKED, NO_ACCESS or FAILED.
pub fn spawn_check() {
    std::thread::spawn(move || {
        let (channel, token) = setup();
        match latest(&channel, &token) {
            Ok(r) if wanted(&r, &channel) => msg::post(msg::UPDATE, AVAILABLE, r.build as isize),
            Ok(_) => msg::post(msg::UPDATE, CHECKED, current() as isize),
            Err(true) => msg::post(msg::UPDATE, NO_ACCESS, 0),
            Err(false) => msg::post(msg::UPDATE, FAILED, 0),
        };
    });
}
/// With FAILED, in lparam: the step that failed (0: reading the release).
pub const FAIL_DOWNLOAD: isize = 1;
pub const FAIL_REPLACE: isize = 2;

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

/// GET from the API: the body, or Err(true) when GitHub refused access
/// (a private repo without a token, or a missing release), Err(false) otherwise.
fn api(path: &str, token: &str) -> Result<String, bool> {
    let url = format!("https://api.github.com/repos/{REPO}/{path}");
    let r = get(&url, &headers(token, "application/vnd.github+json"), true).ok_or(false)?;
    if r.status == 404 || r.status == 401 || r.status == 403 {
        return Err(true);
    }
    if r.status != 200 {
        return Err(false);
    }
    Ok(String::from_utf8_lossy(&r.body).into_owned())
}

/// One release's JSON: its build number (from the tag, or from the name of
/// a branch release) and its exe.
fn parse_release(body: &str) -> Option<Release> {
    let tag = http::string_after(body, "\"tag_name\":")?;
    let build = match tag.strip_prefix("build-") {
        Some(n) => n.parse().ok()?,
        None => parse_branch_name(&http::string_after(body, "\"name\":")?)?.1,
    };
    let assets = &body[body.find("\"assets\":")?..];
    Some(Release { build, asset_url: exe_asset_url(assets)? })
}

/// The newest build on `channel` ("" for the default branch's releases).
fn latest(channel: &str, token: &str) -> Result<Release, bool> {
    let body = if channel.is_empty() {
        api("releases/latest", token)?
    } else {
        api(&format!("releases/tags/{}", tag_for(channel)), token)?
    };
    parse_release(&body).ok_or(false)
}

/// The branches with a build to update from, newest build first.
fn branches(body: &str) -> Vec<(String, u32)> {
    let mut out: Vec<(String, u32)> = body
        .split("\"tag_name\":")
        .skip(1)
        .filter_map(|r| {
            // Each piece starts at a release's tag, then its name.
            let tag = http::string_after(&format!("\"t\":{r}"), "\"t\":")?;
            tag.starts_with("branch-").then_some(())?;
            parse_branch_name(&http::string_after(r, "\"name\":")?)
        })
        .collect();
    out.sort_by_key(|b| std::cmp::Reverse(b.1));
    let mut seen = std::collections::HashSet::new();
    out.retain(|b| seen.insert(b.0.clone()));
    out
}

/// Whether to install `r`: newer on the same channel, or anything on another one.
fn wanted(r: &Release, channel: &str) -> bool {
    r.build > current() || channel != running_channel()
}

/// Settings > Updates > Update from: lists Stable and every branch with a
/// build, and keeps the choice without installing anything.
pub fn pick_channel(owner: HWND) {
    let (channel, token) = setup();
    // A short wait on the network, on a thread so a slow answer can be left.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(api("releases?per_page=100", &token));
    });
    let body = match rx.recv_timeout(std::time::Duration::from_secs(10)) {
        Ok(Ok(b)) => b,
        Ok(Err(true)) => {
            crate::winset::tell("Can't reach the GitHub repo. If it is private, add a GitHub token below.".into());
            return;
        }
        _ => {
            crate::winset::tell("Couldn't reach GitHub. Try again in a moment.".into());
            return;
        }
    };
    let mut keys = vec![String::new()];
    let mut options = vec!["Stable (the default branch's releases)".to_string()];
    for (branch, build) in branches(&body) {
        options.push(format!("{branch}  \u{b7}  build {build}"));
        keys.push(branch);
    }
    let current = keys.iter().position(|k| *k == channel);
    let Some(i) = crate::winset::pick(owner, &options, current) else { return };
    let chosen = keys.swap_remove(i);
    // Only the choice is kept: nothing is installed until Update now (or the
    // automatic check's notice) is clicked.
    crate::config::Config::set("update_branch", &chosen);
    msg::post(msg::UPDATE, PICKED, 0);
}

/// The API url of the asset named `cascade.exe` (a release may carry other
/// files too, like the installer), or the first asset's when none has that name.
fn exe_asset_url(assets: &str) -> Option<String> {
    let first = || http::string_after(assets, "\"url\":");
    let Some(name_at) = assets.find("\"name\":\"cascade.exe\"") else { return first() };
    // An asset lists its own url before its name.
    match assets[..name_at].rfind("\"url\":") {
        Some(url_at) => http::string_after(&assets[url_at..], "\"url\":"),
        None => first(),
    }
}

fn download_once(asset_url: &str, token: &str) -> Result<Vec<u8>, String> {
    // The asset endpoint redirects to a signed storage URL; follow it
    // ourselves so the token is never sent to the storage host.
    let r = get(asset_url, &headers(token, "application/octet-stream"), false).ok_or("no answer from GitHub")?;
    let body = match (r.status, r.location) {
        (200, _) => r.body,
        (301 | 302 | 303 | 307 | 308, Some(loc)) => {
            let r = http::get(&loc, &[], true).ok_or("no answer from GitHub's file storage")?;
            if r.status != 200 {
                return Err(format!("file storage answered {}", r.status));
            }
            r.body
        }
        (s, _) => return Err(format!("GitHub answered {s}")),
    };
    // Sanity check: a Windows executable of a plausible size.
    if body.len() > 64 * 1024 && body.starts_with(b"MZ") {
        Ok(body)
    } else {
        Err(format!("got {} bytes that aren't a program", body.len()))
    }
}

/// The new exe, with a couple of tries (a dropped connection is common on a 4 MB file).
fn download(asset_url: &str, token: &str) -> Result<Vec<u8>, String> {
    let mut last = String::new();
    for i in 0..3 {
        match download_once(asset_url, token) {
            Ok(b) => return Ok(b),
            Err(e) => {
                crate::config::log(&format!("update: download try {}: {e}", i + 1));
                last = e;
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    Err(last)
}

fn exe_paths() -> Option<(PathBuf, PathBuf, PathBuf)> {
    let exe = std::env::current_exe().ok()?;
    Some((exe.clone(), exe.with_file_name("cascade.new.exe"), exe.with_file_name("cascade.old.exe")))
}

/// Removes the previous exes left behind by updates (those still running,
/// such as a helper started by an older copy, stay until it ends).
pub fn cleanup() {
    let Some((exe, new, _)) = exe_paths() else { return };
    let _ = std::fs::remove_file(new);
    let Some(dir) = exe.parent() else { return };
    let Ok(list) = std::fs::read_dir(dir) else { return };
    for e in list.flatten() {
        let name = e.file_name().to_string_lossy().to_ascii_lowercase();
        if name.starts_with("cascade.old") && name.ends_with(".exe") {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Where the running exe goes: `cascade.old.exe`, or a fresh name when that
/// one is still in use (a process of an older copy keeps its file locked).
fn old_slot(old: &std::path::Path) -> PathBuf {
    if std::fs::remove_file(old).is_ok() || !old.exists() {
        return old.to_path_buf();
    }
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    old.with_file_name(format!("cascade.old-{stamp}.exe"))
}

fn swap_and_relaunch(bytes: &[u8]) -> Result<(), String> {
    let Some((exe, new, old)) = exe_paths() else { return Err("can't tell where cascade.exe is".into()) };
    std::fs::write(&new, bytes).map_err(|e| format!("writing {}: {e}", new.display()))?;
    let old = old_slot(&old);
    if let Err(e) = std::fs::rename(&exe, &old) {
        let _ = std::fs::remove_file(&new);
        return Err(format!("moving {} aside: {e}", exe.display()));
    }
    if let Err(e) = std::fs::rename(&new, &exe) {
        let _ = std::fs::rename(&old, &exe);
        return Err(format!("putting the new exe in place: {e}"));
    }
    // The new copy waits for this one to exit (single-instance lock).
    crate::shellmode::set_intent(crate::shellmode::Intent::Restart);
    std::process::Command::new(&exe)
        .arg("--updated")
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("starting the new copy: {e}"))
}

/// Background check: posts AVAILABLE when a newer build exists.
pub fn spawn_checker() {
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(20));
        loop {
            let (channel, token) = setup();
            if let Ok(r) = latest(&channel, &token) {
                if wanted(&r, &channel) {
                    msg::post(msg::UPDATE, AVAILABLE, r.build as isize);
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(6 * 3600));
        }
    });
}

/// The one-click update: check, download, swap, restart.
pub fn spawn_install() {
    std::thread::spawn(move || {
        let (channel, token) = setup();
        let release = match latest(&channel, &token) {
            Ok(r) => r,
            Err(true) => return void(msg::post(msg::UPDATE, NO_ACCESS, 0)),
            Err(false) => {
                crate::config::log(&format!(
                    "update: couldn't read the newest release on \"{}\"",
                    channel_label(&channel)
                ));
                return void(msg::post(msg::UPDATE, FAILED, 0));
            }
        };
        if !wanted(&release, &channel) {
            msg::post(msg::UPDATE, UP_TO_DATE, current() as isize);
            return;
        }
        msg::post(msg::UPDATE, INSTALLING, release.build as isize);
        let result = download(&release.asset_url, &token)
            .map_err(|e| (FAIL_DOWNLOAD, e))
            .and_then(|bytes| swap_and_relaunch(&bytes).map_err(|e| (FAIL_REPLACE, e)));
        match result {
            Ok(()) => {
                crate::config::log(&format!("update: installed build {}", release.build));
                msg::post(msg::UPDATE, INSTALLED, release.build as isize);
            }
            Err((why, e)) => {
                crate::config::log(&format!("update: build {} failed: {e}", release.build));
                msg::post(msg::UPDATE, FAILED, why);
            }
        }
    });
}

fn void(_: bool) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_exe_among_assets() {
        let assets = r#""assets":[{"url":"https://api/assets/1","id":1,"node_id":"a","name":"CascadeSetup.exe","uploader":{"url":"https://api/users/bot"}},{"url":"https://api/assets/2","id":2,"node_id":"b","name":"cascade.exe","uploader":{"url":"https://api/users/bot"}}]"#;
        assert_eq!(exe_asset_url(assets), Some("https://api/assets/2".into()));
        let only = r#""assets":[{"url":"https://api/assets/9","id":9,"name":"cascade.exe"}]"#;
        assert_eq!(exe_asset_url(only), Some("https://api/assets/9".into()));
        let other = r#""assets":[{"url":"https://api/assets/5","id":5,"name":"other.exe"}]"#;
        assert_eq!(exe_asset_url(other), Some("https://api/assets/5".into()));
    }

    #[test]
    fn reads_branch_releases() {
        assert_eq!(parse_branch_name("claude/x-y (build 240)"), Some(("claude/x-y".into(), 240)));
        assert_eq!(parse_branch_name("Cascade build 12"), None);
        assert_eq!(tag_for("claude/x-y"), "branch-claude-x-y");
        let list = r#"[{"url":"u","tag_name":"branch-a-b","target_commitish":"s","name":"a/b (build 9)","assets":[{"url":"https://api/assets/1","name":"cascade.exe"}],"body":"x"},{"tag_name":"build-7","name":"Cascade build 7","assets":[]},{"tag_name":"branch-c","name":"c (build 12)","assets":[{"url":"https://api/assets/3","name":"cascade.exe"}]}]"#;
        assert_eq!(branches(list), vec![("c".to_string(), 12), ("a/b".to_string(), 9)]);
        let one = r#"{"tag_name":"branch-a-b","name":"a/b (build 9)","assets":[{"url":"https://api/assets/1","id":1,"name":"cascade.exe"}]}"#;
        let r = parse_release(one).unwrap();
        assert_eq!((r.build, r.asset_url.as_str()), (9, "https://api/assets/1"));
    }
}
