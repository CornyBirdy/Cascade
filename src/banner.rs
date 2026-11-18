//! Notification pop-ups in shell mode: Windows' own banners are Explorer's.
//! A new notification slides up at the bottom right of the main screen, in
//! Cascade's theme: the app, the title and the text. Click it to open the
//! app (the notification goes, as in Windows); its × only puts the banner
//! away (the notification stays in the widgets panel's list). It goes by
//! itself after a few seconds, unless the pointer is resting on it. Do Not
//! Disturb keeps it quiet.
//!
//! What Windows' banners have that this can't: an app's own buttons and
//! quick-reply box. Windows lets other programs read notifications, but only
//! the notification platform (Explorer's) can press their buttons.

use windows::core::Result;
use windows::Win32::Foundation::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::backdrop::Backdrop;
use crate::gfx::{Align, Color, Gfx, Layer};
use crate::notifications::Notif;
use crate::spring::Spring;
use crate::theme;

const W: f32 = 364.0;
const H: f32 = 96.0;
const PAD: f32 = 14.0;
const MARGIN: f32 = 12.0;
const CLOSE: f32 = 26.0;
/// How long a banner stays up.
const SHOW_MS: u32 = 6000;
pub const TIMER_HIDE: usize = 1;

pub enum Action {
    /// Open the app the notification came from (and remove it).
    Open(Notif),
}

pub struct Banner {
    pub hwnd: HWND,
    layer: Layer,
    backdrop: Backdrop,
    reveal: Spring,
    visible: bool,
    shown: bool,
    item: Option<Notif>,
    /// More arrived while this one was up.
    more: usize,
    hovered: bool,
    over_close: bool,
    tracking: bool,
    /// Where it sits: the main screen's free area's bottom right.
    work: RECT,
    k: f32,
    acrylic: bool,
    opacity: f32,
    radius: f32,
    pub dirty: bool,
}

impl Banner {
    pub fn new(hwnd: HWND, backdrop: Backdrop) -> Self {
        Self {
            hwnd,
            layer: Layer::default(),
            backdrop,
            reveal: Spring::new(0.0, 480.0, 40.0),
            visible: false,
            shown: false,
            item: None,
            more: 0,
            hovered: false,
            over_close: false,
            tracking: false,
            work: RECT::default(),
            k: 1.0,
            acrylic: true,
            opacity: 0.55,
            radius: 8.0,
            dirty: false,
        }
    }

    pub fn configure(&mut self, acrylic: bool, opacity: f32, radius: f32) {
        self.acrylic = theme::style_backdrop(&self.backdrop, acrylic, opacity, radius);
        self.opacity = opacity.clamp(0.0, 1.0);
        self.radius = radius;
        self.dirty = true;
    }

    /// Shows `n` above the taskbar of the screen whose free area is `work`.
    pub fn show(&mut self, n: Notif, work: RECT, k: f32) {
        if self.shown && self.item.is_some() {
            self.more += 1;
        } else {
            self.more = 0;
        }
        self.item = Some(n);
        self.work = work;
        self.k = k;
        self.shown = true;
        if !self.visible {
            self.visible = true;
            self.reveal.snap(0.0);
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
                let _ =
                    SetWindowPos(self.hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }
        }
        self.reveal.set(1.0);
        self.arm();
        self.dirty = true;
    }

    fn arm(&self) {
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_HIDE, SHOW_MS, None);
        }
    }

    /// Puts it away (the notification stays in the list).
    pub fn hide(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_HIDE);
        }
        self.shown = false;
        self.reveal.set(0.0);
        self.dirty = true;
    }

    pub fn on_timer(&mut self) {
        // Not while the pointer rests on it.
        if self.hovered {
            self.arm();
            return;
        }
        self.hide();
    }

    fn close_hit(&self, x: i32, y: i32) -> bool {
        let (lx, ly) = (x as f32 / self.k, y as f32 / self.k);
        lx >= W - CLOSE - 6.0 && ly <= CLOSE + 6.0
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
        let close = self.close_hit(x, y);
        if !self.hovered || close != self.over_close {
            self.hovered = true;
            self.over_close = close;
            self.dirty = true;
        }
    }

    pub fn on_leave(&mut self) {
        self.tracking = false;
        self.hovered = false;
        self.over_close = false;
        self.dirty = true;
        self.arm();
    }

    pub fn on_click(&mut self, x: i32, y: i32) -> Option<Action> {
        if self.close_hit(x, y) {
            self.hide();
            return None;
        }
        let item = self.item.take();
        self.hide();
        item.map(Action::Open)
    }

    pub fn wants_frame(&self) -> bool {
        self.visible && (self.dirty || !self.reveal.settled())
    }

    pub fn step(&mut self, dt: f32) -> bool {
        self.reveal.step(dt)
    }

    pub fn render(&mut self, gfx: &mut Gfx) -> Result<()> {
        self.dirty = false;
        if !self.visible {
            return Ok(());
        }
        if !self.shown && self.reveal.value < 0.01 {
            self.visible = false;
            self.backdrop.hide();
            self.layer.release();
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
            return Ok(());
        }
        let Some(n) = self.item.clone() else { return Ok(()) };
        let k = self.k;
        let (pw, ph) = ((W * k).ceil() as i32, (H * k).ceil() as i32);
        let reveal = self.reveal.value.clamp(0.0, 1.0);
        let margin = (MARGIN * k) as i32;
        let x = self.work.right - pw - margin;
        let y = self.work.bottom - ph - margin + ((1.0 - reveal) * 18.0 * k) as i32;
        let glass = self.acrylic && reveal > theme::GLASS_FROM;
        gfx.begin(&mut self.layer, pw, ph, k)?;
        {
            let p = gfx.painter();
            let f = &gfx.fonts;
            theme::panel(&p, 0.0, 0.0, W, H, self.radius, glass, self.opacity, 0.97, 1.0);
            if self.hovered {
                p.rrect(0.0, 0.0, W, H, self.radius, Color::white(0.04));
            }
            let app = if self.more > 0 { format!("{}  \u{b7}  {} more", n.app, self.more) } else { n.app.clone() };
            p.text(&app, &f.small, PAD, 10.0, W - PAD * 2.0 - CLOSE, 18.0, Align::Left, Color::white(0.6));
            p.text(&n.title, &f.body, PAD, 30.0, W - PAD * 2.0, 22.0, Align::Left, Color::white(0.95));
            p.text(&n.body, &f.small, PAD, 54.0, W - PAD * 2.0, 34.0, Align::Left, Color::white(0.72));
            // The ×, which only puts the banner away.
            let (cx, cy) = (W - CLOSE / 2.0 - 8.0, CLOSE / 2.0 + 6.0);
            if self.over_close {
                p.rrect(cx - CLOSE / 2.0, cy - CLOSE / 2.0, CLOSE, CLOSE, 6.0, Color::white(0.1));
            }
            let a = if self.hovered { 0.8 } else { 0.4 };
            p.line(cx - 4.5, cy - 4.5, cx + 4.5, cy + 4.5, Color::white(a), 1.3);
            p.line(cx + 4.5, cy - 4.5, cx - 4.5, cy + 4.5, Color::white(a), 1.3);
        }
        let alpha = (reveal * 255.0) as u8;
        let shown = gfx.present(&self.layer, self.hwnd, POINT { x, y }, pw, ph, alpha);
        let screen = RECT { left: x, top: y, right: x + pw, bottom: y + ph };
        if !glass {
            self.backdrop.hold(screen);
        } else if self.backdrop.track(screen, None, self.hwnd) {
            self.dirty = true;
        }
        shown
    }
}
