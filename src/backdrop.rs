//! Acrylic (or plain) blur behind panels (taskbar islands, Start menu, the
//! notch's lip).
//!
//! A plain window the size of the panel sits just under the drawn content
//! and asks DWM to blur what's behind it. The blur runs on the GPU inside
//! DWM, the same way the Windows taskbar does it, and keeps working while the
//! window is inactive (unlike the newer system backdrops).
//!
//! Liquid Glass with Refraction on is drawn on the GPU instead (see `glass`):
//! a window of its own under the panel that shows what's behind it bent by
//! its curved edge, in place of DWM's blur. Where that can't work (an old
//! Windows, no Direct3D 11, a screen that can't be duplicated) it falls back
//! to DWM's blur with a second window between the two: a click-through
//! layered ring holding the light bent round the panel's edge (see `lens`).

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

use crate::glass::{Pane, Shown};
use crate::glassmath::Look;
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
/// A see-through window in the gradient's colour: with no colour, nothing
/// at all (clear glass, whose tint the panel paints).
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
    /// The panel's own window, which the blur (and the ring) sit under.
    content: Cell<HWND>,
    /// Light bent round the panel's edge, over the blur (see `lens`).
    lens: Cell<Option<Lens>>,
    /// The layered window that ring is drawn in, made when first needed.
    ring: Cell<Option<HWND>>,
    /// Liquid Glass drawn on the GPU, if the theme asks for it (sizes at
    /// 100% scaling), its pane (made when first shown), whether it's what's
    /// on screen now, and whether it failed here (so DWM's blur is used).
    glass: Cell<Option<Look>>,
    pane: Option<Pane>,
    glass_on: bool,
    glass_failed: Cell<bool>,
    /// The glass asked for one more frame of the panel (see `track`).
    again: bool,
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
                content: Cell::new(HWND::default()),
                lens: Cell::new(None),
                ring: Cell::new(None),
                glass: Cell::new(None),
                pane: None,
                glass_on: false,
                glass_failed: Cell::new(false),
                again: false,
            })
        }
    }

    /// DWM's corner radius in pixels at the panel's scaling (its own window's,
    /// once known: this one stays hidden, so unmoved, under the GPU glass).
    fn corner_px(&self) -> i32 {
        let content = self.content.get();
        let at = if content.is_invalid() { self.hwnd } else { content };
        let dpi = match unsafe { GetDpiForWindow(at) } {
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
                Glass::Acrylic => ACCENT_ENABLE_ACRYLICBLURBEHIND,
                Glass::Blur => ACCENT_ENABLE_BLURBEHIND,
                Glass::Solid => ACCENT_DISABLED,
                Glass::Clear => ACCENT_ENABLE_TRANSPARENTGRADIENT,
            },
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

    /// Puts the blur back directly under `content` (after `content` was
    /// brought to the top), where it is.
    pub fn restack(&self, content: HWND) {
        if self.glass_on {
            if let Some(p) = &self.pane {
                p.restack(content);
            }
            return;
        }
        if !self.shown {
            return;
        }
        let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE;
        unsafe {
            match self.ring.get().filter(|_| self.lens.get().is_some()) {
                // The ring right under the content, the blur under the ring.
                Some(ring) => {
                    let _ = SetWindowPos(ring, Some(content), 0, 0, 0, 0, flags);
                    let _ = SetWindowPos(self.hwnd, Some(ring), 0, 0, 0, 0, flags);
                }
                None => {
                    let _ = SetWindowPos(self.hwnd, Some(content), 0, 0, 0, 0, flags);
                }
            }
        }
    }

    /// Draws the panel's glass on the GPU (see `glass`), looking like `look`,
    /// instead of DWM's blur, or (`None`) goes back to DWM's blur. Where the
    /// GPU glass can't work the blur (and the ring of `set_lens`) stays.
    /// Call before `style`.
    pub fn set_glass(&self, look: Option<Look>) {
        if look == self.glass.get() {
            return;
        }
        self.glass.set(look);
        // A new look is worth another try.
        self.glass_failed.set(false);
        if look.is_none() {
            if let Some(p) = &self.pane {
                p.hide();
            }
        }
    }

    /// Adds a ring of light bent round the panel's edge (see `lens`) over
    /// the blur, or (`None`) takes it away. Call before `style`.
    pub fn set_lens(&self, lens: Option<Lens>) {
        if lens == self.lens.get() {
            return;
        }
        self.lens.set(lens);
        if lens.is_none() {
            self.hide_ring();
        }
        self.rect_changed();
    }

    /// The ring's window: layered (per-pixel alpha), click-through, never
    /// activated. Made the first time it's needed.
    fn ring_hwnd(&self) -> Option<HWND> {
        if let Some(h) = self.ring.get() {
            return Some(h);
        }
        let h = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_TRANSPARENT,
                w!("CascadeBackdrop"),
                w!(""),
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                GetModuleHandleW(None).ok().map(|m| HINSTANCE(m.0)),
                None,
            )
        }
        .ok()?;
        self.ring.set(Some(h));
        Some(h)
    }

    fn hide_ring(&self) {
        if let Some(h) = self.ring.get() {
            lens::hide(h);
            unsafe {
                let _ = ShowWindow(h, SW_HIDE);
            }
        }
    }

    /// Moves the ring with the blur and redraws it for the new rect.
    fn rect_changed(&self) {
        if let (Some(l), true) = (self.lens.get(), self.shown) {
            if let Some(ring) = self.ring_hwnd() {
                unsafe {
                    let r = self.rect;
                    let _ = SetWindowPos(
                        ring,
                        Some(self.content.get()),
                        r.left,
                        r.top,
                        (r.right - r.left).max(1),
                        (r.bottom - r.top).max(1),
                        SWP_NOACTIVATE | SWP_SHOWWINDOW,
                    );
                }
                lens::show(ring, self.rect, self.corner_px(), l);
            }
        }
    }

    /// Puts the blur at `r` (screen px), directly under `content`.
    pub fn place(&mut self, r: RECT, content: HWND) {
        if self.place_glass(r, content) {
            return;
        }
        if self.glass_on {
            // The GPU glass gave up: DWM's blur takes over.
            self.glass_on = false;
            if let Some(p) = &self.pane {
                p.hide();
            }
        }
        if r == self.rect && self.shown {
            return;
        }
        self.rect = r;
        self.shown = true;
        self.content.set(content);
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

    /// Shows the GPU glass at `r` in place of DWM's blur, if the theme asks for
    /// it and it works here. False to use DWM's blur.
    fn place_glass(&mut self, r: RECT, content: HWND) -> bool {
        let Some(look) = self.glass.get() else { return false };
        if self.glass_failed.get() {
            return false;
        }
        if self.pane.is_none() {
            self.pane = Pane::new();
        }
        self.content.set(content);
        let radius = self.corner_px();
        let shown = match &self.pane {
            Some(p) => p.show(r, self.shape, radius, look, content),
            None => Shown::No,
        };
        match shown {
            Shown::Yes { again } => {
                if self.shown {
                    // DWM's blur and the ring step aside.
                    self.shown = false;
                    self.hide_ring();
                    unsafe {
                        let _ = ShowWindow(self.hwnd, SW_HIDE);
                    }
                }
                self.rect = r;
                self.content.set(content);
                self.glass_on = true;
                self.again |= again;
                true
            }
            Shown::No => {
                self.glass_failed.set(true);
                false
            }
        }
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
        moving || std::mem::take(&mut self.again)
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
        if self.glass_on {
            self.glass_on = false;
            if let Some(p) = &self.pane {
                p.hide();
            }
        }
        if self.shown {
            self.shown = false;
            self.hide_ring();
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
        }
    }
}
