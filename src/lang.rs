//! Keyboard languages: their names, the one the app in front is typing in,
//! and switching to the next (Win+Space, or the taskbar's language button).
//! Windows' own switcher and indicator belong to Explorer; switching itself
//! is asked of the app in front, the way they ask it (Alt+Shift still works
//! on its own, being Windows' own).

use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::Globalization::{GetLocaleInfoW, LOCALE_SLOCALIZEDDISPLAYNAME};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyboardLayout, GetKeyboardLayoutList, HKL};
use windows::Win32::UI::WindowsAndMessaging::*;

fn locale_text(hkl: HKL, what: u32) -> String {
    let lcid = (hkl.0 as usize & 0xFFFF) as u32;
    let mut buf = [0u16; 128];
    let n = unsafe { GetLocaleInfoW(lcid, what, Some(&mut buf)) } as usize;
    String::from_utf16_lossy(&buf[..n.saturating_sub(1).min(128)])
}

/// "English (Australia)".
pub fn layout_name(hkl: HKL) -> String {
    locale_text(hkl, LOCALE_SLOCALIZEDDISPLAYNAME)
}

#[allow(dead_code)] // for the taskbar's language button
/// The short name the taskbar shows: "ENG", "DEU"... (the language's three ISO letters, as Windows' own).
pub fn short_name(hkl: HKL) -> String {
    const LOCALE_SISO639LANGNAME2: u32 = 0x67;
    locale_text(hkl, LOCALE_SISO639LANGNAME2).to_uppercase()
}

pub fn layouts() -> Vec<HKL> {
    let mut list = [HKL::default(); 32];
    let n = unsafe { GetKeyboardLayoutList(Some(&mut list)) } as usize;
    list[..n.min(32)].to_vec()
}

/// The layout the window in front is typing with.
pub fn current() -> HKL {
    unsafe {
        let fg = GetForegroundWindow();
        GetKeyboardLayout(GetWindowThreadProcessId(fg, None))
    }
}

/// Switches the app in front to the next layout; returns its name.
pub fn next() -> Option<String> {
    let all = layouts();
    if all.len() < 2 {
        return None;
    }
    let now = current();
    let i = all.iter().position(|h| *h == now).map(|i| (i + 1) % all.len()).unwrap_or(0);
    let to = all[i];
    unsafe {
        let fg = GetForegroundWindow();
        let _ = PostMessageW(Some(fg), WM_INPUTLANGCHANGEREQUEST, WPARAM(0), LPARAM(to.0 as isize));
    }
    Some(layout_name(to))
}
