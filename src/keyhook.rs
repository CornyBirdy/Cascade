//! Opens the Cascade launcher on a lone Win key press. Win+anything
//! (Win+E, Win+R, Win+Shift+S...) is left completely alone.
//!
//! When Win is released without another key in between, the release is
//! swallowed and replaced by one batch of input: an unassigned "mask" key tap,
//! then the Win release. To Windows that looks like a key combination, so it
//! doesn't open its own Start menu. (Tapping the mask after the real release
//! is too late, and tapping it before leaves the real release to open Start.)
//!
//! Swallowing a release risks leaving Windows thinking Win is still held, so:
//! nothing is swallowed unless the replacement was accepted, and shortly
//! afterwards the key state is checked and a release sent if it's still down.

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::msg;

/// Set in the admin helper, which tells the main Cascade window instead.
static IN_HELPER: AtomicBool = AtomicBool::new(false);
static HELPER_WINDOW: AtomicIsize = AtomicIsize::new(0);

/// Sent to the admin helper's window to check the Win key afterwards.
pub const KEY_CHECK_MSG: u32 = WM_APP + 41;

fn on_lone_win() {
    if IN_HELPER.load(Ordering::Relaxed) {
        crate::helper::lone_win();
    } else {
        msg::post(msg::OPEN_LAUNCHER, 0, 0);
    }
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

/// Whether this copy should act on the Win key: the admin helper takes over
/// from the main app while it runs, and quits once Cascade has gone.
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
static WIN_DOWN: AtomicBool = AtomicBool::new(false);
static COMBO: AtomicBool = AtomicBool::new(false);

/// An unassigned virtual key, used only to "break" the lone Win press.
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

/// Releases a Win key that Windows still thinks is held, if there is one.
pub fn fix_stuck() {
    if WIN_DOWN.load(Ordering::Relaxed) {
        return; // really held right now
    }
    let mut inputs = Vec::new();
    for vk in [VK_LWIN.0, VK_RWIN.0] {
        if unsafe { GetAsyncKeyState(vk as i32) } as u16 & 0x8000 != 0 {
            inputs.push(key(vk, true));
        }
    }
    if !inputs.is_empty() {
        unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
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
        if !injected {
            if is_win && down {
                if !WIN_DOWN.load(Ordering::Relaxed) && handled_here() {
                    WIN_DOWN.store(true, Ordering::Relaxed);
                    COMBO.store(false, Ordering::Relaxed);
                }
            } else if is_win && up {
                let was_down = WIN_DOWN.swap(false, Ordering::Relaxed);
                if was_down && !COMBO.load(Ordering::Relaxed) {
                    on_lone_win();
                    // Mask tap and Win release as one batch, in that order.
                    let inputs = [key(MASK_KEY, false), key(MASK_KEY, true), key(vk, true)];
                    let sent = SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
                    if sent as usize == inputs.len() {
                        request_check();
                        return LRESULT(1);
                    }
                    // Refused (e.g. an admin window is in front): let the real
                    // release through rather than risk a stuck key.
                }
            } else if down && WIN_DOWN.load(Ordering::Relaxed) {
                COMBO.store(true, Ordering::Relaxed);
            }
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

/// Installs or removes the hook. Must be called on the UI thread (the hook
/// runs on the thread that installed it, through its message loop).
pub fn set(enabled: bool) {
    let current = HOOK.load(Ordering::Relaxed);
    unsafe {
        if enabled && current == 0 {
            if let Ok(h) = SetWindowsHookExW(WH_KEYBOARD_LL, Some(proc), None, 0) {
                HOOK.store(h.0 as isize, Ordering::Relaxed);
            }
        } else if !enabled && current != 0 {
            let _ = UnhookWindowsHookEx(HHOOK(current as *mut _));
            HOOK.store(0, Ordering::Relaxed);
            WIN_DOWN.store(false, Ordering::Relaxed);
        }
    }
}
