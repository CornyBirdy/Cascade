//! Acrylic (or clear) blur behind panels (taskbar islands, Start menu, the
//! notch's lip).
//!
//! A plain window the size of the panel sits just under the drawn content
//! and asks DWM to blur what's behind it. The blur runs on the GPU inside
//! DWM, the same way the Windows taskbar does it, and keeps working while the
//! window is inactive (unlike the newer system backdrops).

use std::cell::Cell;
use std::ffi::c_void;
use std::sync::OnceLock;

use windows::core::{s, w, Result, BOOL};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::{CreateRoundRectRgn, GetStockObject, SetWindowRgn, BLACK_BRUSH, HBRUSH};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::lens::{self, Lens};
use crate::theme::Glass;

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
const ACCENT_ENABLE_TRANSPARENTGRADIENT: u32 = 2;
const ACCENT_ENABLE_BLURBEHIND: u32 = 3;
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
    /// The rounded rect the window is cut to, if any (see `place_shaped`).
    shape: Option<(RECT, i32)>,
    /// Where `track` was last asked to put the blur.
    last: Option<RECT>,
    /// Radius of DWM's rounded corners at 100% scaling (0 = square).
    corner: Cell<f32>,
    /// Bent light along the panel's edge instead of a blur (see `lens`).
    lens: Cell<Option<Lens>>,
    /// Hidden while switching between a blur and a ring: show it again at
    /// the next `place`, even if the panel hasn't moved.
    reshow: Cell<bool>,
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
            Ok(Self {
                hwnd,
                rect: RECT::default(),
                shown: false,
                shape: None,
                last: None,
                corner: Cell::new(0.0),
                lens: Cell::new(None),
                reshow: Cell::new(false),
            })
        }
    }

    /// Makes the window a ring of light bent round the panel's edge (see
    /// `lens`) instead of a blur, or (`None`) a blur again. Call before
    /// `style`.
    pub fn set_lens(&self, lens: Option<Lens>) {
        if lens == self.lens.get() {
            return;
        }
        let was = self.lens.replace(lens);
        if was.is_some() == lens.is_some() {
            // Only the bend changed: drawn again at the next `place`.
            self.rect_changed();
            return;
        }
        unsafe {
            // A layered window keeps showing its last picture after it stops
            // being layered (the old ring stayed on as an outline round a
            // panel switched to Acrylic), and a window just made layered
            // shows nothing until it's drawn: hide it while it changes, and
            // let `place` show it again in its new kind.
            lens::hide(self.hwnd);
            if IsWindowVisible(self.hwnd).as_bool() {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
                self.reshow.set(true);
            }
            let ex = GetWindowLongW(self.hwnd, GWL_EXSTYLE) as u32;
            let ex = if lens.is_some() { ex | WS_EX_LAYERED.0 } else { ex & !WS_EX_LAYERED.0 };
            SetWindowLongW(self.hwnd, GWL_EXSTYLE, ex as i32);
            if lens.is_none() {
                // Back to a plain window: its frame covers it again, so the
                // blur shows through (layered windows ignore this).
                let margins = MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 };
                let _ = DwmExtendFrameIntoClientArea(self.hwnd, &margins);
            }
            let _ = SetWindowPos(
                self.hwnd,
                None,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            );
        }
        // Drawn again at the next `place`.
        self.rect_changed();
    }

    fn rect_changed(&self) {
        // `place` skips a rect it already holds; a new lens must be drawn.
        if let (Some(l), true) = (self.lens.get(), self.shown) {
            lens::show(self.hwnd, self.rect, self.corner_px(), l);
        }
    }

    /// DWM's corner radius in pixels at this window's scaling.
    fn corner_px(&self) -> i32 {
        let dpi = match unsafe { GetDpiForWindow(self.hwnd) } {
            0 => 96,
            d => d,
        };
        (self.corner.get() * dpi as f32 / 96.0).round() as i32
    }

    /// `glass` picks the kind of blur (see `theme`), `tint` is 0xAARRGGBB and
    /// `radius` picks the DWM corner style.
    pub fn style(&self, glass: Glass, tint: u32, radius: f32) {
        let (a, r, g, b) = (tint >> 24, (tint >> 16) & 0xFF, (tint >> 8) & 0xFF, tint & 0xFF);
        let mut policy = AccentPolicy {
            state: match glass {
                // The ring draws everything itself.
                _ if self.lens.get().is_some() => ACCENT_DISABLED,
                Glass::Acrylic => ACCENT_ENABLE_ACRYLICBLURBEHIND,
                Glass::Blur => ACCENT_ENABLE_BLURBEHIND,
                // Fully see-through: the gradient colour, with its alpha of 0.
                Glass::Clear => ACCENT_ENABLE_TRANSPARENTGRADIENT,
                Glass::Solid => ACCENT_DISABLED,
            },
            // Use the gradient colour (only the transparent gradient reads it).
            flags: if glass == Glass::Clear { 2 } else { 0 },
            // ABGR, as the API wants it.
            gradient: (a << 24) | (b << 16) | (g << 8) | r,
            animation: 0,
        };
        let mut data = CompositionData {
            attribute: WCA_ACCENT_POLICY,
            data: &mut policy as *mut _ as *mut c_void,
            size: std::mem::size_of::<AccentPolicy>(),
        };
        let (corners, corner) = if radius >= 6.0 {
            (DWMWCP_ROUND, 8.0)
        } else if radius >= 2.0 {
            (DWMWCP_ROUNDSMALL, 4.0)
        } else {
            (DWMWCP_DONOTROUND, 0.0)
        };
        self.corner.set(corner);
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
        if r == self.rect && self.shown && !self.reshow.get() {
            return;
        }
        self.reshow.set(false);
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
        self.rect_changed();
    }

    /// Like `place`, but cut to a rounded rect `shape` (window-relative px,
    /// which may reach past the window) with corners of `radius` px. For a
    /// panel against a screen edge shared with another screen, where the
    /// blur can't run past the edge to hide its own corners there. `None`
    /// leaves the window whole.
    pub fn place_shaped(&mut self, r: RECT, shape: Option<(RECT, i32)>, content: HWND) {
        if shape != self.shape {
            self.shape = shape;
            unsafe {
                let rgn = shape
                    .map(|(s, rad)| CreateRoundRectRgn(s.left, s.top, s.right + 1, s.bottom + 1, rad * 2, rad * 2));
                // The window owns the region from here on.
                SetWindowRgn(self.hwnd, rgn, true);
            }
        }
        self.place(r, content);
    }

    /// For a panel that moves or changes size from frame to frame: puts the
    /// blur at `r` (cut to `shape`, as in `place_shaped`) once it holds
    /// still, and while it moves pulls each side of the blur in by as far as
    /// that side moved since the last frame. The blur and the panel are two
    /// windows, so one can reach the screen a frame before the other; pulled
    /// in, the blur stays inside the panel either way instead of showing
    /// past its edge, and is cut to rounded corners of its own (DWM doesn't
    /// keep a window's corners round while it is resized every frame, so
    /// square ones would show inside the panel's round ones). Call it after
    /// the panel's frame is presented. Returns
    /// true while the blur is pulled in: draw one more frame, so it ends up
    /// exactly at `r`.
    pub fn track(&mut self, r: RECT, shape: Option<(RECT, i32)>, content: HWND) -> bool {
        let last = self.last.unwrap_or(r);
        self.last = Some(r);
        // A side that moved d pixels may move about d more by the next frame.
        let d = |a: i32, b: i32| if a == b { 0 } else { (a - b).abs() + 1 };
        let (dl, dt, dr, db) =
            (d(r.left, last.left), d(r.top, last.top), d(r.right, last.right), d(r.bottom, last.bottom));
        let inner = RECT { left: r.left + dl, top: r.top + dt, right: r.right - dr, bottom: r.bottom - db };
        if inner.right - inner.left < 2 || inner.bottom - inner.top < 2 {
            self.hide_blur();
            return true;
        }
        let moving = inner != r;
        let shape = match shape {
            // The cut-out shape is relative to the window, which moved with `inner`.
            Some((s, rad)) => {
                Some((RECT { left: s.left - dl, top: s.top - dt, right: s.right - dl, bottom: s.bottom - dt }, rad))
            }
            None if moving => {
                let rad = self.corner_px();
                let size = RECT { left: 0, top: 0, right: inner.right - inner.left, bottom: inner.bottom - inner.top };
                (rad > 0).then_some((size, rad))
            }
            None => None,
        };
        self.place_shaped(inner, shape, content);
        moving
    }

    /// Hides the blur while the panel at `r` is shown without it, keeping
    /// track of where the panel is for when `track` brings the blur back.
    pub fn hold(&mut self, r: RECT) {
        self.last = Some(r);
        self.hide_blur();
    }

    pub fn hide(&mut self) {
        self.last = None;
        self.hide_blur();
    }

    fn hide_blur(&mut self) {
        if self.shown {
            self.shown = false;
            lens::hide(self.hwnd);
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
        }
    }
}
