//! Acrylic blur behind panels (taskbar islands, Start menu).
//!
//! A plain window the size of the panel sits just under the drawn content
//! and asks DWM to blur what's behind it. The blur runs on the GPU inside
//! DWM, the same way the Windows taskbar does it, and keeps working while the
//! window is inactive (unlike the newer system backdrops).

use std::ffi::c_void;
use std::sync::OnceLock;

use windows::core::{s, w, Result, BOOL};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::{GetStockObject, HBRUSH, BLACK_BRUSH};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::WindowsAndMessaging::*;

#[repr(C)]
struct AccentPolicy {
    state: u32,
    flags: u32,
    gradient: u32,
    animation: u32,
}

#[repr(C)]
struct CompositionData {
    attribute: u32,
    data: *mut c_void,
    size: usize,
}

type SetWindowCompositionAttribute = unsafe extern "system" fn(HWND, *mut CompositionData) -> BOOL;

/// Undocumented (but what Explorer itself uses), so it isn't in the import
/// library: look it up once at run time.
fn set_composition() -> Option<SetWindowCompositionAttribute> {
    static F: OnceLock<Option<SetWindowCompositionAttribute>> = OnceLock::new();
    *F.get_or_init(|| unsafe {
        let user32 = GetModuleHandleW(w!("user32.dll")).ok()?;
        let f = GetProcAddress(user32, s!("SetWindowCompositionAttribute"))?;
        // SAFETY: it is user32's SetWindowCompositionAttribute, which has this signature.
        Some(std::mem::transmute::<unsafe extern "system" fn() -> isize, SetWindowCompositionAttribute>(f))
    })
}

const WCA_ACCENT_POLICY: u32 = 19;
const ACCENT_DISABLED: u32 = 0;
const ACCENT_ENABLE_ACRYLICBLURBEHIND: u32 = 4;

unsafe extern "system" fn proc(hwnd: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match m {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        _ => DefWindowProcW(hwnd, m, wp, lp),
    }
}

/// Registers the window class (once, from the UI thread).
pub fn register(instance: HINSTANCE) {
    unsafe {
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(proc),
            hInstance: instance,
            // Black paints as "transparent" once the frame covers the client.
            hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
            lpszClassName: w!("CascadeBackdrop"),
            ..Default::default()
        };
        RegisterClassExW(&wc);
    }
}

pub struct Backdrop {
    pub hwnd: HWND,
    rect: RECT,
    shown: bool,
}

impl Backdrop {
    pub fn create(instance: HINSTANCE) -> Result<Self> {
        unsafe {
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
                w!("CascadeBackdrop"),
                w!(""),
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                Some(instance),
                None,
            )?;
            let margins = MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 };
            let _ = DwmExtendFrameIntoClientArea(hwnd, &margins);
            let dark = BOOL(1);
            let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as *const _, 4);
            // No DWM shadow/border lines of its own.
            let border = COLORREF(0xFFFFFFFE); // DWMWA_COLOR_NONE
            let _ = DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, &border as *const _ as *const _, 4);
            Ok(Self { hwnd, rect: RECT::default(), shown: false })
        }
    }

    /// `tint` is 0xAARRGGBB; `radius` picks the DWM corner style.
    pub fn style(&self, acrylic: bool, tint: u32, radius: f32) {
        let (a, r, g, b) = (tint >> 24, (tint >> 16) & 0xFF, (tint >> 8) & 0xFF, tint & 0xFF);
        let mut policy = AccentPolicy {
            state: if acrylic { ACCENT_ENABLE_ACRYLICBLURBEHIND } else { ACCENT_DISABLED },
            flags: 0,
            // ABGR, as the API wants it.
            gradient: (a << 24) | (b << 16) | (g << 8) | r,
            animation: 0,
        };
        let mut data = CompositionData {
            attribute: WCA_ACCENT_POLICY,
            data: &mut policy as *mut _ as *mut c_void,
            size: std::mem::size_of::<AccentPolicy>(),
        };
        let corners = if radius >= 6.0 {
            DWMWCP_ROUND
        } else if radius >= 2.0 {
            DWMWCP_ROUNDSMALL
        } else {
            DWMWCP_DONOTROUND
        };
        unsafe {
            if let Some(f) = set_composition() {
                let _ = f(self.hwnd, &mut data);
            }
            let _ = DwmSetWindowAttribute(
                self.hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &corners as *const _ as *const _,
                std::mem::size_of_val(&corners) as u32,
            );
        }
    }

    /// Puts the blur at `r` (screen px), directly under `content`.
    pub fn place(&mut self, r: RECT, content: HWND) {
        if r == self.rect && self.shown {
            return;
        }
        self.rect = r;
        self.shown = true;
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(content),
                r.left,
                r.top,
                (r.right - r.left).max(1),
                (r.bottom - r.top).max(1),
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
    }

    pub fn hide(&mut self) {
        if self.shown {
            self.shown = false;
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
        }
    }
}
