//! Virtual desktops of Cascade's own, for shell mode (see `shellmode`).
//!
//! Windows' virtual desktops are Explorer's, so without it there are none.
//! Cascade keeps its own instead: going to another desktop hides the windows
//! on the one you leave and shows those on the one you go to. The desktop
//! you're on is simply the windows on screen; the others are lists of the
//! windows Cascade hid for them. Windows that belong to one (dialogs, tool
//! palettes) go with it.
//!
//! Hidden windows mustn't get lost, so the lists are kept in a file as well.
//! When Cascade closes they're all shown again (except when it's restarting,
//! and the next copy carries on with them), and if Cascade stops without
//! closing, the guard shows them (`restore_from_file`).

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::Mutex;

use windows::core::BOOL;
use windows::Win32::Foundation::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::desktops::{DeskWindow, Desktops};
use crate::{config, msg};

/// The most desktops there can be.
const MAX: usize = 20;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Hidden {
    hwnd: isize,
    pid: u32,
    /// It was minimised (it comes back minimised).
    min: bool,
    /// It belongs to another window in the list (a dialog, a palette).
    owned: bool,
    /// When it was hidden (Windows' tick count in ms; 0 if read back from the file).
    at: u64,
}

fn now_ms() -> u64 {
    unsafe { windows::Win32::System::SystemInformation::GetTickCount64() }
}

#[derive(Debug, PartialEq)]
struct State {
    current: usize,
    /// The hidden windows of each desktop, front one first (the current
    /// desktop's list is empty: its windows are on screen).
    hidden: Vec<Vec<Hidden>>,
}

impl State {
    fn new() -> Self {
        Self { current: 0, hidden: vec![Vec::new()] }
    }
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    f(guard.get_or_insert_with(State::new))
}

fn path() -> PathBuf {
    config::dir().join("shell-desktops")
}

fn hwnd(h: isize) -> HWND {
    HWND(h as *mut c_void)
}

fn pid_of(h: HWND) -> u32 {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    pid
}

/// Whether `h` is still the window that was hidden (a closed window's handle can come back as another's).
fn same(h: &Hidden) -> bool {
    let alive = unsafe { IsWindow(Some(hwnd(h.hwnd))).as_bool() };
    alive && pid_of(hwnd(h.hwnd)) == h.pid
}

// ---------------------------------------------------------------- file --

