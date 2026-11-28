//! Super drag: hold the Win key and drag a window by any part of it, not only
//! its title bar (`super_drag`).
//!
//! A mouse hook, on a thread of its own so a busy moment elsewhere never holds
//! the mouse up, watches for the left button going down while Win is held.
//! That click is kept from the window under it (it is the start of a drag, not
//! a click), and while the button stays down the window follows the pointer.
//! The moves are posted to the window rather than waited for, so one that has
//! stopped answering can't hold up the mouse.
//!
//! The hook is only in while Win is held (the key hook says when it goes down
//! and up), so the rest of the time no mouse movement anywhere on the PC
//! passes through Cascade. A mouse polling at 1000 Hz or more would otherwise
//! wake it that many times a second, and every move would wait on it. While
//! an admin window is in front the key hook can't see Win at all, so then
//! the mouse hook stays in.
//!
//! The Win key itself is left alone: one mask key tap (see `keyhook`) makes
//! Windows treat it as part of a combination, so letting go of it doesn't open
//! its Start menu. A maximised window comes out of it under the pointer, as
//! it does dragged by its title bar, and follows. Windows of programs with
//! admin rights (Task Manager, installers) are moved by the admin helper
//! when it runs (Windows doesn't let Cascade move them itself); the desktop
//! and the taskbar are left to the click.

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicIsize, AtomicU32, AtomicU8, Ordering};
use std::sync::Mutex;
use std::thread::JoinHandle;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::System::Threading::{
    GetCurrentProcessId, GetCurrentThread, GetCurrentThreadId, SetThreadPriority, THREAD_PRIORITY_TIME_CRITICAL,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_LWIN, VK_RWIN};
use windows::Win32::UI::WindowsAndMessaging::*;

/// The hook's thread (its id, to tell it to stop) while it's installed.
static THREAD: Mutex<Option<(u32, JoinHandle<()>)>> = Mutex::new(None);
/// The window being dragged (0 for none).
static WINDOW: AtomicIsize = AtomicIsize::new(0);
/// Where the pointer was as the drag began, and where the window's corner was.
static FROM: [AtomicI32; 4] = [const { AtomicI32::new(0) }; 4];
/// Set while a drag's button is down, to swallow its release.
static HELD: AtomicBool = AtomicBool::new(false);
/// The window being dragged has admin rights: the helper moves it.
static VIA_HELPER: AtomicBool = AtomicBool::new(false);
/// The hook thread's id while it runs (0: super drag is off).
static TID: AtomicU32 = AtomicU32::new(0);
/// Which Win keys are held, as the key hook saw them (bit 0 left, bit 1 right).
static WIN_KEYS: AtomicU8 = AtomicU8::new(0);
/// The window in front has admin rights, so the key hook can't see Win.
static ADMIN_FRONT: AtomicBool = AtomicBool::new(false);
/// Thread messages: put the mouse hook in, or take it out if it isn't needed.
const ARM: u32 = WM_APP + 1;
const DISARM: u32 = WM_APP + 2;

/// Whether the mouse hook is needed now: Win is held, a drag is going on, or
/// an admin window has the keyboard (Win can't be seen then).
fn wanted() -> bool {
    WIN_KEYS.load(Ordering::Relaxed) != 0 || HELD.load(Ordering::Relaxed) || ADMIN_FRONT.load(Ordering::Relaxed)
}

/// Tells the hook thread to put the hook in or take it out, to match `wanted`.
fn sync() {
    let tid = TID.load(Ordering::Relaxed);
    if tid != 0 {
        let m = if wanted() { ARM } else { DISARM };
        unsafe {
            let _ = PostThreadMessageW(tid, m, WPARAM(0), LPARAM(0));
        }
    }
}

/// A Win key went down or up (from the key hook, so it must be quick).
pub fn win_key(right: bool, down: bool) {
    let bit = if right { 2 } else { 1 };
    let before =
        if down { WIN_KEYS.fetch_or(bit, Ordering::Relaxed) } else { WIN_KEYS.fetch_and(!bit, Ordering::Relaxed) };
    // Only when it changes (a held key repeats its press).
    if (before != 0) != (WIN_KEYS.load(Ordering::Relaxed) != 0) {
        sync();
    }
}

