//! Window previews above the dock. Live thumbnails are composited by DWM
//! itself (DwmRegisterThumbnail), so Cascade never copies window pixels.
//!
//! Resting the pointer on a preview peeks at its window, as Windows' own
//! taskbar does: every other window turns to glass, so you can see where
//! that one is. That's DWM's live preview (the same call alternative
//! taskbars use); Cascade's own windows are kept out of it.

use std::sync::OnceLock;

use windows::core::{w, Result, BOOL, HRESULT, PCSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT};
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::*;

use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::System::Threading::GetCurrentThreadId;

use crate::gfx::{Align, Color, Gfx};
use crate::theme;

const ITEM_W: f32 = 220.0;
const THUMB_W: f32 = 204.0;
const THUMB_H: f32 = 128.0;
const TITLE_H: f32 = 26.0;
const PAD: f32 = 8.0;
const HEIGHT: f32 = PAD + TITLE_H + THUMB_H + PAD + 4.0;
/// How long the pointer rests on a preview before its window is peeked at.
const PEEK_MS: u32 = 400;
pub const TIMER_PEEK: usize = 1;

/// dwmapi's DwmpActivateLivePreview (ordinal 113): (on, the window to show,
/// a window to keep on top, 3 = a window peek, reserved).
type LivePreview = unsafe extern "system" fn(u32, HWND, HWND, u32, usize) -> HRESULT;

fn live_preview() -> Option<LivePreview> {
    static F: OnceLock<Option<LivePreview>> = OnceLock::new();
    *F.get_or_init(|| unsafe {
        let dwm = GetModuleHandleW(w!("dwmapi.dll")).ok()?;
        let f = GetProcAddress(dwm, PCSTR(113 as *const u8))?;
        // SAFETY: ordinal 113 of dwmapi is DwmpActivateLivePreview, with this signature.
        Some(std::mem::transmute::<unsafe extern "system" fn() -> isize, LivePreview>(f))
    })
}

/// Keeps every window of Cascade's (on this, the UI thread) visible during
/// a peek: the taskbar, its glass, the notch and the previews themselves.
fn exclude_own_from_peek() {
    unsafe extern "system" fn each(h: HWND, _: LPARAM) -> BOOL {
        let on = BOOL(1);
        let _ = DwmSetWindowAttribute(h, DWMWA_EXCLUDED_FROM_PEEK, &on as *const _ as *const _, 4);
        true.into()
    }
    unsafe {
        let _ = EnumThreadWindows(GetCurrentThreadId(), Some(each), LPARAM(0));
    }
}

struct Item {
    window: HWND,
    thumb: isize,
    title: String,
}

pub enum Action {
    Focus(HWND),
    Close(HWND),
}

pub struct Preview {
    pub hwnd: HWND,
    pub app: Option<String>,
    items: Vec<Item>,
    hovered: Option<usize>,
    hover_close: bool,
    tracking: bool,
    k: f32,
    size: (i32, i32),
    /// Peek at a window while the pointer rests on its preview (`taskbar_peek`).
    pub peek: bool,
    peeking: bool,
}