/// This sign-in, so a list from another one (whose windows are long gone,
/// and whose handles may now be other windows') is never acted on.
fn session_tag() -> String {
    use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    use windows::Win32::System::Threading::GetCurrentProcessId;
    let mut session = 0u32;
    unsafe {
        let _ = ProcessIdToSessionId(GetCurrentProcessId(), &mut session);
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    // When Windows started, to the minute.
    let boot = now.saturating_sub(crate::shell::uptime_secs()) / 60;
    format!("session {session} boot {boot}")
}

fn tag_matches(line: &str) -> bool {
    let mine = session_tag();
    let parse = |t: &str| -> Option<(u32, u64)> {
        let w: Vec<&str> = t.split_whitespace().collect();
        (w.len() == 4).then(|| Some((w[1].parse().ok()?, w[3].parse().ok()?))).flatten()
    };
    match (parse(line), parse(&mine)) {
        (Some((s1, b1)), Some((s2, b2))) => s1 == s2 && b1.abs_diff(b2) <= 1,
        _ => false,
    }
}

fn write(state: &State) {
    let hidden: usize = state.hidden.iter().map(Vec::len).sum();
    if state.hidden.len() <= 1 && hidden == 0 {
        let _ = std::fs::remove_file(path());
        return;
    }
    let mut text = format!("{}\ndesktops {} current {}\n", session_tag(), state.hidden.len(), state.current);
    for (d, list) in state.hidden.iter().enumerate() {
        for h in list {
            text.push_str(&format!("{d} {} {} {} {}\n", h.hwnd, h.pid, h.min as u8, h.owned as u8));
        }
    }
    let _ = std::fs::create_dir_all(config::dir());
    let _ = std::fs::write(path(), text);
}

fn parse(text: &str) -> Option<State> {
    let mut lines = text.lines();
    if !tag_matches(lines.next()?) {
        return None;
    }
    let head: Vec<&str> = lines.next()?.split_whitespace().collect();
    if head.len() != 4 || head[0] != "desktops" || head[2] != "current" {
        return None;
    }
    let count: usize = head[1].parse().ok().filter(|n| (1..=MAX).contains(n))?;
    let current: usize = head[3].parse().ok().filter(|c| *c < count)?;
    let mut hidden = vec![Vec::new(); count];
    for line in lines {
        let w: Vec<&str> = line.split_whitespace().collect();
        if w.len() != 5 {
            continue;
        }
        let (Ok(d), Ok(h), Ok(pid)) = (w[0].parse::<usize>(), w[1].parse::<isize>(), w[2].parse::<u32>()) else {
            continue;
        };
        if d < count && d != current {
            hidden[d].push(Hidden { hwnd: h, pid, min: w[3] == "1", owned: w[4] == "1", at: 0 });
        }
    }
    Some(State { current, hidden })
}

/// Picks up where the last copy left off (it restarted), or starts with one desktop.
pub fn load() {
    let found = std::fs::read_to_string(path()).ok().and_then(|t| parse(&t));
    let mut state = found.unwrap_or_else(State::new);
    prune(&mut state);
    let count = state.hidden.len();
    with(|s| *s = state);
    if count > 1 {
        config::log(&format!("desktops: carried on with {count} desktops"));
    }
    msg::post(msg::DESKTOPS, 0, 0);
}

/// Shows every window the file lists and removes it. For the guard, after
/// Cascade stopped without closing (so nothing is left hidden).
pub fn restore_from_file() {
    let Some(state) = std::fs::read_to_string(path()).ok().and_then(|t| parse(&t)) else {
        let _ = std::fs::remove_file(path());
        return;
    };
    let n = show_all(&state);
    let _ = std::fs::remove_file(path());
    config::log(&format!("desktops: showed the {n} windows Cascade had hidden on other desktops"));
}

// ------------------------------------------------------------- windows --

unsafe extern "system" fn collect(h: HWND, out: LPARAM) -> BOOL {
    (*(out.0 as *mut Vec<HWND>)).push(h);
    true.into()
}

/// Every top-level window, front to back.
fn all_windows() -> Vec<HWND> {
    let mut all: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(collect), LPARAM(&mut all as *mut _ as isize));
    }
    all
}

/// The window `h` ultimately belongs to (itself, if it belongs to none).
fn root_owner(h: HWND) -> HWND {
    let mut at = h;
    for _ in 0..16 {
        match unsafe { GetWindow(at, GW_OWNER) } {
            Ok(o) if !o.is_invalid() => at = o,
            _ => break,
        }
    }
    at
}

/// The windows on screen that make up the desktop you're on: its apps'
/// windows, and the visible windows that belong to them. Front to back.
fn on_screen() -> Vec<Hidden> {
    let own = unsafe { windows::Win32::System::Threading::GetCurrentProcessId() };
    let all = all_windows();
    let apps: Vec<HWND> = all.iter().copied().filter(|&h| crate::dock::is_app_window(h, own)).collect();
    let mut out = Vec::new();
    for h in all {
        let app = apps.contains(&h);
        let owned = !app
            && unsafe { IsWindowVisible(h).as_bool() }
            && pid_of(h) != own
            && apps.contains(&root_owner(h))
            && root_owner(h) != h;
        if app || owned {
            out.push(Hidden {
                hwnd: h.0 as isize,
                pid: pid_of(h),
                min: unsafe { IsIconic(h).as_bool() },
                owned,
                at: 0,
            });
        }
    }
    out
}

/// Hides the windows in `list`; returns the ones that went. Some stay, on
/// every desktop: an admin app's (Windows won't let Cascade hide them), and
/// a program's that isn't responding (it would only hide once it answers,
/// by when it might be listed nowhere).
fn hide(list: Vec<Hidden>) -> Vec<Hidden> {
    let at = now_ms();
    list.into_iter()
        .filter(|h| {
            let w = hwnd(h.hwnd);
            if crate::helper::window_above_us(w) || unsafe { IsHungAppWindow(w).as_bool() } {
                return false;
            }
            unsafe { ShowWindowAsync(w, SW_HIDE).as_bool() }
        })
        .map(|h| Hidden { at, ..h })
        .collect()
}

