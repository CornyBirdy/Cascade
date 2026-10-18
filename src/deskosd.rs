//! The desktop switcher pop-up: switch virtual desktops (Ctrl+Win+Left or
//! Right, the taskbar's desktops widget, Task View) and a strip of your
//! desktops shows in the middle of the screen for a moment, the one you're
//! on now lit and named, the way GNOME and KDE show a workspace change.
//!
//! It never takes the mouse or the keyboard, and fades out by itself.

use windows::core::Result;
use windows::Win32::Foundation::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::backdrop::Backdrop;
use crate::desktops::Desktops;
use crate::gfx::{Align, Color, Gfx, Layer};
use crate::spring::Spring;
use crate::theme;

const TILE_W: f32 = 104.0;
const TILE_H: f32 = 64.0;
const GAP: f32 = 10.0;
const PAD: f32 = 16.0;
/// The current desktop's name, under the tiles.
const NAME_H: f32 = 34.0;
/// How long it stays up after the last switch.
pub const SHOW_MS: u32 = 1100;
pub const TIMER_HIDE: usize = 1;

pub struct DeskOsd {
    pub hwnd: HWND,
    layer: Layer,
    backdrop: Backdrop,
    reveal: Spring,
    visible: bool,
    shown: bool,
    desks: Desktops,
    /// Where the lit tile is, sliding from the desktop left to the one arrived at.
    lit: Spring,
    monitor: RECT,
    k: f32,
    acrylic: bool,
    opacity: f32,
    radius: f32,
    pub dirty: bool,
}

impl DeskOsd {
    pub fn new(hwnd: HWND, backdrop: Backdrop) -> Self {
        unsafe {
            // Clicks go through to whatever is under it.
            let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
            SetWindowLongW(hwnd, GWL_EXSTYLE, (ex | WS_EX_TRANSPARENT.0 | WS_EX_NOACTIVATE.0) as i32);
        }
        Self {
            hwnd,
            layer: Layer::default(),
            backdrop,
            reveal: Spring::new(0.0, 520.0, 42.0),
            visible: false,
            shown: false,
            desks: Desktops::default(),
            lit: Spring::new(0.0, 500.0, 40.0),
            monitor: RECT::default(),
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

    /// The desktops changed from `from` to `to`: show the strip on `monitor`
    /// (a switch, not just a rename or a new desktop).
    pub fn switched(&mut self, from: &Desktops, to: &Desktops, monitor: RECT, k: f32) {
        if to.names.len() < 2 {
            return;
        }
        if !self.shown || self.desks.names.len() != to.names.len() {
            // Starts lit where you were, and slides to where you are.
            self.lit.snap(from.current.min(to.names.len() - 1) as f32);
        }
        self.desks = to.clone();
        self.monitor = monitor;
        self.k = k;
        self.shown = true;
        if !self.visible {
            self.visible = true;
            self.reveal.snap(0.0);
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            }
        }
        self.reveal.set(1.0);
        self.lit.set(to.current as f32);
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_HIDE, SHOW_MS, None);
        }
        self.dirty = true;
    }

    /// Time's up: fade out.
    pub fn on_timer(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_HIDE);
        }
        self.shown = false;
        self.reveal.set(0.0);
        self.dirty = true;
    }

    pub fn hide_now(&mut self) {
        self.shown = false;
        self.visible = false;
        self.reveal.snap(0.0);
        self.backdrop.hide();
        self.layer.release();
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_HIDE);
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    pub fn wants_frame(&self) -> bool {
        self.visible && (self.dirty || !self.reveal.settled() || !self.lit.settled())
    }

    pub fn step(&mut self, dt: f32) -> bool {
        let a = self.reveal.step(dt);
        self.lit.step(dt) || a
    }

    fn size(&self) -> (f32, f32) {
        let n = self.desks.names.len() as f32;
        (PAD * 2.0 + n * TILE_W + (n - 1.0) * GAP, PAD * 2.0 + TILE_H + NAME_H)
    }

    pub fn render(&mut self, gfx: &mut Gfx) -> Result<()> {
        self.dirty = false;
        if !self.visible {
            return Ok(());
        }
        if !self.shown && self.reveal.value < 0.01 {
            self.hide_now();
            return Ok(());
        }
        let k = self.k;
        let (w, h) = self.size();
        let (pw, ph) = ((w * k).ceil() as i32, (h * k).ceil() as i32);
        let m = self.monitor;
        let reveal = self.reveal.value.clamp(0.0, 1.0);
        // A little below the middle, as GNOME's; it rises as it appears.
        let x = m.left + (m.right - m.left - pw) / 2;
        let y = m.top + (m.bottom - m.top) * 3 / 5 - ph / 2 + ((1.0 - reveal) * 12.0 * k) as i32;
        let glass = self.acrylic && reveal > theme::GLASS_FROM;
        gfx.begin(&mut self.layer, pw, ph, k)?;
        {
            let p = gfx.painter();
            let f = &gfx.fonts;
            theme::panel(&p, 0.0, 0.0, w, h, self.radius, glass, self.opacity, 0.97, 1.0);
            let r = self.radius.max(6.0);
            for i in 0..self.desks.names.len() {
                let tx = PAD + i as f32 * (TILE_W + GAP);
                // How lit this tile is, as the light slides along.
                let lit = (1.0 - (self.lit.value - i as f32).abs()).clamp(0.0, 1.0);
                p.rrect(tx, PAD, TILE_W, TILE_H, r, Color::white(0.06 + 0.08 * lit));
                if lit > 0.0 {
                    p.stroke_rrect(tx + 1.0, PAD + 1.0, TILE_W - 2.0, TILE_H - 2.0, r, theme::accent().alpha(lit), 2.0);
                }
                let c = Color::white(0.55 + 0.4 * lit);
                p.text(&(i + 1).to_string(), &f.big, tx, PAD, TILE_W, TILE_H, Align::Center, c);
            }
            let name = self.desks.names.get(self.desks.current).map(String::as_str).unwrap_or("");
            p.text(
                name,
                &f.body,
                PAD,
                PAD + TILE_H + 4.0,
                w - 2.0 * PAD,
                NAME_H - 4.0,
                Align::Center,
                Color::white(0.92),
            );
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