impl Preview {
    pub fn new(hwnd: HWND) -> Self {
        unsafe {
            let round = DWMWCP_ROUND;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &round as *const _ as *const _,
                std::mem::size_of_val(&round) as u32,
            );
            let dark = BOOL(1);
            let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as *const _, 4);
        }
        Self {
            hwnd,
            app: None,
            items: Vec::new(),
            hovered: None,
            hover_close: false,
            tracking: false,
            k: 1.0,
            size: (0, 0),
            peek: true,
            peeking: false,
        }
    }

    /// The pointer has rested on a preview: peek at its window.
    pub fn on_timer(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_PEEK);
        }
        let Some(target) = self.hovered.filter(|_| !self.hover_close).and_then(|i| self.items.get(i)) else {
            return;
        };
        let Some(f) = live_preview() else { return };
        exclude_own_from_peek();
        unsafe {
            let _ = f(1, target.window, self.hwnd, 3, 0);
        }
        self.peeking = true;
    }

    /// Ends a peek (or one about to start).
    fn unpeek(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_PEEK);
        }
        if self.peeking {
            self.peeking = false;
            if let Some(f) = live_preview() {
                unsafe {
                    let _ = f(0, HWND::default(), HWND::default(), 3, 0);
                }
            }
        }
    }

    pub fn visible(&self) -> bool {
        self.app.is_some()
    }

    /// Shows previews of `windows` centred above `anchor_x`, ending at `bottom`.
    pub fn show(&mut self, app: &str, windows: &[HWND], anchor_x: i32, bottom: i32, monitor: RECT, k: f32) {
        self.clear_thumbs();
        self.k = k;
        let max = (((monitor.right - monitor.left) as f32 / k - PAD) / (ITEM_W + PAD)).max(1.0) as usize;
        for &w in windows.iter().take(max) {
            let mut title = [0u16; 256];
            let n = unsafe { GetWindowTextW(w, &mut title) } as usize;
            let thumb = unsafe { DwmRegisterThumbnail(self.hwnd, w) }.unwrap_or(0);
            self.items.push(Item { window: w, thumb, title: String::from_utf16_lossy(&title[..n]) });
        }
        if self.items.is_empty() {
            self.hide();
            return;
        }
        let width = PAD + self.items.len() as f32 * (ITEM_W + PAD);
        let (pw, ph) = ((width * k).round() as i32, (HEIGHT * k).round() as i32);
        let x = (anchor_x - pw / 2).clamp(monitor.left + 4, (monitor.right - pw - 4).max(monitor.left));
        let y = bottom - ph;
        self.size = (pw, ph);
        self.app = Some(app.to_string());
        self.hovered = None;
        unsafe {
            let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), x, y, pw, ph, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        }
        self.place_thumbs();
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn item_x(i: usize) -> f32 {
        PAD + i as f32 * (ITEM_W + PAD)
    }

    fn place_thumbs(&self) {
        let k = self.k;
        for (i, it) in self.items.iter().enumerate() {
            if it.thumb == 0 {
                continue;
            }
            let src = unsafe { DwmQueryThumbnailSourceSize(it.thumb) }.unwrap_or(SIZE { cx: 16, cy: 9 });
            let aspect = src.cx.max(1) as f32 / src.cy.max(1) as f32;
            let (mut w, mut h) = (THUMB_W, THUMB_W / aspect);
            if h > THUMB_H {
                h = THUMB_H;
                w = THUMB_H * aspect;
            }
            let x = Self::item_x(i) + (ITEM_W - w) / 2.0;
            let y = PAD + TITLE_H + (THUMB_H - h) / 2.0;
            let props = DWM_THUMBNAIL_PROPERTIES {
                dwFlags: DWM_TNP_RECTDESTINATION | DWM_TNP_VISIBLE | DWM_TNP_OPACITY | DWM_TNP_SOURCECLIENTAREAONLY,
                rcDestination: RECT {
                    left: (x * k) as i32,
                    top: (y * k) as i32,
                    right: ((x + w) * k) as i32,
                    bottom: ((y + h) * k) as i32,
                },
                opacity: 255,
                fVisible: true.into(),
                fSourceClientAreaOnly: false.into(),
                ..Default::default()
            };
            unsafe {
                let _ = DwmUpdateThumbnailProperties(it.thumb, &props);
            }
        }
    }

    fn clear_thumbs(&mut self) {
        for it in self.items.drain(..) {
            if it.thumb != 0 {
                unsafe {
                    let _ = DwmUnregisterThumbnail(it.thumb);
                }
            }
        }
    }

    pub fn hide(&mut self) {
        self.unpeek();
        self.clear_thumbs();
        self.app = None;
        self.hovered = None;
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    fn hit(&self, x: i32, y: i32) -> (Option<usize>, bool) {
        let (lx, ly) = (x as f32 / self.k, y as f32 / self.k);
        for i in 0..self.items.len() {
            let ix = Self::item_x(i);
            if (ix..ix + ITEM_W).contains(&lx) && (0.0..HEIGHT).contains(&ly) {
                let close = lx >= ix + ITEM_W - 28.0 && ly < PAD + TITLE_H;
                return (Some(i), close);
            }
        }
        (None, false)
    }

    pub fn on_move(&mut self, x: i32, y: i32) {
        unsafe {
            if !self.tracking {
                let mut t = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: self.hwnd,
                    dwHoverTime: 0,
                };
                self.tracking = TrackMouseEvent(&mut t).is_ok();
            }
        }
        let (hovered, close) = self.hit(x, y);
        if hovered != self.hovered || close != self.hover_close {
            // A different preview (or its close button): the peek starts over.
            let peeked = self.peeking;
            self.unpeek();
            if self.peek && hovered.is_some() && !close {
                // Straight on to the next window while already peeking.
                let wait = if peeked { 60 } else { PEEK_MS };
                unsafe {
                    SetTimer(Some(self.hwnd), TIMER_PEEK, wait, None);
                }
            }
            self.hovered = hovered;
            self.hover_close = close;
            unsafe {
                let _ = InvalidateRect(Some(self.hwnd), None, false);
            }
        }
    }

    pub fn on_leave(&mut self) {
        self.unpeek();
        self.tracking = false;
        self.hovered = None;
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    pub fn on_click(&mut self, x: i32, y: i32) -> Option<Action> {
        self.unpeek();
        match self.hit(x, y) {
            (Some(i), true) => Some(Action::Close(self.items[i].window)),
            (Some(i), false) => Some(Action::Focus(self.items[i].window)),
            _ => None,
        }
    }

    pub fn paint(&self, gfx: &mut Gfx) -> Result<()> {
        let mut ps = PAINTSTRUCT::default();
        let hdc = unsafe { BeginPaint(self.hwnd, &mut ps) };
        let r = (|| {
            let (r, g, b) = theme::current().solid;
            gfx.begin_dc(hdc, self.size.0, self.size.1, self.k, Color(r, g, b, 1.0))?;
            {
                let p = gfx.painter();
                for (i, it) in self.items.iter().enumerate() {
                    let x = Self::item_x(i);
                    let hovered = self.hovered == Some(i);
                    if hovered {
                        p.rrect(x, 4.0, ITEM_W, HEIGHT - 8.0, 10.0, Color::white(0.08));
                    }
                    let title_w = ITEM_W - if hovered { 40.0 } else { 16.0 };
                    p.text(
                        &it.title,
                        &gfx.fonts.label,
                        x + 8.0,
                        PAD,
                        title_w,
                        TITLE_H - 4.0,
                        Align::Left,
                        Color::white(0.9),
                    );
                    if hovered {
                        let (cx, cy) = (x + ITEM_W - 16.0, PAD + (TITLE_H - 4.0) / 2.0);
                        if self.hover_close {
                            p.circle(cx, cy, 10.0, Color(0.85, 0.25, 0.25, 1.0));
                        }
                        p.line(cx - 4.0, cy - 4.0, cx + 4.0, cy + 4.0, Color::white(0.9), 1.5);
                        p.line(cx - 4.0, cy + 4.0, cx + 4.0, cy - 4.0, Color::white(0.9), 1.5);
                    }
                    if it.thumb == 0 {
                        p.rrect(x + 8.0, PAD + TITLE_H, THUMB_W, THUMB_H, 6.0, Color::white(0.05));
                    }
                }
            }
            gfx.end()
        })();
        unsafe {
            let _ = EndPaint(self.hwnd, &ps);
        }
        r
    }
}
