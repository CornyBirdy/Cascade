//! Global keyboard hook: opens the Cascade launcher on a lone Win key press,
//! drives the Alt+Tab switcher, and takes the Win+key shortcuts Cascade has
//! its own menus for (Win+X, Win+N, Win+A, Win+V, Win+Tab, each only when
//! turned on). Every
//! other Win+key (Win+E, Win+R, Win+Shift+S...) is left completely alone.
//!
//! A shortcut's key press and release are kept from Windows, so it never
//! sees the combination; a mask key tap (see below) stops the Win release
//! that follows from opening its Start menu.
//!
//! The Windows Start menu only opens when Win is released with no other key
//! pressed since it went down. So just after Win goes down (from a helper
//! thread, once Windows has taken the press in), one unassigned "mask" key tap
//! is injected: to Windows that looks like a key combination and its own
//! Start menu stays shut. The real Win press and release are passed on
//! untouched, so other programs that watch the keyboard (window-drag tools,
//! games) always see the true state of the key and it can't get stuck.
//!
//! Only when the key is released before that tap has landed (a very quick
//! tap) is the release held back and replaced by mask + release as one batch.
//!
//! Alt+Tab works the same way: Tab is swallowed while Alt is held, and when
//! Alt is released it is replaced by mask + release, so the app in front
//! doesn't see a lone Alt and light up its menu bar.
//!
//! Alt+Tab swallows a release, which risks leaving Windows thinking Alt is
//! still held, so: nothing is swallowed unless the replacement was accepted,
//! and shortly afterwards the key state is checked and a release sent if it's
//! still down (and checked again until it's up). Nothing is swallowed either
//! while a program with admin rights is in front and Cascade has none:
//! Windows quietly drops the keys Cascade sends it then, replacement and all.
//!
//! The hook runs on a thread of its own that does nothing else. Windows
//! waits for a keyboard hook before every key reaches any app, and drops a
//! hook that keeps it waiting too long; on the UI thread a busy moment
//! (drawing, loading icons, a slow system call) would hold up typing, let a
//! key slip past half-handled, or lose the hook altogether.

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicIsize, AtomicU32, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::Duration;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::System::Threading::{
    GetCurrentThread, GetCurrentThreadId, SetThreadPriority, THREAD_PRIORITY_TIME_CRITICAL,
};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::msg;

/// Set in the admin helper, which tells the main Cascade window instead.
static IN_HELPER: AtomicBool = AtomicBool::new(false);
static MAIN_SEEN: AtomicBool = AtomicBool::new(false);
/// Tick count of the latest Win key-down (repeats included).
static LAST_WIN: AtomicU32 = AtomicU32::new(0);
static HELPER_WINDOW: AtomicIsize = AtomicIsize::new(0);
/// Checks left before `fix_stuck` stops trying.
static CHECKS: AtomicU32 = AtomicU32::new(0);
/// How many times a stuck key is checked (and released) after a swallowed release.
const MAX_CHECKS: u32 = 4;

/// Sent to the admin helper's window to check the keys afterwards.
pub const KEY_CHECK_MSG: u32 = WM_APP + 41;

/// Tells the main Cascade window something happened.
fn notify(m: u32, wparam: usize) {
    if IN_HELPER.load(Ordering::Relaxed) {
        crate::helper::post_main(m, wparam);
    } else {
        msg::post(m, wparam, 0);
    }
}

/// How the latest lone Win press went, for `cascade.log` (see `last_lone`):
/// 1 the mask tap landed before the release, 2 it didn't and the release was
/// replaced by mask + release, 3 the same but Windows refused that (the real
/// release went through), 4 and 5 the same as 2 and 3 for a long hold.
static LONE: AtomicU32 = AtomicU32::new(0);
/// How long (ms) that press was held.
static LONE_HELD: AtomicU32 = AtomicU32::new(0);
/// A Win press held longer than this (ms) gets its release replaced by mask +
/// release even if the early mask went through.
const LONG_HOLD: u32 = 350;

/// How the latest lone Win press went (see `LONE`).
pub fn last_lone() -> String {
    let how = match LONE.load(Ordering::Relaxed) {
        1 => "mask landed in time",
        2 => "released early; release replaced with mask + release",
        3 => "released early; Windows refused the replacement",
        4 => "long hold; release replaced with mask + release",
        5 => "long hold; Windows refused the replacement",
        _ => "unknown",
    };
    format!("{how}, held {} ms", LONE_HELD.load(Ordering::Relaxed))
}

