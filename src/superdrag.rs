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
//! The Win key itself is left alone: one mask key tap (see `keyhook`) makes
//! Windows treat it as part of a combination, so letting go of it doesn't open
//! its Start menu. Windows of programs with admin rights (which Cascade can't
//! move), maximised ones, the desktop, the taskbar and Cascade's own are left
//! to the click.

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicIsize, Ordering};
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

fn down(vk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY) -> bool {
    unsafe { GetAsyncKeyState(vk.0 as i32) as u16 & 0x8000 != 0 }
}

fn class_of(h: HWND) -> String {
    let mut class = [0u16; 64];
    let n = unsafe { GetClassNameW(h, &mut class) } as usize;
    String::from_utf16_lossy(&class[..n])
}

/// The window to drag from a click at `pt`: the top-level window under it, if
/// it's one that can be moved this way.
fn pick(pt: POINT) -> Option<HWND> {
    unsafe {
        let under = WindowFromPoint(pt);
        if under.is_invalid() {
            return None;
        }
        let root = GetAncestor(under, GA_ROOT);
        if root.is_invalid() || !IsWindowVisible(root).as_bool() || IsIconic(root).as_bool() || IsZoomed(root).as_bool()
        {
            return None;
        }
        if matches!(
            class_of(root).as_str(),
            "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" | "Windows.UI.Core.CoreWindow"
        ) {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(root, Some(&mut pid));
        if pid == GetCurrentProcessId() || crate::helper::window_above_us(root) {
            return None;
        }
        Some(root)
    }
}

unsafe extern "system" fn proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let info = &*(lp.0 as *const MSLLHOOKSTRUCT);
        let injected = info.flags & LLMHF_INJECTED != 0;
        match wp.0 as u32 {
            WM_LBUTTONDOWN if !injected && (down(VK_LWIN) || down(VK_RWIN)) => {
                if let Some(h) = pick(info.pt) {
                    let mut r = RECT::default();
                    if GetWindowRect(h, &mut r).is_ok() {
                        for (slot, v) in FROM.iter().zip([info.pt.x, info.pt.y, r.left, r.top]) {
                            slot.store(v, Ordering::Relaxed);
                        }
                        WINDOW.store(h.0 as isize, Ordering::Relaxed);
                        HELD.store(true, Ordering::Relaxed);
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
                } else {
                    let h = HWND(WINDOW.load(Ordering::Relaxed) as *mut _);
                    let f = |i: usize| FROM[i].load(Ordering::Relaxed);
                    let (x, y) = (f(2) + info.pt.x - f(0), f(3) + info.pt.y - f(1));
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
                return LRESULT(1);
            }
            _ => {}
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

/// Turns super drag on or off, starting or stopping the hook's thread.
pub fn configure(on: bool) {
    let mut thread = THREAD.lock().unwrap_or_else(|e| e.into_inner());
    if on && thread.is_none() {
        *thread = start();
    } else if !on {
        if let Some((id, handle)) = thread.take() {
            unsafe {
                let _ = PostThreadMessageW(id, WM_QUIT, WPARAM(0), LPARAM(0));
            }
            let _ = handle.join();
            WINDOW.store(0, Ordering::Relaxed);
            HELD.store(false, Ordering::Relaxed);
        }
    }
}

/// Starts the hook's thread: it installs the hook, then only waits for the
/// mouse (and the word to stop). None if the hook couldn't be installed.
fn start() -> Option<(u32, JoinHandle<()>)> {
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
        let mut m = MSG::default();
        // Makes its message queue, so it can be told to stop.
        let _ = PeekMessageW(&mut m, None, 0, 0, PM_NOREMOVE);
        let hook = SetWindowsHookExW(WH_MOUSE_LL, Some(proc), None, 0).ok();
        let _ = tx.send(hook.is_some().then(|| GetCurrentThreadId()));
        let Some(hook) = hook else { return };
        while GetMessageW(&mut m, None, 0, 0).0 > 0 {
            DispatchMessageW(&m);
        }
        let _ = UnhookWindowsHookEx(hook);
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
