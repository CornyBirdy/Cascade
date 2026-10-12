//! Global keyboard hook: opens the Cascade launcher on a lone Win key press
//! and drives the Alt+Tab switcher. Win+anything (Win+E, Win+R, Win+Shift+S...)
//! is left completely alone.
//!
//! The Windows Start menu only opens when Win is released with no other key
//! pressed since it went down. So as soon as Win goes down, one unassigned
//! "mask" key tap is injected: to Windows that looks like a key combination
//! and its own Start menu stays shut. The real Win release is never touched,
//! so other programs that watch the keyboard (window-drag tools, games) always
//! see the true state of the key and it can't get stuck.
//!
//! Alt+Tab works the same way: Tab is swallowed while Alt is held, and when
//! Alt is released it is replaced by mask + release, so the app in front
//! doesn't see a lone Alt and light up its menu bar.
//!
//! Alt+Tab swallows a release, which risks leaving Windows thinking Alt is
//! still held, so: nothing is swallowed unless the replacement was accepted,
//! and shortly afterwards the key state is checked and a release sent if it's
//! still down.

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::msg;

/// Set in the admin helper, which tells the main Cascade window instead.
static IN_HELPER: AtomicBool = AtomicBool::new(false);
static HELPER_WINDOW: AtomicIsize = AtomicIsize::new(0);

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

fn on_lone_win() {
    notify(msg::OPEN_LAUNCHER, 0);
}

/// Asks the owner of this hook to call `fix_stuck` in a moment.
fn request_check() {
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
        let up = crate::helper::main_window().is_some();
        if !up {
            unsafe { PostQuitMessage(0) };
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

static HOOK: AtomicIsize = AtomicIsize::new(0);
static WIN_ON: AtomicBool = AtomicBool::new(false);
static TAB_ON: AtomicBool = AtomicBool::new(false);
static WIN_DOWN: AtomicBool = AtomicBool::new(false);
static COMBO: AtomicBool = AtomicBool::new(false);
static ALT_DOWN: AtomicBool = AtomicBool::new(false);
static CTRL_DOWN: AtomicBool = AtomicBool::new(false);
static SHIFT_DOWN: AtomicBool = AtomicBool::new(false);
/// A Tab was swallowed while Alt was held: the switcher is in use.
static TAB_ACTIVE: AtomicBool = AtomicBool::new(false);
static TAB_CANCEL: AtomicBool = AtomicBool::new(false);

/// An unassigned virtual key, used only to "break" a lone Win or Alt press.
const MASK_KEY: u16 = 0xE8;

fn key(vk: u16, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: 0,
                dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Releases a Win or Alt key that Windows still thinks is held, if there is one.
pub fn fix_stuck() {
    let mut inputs = Vec::new();
    for (vk, tracked) in
        [(VK_LWIN.0, &WIN_DOWN), (VK_RWIN.0, &WIN_DOWN), (VK_LMENU.0, &ALT_DOWN), (VK_RMENU.0, &ALT_DOWN)]
    {
        // Really held right now? Then it isn't stuck.
        if !tracked.load(Ordering::Relaxed) && unsafe { GetAsyncKeyState(vk as i32) } as u16 & 0x8000 != 0 {
            inputs.push(key(vk, true));
        }
    }
    if !inputs.is_empty() {
        unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    }
}

/// Mask tap and the key's release as one batch, in that order. True if
/// Windows accepted all of it (so the real release can be swallowed).
unsafe fn masked_release(vk: u16) -> bool {
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
        if !injected {
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

            // Win key.
            if WIN_ON.load(Ordering::Relaxed) {
                if is_win && down {
                    if !WIN_DOWN.load(Ordering::Relaxed) && handled_here() {
                        WIN_DOWN.store(true, Ordering::Relaxed);
                        COMBO.store(false, Ordering::Relaxed);
                        // Break the lone Win now, while it's held; the real
                        // release then passes through untouched.
                        mask_tap();
                    }
                } else if is_win && up {
                    let was_down = WIN_DOWN.swap(false, Ordering::Relaxed);
                    if was_down && !COMBO.load(Ordering::Relaxed) {
                        on_lone_win();
                    }
                } else if down && WIN_DOWN.load(Ordering::Relaxed) {
                    COMBO.store(true, Ordering::Relaxed);
                }
            }
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

/// Turns the Win-key launcher and the Alt+Tab switcher on or off, installing
/// or removing the hook as needed. Must be called on the UI thread (the hook
/// runs on the thread that installed it, through its message loop).
pub fn configure(win: bool, tab: bool) {
    WIN_ON.store(win, Ordering::Relaxed);
    TAB_ON.store(tab, Ordering::Relaxed);
    let current = HOOK.load(Ordering::Relaxed);
    unsafe {
        if (win || tab) && current == 0 {
            if let Ok(h) = SetWindowsHookExW(WH_KEYBOARD_LL, Some(proc), None, 0) {
                HOOK.store(h.0 as isize, Ordering::Relaxed);
            }
        } else if !(win || tab) && current != 0 {
            let _ = UnhookWindowsHookEx(HHOOK(current as *mut _));
            HOOK.store(0, Ordering::Relaxed);
            WIN_DOWN.store(false, Ordering::Relaxed);
            ALT_DOWN.store(false, Ordering::Relaxed);
            TAB_ACTIVE.store(false, Ordering::Relaxed);
        }
    }
}