fn on_lone_win() {
    notify(msg::OPEN_LAUNCHER, 0);
}

/// Asks the owner of this hook to call `fix_stuck` in a moment.
fn request_check() {
    CHECKS.store(MAX_CHECKS, Ordering::Relaxed);
    schedule_check();
}

fn schedule_check() {
    if IN_HELPER.load(Ordering::Relaxed) {
        let h = HWND(HELPER_WINDOW.load(Ordering::Relaxed) as *mut _);
        unsafe {
            let _ = PostMessageW(Some(h), KEY_CHECK_MSG, WPARAM(0), LPARAM(0));
        }
    } else {
        msg::post(msg::KEY_CHECK, 0, 0);
    }
}

/// Whether this copy should act on keys: the admin helper takes over from
/// the main app while it runs, and quits once Cascade has gone.
fn handled_here() -> bool {
    if IN_HELPER.load(Ordering::Relaxed) {
        // The helper starts at sign-in, possibly before Cascade does: only
        // quit once Cascade has been seen and has gone again.
        let up = crate::helper::main_window().is_some();
        if up {
            MAIN_SEEN.store(true, Ordering::Relaxed);
        } else if MAIN_SEEN.load(Ordering::Relaxed) {
            // (This is the hook's thread: the helper's own loop is told.)
            crate::helper::stop();
        }
        up
    } else {
        !crate::helper::running()
    }
}

pub fn set_helper_mode(hwnd: HWND) {
    IN_HELPER.store(true, Ordering::Relaxed);
    HELPER_WINDOW.store(hwnd.0 as isize, Ordering::Relaxed);
}

/// The hook's thread (its id, to tell it to stop) while it's installed.
static THREAD: Mutex<Option<(u32, JoinHandle<()>)>> = Mutex::new(None);
static WIN_ON: AtomicBool = AtomicBool::new(false);
/// The Win+letter shortcuts Cascade takes: bit n is the letter 'A' + n (and
/// `TAB_BIT` is Win+Tab).
static SHORTCUTS: AtomicU32 = AtomicU32::new(0);
/// Shortcut keys whose press was kept from Windows, so their release is too.
static HELD_SHORTCUTS: AtomicU32 = AtomicU32::new(0);
static TAB_ON: AtomicBool = AtomicBool::new(false);
static WIN_DOWN: AtomicBool = AtomicBool::new(false);
/// The last Win key event to come by (anyone's, real or sent) was a press:
/// a Win key Windows thinks is held after that is stuck (see `fix_stuck`).
static WIN_LAST_DOWN: AtomicBool = AtomicBool::new(false);
/// Tick count of that last Win key event.
static WIN_EVENT_AT: AtomicU32 = AtomicU32::new(0);
/// How long after a Win release Windows may still be catching up, so a key
/// that comes sooner doesn't count as proof the key is stuck.
const SETTLE: u32 = 60;
static COMBO: AtomicBool = AtomicBool::new(false);
/// The mask key tap for the current Win press has gone by.
static MASK_SEEN: AtomicBool = AtomicBool::new(false);
/// Tick count of the latest swallowed Win+key shortcut press (repeats included).
static SHORTCUT_AT: AtomicU32 = AtomicU32::new(0);
/// Asks the mask worker for a tap while it's running.
static MASK_TX: Mutex<Option<Sender<()>>> = Mutex::new(None);
/// How long the worker waits after Win goes down before tapping the mask key,
/// so Windows has taken the Win press in first.
const MASK_DELAY: Duration = Duration::from_millis(8);
/// A swallowed shortcut key held longer than this without repeating was
/// released unseen: the next press is a new one.
const REPEAT_GAP: u32 = 1500;
static ALT_DOWN: AtomicBool = AtomicBool::new(false);
static CTRL_DOWN: AtomicBool = AtomicBool::new(false);
static SHIFT_DOWN: AtomicBool = AtomicBool::new(false);
/// A Tab was swallowed while Alt was held: the switcher is in use.
static TAB_ACTIVE: AtomicBool = AtomicBool::new(false);
static TAB_CANCEL: AtomicBool = AtomicBool::new(false);
/// Where the pointer was, and when, as Win went down: a Win key held while
/// the mouse drags a window (window-drag tools) isn't a lone press.
static WIN_AT: [AtomicI32; 2] = [AtomicI32::new(0), AtomicI32::new(0)];
static WIN_SINCE: AtomicU32 = AtomicU32::new(0);

