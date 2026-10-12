//! Opens the Cascade launcher on a lone Win key press. Win+anything
//! (Win+E, Win+R, Win+Shift+S...) is left completely alone.
//!
//! While Win is held, an unassigned "mask" key is tapped. To Windows that
//! looks like a key combination, so releasing Win doesn't open its own Start
//! menu. No real key event is ever swallowed or replayed, so the Win key
//! can't get stuck down.
//!
//! Timing matters: keys sent from inside the hook are handled by Windows
//! straight away, before the key the hook is looking at. So the tap is
//! posted to run just after the Win press has gone through; if Win is let go
//! before that happens, the tap is sent from the release instead, where
//! "before this key" is exactly what's needed.

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
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

/// Posted to the window that owns the hook to send the mask tap.
pub const MASK_MSG: u32 = WM_APP + 40;
static MASK_TARGET: AtomicIsize = AtomicIsize::new(0);
static MASKED: AtomicBool = AtomicBool::new(false);

/// In the admin helper, `hwnd` is its window (it gets `MASK_MSG`).
pub fn set_helper_mode(hwnd: HWND) {
    IN_HELPER.store(true, Ordering::Relaxed);
    MASK_TARGET.store(hwnd.0 as isize, Ordering::Relaxed);
}

fn send_mask() {
    if !MASKED.swap(true, Ordering::Relaxed) {
        let inputs = [key(MASK_KEY, false), key(MASK_KEY, true)];
        unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    }
}

/// Handles `MASK_MSG`: Win is (probably) still held, and Windows has seen
/// it go down by now.
pub fn deferred_mask() {
    if WIN_DOWN.load(Ordering::Relaxed) && !COMBO.load(Ordering::Relaxed) {
        send_mask();
    }
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
                    MASKED.store(false, Ordering::Relaxed);
                    // Not from here: it would reach Windows before this Win press.
                    if IN_HELPER.load(Ordering::Relaxed) {
                        let target = HWND(MASK_TARGET.load(Ordering::Relaxed) as *mut _);
                        let _ = PostMessageW(Some(target), MASK_MSG, WPARAM(0), LPARAM(0));
                    } else {
                        msg::post(MASK_MSG, 0, 0);
                    }
                }
            } else if is_win && up {
                let was_down = WIN_DOWN.swap(false, Ordering::Relaxed);
                if was_down && !COMBO.load(Ordering::Relaxed) {
                    // A very quick tap: mask it now, ahead of this release.
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