/// Shows the windows in `list` again, as they were. Returns the front one
/// that isn't minimised, to bring forward.
fn show(list: &[Hidden]) -> Option<HWND> {
    let mut front = None;
    for h in list.iter().filter(|h| same(h)) {
        let w = hwnd(h.hwnd);
        unsafe {
            let _ = ShowWindowAsync(w, if h.min { SW_SHOWMINNOACTIVE } else { SW_SHOWNA });
        }
        if front.is_none() && !h.min && !h.owned {
            front = Some(w);
        }
    }
    front
}

fn show_all(state: &State) -> usize {
    let mut n = 0;
    for list in &state.hidden {
        n += list.iter().filter(|h| same(h)).count();
        show(list);
    }
    n
}

/// Drops windows that have closed, and ones that came back on screen by
/// themselves (an app showed its window: it's on the desktop you're on now).
/// A window hidden a moment ago may not have gone yet (hiding is asked of
/// the program, which does it when it gets to it): those are left listed.
fn prune(state: &mut State) {
    let now = now_ms();
    for list in &mut state.hidden {
        list.retain(|h| {
            let settled = h.at == 0 || now.saturating_sub(h.at) > 1500;
            same(h) && !(settled && unsafe { IsWindowVisible(hwnd(h.hwnd)).as_bool() })
        });
    }
    if state.current >= state.hidden.len() {
        state.current = 0;
    }
    let current = state.current;
    state.hidden[current].clear();
}

fn changed(state: &State) {
    write(state);
    msg::post(msg::DESKTOPS, 0, 0);
}

// ------------------------------------------------------------ desktops --

/// The desktops as they are now.
pub fn read() -> Desktops {
    with(|s| Desktops { names: (1..=s.hidden.len()).map(|i| format!("Desktop {i}")).collect(), current: s.current })
}

fn go(s: &mut State, to: usize) -> Option<HWND> {
    prune(s);
    if to == s.current || to >= s.hidden.len() {
        return None;
    }
    let leaving = hide(on_screen());
    let from = s.current;
    s.hidden[from] = leaving;
    let coming = std::mem::take(&mut s.hidden[to]);
    s.current = to;
    let front = show(&coming);
    changed(s);
    front
}

/// Goes to desktop number `to` (0 is the first).
pub fn switch(to: usize) {
    if let Some(front) = with(|s| go(s, to)) {
        crate::dock::focus_window(front);
    }
}

/// Goes to the next desktop (`right`) or the previous one, if there is one.
pub fn step(right: bool) {
    let (n, cur) = with(|s| (s.hidden.len(), s.current));
    let to = if right { cur + 1 } else { cur.wrapping_sub(1) };
    if to < n {
        switch(to);
    }
}

/// Adds a desktop at the end; goes there if `go_there`.
pub fn create(go_there: bool) {
    let made = with(|s| {
        if s.hidden.len() >= MAX {
            return None;
        }
        s.hidden.push(Vec::new());
        changed(s);
        Some(s.hidden.len() - 1)
    });
    if let (Some(i), true) = (made, go_there) {
        switch(i);
    }
}

/// Closes desktop number `which`. Its windows go to the desktop next to it
/// (the one before, or after for the first), as Windows does; closing the
/// one you're on takes you there.
pub fn close(which: usize) {
    let front = with(|s| {
        prune(s);
        let n = s.hidden.len();
        if n < 2 || which >= n {
            return None;
        }
        let to = if which > 0 { which - 1 } else { 1 };
        let mut front = None;
        if which == s.current {
            // Its windows stay on screen, and the other desktop's join them.
            let coming = std::mem::take(&mut s.hidden[to]);
            front = show(&coming);
            s.current = to;
        } else {
            let moving = std::mem::take(&mut s.hidden[which]);
            if to == s.current {
                show(&moving);
            } else {
                s.hidden[to].extend(moving);
            }
        }
        s.hidden.remove(which);
        if s.current > which {
            s.current -= 1;
        }
        changed(s);
        front
    });
    if let Some(f) = front {
        crate::dock::focus_window(f);
    }
}