/// An unassigned virtual key, used only to "break" a lone Win or Alt press.
const MASK_KEY: u16 = 0xE8;

fn key(vk: u16, up: bool) -> INPUT {
    // The Win keys and right Alt are extended keys: sent with their scan
    // code and that flag, their release looks just like the real one to
    // everything that reads the keyboard.
    let extended = vk == VK_LWIN.0 || vk == VK_RWIN.0 || vk == VK_RMENU.0;
    let scan = if vk == MASK_KEY { 0 } else { (unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) }) as u16 };
    let mut flags = if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) };
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: VIRTUAL_KEY(vk), wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 },
        },
    }
}

/// Releases a Win or Alt key that Windows still thinks is held, if there is one.
pub fn fix_stuck() {
    let mut inputs = Vec::new();
    for (vk, tracked) in
        [(VK_LWIN.0, &WIN_LAST_DOWN), (VK_RWIN.0, &WIN_LAST_DOWN), (VK_LMENU.0, &ALT_DOWN), (VK_RMENU.0, &ALT_DOWN)]
    {
        // Really held right now? Then it isn't stuck.
        if !tracked.load(Ordering::Relaxed) && unsafe { GetAsyncKeyState(vk as i32) } as u16 & 0x8000 != 0 {
            inputs.push(key(vk, true));
        }
    }
    if !inputs.is_empty() {
        unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
        // Check again that it took.
        let left = CHECKS.load(Ordering::Relaxed);
        CHECKS.store(left.saturating_sub(1), Ordering::Relaxed);
        if left > 1 {
            schedule_check();
        }
    }
}

/// Mask tap and the key's release as one batch, in that order. True if
/// Windows accepted all of it (so the real release can be swallowed).
unsafe fn masked_release(vk: u16) -> bool {
    if crate::helper::foreground_above_us() {
        // Windows would drop it: let the real release through instead.
        return false;
    }
    let inputs = [key(MASK_KEY, false), key(MASK_KEY, true), key(vk, true)];
    SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) as usize == inputs.len()
}

/// Taps the mask key (down and up as one batch).
unsafe fn mask_tap() {
    let inputs = [key(MASK_KEY, false), key(MASK_KEY, true)];
    SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
}

fn track(flag: &AtomicBool, down: bool, up: bool) {
    if down {
        flag.store(true, Ordering::Relaxed);
    } else if up {
        flag.store(false, Ordering::Relaxed);
    }
}

/// Whether the key is physically down right now.
fn down_now(vk: VIRTUAL_KEY) -> bool {
    let state = unsafe { GetAsyncKeyState(vk.0 as i32) };
    state as u16 & 0x8000 != 0
}

/// Asks the mask worker to tap the mask key a moment from now.
fn request_mask() {
    if let Ok(tx) = MASK_TX.try_lock() {
        if let Some(tx) = tx.as_ref() {
            let _ = tx.send(());
        }
    }
}

fn cursor() -> POINT {
    let mut p = POINT::default();
    let _ = unsafe { GetCursorPos(&mut p) };
    p
}

/// Whether the mouse was used while Win was held (a window-drag tool's
/// Win+drag): a button is still down, or the pointer travelled while the key
/// was held for longer than a tap.
fn mouse_used(now: u32) -> bool {
    let button = [VK_LBUTTON, VK_RBUTTON, VK_MBUTTON]
        .iter()
        .any(|k| unsafe { GetAsyncKeyState(k.0 as i32) } as u16 & 0x8000 != 0);
    let p = cursor();
    let (dx, dy) = (p.x - WIN_AT[0].load(Ordering::Relaxed), p.y - WIN_AT[1].load(Ordering::Relaxed));
    let held = now.wrapping_sub(WIN_SINCE.load(Ordering::Relaxed));
    button || (held > 200 && dx * dx + dy * dy > 12 * 12)
}

