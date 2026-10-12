//! Opens the Cascade launcher on a lone Win key press. Win+anything
//! (Win+E, Win+R, Win+Shift+S...) is left completely alone.
//!
//! On a lone Win release, an unassigned "mask" key is tapped just before the
//! release goes through. To Windows that looks like a key combination, so it
//! doesn't open its own Start menu. Keys sent from inside the hook are
//! queued ahead of the key being hooked, which gives exactly the order
//! Win down, mask down, mask up, Win up. Nothing is swallowed or replayed,
//! so the Win key can't get stuck down.

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::msg;

/// Set in the admin helper, which tells the main Cascade window instead.
static IN_HELPER: AtomicBool = AtomicBool::new(false);

fn on_lone_win() {
    if IN_HELPER.load(Ordering::Relaxed) {
        crate::helper::lone_win();
    } else {
        msg::post(msg::OPEN_LAUNCHER, 0, 0);
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

pub fn set_helper_mode() {
    IN_HELPER.store(true, Ordering::Relaxed);
}

fn send_mask() {
    let inputs = [key(MASK_KEY, false), key(MASK_KEY, true)];
    unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
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
                    send_mask();
                    on_lone_win();
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
