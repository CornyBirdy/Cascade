//! Closes pop-ups (the Start menu, the hidden-icons flyout, the widgets panel,
//! the notch opened from the taskbar, the Win+X and Win+V menus) when you click anywhere
//! outside them.
//!
//! Windows only tells a pop-up it lost focus when something else takes it,
//! which clicking the desktop, the taskbar or an admin window doesn't always
//! do. While a pop-up is open, a mouse hook watches button presses and
//! reports ones outside every pop-up. It's installed only while one is open,
//! so the rest of the time it costs nothing.

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicIsize, Ordering};

use windows::Win32::Foundation::{LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::msg;

const SLOTS: usize = 6;

static HOOK: AtomicIsize = AtomicIsize::new(0);
static ACTIVE: [AtomicBool; SLOTS] = [const { AtomicBool::new(false) }; SLOTS];
/// left, top, right, bottom per slot.
static RECTS: [AtomicI32; SLOTS * 4] = [const { AtomicI32::new(0) }; SLOTS * 4];

fn inside(slot: usize, x: i32, y: i32) -> bool {
    let r = |i: usize| RECTS[slot * 4 + i].load(Ordering::Relaxed);
    x >= r(0) && x < r(2) && y >= r(1) && y < r(3)
}

unsafe extern "system" fn proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let m = wp.0 as u32;
        if matches!(m, WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN) {
            let pt = (*(lp.0 as *const MSLLHOOKSTRUCT)).pt;
            let hit = (0..SLOTS).any(|i| ACTIVE[i].load(Ordering::Relaxed) && inside(i, pt.x, pt.y));
            if !hit {
                msg::post(msg::OUTSIDE_CLICK, 0, 0);
            }
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

/// Sets (or clears) the screen rect of pop-up `slot`, installing the hook
/// while any pop-up is open. UI thread only.
pub fn set_rect(slot: usize, rect: Option<RECT>) {
    if let Some(r) = rect {
        for (i, v) in [r.left, r.top, r.right, r.bottom].into_iter().enumerate() {
            RECTS[slot * 4 + i].store(v, Ordering::Relaxed);
        }
    }
    ACTIVE[slot].store(rect.is_some(), Ordering::Relaxed);
    let any = ACTIVE.iter().any(|a| a.load(Ordering::Relaxed));
    let current = HOOK.load(Ordering::Relaxed);
    unsafe {
        if any && current == 0 {
            if let Ok(h) = SetWindowsHookExW(WH_MOUSE_LL, Some(proc), None, 0) {
                HOOK.store(h.0 as isize, Ordering::Relaxed);
            }
        } else if !any && current != 0 {
            let _ = UnhookWindowsHookEx(HHOOK(current as *mut _));
            HOOK.store(0, Ordering::Relaxed);
        }
    }
}