/// Moves `window` (one on screen) to desktop number `to`.
pub fn move_window(window: HWND, to: usize) -> bool {
    with(|s| {
        prune(s);
        if to >= s.hidden.len() {
            return false;
        }
        if to == s.current {
            return true;
        }
        // It and the windows that belong to it.
        let mut list: Vec<Hidden> = on_screen().into_iter().filter(|h| root_owner(hwnd(h.hwnd)) == window).collect();
        if list.is_empty() {
            list.push(Hidden {
                hwnd: window.0 as isize,
                pid: pid_of(window),
                min: unsafe { IsIconic(window).as_bool() },
                owned: false,
                at: 0,
            });
        }
        let gone = hide(list);
        let ok = gone.iter().any(|h| h.hwnd == window.0 as isize);
        s.hidden[to].splice(0..0, gone);
        changed(s);
        ok
    })
}

/// Shows every hidden window and goes back to one desktop (Cascade is
/// closing, and not to restart).
pub fn restore_all() {
    with(|s| {
        let n = show_all(s);
        if n > 0 {
            config::log(&format!("desktops: showed the {n} windows on other desktops (Cascade is closing)"));
        }
        *s = State::new();
    });
    let _ = std::fs::remove_file(path());
}

/// The windows on each desktop, for Task View's pictures of them, placed on `monitor`.
pub fn windows(monitor: RECT) -> Vec<Vec<DeskWindow>> {
    let (current, lists) = with(|s| {
        prune(s);
        (s.current, s.hidden.clone())
    });
    let (mw, mh) = ((monitor.right - monitor.left).max(1) as f32, (monitor.bottom - monitor.top).max(1) as f32);
    let describe = |h: HWND| -> Option<DeskWindow> {
        let mut r = RECT::default();
        unsafe {
            if IsIconic(h).as_bool() {
                let mut wp =
                    WINDOWPLACEMENT { length: std::mem::size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
                if GetWindowPlacement(h, &mut wp).is_ok() {
                    r = wp.rcNormalPosition;
                }
            } else {
                let _ = GetWindowRect(h, &mut r);
            }
        }
        let x0 = ((r.left - monitor.left) as f32 / mw).clamp(0.0, 1.0);
        let y0 = ((r.top - monitor.top) as f32 / mh).clamp(0.0, 1.0);
        let x1 = ((r.right - monitor.left) as f32 / mw).clamp(0.0, 1.0);
        let y1 = ((r.bottom - monitor.top) as f32 / mh).clamp(0.0, 1.0);
        (x1 - x0 >= 0.02 && y1 - y0 >= 0.02).then(|| DeskWindow {
            name: crate::dock::window_app_name(h),
            id: crate::dock::window_app_id(h),
            at: (x0, y0, x1 - x0, y1 - y0),
        })
    };
    lists
        .iter()
        .enumerate()
        .map(|(i, list)| {
            let hwnds: Vec<HWND> = if i == current {
                on_screen().into_iter().filter(|h| !h.owned).map(|h| hwnd(h.hwnd)).collect()
            } else {
                list.iter().filter(|h| !h.owned).map(|h| hwnd(h.hwnd)).collect()
            };
            hwnds.into_iter().filter_map(describe).collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_back_what_it_wrote() {
        let state = State {
            current: 1,
            hidden: vec![
                vec![
                    Hidden { hwnd: 0x1234, pid: 40, min: false, owned: false, at: 0 },
                    Hidden { hwnd: 0x5678, pid: 40, min: true, owned: true, at: 0 },
                ],
                vec![],
                vec![Hidden { hwnd: 99, pid: 7, min: true, owned: false, at: 0 }],
            ],
        };
        let mut text = format!("{}\ndesktops 3 current 1\n", session_tag());
        for (d, list) in state.hidden.iter().enumerate() {
            for h in list {
                text.push_str(&format!("{d} {} {} {} {}\n", h.hwnd, h.pid, h.min as u8, h.owned as u8));
            }
        }
        assert_eq!(parse(&text), Some(state));
    }

    #[test]
    fn ignores_another_sign_ins_list() {
        assert_eq!(parse("session 99999 boot 1\ndesktops 2 current 0\n1 5 5 0 0\n"), None);
        assert_eq!(parse(&format!("{}\ndesktops 0 current 0\n", session_tag())), None);
        assert_eq!(parse(&format!("{}\ndesktops 2 current 5\n", session_tag())), None);
        assert_eq!(parse("rubbish"), None);
    }
}