/// A window came to the front: with admin rights, the hook stays in while
/// it's there (the key hook can't see Win then). UI thread.
pub fn foreground(h: HWND) {
    if TID.load(Ordering::Relaxed) == 0 {
        return;
    }
    let admin = crate::helper::window_above_us(h);
    let mut changed = ADMIN_FRONT.swap(admin, Ordering::Relaxed) != admin;
    // A Win release the key hook never saw (an admin window had the keyboard)
    // would leave the hook in for good.
    if WIN_KEYS.load(Ordering::Relaxed) != 0 && !down(VK_LWIN) && !down(VK_RWIN) {
        WIN_KEYS.store(0, Ordering::Relaxed);
        changed = true;
    }
    if changed {
        sync();
    }
}

fn down(vk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY) -> bool {
    unsafe { GetAsyncKeyState(vk.0 as i32) as u16 & 0x8000 != 0 }
}

fn class_of(h: HWND) -> String {
    let mut class = [0u16; 64];
    let n = unsafe { GetClassNameW(h, &mut class) } as usize;
    String::from_utf16_lossy(&class[..n])
}

/// The window to drag from a click at `pt`: the top-level window under it, if
/// it's one that can be moved this way. Otherwise why not, for the log.
fn pick(pt: POINT) -> Result<HWND, String> {
    unsafe {
        let under = WindowFromPoint(pt);
        if under.is_invalid() {
            return Err("no window under the pointer".into());
        }
        let root = GetAncestor(under, GA_ROOT);
        if root.is_invalid() {
            return Err(format!("no top-level window ({})", class_of(under)));
        }
        let class = class_of(root);
        if !IsWindowVisible(root).as_bool() {
            return Err(format!("{class} is not visible"));
        }
        if IsIconic(root).as_bool() {
            return Err(format!("{class} is minimised"));
        }
        if matches!(
            class.as_str(),
            "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" | "Windows.UI.Core.CoreWindow"
        ) {
            return Err(format!("{class} is the desktop or the taskbar"));
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(root, Some(&mut pid));
        // Cascade's own windows are its panels, except Settings (and its apps, other processes).
        if pid == GetCurrentProcessId() && class != "CascadeSettings" {
            return Err(format!("{class} is one of Cascade's own panels"));
        }
        if crate::helper::window_above_us(root) && !crate::helper::running() {
            return Err(format!("{class} has admin rights (turn on Start menu > Work over admin apps to drag those)"));
        }
        Ok(root)
    }
}

unsafe extern "system" fn proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let info = &*(lp.0 as *const MSLLHOOKSTRUCT);
        let injected = info.flags & LLMHF_INJECTED != 0;
        match wp.0 as u32 {
            WM_LBUTTONDOWN if !injected && (down(VK_LWIN) || down(VK_RWIN)) => {
                let picked = pick(info.pt);
                // Why a Win+click wasn't a drag, for `cascade.log`.
                if let Err(why) = &picked {
                    crate::config::log(&format!("super drag: left alone: {why}"));
                }
                if let Ok(h) = picked {
                    let mut r = RECT::default();
                    if GetWindowRect(h, &mut r).is_ok() {
                        let admin = crate::helper::window_above_us(h);
                        VIA_HELPER.store(admin, Ordering::Relaxed);
                        // Maximised: out of it, at its normal size, with the pointer
                        // over the same part of it (across), near its top.
                        if IsZoomed(h).as_bool() {
                            let mut place = WINDOWPLACEMENT {
                                length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
                                ..Default::default()
                            };
                            let _ = GetWindowPlacement(h, &mut place);
                            let n = place.rcNormalPosition;
                            let (nw, nh) = (n.right - n.left, n.bottom - n.top);
                            let across = (info.pt.x - r.left) as f32 / (r.right - r.left).max(1) as f32;
                            let left = info.pt.x - (nw as f32 * across) as i32;
                            let top = info.pt.y - (info.pt.y - r.top).clamp(8, 40);
                            r = RECT { left, top, right: left + nw, bottom: top + nh };
                            if admin {
                                crate::helper::request_move(h, left, top, true);
                            } else {
                                let _ = ShowWindowAsync(h, SW_RESTORE);
                                let flags = SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS;
                                let _ = SetWindowPos(h, None, left, top, 0, 0, flags);
                            }
                        }
                        for (slot, v) in FROM.iter().zip([info.pt.x, info.pt.y, r.left, r.top]) {
                            slot.store(v, Ordering::Relaxed);
                        }
                        WINDOW.store(h.0 as isize, Ordering::Relaxed);
                        HELD.store(true, Ordering::Relaxed);
                        // Wobbly windows wobble here too (not admin ones: they can't be reached).
                        if !admin {
                            crate::wobble::start(h);
                        }
                        // Win is part of a combination now: letting go of it
                        // won't open Windows' Start menu.
                        crate::keyhook::mask_win();
                        // To the front, without taking the keyboard from anyone.
                        let _ = SetWindowPos(
                            h,
                            Some(HWND_TOP),
                            0,
                            0,
                            0,
                            0,
                            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS,
                        );
                        return LRESULT(1);
                    }
                }
            }
            WM_MOUSEMOVE if WINDOW.load(Ordering::Relaxed) != 0 => {
                // The release was never seen (a lock screen, a hook that was
                // dropped): the drag is over.
                if !down(VK_LBUTTON) {
                    WINDOW.store(0, Ordering::Relaxed);
                    HELD.store(false, Ordering::Relaxed);
                    sync();
                } else {
                    let h = HWND(WINDOW.load(Ordering::Relaxed) as *mut _);
                    let f = |i: usize| FROM[i].load(Ordering::Relaxed);
                    let (x, y) = (f(2) + info.pt.x - f(0), f(3) + info.pt.y - f(1));
                    if VIA_HELPER.load(Ordering::Relaxed) {
                        crate::helper::request_move(h, x, y, false);
                        return CallNextHookEx(None, code, wp, lp);
                    }
                    let _ = SetWindowPos(
                        h,
                        None,
                        x,
                        y,
                        0,
                        0,
                        SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS,
                    );
                }
            }
            WM_LBUTTONUP if HELD.swap(false, Ordering::Relaxed) => {
                WINDOW.store(0, Ordering::Relaxed);
                crate::wobble::release();
                // Win let go during the drag: the hook comes out now.
                sync();
                return LRESULT(1);
            }
            _ => {}
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

/// Turns super drag on or off, starting or stopping the hook's thread.
pub fn configure(on: bool) {
    crate::keyhook::watch_super(on);
    let mut thread = THREAD.lock().unwrap_or_else(|e| e.into_inner());
    if on && thread.is_none() {
        *thread = start();
    } else if !on {
        if let Some((id, handle)) = thread.take() {
            TID.store(0, Ordering::Relaxed);
            unsafe {
                let _ = PostThreadMessageW(id, WM_QUIT, WPARAM(0), LPARAM(0));
            }
            let _ = handle.join();
            WINDOW.store(0, Ordering::Relaxed);
            HELD.store(false, Ordering::Relaxed);
        }
    }
}

/// Starts the hook's thread: it tries the hook once (to say whether it can be
/// had), then waits to be told to put it in (Win held) or take it out, for the
/// mouse while it's in, and for the word to stop. None if the hook couldn't be
/// installed.
fn start() -> Option<(u32, JoinHandle<()>)> {
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
        let mut m = MSG::default();
        // Makes its message queue, so it can be told to stop.
        let _ = PeekMessageW(&mut m, None, 0, 0, PM_NOREMOVE);
        let mut hook = SetWindowsHookExW(WH_MOUSE_LL, Some(proc), None, 0).ok();
        if hook.is_none() {
            let _ = tx.send(None);
            return;
        }
        TID.store(GetCurrentThreadId(), Ordering::Relaxed);
        let _ = tx.send(Some(GetCurrentThreadId()));
        // Out again until it's wanted (anything wanted meanwhile has posted an ARM).
        if !wanted() {
            if let Some(h) = hook.take() {
                let _ = UnhookWindowsHookEx(h);
            }
        }
        while GetMessageW(&mut m, None, 0, 0).0 > 0 {
            match m.message {
                ARM if hook.is_none() => {
                    hook = SetWindowsHookExW(WH_MOUSE_LL, Some(proc), None, 0).ok();
                    if hook.is_none() {
                        crate::config::log("super drag: the mouse hook could not be put back in");
                    }
                }
                DISARM if !wanted() => {
                    if let Some(h) = hook.take() {
                        let _ = UnhookWindowsHookEx(h);
                    }
                }
                ARM | DISARM => {}
                _ => {
                    DispatchMessageW(&m);
                }
            }
        }
        if let Some(h) = hook {
            let _ = UnhookWindowsHookEx(h);
        }
    });
    match rx.recv() {
        Ok(Some(id)) => Some((id, handle)),
        _ => {
            let _ = handle.join();
            crate::config::log("super drag: the mouse hook could not be installed");
            None
        }
    }
}