unsafe extern "system" fn proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let k = &*(lp.0 as *const KBDLLHOOKSTRUCT);
        let injected = k.flags.0 & LLKHF_INJECTED.0 != 0;
        let msg_id = wp.0 as u32;
        let down = msg_id == WM_KEYDOWN || msg_id == WM_SYSKEYDOWN;
        let up = msg_id == WM_KEYUP || msg_id == WM_SYSKEYUP;
        let vk = k.vkCode as u16;
        let is_win = vk == VK_LWIN.0 || vk == VK_RWIN.0;
        let is_alt = vk == VK_LMENU.0 || vk == VK_RMENU.0;
        if is_win {
            track(&WIN_LAST_DOWN, down, up);
            WIN_EVENT_AT.store(k.time, Ordering::Relaxed);
            // Whatever happened while it was held (a mask tap, a swallowed
            // shortcut, a window taking the keyboard), look again shortly
            // after it's let go that Windows let go too.
            if up && !injected {
                request_check();
            }
        } else if down && !injected && !WIN_LAST_DOWN.load(Ordering::Relaxed) {
            // A key arrives though the Win key was let go a while ago: if
            // Windows still holds it, every key is a shortcut. Let it go.
            let settled = k.time.wrapping_sub(WIN_EVENT_AT.load(Ordering::Relaxed)) > SETTLE;
            if settled && (down_now(VK_LWIN) || down_now(VK_RWIN)) {
                request_check();
            }
        }
        if injected {
            // Window-drag tools press a key of their own while Win is held,
            // to keep Windows' Start menu shut: that makes it a combination.
            if down && !is_win && WIN_DOWN.load(Ordering::Relaxed) {
                if vk == MASK_KEY {
                    MASK_SEEN.store(true, Ordering::Relaxed);
                } else {
                    COMBO.store(true, Ordering::Relaxed);
                }
            }
        } else {
            if is_alt {
                track(&ALT_DOWN, down, up);
            } else if vk == VK_LCONTROL.0 || vk == VK_RCONTROL.0 {
                track(&CTRL_DOWN, down, up);
            } else if vk == VK_LSHIFT.0 || vk == VK_RSHIFT.0 {
                track(&SHIFT_DOWN, down, up);
            }

            // Alt+Tab.
            if TAB_ON.load(Ordering::Relaxed) {
                if vk == VK_TAB.0 && ALT_DOWN.load(Ordering::Relaxed) && !CTRL_DOWN.load(Ordering::Relaxed) {
                    // Games and fullscreen video keep Windows' own Alt+Tab.
                    if down && handled_here() && !crate::shell::fullscreen_now() {
                        TAB_ACTIVE.store(true, Ordering::Relaxed);
                        TAB_CANCEL.store(false, Ordering::Relaxed);
                        notify(msg::SWITCH, SHIFT_DOWN.load(Ordering::Relaxed) as usize);
                        return LRESULT(1);
                    }
                    if up && TAB_ACTIVE.load(Ordering::Relaxed) {
                        return LRESULT(1);
                    }
                }
                if TAB_ACTIVE.load(Ordering::Relaxed) {
                    if down && vk == VK_ESCAPE.0 {
                        TAB_CANCEL.store(true, Ordering::Relaxed);
                        notify(msg::SWITCH, 3);
                        return LRESULT(1);
                    }
                    if down && (vk == VK_LEFT.0 || vk == VK_RIGHT.0) {
                        notify(msg::SWITCH, (vk == VK_LEFT.0) as usize);
                        return LRESULT(1);
                    }
                    // Up and down a row (4 and 5).
                    if down && (vk == VK_UP.0 || vk == VK_DOWN.0) {
                        notify(msg::SWITCH, if vk == VK_UP.0 { 4 } else { 5 });
                        return LRESULT(1);
                    }
                    if up && matches!(vk, v if v == VK_LEFT.0 || v == VK_RIGHT.0 || v == VK_UP.0 || v == VK_DOWN.0) {
                        return LRESULT(1);
                    }
                    if is_alt && up {
                        TAB_ACTIVE.store(false, Ordering::Relaxed);
                        if !TAB_CANCEL.load(Ordering::Relaxed) {
                            notify(msg::SWITCH, 2);
                        }
                        // Break the lone Alt so the app in front doesn't show its menu bar.
                        if masked_release(vk) {
                            request_check();
                            return LRESULT(1);
                        }
                    }
                }
            }

            // Win+letter shortcuts, and Win+Tab.
            let bit = match vk {
                v if (b'A' as u16..=b'Z' as u16).contains(&v) => 1u32 << (v - b'A' as u16),
                v if v == VK_TAB.0 => TAB_BIT,
                _ => 0,
            };
            if bit != 0 {
                let win_held = WIN_DOWN.load(Ordering::Relaxed) || down_now(VK_LWIN) || down_now(VK_RWIN);
                // The keys themselves, not what this hook last heard of them:
                // a release it never saw (a lock screen, an elevated window
                // in front) would otherwise leave Windows' own shortcut free.
                let plain = !down_now(VK_MENU) && !down_now(VK_CONTROL) && !down_now(VK_SHIFT);
                if down && win_held && plain && SHORTCUTS.load(Ordering::Relaxed) & bit != 0 && handled_here() {
                    COMBO.store(true, Ordering::Relaxed);
                    // Held down, the key repeats: act on the first press only.
                    let held = HELD_SHORTCUTS.fetch_or(bit, Ordering::Relaxed) & bit != 0;
                    let gap = k.time.wrapping_sub(SHORTCUT_AT.swap(k.time, Ordering::Relaxed));
                    if !held || gap > REPEAT_GAP {
                        // To Windows it's a combination now, so letting Win go
                        // won't open its Start menu.
                        mask_tap();
                        notify(msg::SHORTCUT, vk as usize);
                    }
                    return LRESULT(1);
                }
                if up && HELD_SHORTCUTS.fetch_and(!bit, Ordering::Relaxed) & bit != 0 {
                    return LRESULT(1);
                }
            }

            // Win key.
            if WIN_ON.load(Ordering::Relaxed) {
                if is_win && down {
                    // A Win release this hook never saw (an elevated window had
                    // the keyboard) leaves the key looking held. Real repeats
                    // come at least every second, so a longer gap means stale.
                    let gap = k.time.wrapping_sub(LAST_WIN.swap(k.time, Ordering::Relaxed));
                    let stale = WIN_DOWN.load(Ordering::Relaxed) && gap > 1500;
                    if (!WIN_DOWN.load(Ordering::Relaxed) || stale) && handled_here() {
                        WIN_DOWN.store(true, Ordering::Relaxed);
                        COMBO.store(false, Ordering::Relaxed);
                        let p = cursor();
                        WIN_AT[0].store(p.x, Ordering::Relaxed);
                        WIN_AT[1].store(p.y, Ordering::Relaxed);
                        WIN_SINCE.store(k.time, Ordering::Relaxed);
                        MASK_SEEN.store(false, Ordering::Relaxed);
                        // Break the lone Win while it's held; the real
                        // release then passes through untouched.
                        request_mask();
                    }
                } else if is_win && up {
                    let was_down = WIN_DOWN.swap(false, Ordering::Relaxed);
                    if was_down && !COMBO.load(Ordering::Relaxed) && !mouse_used(k.time) {
                        let held = k.time.wrapping_sub(WIN_SINCE.load(Ordering::Relaxed));
                        LONE_HELD.store(held, Ordering::Relaxed);
                        if MASK_SEEN.load(Ordering::Relaxed) && held <= LONG_HOLD {
                            // Windows has seen a key come between the press and
                            // this release: its Start menu stays shut.
                            LONE.store(1, Ordering::Relaxed);
                            on_lone_win();
                        } else {
                            // Released before the mask tap went out, or after
                            // a long hold (a program watching the Win key,
                            // such as PowerToys' Shortcut Guide, may have
                            // undone the early mask by then). Send one right
                            // before the release, as one batch with it, and
                            // hold back the real release. If Windows didn't
                            // accept the batch the real one passes.
                            let handled = masked_release(vk);
                            let long = held > LONG_HOLD;
                            LONE.store(
                                match (long, handled) {
                                    (false, true) => 2,
                                    (false, false) => 3,
                                    (true, true) => 4,
                                    (true, false) => 5,
                                },
                                Ordering::Relaxed,
                            );
                            on_lone_win();
                            if handled {
                                request_check();
                                return LRESULT(1);
                            }
                        }
                    }
                } else if down && WIN_DOWN.load(Ordering::Relaxed) {
                    COMBO.store(true, Ordering::Relaxed);
                }
            }
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

/// The Win+letter shortcuts Cascade takes with `cfg`, for `configure`.
/// Win+Tab's bit in `SHORTCUTS` (past the letters).
pub const TAB_BIT: u32 = 1 << 26;

pub fn shortcuts(cfg: &crate::config::Config) -> u32 {
    let bit = |c: u8, on: bool| if on { 1u32 << (c - b'A') } else { 0 };
    bit(b'X', cfg.shortcut_win_x)
        // The widgets panel opens from Cascade's taskbar. Controls come down
        // from the notch, or on their own with the notch off.
        | bit(b'N', cfg.shortcut_win_n && cfg.dock)
        | bit(b'A', cfg.shortcut_win_a)
        | bit(b'V', cfg.shortcut_win_v)
        | if cfg.shortcut_win_tab { TAB_BIT } else { 0 }
}

/// Turns the Win-key launcher, the Alt+Tab switcher and the Win+key
/// `shortcuts` (see `shortcuts`) on or off, starting or stopping the hook's
/// thread as needed.
pub fn configure(win: bool, tab: bool, shortcuts: u32) {
    WIN_ON.store(win, Ordering::Relaxed);
    TAB_ON.store(tab, Ordering::Relaxed);
    SHORTCUTS.store(shortcuts, Ordering::Relaxed);
    let wanted = win || tab || shortcuts != 0;
    let mut thread = THREAD.lock().unwrap_or_else(|e| e.into_inner());
    if wanted && thread.is_none() {
        *thread = start();
    } else if !wanted {
        stop(&mut thread);
    }
}

/// Stops the hook's thread and forgets what it knew of the keys.
fn stop(thread: &mut Option<(u32, JoinHandle<()>)>) {
    let Some((id, handle)) = thread.take() else { return };
    unsafe {
        let _ = PostThreadMessageW(id, WM_QUIT, WPARAM(0), LPARAM(0));
    }
    let _ = handle.join();
    // Lets the mask worker finish.
    *MASK_TX.lock().unwrap_or_else(|e| e.into_inner()) = None;
    for flag in [&WIN_DOWN, &WIN_LAST_DOWN, &COMBO, &ALT_DOWN, &CTRL_DOWN, &SHIFT_DOWN, &TAB_ACTIVE] {
        flag.store(false, Ordering::Relaxed);
    }
    HELD_SHORTCUTS.store(0, Ordering::Relaxed);
}

/// Installs the hook afresh. Windows can quietly drop a keyboard hook (one
/// that was held up too long, or across sleep and the lock screen) and then
/// the keys simply stop reaching Cascade: after those, put it back.
pub fn reinstall() {
    let mut thread = THREAD.lock().unwrap_or_else(|e| e.into_inner());
    if thread.is_none() {
        return;
    }
    stop(&mut thread);
    *thread = start();
}

/// Starts the hook's thread: it installs the hook, then only waits for keys
/// (and the word to stop). None if the hook couldn't be installed.
fn start() -> Option<(u32, JoinHandle<()>)> {
    let (mask_tx, mask_rx) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        while mask_rx.recv().is_ok() {
            std::thread::sleep(MASK_DELAY);
            // Still held, and still waiting for its mask.
            if WIN_DOWN.load(Ordering::Relaxed) && !MASK_SEEN.load(Ordering::Relaxed) {
                unsafe { mask_tap() };
            }
        }
    });
    *MASK_TX.lock().unwrap_or_else(|e| e.into_inner()) = Some(mask_tx);
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || unsafe {
        // Keys are waiting on it: it goes first.
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
        // For the shell's fullscreen check (Alt+Tab leaves games alone).
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let mut m = MSG::default();
        // Makes its message queue, so it can be told to stop.
        let _ = PeekMessageW(&mut m, None, 0, 0, PM_NOREMOVE);
        let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(proc), None, 0).ok();
        let id = GetCurrentThreadId();
        let _ = tx.send(hook.is_some().then_some(id));
        let Some(hook) = hook else { return };
        // The hook is called from in here, as keys come.
        while GetMessageW(&mut m, None, 0, 0).0 > 0 {
            DispatchMessageW(&m);
        }
        let _ = UnhookWindowsHookEx(hook);
    });
    match rx.recv() {
        Ok(Some(id)) => Some((id, handle)),
        _ => {
            let _ = handle.join();
            *MASK_TX.lock().unwrap_or_else(|e| e.into_inner()) = None;
            crate::config::log("keys: the keyboard hook could not be installed");
            None
        }
    }
}
